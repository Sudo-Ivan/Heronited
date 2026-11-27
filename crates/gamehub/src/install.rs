//! Downloading a GameHub PC game and turning it into an installed local game.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use heronite_data::sideload::{save_game, SideloadPlatform, SideloadSpec};
use heronite_data::DataPaths;
use heronite_i18n::tr;
use heronite_installer::exec::Io;
use heronite_installer::{archive, fsops};

use crate::{app_name, Client, Game};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    pub app_name: String,
    pub executable: PathBuf,
    pub folder: PathBuf,
    pub windows: bool,
}

fn copy_response(
    resp: &mut ureq::http::Response<ureq::Body>,
    dest: &Path,
    progress: &dyn Fn(u64, Option<u64>),
    cancel: &dyn Fn() -> bool,
) -> Result<(), String> {
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).filter(|n| *n > 0);
    let part = dest.with_extension("part");
    let mut file = fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let mut body = resp.body_mut().with_config().limit(u64::MAX).reader();
    let mut buf = vec![0; 256 * 1024];
    let mut got = 0;
    let result = (|| {
        loop {
            if cancel() {
                return Err(tr!("The download was stopped").into());
            }
            let n = body.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| format!("{}: {e}", part.display()))?;
            got += n as u64;
            progress(got, total);
        }
        file.flush().map_err(|e| e.to_string())?;
        if total.is_some_and(|t| got < t) {
            return Err(tr!("The download stopped before the end").into());
        }
        Ok(())
    })();
    match result {
        Ok(()) => fs::rename(&part, dest).map_err(|e| format!("{}: {e}", dest.display())),
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e)
        }
    }
}

fn content_filename(resp: &ureq::http::Response<ureq::Body>) -> Option<String> {
    let v = resp.headers().get("content-disposition")?.to_str().ok()?.to_string();
    let name = v
        .split("filename*=")
        .nth(1)
        .and_then(|s| s.split("''").nth(1))
        .or_else(|| v.split("filename=").nth(1).map(|s| s.trim_matches('"')))?;
    let name = url::form_urlencoded::parse(name.as_bytes()).map(|(k, _)| k).next().unwrap_or_else(|| name.into()).to_string();
    let file = Path::new(&name).file_name()?.to_string_lossy().to_string();
    (!file.is_empty() && file != "." && file != "..").then_some(file)
}

fn filename_ext(resp: &ureq::http::Response<ureq::Body>) -> Option<String> {
    Path::new(&content_filename(resp)?).extension().map(|e| e.to_string_lossy().to_ascii_lowercase())
}

fn download_zip(
    client: &Client,
    game: &Game,
    cache: &Path,
    progress: &dyn Fn(u64, Option<u64>),
    cancel: &dyn Fn() -> bool,
) -> Result<PathBuf, String> {
    let mut resp = client.download(&game.id, game.preferred_version())?;
    let ext = filename_ext(&resp).unwrap_or_else(|| "zip".into());
    let name = content_filename(&resp).unwrap_or_else(|| format!("{}.{}", game.id, ext));
    let name = if Path::new(&name).extension().is_some() { name } else { format!("{name}.zip") };
    let dest = cache.join(name);
    copy_response(&mut resp, &dest, progress, cancel)?;
    Ok(dest)
}

fn score_exe(path: &Path, windows: bool) -> Option<i32> {
    if !path.is_file() {
        return None;
    }
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    let exe = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
    let mut score = match (windows, exe.as_deref()) {
        (true, Some("exe")) => 100,
        (false, None | Some("sh") | Some("appimage") | Some("x86") | Some("x86_64")) => 100,
        _ => return None,
    };
    for bad in ["unins", "setup", "install", "redist", "vcredist", "dxwebsetup", "launcher", "crash", "updater", "helper"] {
        if name.contains(bad) {
            score -= 80;
        }
    }
    if name.contains("game") || name.contains("start") || name.contains("play") {
        score += 10;
    }
    score += (path.metadata().map(|m| m.len()).unwrap_or(0) / (1024 * 1024)).min(20) as i32;
    Some(score)
}

/// The most likely program of an extracted game.
pub fn find_executable(root: &Path, windows: bool) -> Option<PathBuf> {
    let mut best: Option<(i32, usize, PathBuf)> = None;
    for entry in walkdir(root) {
        if let Some(score) = score_exe(&entry, windows) {
            let depth = entry.components().count();
            let replace = best.as_ref().is_none_or(|(s, d, _)| score > *s || (score == *s && depth < *d));
            if replace {
                best = Some((score, depth, entry));
            }
        }
    }
    best.map(|(_, _, p)| p)
}

fn walkdir(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn lower_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

fn setupish_name(name: &str) -> bool {
    name.contains("setup") || name.contains("install")
}

/// Inno Setup and NSIS installers carry these strings near the start. This is
/// the same quick check the install-script engine uses before it decides how
/// to run a Windows setup program.
fn installer_signature(path: &Path) -> bool {
    let mut head = vec![0; 512 * 1024];
    let n = fs::File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    let head = &head[..n];
    let has = |needle: &[u8]| head.windows(needle.len()).any(|w| w == needle);
    has(b"Inno Setup") || has(b"NullsoftInst")
}

/// A setup program worth opening when the zip has no ready-to-run game exe.
/// GameHub serves files as they are. Many libraries therefore contain
/// `hk-setup.exe` rather than an already unpacked game, and treating that file
/// as the game would reinstall it every time it starts.
fn setup_to_extract(root: &Path, windows: bool) -> Option<PathBuf> {
    if !windows {
        return None;
    }
    let exes: Vec<PathBuf> = walkdir(root).into_iter().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))).collect();
    let junk = ["unins", "redist", "vcredist", "dxwebsetup", "updater", "helper", "crash"];
    let ready_game = exes.iter().any(|p| {
        let name = lower_name(p);
        !setupish_name(&name) && !junk.iter().any(|bad| name.contains(bad)) && !installer_signature(p)
    });
    if ready_game {
        return None;
    }
    let setups: Vec<PathBuf> = exes
        .into_iter()
        .filter(|p| {
            let name = lower_name(p);
            !name.contains("unins") && (setupish_name(&name) || installer_signature(p))
        })
        .collect();
    setups.into_iter().max_by_key(|p| {
        let name = lower_name(p);
        let x64 = (name.contains("x64") || name.contains("amd64")) as u8;
        (x64, p.metadata().map(|m| m.len()).unwrap_or(0))
    })
}

/// Installer extraction leaves the archiver's own scaffolding next to the
/// game. The payload is new and private to this install, so those well-known
/// files can go.
fn remove_installer_scaffolding(root: &Path) {
    for entry in fs::read_dir(root).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = lower_name(&path);
        let junk = name.starts_with('$') || name == "[nsis].nsi" || name.starts_with("unins") || name == "install_script.iss";
        if !junk {
            continue;
        }
        let _ = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
    }
}

/// Downloads and installs `game` into `base/<title>`. Blocking. When that
/// folder is taken, the server id keeps a second server's copy separate.
pub fn install(
    paths: &DataPaths,
    client: &Client,
    server_id: &str,
    game: &Game,
    base: &Path,
    progress: &dyn Fn(u64, Option<u64>),
    cancel: &dyn Fn() -> bool,
) -> Result<InstallOutcome, String> {
    let title = if game.title.is_empty() { game.id.clone() } else { game.title.clone() };
    let linux = game.linux_native();
    let has_windows = game.windows_pc();
    if !linux && !has_windows {
        return Err(tr!("{title} is not a Windows or Linux PC game, so Heronited cannot install it", title = title.clone()));
    }
    let windows = has_windows || !linux;
    let folder_name = title
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_') { c } else { '_' })
        .collect::<String>()
        .trim()
        .to_string();
    let name = if folder_name.is_empty() { format!("gamehub-{}", game.id) } else { folder_name };
    let first = base.join(&name);
    let occupied = |path: &Path| path.exists() && fs::read_dir(path).map(|mut d| d.next().is_some()).unwrap_or(true);
    // Two servers often have the same game under the same title. Give the
    // second copy its own folder instead of making one server impossible.
    let folder = if occupied(&first) { base.join(format!("{name}-{server_id}")) } else { first };
    if occupied(&folder) {
        return Err(tr!("{path} already exists. Choose another install folder or uninstall the game first.", path = folder.display()));
    }
    let cache = paths.images_cache_dir().join("gamehub/downloads").join(server_id).join(&game.id);
    let _ = fs::remove_dir_all(&cache);
    let result = (|| {
        fs::create_dir_all(&cache).map_err(|e| format!("{}: {e}", cache.display()))?;
        let file = download_zip(client, game, &cache, progress, cancel)?;
        let io = Io { cancel, log: &|_| () };
        let unpacked = cache.join("unpacked");
        archive::extract(&file, &unpacked, archive::by_name(&file.to_string_lossy()), true, &io)?;
        let source = if let Some(setup) = setup_to_extract(&unpacked, windows) {
            let payload = cache.join("payload");
            archive::extract(&setup, &payload, Some(archive::Format::Exe), true, &io)?;
            remove_installer_scaffolding(&payload);
            payload
        } else {
            unpacked
        };
        fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
        fsops::move_contents(&source, &folder)?;
        let exe = find_executable(&folder, windows).ok_or_else(|| tr!("No runnable game program was found in the download"))?;
        #[cfg(unix)]
        if !windows {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&exe, fs::Permissions::from_mode(0o755));
        }
        let app = app_name(server_id, &game.id);
        let spec = SideloadSpec {
            app_name: app.clone(),
            title: title.clone(),
            executable: exe.to_string_lossy().into_owned(),
            platform: if windows { SideloadPlatform::Windows } else { SideloadPlatform::Linux },
            art_cover: String::new(),
            art_square: String::new(),
            browser_url: String::new(),
            custom_user_agent: String::new(),
            launch_full_screen: false,
            run_as: Some(if windows { "windows" } else { "linux" }.into()),
            install: true,
            install_folder: folder.to_string_lossy().into_owned(),
            command: String::new(),
        };
        save_game(paths, &spec).map_err(|e| format!("{e:#}"))?;
        Ok(InstallOutcome { app_name: app, executable: exe, folder: folder.clone(), windows })
    })();
    let _ = fs::remove_dir_all(&cache);
    if result.is_err() {
        // The folder did not exist before this install: a failed download or
        // extraction must not block the retry with its remains.
        let _ = fs::remove_dir_all(&folder);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_game_program_not_setup_or_uninstall() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("bin/setup.exe"), b"x").unwrap();
        fs::write(root.join("bin/unins000.exe"), b"x").unwrap();
        fs::write(root.join("bin/MyGame.exe"), vec![0; 2 * 1024 * 1024]).unwrap();
        assert_eq!(find_executable(root, true).unwrap().file_name().unwrap(), "MyGame.exe");
    }

    #[test]
    fn installer_only_download_chooses_the_setup_for_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("hk-setup.exe"), b"MZ Inno Setup payload").unwrap();
        fs::write(root.join("unins000.exe"), b"x").unwrap();
        assert_eq!(setup_to_extract(root, true).unwrap().file_name().unwrap(), "hk-setup.exe");
    }

    #[test]
    fn a_ready_game_exe_stops_installer_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("setup.exe"), b"MZ Inno Setup payload").unwrap();
        fs::write(root.join("MyGame.exe"), b"MZ game").unwrap();
        assert!(setup_to_extract(root, true).is_none());
    }

    #[test]
    fn an_installer_is_recognized_without_setup_in_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("HollowKnight.exe"), b"MZ Inno Setup payload").unwrap();
        assert_eq!(setup_to_extract(root, true).unwrap().file_name().unwrap(), "HollowKnight.exe");
    }

    #[test]
    fn refuses_console_only_games_before_downloading() {
        let data = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(data.path());
        let base = tempfile::tempdir().unwrap();
        let client = Client::new("http://localhost:1").unwrap();
        let game = Game {
            id: "mac-only".into(),
            title: "Mac only".into(),
            os: Some(crate::OsSupport { windows: false, mac: true, linux: None }),
            ..Default::default()
        };
        let err = install(&paths, &client, "s", &game, base.path(), &|_, _| {}, &|| false).unwrap_err();
        assert!(err.contains("not a Windows or Linux PC game"));
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in bytes {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }

    fn zip_one(name: &str, data: &[u8]) -> Vec<u8> {
        let crc = crc32(data);
        let mut out = Vec::new();
        let push16 = |out: &mut Vec<u8>, v: u16| out.extend_from_slice(&v.to_le_bytes());
        let push32 = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
        push32(&mut out, 0x0403_4b50);
        push16(&mut out, 20);
        push16(&mut out, 0);
        push16(&mut out, 0);
        push16(&mut out, 0);
        push16(&mut out, 0);
        push32(&mut out, crc);
        push32(&mut out, data.len() as u32);
        push32(&mut out, data.len() as u32);
        push16(&mut out, name.len() as u16);
        push16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        let cd = out.len() as u32;
        push32(&mut out, 0x0201_4b50);
        push16(&mut out, 20);
        push16(&mut out, 20);
        push16(&mut out, 0);
        push16(&mut out, 0);
        push16(&mut out, 0);
        push16(&mut out, 0);
        push32(&mut out, crc);
        push32(&mut out, data.len() as u32);
        push32(&mut out, data.len() as u32);
        push16(&mut out, name.len() as u16);
        for _ in 0..4 {
            push16(&mut out, 0);
        }
        push32(&mut out, 0);
        push32(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        let cd_len = out.len() as u32 - cd;
        push32(&mut out, 0x0605_4b50);
        push16(&mut out, 0);
        push16(&mut out, 0);
        push16(&mut out, 1);
        push16(&mut out, 1);
        push32(&mut out, cd_len);
        push32(&mut out, cd);
        push16(&mut out, 0);
        out
    }

    fn unzip_available() -> bool {
        heronite_installer::exec::which_any(&["unzip", "7z", "7za", "7zz", "bsdtar"]).is_some()
    }

    #[test]
    fn installs_the_zip_of_a_linux_game() {
        if !unzip_available() {
            return;
        }
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0; 8192];
            let n = stream.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(req.starts_with("GET /api/games/hollow-knight/zip?version=1.0"));
            let body = zip_one("Hollow Knight/game", b"#!/bin/sh\n");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/zip\r\ncontent-disposition: attachment; filename=\"hollow.zip\"\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        let data = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(data.path());
        let base_dir = tempfile::tempdir().unwrap();
        let client = Client::new(&base).unwrap();
        let game = Game {
            id: "hollow-knight".into(),
            title: "Hollow Knight".into(),
            versions: vec!["1.0".into()],
            os: Some(crate::OsSupport { windows: false, mac: false, linux: Some("native".into()) }),
            ..Default::default()
        };
        let out = install(&paths, &client, "mine", &game, base_dir.path(), &|_, _| {}, &|| false).unwrap();
        assert_eq!(out.executable.file_name().unwrap(), "game");
        assert!(out.executable.exists());
        let lib = heronite_data::json_store::read_value(&paths.sideload_library()).unwrap();
        assert_eq!(lib["games"][0]["app_name"], serde_json::json!("gamehub_mine_hollow-knight"));
        assert_eq!(lib["games"][0]["is_installed"], serde_json::json!(true));
    }

    #[test]
    fn installer_only_windows_zip_is_opened_and_the_game_is_installed() {
        if !unzip_available() || heronite_installer::exec::which_any(&["7z", "7za", "7zz"]).is_none() {
            return;
        }
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0; 8192];
            let n = stream.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(req.starts_with("GET /api/games/hollow-knight/zip"));
            let payload = zip_one("MyGame.exe", b"MZ game");
            let mut installer = b"MZ Inno Setup payload\n".to_vec();
            installer.extend_from_slice(&payload);
            let body = zip_one("Hollow Knight/hk-setup.exe", &installer);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/zip\r\ncontent-disposition: attachment; filename=\"hollow.zip\"\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        let data = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(data.path());
        let base_dir = tempfile::tempdir().unwrap();
        let client = Client::new(&base).unwrap();
        let game = Game {
            id: "hollow-knight".into(),
            title: "Hollow Knight".into(),
            os: Some(crate::OsSupport { windows: true, mac: false, linux: None }),
            ..Default::default()
        };
        let out = install(&paths, &client, "mine", &game, base_dir.path(), &|_, _| {}, &|| false).unwrap();
        assert_eq!(out.executable.file_name().unwrap(), "MyGame.exe");
        assert!(out.executable.exists());
        assert!(!out.folder.join("hk-setup.exe").exists(), "the installer itself is not installed as the game");
    }
}
