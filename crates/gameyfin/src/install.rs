//! Downloading a Gameyfin PC game and turning it into an installed local game.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

use heronite_data::sideload::{save_game, SideloadPlatform, SideloadSpec};
use heronite_data::DataPaths;
use heronite_i18n::tr;
use heronite_installer::archive;
use heronite_installer::exec::Io;

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

fn download_file(
    client: &Client,
    game: &Game,
    provider: &str,
    cache: &Path,
    progress: &dyn Fn(u64, Option<u64>),
    cancel: &dyn Fn() -> bool,
) -> Result<PathBuf, String> {
    let mut resp = client.download(&game.id, provider)?;
    let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    if ctype.contains("application/json") {
        let text = resp.body_mut().with_config().limit(1024 * 1024).read_to_string().map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let url = v.get("url").and_then(Value::as_str).ok_or_else(|| tr!("Gameyfin returned a download without an address"))?;
        // Linked downloads belong to the provider and must not receive the API key.
        let mut r = ureq::get(url).call().map_err(|e| e.to_string())?;
        let ext = filename_ext(&r)
            .or_else(|| {
                url::Url::parse(url).ok().and_then(|u| Path::new(u.path()).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()))
            })
            .unwrap_or_else(|| "bin".into());
        let name = content_filename(&r).unwrap_or_else(|| format!("download.{ext}"));
        let dest = cache.join(name);
        copy_response(&mut r, &dest, progress, cancel)?;
        return Ok(dest);
    }
    let ext = filename_ext(&resp).unwrap_or_else(|| "zip".into());
    let name = content_filename(&resp).unwrap_or_else(|| format!("download.{ext}"));
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
        (false, None | Some("sh") | Some("appimage")) => 100,
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

/// Downloads and installs `game` into `base/<title>`. Blocking. When that
/// folder is taken, the server id keeps a second server's copy separate.
pub fn install(
    paths: &DataPaths,
    client: &Client,
    server_id: &str,
    game: &Game,
    provider: &str,
    base: &Path,
    progress: &dyn Fn(u64, Option<u64>),
    cancel: &dyn Fn() -> bool,
) -> Result<InstallOutcome, String> {
    let title = game.title.clone().unwrap_or_else(|| game.id.clone());
    let linux = game.platforms.iter().any(|p| p == "LINUX");
    let has_windows = game.platforms.iter().any(|p| p == "PC_MICROSOFT_WINDOWS");
    if !linux && !has_windows {
        return Err(tr!("{title} is not a Windows or Linux PC game, so Heronited cannot install it", title = title.clone()).into());
    }
    let windows = has_windows || !linux;
    let folder_name = title
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_') { c } else { '_' })
        .collect::<String>()
        .trim()
        .to_string();
    let name = if folder_name.is_empty() { format!("gameyfin-{}", game.id) } else { folder_name };
    let first = base.join(&name);
    let occupied = |path: &Path| path.exists() && fs::read_dir(path).map(|mut d| d.next().is_some()).unwrap_or(true);
    // Two servers often have the same game under the same title. Give the
    // second copy its own folder instead of making one server impossible.
    let folder = if occupied(&first) { base.join(format!("{name}-{server_id}")) } else { first };
    if occupied(&folder) {
        return Err(
            tr!("{path} already exists. Choose another install folder or uninstall the game first.", path = folder.display()).into()
        );
    }
    let cache = paths.images_cache_dir().join("gameyfin/downloads").join(server_id).join(&game.id);
    let _ = fs::remove_dir_all(&cache);
    let result = (|| {
        fs::create_dir_all(&cache).map_err(|e| format!("{}: {e}", cache.display()))?;
        let file = download_file(client, game, provider, &cache, progress, cancel)?;
        fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
        let format = archive::by_name(&file.to_string_lossy());
        let direct_program =
            matches!(format, Some(heronite_installer::archive::Format::Exe | heronite_installer::archive::Format::AppImage));
        if format.is_some() && !direct_program {
            let io = Io { cancel, log: &|_| () };
            archive::extract(&file, &folder, format, true, &io)?;
        } else {
            let name = file.file_name().and_then(|n| n.to_str()).unwrap_or(if windows { "game.exe" } else { "game" });
            let name = if name.starts_with("download.") {
                if windows {
                    "game.exe"
                } else {
                    "game"
                }
            } else {
                name
            };
            fs::copy(&file, folder.join(name)).map_err(|e| e.to_string())?;
        }
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
    fn refuses_console_only_games_before_downloading() {
        let data = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(data.path());
        let base = tempfile::tempdir().unwrap();
        let client = Client::new("http://localhost:1", "x").unwrap();
        let game = Game { id: "1".into(), title: Some("Console".into()), platforms: vec!["NINTENDO_SWITCH".into()], ..Default::default() };
        let err = install(&paths, &client, "s", &game, "p", base.path(), &|_, _| {}, &|| false).unwrap_err();
        assert!(err.contains("not a Windows or Linux PC game"));
    }

    #[test]
    fn installs_a_direct_linux_download() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0; 8192];
            let n = stream.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(req.starts_with("GET /v1/games/7/download?provider=files"));
            assert!(req.contains("X-API-Key: secret") || req.contains("x-api-key: secret"));
            let body = vec![7u8; 1024 * 1024];
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\ncontent-disposition: attachment; filename=\"game\"\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        let data = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(data.path());
        let base_dir = tempfile::tempdir().unwrap();
        let client = Client::new(&base, "secret").unwrap();
        let game = Game { id: "7".into(), title: Some("Direct".into()), platforms: vec!["LINUX".into()], ..Default::default() };
        let out = install(&paths, &client, "mine", &game, "files", base_dir.path(), &|_, _| {}, &|| false).unwrap();
        assert_eq!(out.executable.file_name().unwrap(), "game");
        assert!(out.executable.exists());
        let lib = heronite_data::json_store::read_value(&paths.sideload_library()).unwrap();
        assert_eq!(lib["games"][0]["app_name"], serde_json::json!("gameyfin_mine_7"));
        assert_eq!(lib["games"][0]["is_installed"], serde_json::json!(true));
    }
}
