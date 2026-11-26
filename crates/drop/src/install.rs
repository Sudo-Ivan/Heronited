//! Drop manifest downloads. Chunks are decrypted, checked and spliced into
//! files; updates reuse files whose chunks are unchanged because the server
//! leaves those chunks out of the manifest.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

use heronite_data::models::LocalMetaFile;
use heronite_data::sideload::{save_game, SideloadPlatform, SideloadSpec, ARGS_KEY, ENV_KEY};
use heronite_data::{game_config, json_store, DataPaths};
use heronite_i18n::tr;

use crate::client::{ChunkData, Client, FileEntry, Game, ManifestDetails, VersionDetails, VersionOption};
use crate::{app_name, Server, GAME_KEY, PLATFORM_KEY, SERVER_KEY, SETUP_KEY, VERSION_KEY};

pub struct InstallOutcome {
    pub app_name: String,
    pub executable: String,
    pub folder: String,
    pub windows: bool,
    pub version: String,
}

pub fn install(
    paths: &DataPaths,
    server: &Server,
    client: &Client,
    game: &Game,
    previous: Option<&str>,
    base: Option<&str>,
    progress: impl FnMut(u64, u64),
    cancel: impl FnMut() -> bool,
) -> Result<InstallOutcome, String> {
    install_with(paths, server, client, game, previous, base, progress, cancel)
}

/// Separated so tests can give it a server without a valid certificate.
pub(crate) fn install_with(
    paths: &DataPaths,
    server: &Server,
    client: &Client,
    game: &Game,
    previous: Option<&str>,
    base: Option<&str>,
    mut progress: impl FnMut(u64, u64),
    mut cancel: impl FnMut() -> bool,
) -> Result<InstallOutcome, String> {
    let options = client.versions(&game.id, previous)?;
    let option = choose_version(&options).ok_or_else(|| tr!("Drop has no Linux or Windows version of this game").to_string())?;
    if previous == Some(option.version_id.as_str()) {
        return Err(tr!("This Drop game is already up to date").into());
    }
    let details = client.version(&game.id, &option.version_id)?;
    let launch = choose_launch(&details, &option.platform).ok_or_else(|| tr!("Drop has no launch command for this version").to_string())?;
    if launch.emulator.is_some() {
        return Err(tr!("Drop games that need an emulator package are not supported yet").into());
    }
    let windows = option.platform.eq_ignore_ascii_case("windows");

    let root = base.filter(|s| !s.trim().is_empty()).map(PathBuf::from).unwrap_or_else(|| {
        let home = heronite_platform::dirs::home().unwrap_or_default().to_string_lossy().into_owned();
        PathBuf::from(game_config::default_install_dir(&home))
    });
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let app = app_name(&server.id, &game.id);
    let folder = existing_or_target(paths, &app, &root, &game.m_name, &game.library_path, previous.is_some())?;
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;

    let manifest = client.manifest(&option.version_id, previous)?;
    let depots = client.depots_for(&game.id, &option.version_id)?;
    let installed_before = folder.join(".drop-installing");
    std::fs::write(&installed_before, b"").map_err(|e| e.to_string())?;

    let result = download_files(paths, client, game, &option, &manifest, &depots, &folder, &mut progress, &mut cancel);
    if result.is_err() && previous.is_none() {
        let _ = std::fs::remove_dir_all(&folder);
    }
    let _ = std::fs::remove_file(&installed_before);
    result?;

    let (launch_env, executable, launch_args) =
        parse_command_full(&launch.command, &folder).ok_or_else(|| tr!("Drop's launch command could not be read").to_string())?;
    if windows {
        if !Path::new(&executable).exists() {
            return Err(tr!("Drop's launch program was not installed: {path}", path = executable).into());
        }
    } else {
        // Linux commands may start through a script or an environment prefix. Make the first
        // existing program executable when it is a real file in the install.
        if Path::new(&executable).exists() {
            executable_bit(&executable);
        }
    }

    let spec = SideloadSpec {
        app_name: app.clone(),
        title: game.m_name.clone(),
        executable: executable.clone(),
        platform: if windows { SideloadPlatform::Windows } else { SideloadPlatform::Linux },
        art_cover: String::new(),
        art_square: String::new(),
        browser_url: String::new(),
        custom_user_agent: String::new(),
        launch_full_screen: false,
        run_as: Some(if windows { "windows" } else { "linux" }.into()),
        install: true,
        install_folder: folder.to_string_lossy().into_owned(),
        command: if windows { String::new() } else { shell_command(&launch_env, &executable, &launch_args) },
    };
    save_game(paths, &spec).map_err(|e| format!("{e:#}"))?;
    // `save_game` owns the generic executable entry shape. Drop's executable
    // arguments are Heronited's extra key, added after the merge.
    if windows {
        let args = spec_args(&launch.command, &folder).unwrap_or_default();
        let command_env: Vec<String> = launch_env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let _ = json_store::update_value(&paths.sideload_library(), |doc| {
            if let Some(g) = doc
                .get_mut("games")
                .and_then(serde_json::Value::as_array_mut)
                .and_then(|games| games.iter_mut().find(|g| g.get("app_name").and_then(serde_json::Value::as_str) == Some(app.as_str())))
                .and_then(serde_json::Value::as_object_mut)
            {
                if args.is_empty() {
                    g.shift_remove(ARGS_KEY);
                } else {
                    g.insert(ARGS_KEY.into(), json!(args));
                }
                if command_env.is_empty() {
                    g.shift_remove(ENV_KEY);
                } else {
                    g.insert(ENV_KEY.into(), json!(command_env));
                }
            }
            Ok(())
        });
    }

    let mut metas: LocalMetaFile = json_store::read(&paths.local_meta()).unwrap_or_default();
    let meta = metas.games.entry(app.clone()).or_default();
    meta.extra.insert(SERVER_KEY.into(), json!(server.id));
    meta.extra.insert(GAME_KEY.into(), json!(game.id));
    meta.extra.insert(VERSION_KEY.into(), json!(option.version_id));
    meta.extra.insert(crate::LATEST_KEY.into(), json!(option.version_id));
    meta.extra.insert(PLATFORM_KEY.into(), json!(option.platform));
    let setup: Vec<Value> = details
        .setups
        .iter()
        .filter(|s| s.platform.eq_ignore_ascii_case(&option.platform))
        .filter_map(|s| {
            parse_command_full(&s.command, &folder).map(|(env, exe, args)| {
                json!({
                    "parts": std::iter::once(exe).chain(args).collect::<Vec<_>>(),
                    "env": env.into_iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>(),
                })
            })
        })
        .collect();
    if setup.is_empty() {
        meta.extra.remove(SETUP_KEY);
    } else {
        meta.extra.insert(SETUP_KEY.into(), json!(setup));
    }
    json_store::write(&paths.local_meta(), &metas).map_err(|e| e.to_string())?;

    Ok(InstallOutcome { app_name: app, executable, folder: folder.to_string_lossy().into_owned(), windows, version: option.version_id })
}

pub(crate) fn choose_version(options: &[VersionOption]) -> Option<VersionOption> {
    let platform_order = |p: &str| match p.to_ascii_lowercase().as_str() {
        "linux" => 0,
        "windows" => 1,
        _ => 9,
    };
    options.iter().filter(|o| platform_order(&o.platform) < 9).min_by_key(|o| platform_order(&o.platform)).cloned()
}

fn choose_launch<'a>(details: &'a VersionDetails, platform: &str) -> Option<&'a crate::client::Launch> {
    // Emulator entries need a second Drop package. Prefer an ordinary entry when
    // the version offers both; an emulator-only entry is rejected with a clear error.
    details
        .launches
        .iter()
        .find(|l| l.platform.eq_ignore_ascii_case(platform) && l.emulator.is_none())
        .or_else(|| details.launches.iter().find(|l| l.platform.eq_ignore_ascii_case(platform)))
        .or_else(|| details.launches.iter().find(|l| l.emulator.is_none()))
        .or_else(|| details.launches.first())
}

fn existing_or_target(paths: &DataPaths, app: &str, root: &Path, title: &str, library_path: &str, update: bool) -> Result<PathBuf, String> {
    if let Some(entry) = heronite_data::sideload::find_game(paths, app).ok().flatten() {
        if let Some(folder_value) = entry.pointer("/install/install_path").and_then(serde_json::Value::as_str) {
            let folder = PathBuf::from(folder_value);
            if update || folder.exists() {
                return Ok(folder);
            }
        }
    }
    if update {
        return Err(tr!("The installed Drop files were not found").into());
    }
    // Drop servers name the folder themselves (`libraryPath`). It is a
    // relative path inside the install root, never a server-controlled escape.
    let base = library_folder(library_path).unwrap_or_else(|| folder_name(title));
    for n in 0..100 {
        let name = if n == 0 { base.clone() } else { format!("{base}-{n}") };
        let target = root.join(name);
        if !target.exists() {
            return Ok(target);
        }
        if target.read_dir().map_err(|e| e.to_string())?.next().is_none() {
            return Ok(target);
        }
    }
    Err(tr!("No empty install folder is available").into())
}

fn library_folder(path: &str) -> Option<String> {
    let original = path.trim();
    let p = Path::new(original);
    if original.is_empty()
        || p.is_absolute()
        || p.components().any(|c| matches!(c, Component::ParentDir | Component::Prefix(_) | Component::RootDir))
    {
        return None;
    }
    let path = original.trim_matches(['/', '\\']);
    (!path.is_empty()).then(|| path.to_string())
}

fn folder_name(title: &str) -> String {
    let clean: String = title.chars().map(|c| if c.is_ascii_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
    let clean = clean.trim().trim_matches('.');
    if clean.is_empty() {
        "Drop game".into()
    } else {
        clean.into()
    }
}

fn download_files(
    _paths: &DataPaths,
    client: &Client,
    game: &Game,
    _option: &VersionOption,
    manifest: &ManifestDetails,
    depots: &[String],
    folder: &Path,
    progress: &mut impl FnMut(u64, u64),
    cancel: &mut impl FnMut() -> bool,
) -> Result<(), String> {
    clean_removed_files(folder, &manifest.file_list)?;
    let mut lengths: HashMap<String, u64> = HashMap::new();
    for man in manifest.manifests.values() {
        for chunk in man.chunks.values() {
            for file in &chunk.files {
                let end = file.start + file.length;
                lengths.entry(file.filename.clone()).and_modify(|v| *v = (*v).max(end)).or_insert(end);
            }
        }
    }
    let total = manifest.download_size.max(1);
    let mut done = 0u64;
    progress(0, total);
    for (version_id, man) in &manifest.manifests {
        for (chunk_id, chunk) in &man.chunks {
            if cancel() {
                return Err(tr!("Canceled").into());
            }
            let mut last = tr!("Drop did not return a file chunk").to_string();
            let mut ok = None;
            for _try in 0..3 {
                match download_chunk(client, depots, game, version_id, chunk_id, &man.key, chunk, &manifest.file_list, folder, cancel) {
                    Ok(size) => {
                        ok = Some(size);
                        break;
                    }
                    Err(e) => last = e,
                }
                if cancel() {
                    return Err(tr!("Canceled").into());
                }
            }
            let Some(size) = ok else { return Err(last) };
            done += size;
            progress(done.min(total), total);
        }
    }
    progress(total, total);
    // Segments are written without truncating first so multipart files keep their
    // other parts. Once every chunk succeeded, a rewritten file's final size is
    // the end of its last segment; this also removes the tail of a shrunk update.
    for (name, len) in lengths {
        if manifest.file_list.contains_key(&name) {
            let path = safe_path(folder, &name)?;
            if path.exists() {
                OpenOptions::new().write(true).open(&path).map_err(|e| e.to_string())?.set_len(len).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

fn download_chunk(
    client: &Client,
    depots: &[String],
    game: &Game,
    version_id: &str,
    chunk_id: &str,
    key: &[u8; 16],
    chunk: &ChunkData,
    file_list: &HashMap<String, String>,
    folder: &Path,
    cancel: &mut impl FnMut() -> bool,
) -> Result<u64, String> {
    let mut last = tr!("No Drop depot has this game version").to_string();
    for depot in depots {
        match download_chunk_once(client, depot, game, version_id, chunk_id, key, chunk, file_list, folder, cancel) {
            Ok(n) => return Ok(n),
            Err(e) => last = e,
        }
    }
    Err(last)
}

#[allow(clippy::too_many_arguments)]
fn download_chunk_once(
    client: &Client,
    depot: &str,
    game: &Game,
    version_id: &str,
    chunk_id: &str,
    key: &[u8; 16],
    chunk: &ChunkData,
    file_list: &HashMap<String, String>,
    folder: &Path,
    cancel: &mut impl FnMut() -> bool,
) -> Result<u64, String> {
    use ctr::cipher::{KeyIvInit, StreamCipher};
    use sha2::Digest;

    let mut response = client.chunk(depot, &game.id, version_id, chunk_id)?;
    let mut reader = response.body_mut().with_config().limit(u64::MAX).reader();
    let mut cipher = ctr::Ctr64LE::<aes::Aes128>::new(key.into(), (&chunk.iv).into());
    let mut digest = sha2::Sha256::default();
    let mut buf = vec![0u8; 1024 * 256];
    let mut downloaded = 0u64;

    // Entries are in the exact order their plaintext appears in the chunk. `start`
    // is the offset in the destination file, not the offset in the chunk.
    for file in &chunk.files {
        if cancel() {
            return Err(tr!("Canceled").into());
        }
        let should_write = file_list.get(&file.filename).is_some_and(|v| v == version_id);
        let path = safe_path(folder, &file.filename)?;
        let mut handle = if should_write {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut f = OpenOptions::new().create(true).write(true).truncate(false).open(&path).map_err(|e| e.to_string())?;
            f.seek(SeekFrom::Start(file.start)).map_err(|e| e.to_string())?;
            Some(f)
        } else {
            None
        };

        let mut remaining = file.length as usize;
        while remaining > 0 {
            if cancel() {
                return Err(tr!("Canceled").into());
            }
            let take = remaining.min(buf.len());
            let n = reader.read(&mut buf[..take]).map_err(|e| e.to_string())?;
            if n == 0 {
                return Err(tr!("Drop's download ended before the file was complete").into());
            }
            let mut plain = buf[..n].to_vec();
            cipher.apply_keystream(&mut plain);
            digest.update(&plain);
            if let Some(f) = &mut handle {
                f.write_all(&plain).map_err(|e| e.to_string())?;
            }
            remaining -= n;
            downloaded += n as u64;
        }
        drop(handle);
        apply_permissions(folder, file);
    }

    let got = hex::encode(digest.finalize());
    if got != chunk.checksum {
        return Err(tr!("A downloaded Drop file was damaged").into());
    }
    Ok(downloaded)
}

fn apply_permissions(folder: &Path, file: &FileEntry) {
    #[cfg(unix)]
    {
        if let Ok(path) = safe_path(folder, &file.filename) {
            if path.exists() {
                use std::os::unix::fs::PermissionsExt;
                let mode = if file.permissions == 0 { 0o744 } else { file.permissions };
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
            }
        }
    }
}

fn executable_bit(path: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut p = meta.permissions();
            p.set_mode(p.mode() | 0o111);
            let _ = std::fs::set_permissions(path, p);
        }
    }
}

fn safe_path(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute()
        || rel_path.components().any(|c| matches!(c, Component::ParentDir | Component::Prefix(_) | Component::RootDir))
    {
        return Err(tr!("Drop returned an unsafe file path").into());
    }
    Ok(root.join(rel_path))
}

fn clean_removed_files(folder: &Path, file_list: &HashMap<String, String>) -> Result<(), String> {
    if !folder.exists() {
        return Ok(());
    }
    let mut stack = vec![folder.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for item in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let item = item.map_err(|e| e.to_string())?;
            let path = item.path();
            let name = item.file_name().to_string_lossy().into_owned();
            if name == ".drop-installing" {
                continue;
            }
            let kind = item.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(folder).map_err(|e| e.to_string())?.to_string_lossy().replace('\\', "/");
                if !file_list.contains_key(&rel) {
                    std::fs::remove_file(&path).map_err(|e| e.to_string())?;
                }
            }
        }
    }
    // Empty folders are harmless; removing them recursively could surprise people if
    // the install folder contains their own files.
    Ok(())
}

/// Drop command strings start with optional `KEY=value` words. Format fields
/// (`{dir}`, `{exe}`, `{abs_exe}`) are filled here; Drop's optional user
/// launch template is not part of server-provided commands.
pub fn parse_command(command: &str, folder: &Path) -> Option<(String, Vec<String>)> {
    parse_command_full(command, folder).map(|(_, exe, args)| (exe, args))
}

fn parse_command_full(command: &str, folder: &Path) -> Option<(Vec<(String, String)>, String, Vec<String>)> {
    let words = shlex::split(command)?;
    let mut i = 0;
    let mut env = Vec::new();
    let mut raw_env = Vec::new();
    while i < words.len() && is_env(&words[i]) {
        let (key, value) = words[i].split_once('=')?;
        raw_env.push((key.to_string(), value.to_string()));
        i += 1;
    }
    let raw_exe = words.get(i)?.clone();
    let exe = substitute(&raw_exe, folder, &raw_exe);
    let path = if Path::new(&exe).is_absolute() { exe } else { folder.join(&exe).to_string_lossy().into_owned() };
    env.extend(raw_env.into_iter().map(|(k, v)| (k, substitute(&v, folder, &raw_exe))));
    let args = words[i + 1..].iter().map(|a| substitute(a, folder, &raw_exe)).collect();
    Some((env, path, args))
}

fn substitute(value: &str, folder: &Path, exe: &str) -> String {
    let dir = folder.to_string_lossy();
    let abs = if Path::new(exe).is_absolute() { exe.into() } else { folder.join(exe).to_string_lossy().into_owned() };
    value.replace("{dir}", &dir).replace("{exe}", exe).replace("{abs_exe}", &abs)
}

fn shell_command(env: &[(String, String)], executable: &str, args: &[String]) -> String {
    let quote = |v: &str| shlex::try_quote(v).map(|v| v.into_owned()).unwrap_or_else(|_| format!("'{v}'"));
    let mut words: Vec<String> = env.iter().map(|(k, v)| format!("{k}={}", quote(v))).collect();
    words.push(quote(executable));
    words.extend(args.iter().map(|a| quote(a)));
    words.join(" ")
}

fn spec_args(command: &str, folder: &Path) -> Option<Vec<String>> {
    parse_command(command, folder).map(|(_, args)| args)
}

fn is_env(word: &str) -> bool {
    match word.split_once('=') {
        Some((key, _)) => {
            !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !key.chars().next().unwrap().is_ascii_digit()
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_command_and_format_fields() {
        let (exe, args) = parse_command("WINEFSYNC=1 game.exe --dir {dir} --exe {exe}", Path::new("/games/test")).unwrap();
        assert_eq!(exe, "/games/test/game.exe");
        assert_eq!(args, vec!["--dir", "/games/test", "--exe", "game.exe"]);
        let (_, args) = parse_command("./bin/game --exe {exe} --abs {abs_exe}", Path::new("/games/test")).unwrap();
        assert_eq!(args, vec!["--exe", "./bin/game", "--abs", "/games/test/./bin/game"]);
    }

    #[test]
    fn rejects_unsafe_paths() {
        assert!(safe_path(Path::new("/x"), "../evil").is_err());
        assert!(safe_path(Path::new("/x"), "a/b").is_ok());
    }

    #[test]
    fn prefers_native_linux_then_windows() {
        let mut win = VersionOption::default();
        win.platform = "Windows".into();
        let mut lin = VersionOption::default();
        lin.platform = "Linux".into();
        assert_eq!(choose_version(&[win.clone(), lin.clone()]).unwrap().platform, "Linux");
        assert_eq!(choose_version(&[win]).unwrap().platform, "Windows");
    }
    #[test]
    fn decrypts_checks_and_writes_only_wanted_files() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let depot = format!("http://{}/", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0; 8192];
            let n = stream.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(req.starts_with("GET /content/game1/v1/chunk1"));
            assert!(req.to_ascii_lowercase().contains("authorization: jwt client1 "));
            let plain = b"helloworld";
            let key = [7u8; 16];
            let iv = [3u8; 16];
            use ctr::cipher::{KeyIvInit, StreamCipher};
            let mut cipher = ctr::Ctr64LE::<aes::Aes128>::new((&key).into(), (&iv).into());
            let mut encrypted = plain.to_vec();
            cipher.apply_keystream(&mut encrypted);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                encrypted.len()
            )
            .unwrap();
            stream.write_all(&encrypted).unwrap();
        });

        let auth = crate::Auth {
            private: "-----BEGIN PRIVATE KEY-----\nMIG2AgEAMBAGByqGSM49AgEGBSuBBAAiBIGeMIGbAgEBBDD6mi4SPS3hHqtGlIKt\ni4qdaH9TQ4xbhxpLAu8QdIysHz1NBELMtAlk1CUM4c+U74+hZANiAATQRypfalpZ\nxqavnJwrWO1k2NM1Cx9zkK0m1vDos6sdEa0rRaD2xA+f/QsO+n1DbCgQYQZKR1lL\nVXxnLz35+vl8y5eXu0NFk4c2SiPvGKwSjPgSYsX2lzYOv7SK4AqARrc=\n-----END PRIVATE KEY-----\n".into(),
            certificate: String::new(),
            id: "client1".into(),
        };
        let client = Client::new("http://localhost", auth).unwrap();
        let game = Game { id: "game1".into(), ..Default::default() };
        let plain = b"helloworld";
        use sha2::Digest;
        let checksum = hex::encode(sha2::Sha256::digest(plain));
        let chunk = ChunkData {
            files: vec![
                FileEntry { filename: "wanted.txt".into(), start: 0, length: 5, permissions: 0 },
                FileEntry { filename: "skipped.txt".into(), start: 0, length: 5, permissions: 0 },
            ],
            checksum,
            iv: [3u8; 16],
        };
        let files = HashMap::from([("wanted.txt".to_string(), "v1".to_string())]);
        let dir = tempfile::tempdir().unwrap();
        let mut cancel = || false;
        let n = download_chunk_once(&client, &depot, &game, "v1", "chunk1", &[7u8; 16], &chunk, &files, dir.path(), &mut cancel).unwrap();
        assert_eq!(n, 10);
        assert_eq!(std::fs::read(dir.path().join("wanted.txt")).unwrap(), b"hello");
        assert!(!dir.path().join("skipped.txt").exists());
    }

    #[test]
    fn uses_safe_library_folders_and_reads_command_env() {
        assert_eq!(library_folder("Games/My Game").as_deref(), Some("Games/My Game"));
        assert!(library_folder("../evil").is_none());
        assert!(library_folder("/evil").is_none());
        let (env, exe, args) = parse_command_full("WINEDEBUG=-fixme game.exe --x", Path::new("/g")).unwrap();
        assert_eq!(env, [("WINEDEBUG".to_string(), "-fixme".to_string())]);
        assert_eq!(exe, "/g/game.exe");
        assert_eq!(args, ["--x"]);
        assert_eq!(shell_command(&env, "/g/game exe", &["--x".to_string()]), "WINEDEBUG=-fixme '/g/game exe' --x");
    }
}
