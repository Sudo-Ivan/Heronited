//! GOG's offline backup game installers (the classic `setup_*.exe` +
//! `.bin` files of the account pages): a Windows build's are downloaded
//! once at install time and kept in a backup folder, and the game is set
//! up from them on its first start ([`crate::setup`] runs the `setup`
//! program in its Wine prefix, or natively on Windows). A file already
//! there at its expected size is not fetched again, so a backup made
//! earlier doubles as the install source of a reinstall. A game that
//! lists no offline installers falls back to the depot download.
//!
//! `installers.json` in the game's backup folder records which files
//! belong to which product id (the base game and each DLC): that is what
//! [`crate::setup`] runs.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use heronite_data::{game_config, json_store, DataPaths};
use heronite_epic::progress::Progress;
use heronite_epic::session::SessionExpired;
use heronite_epic::util::file_size;
use heronite_i18n::tr;
use heronite_launch::net::Net;

use crate::cmd::{Cancel, Gogdl};
use crate::user;

/// `defaultSettings` key of the toggle (fork default: on).
pub const ENABLED_KEY: &str = "gogOfflineInstallers";
/// `defaultSettings` key of the folder; empty means the default.
pub const DIR_KEY: &str = "gogOfflineInstallerDir";
/// The manifest a game's backup folder gets ([`Bundle::save`]).
pub const MANIFEST: &str = "installers.json";

/// Whether the feature is on (`gogOfflineInstallers`, default `true`).
pub fn enabled(paths: &DataPaths) -> bool {
    let home = heronite_epic::ops::home();
    let g = game_config::load_global(paths, &home);
    g.get(ENABLED_KEY).and_then(Value::as_bool).unwrap_or(true)
}

/// The folder games' installers are kept in (`gogOfflineInstallerDir`, or
/// `~/Games/GOG Offline Installers` when empty). `~` at its start is
/// expanded.
pub fn backup_dir(paths: &DataPaths) -> PathBuf {
    let home = heronite_epic::ops::home();
    let g = game_config::load_global(paths, &home);
    let dir = g
        .get(DIR_KEY)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(|s| game_config::expand_tilde(s.trim(), &home))
        .unwrap_or_else(|| game_config::default_gog_offline_dir(&home));
    PathBuf::from(dir)
}

/// One part of an installer (`setup_x.exe`, `setup_x-1.bin`, ...).
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    /// File name under the game's backup folder.
    pub name: String,
    /// Expected size in bytes; `0` when GOG does not say.
    pub size: u64,
    /// SHA-256 hex of the file as downloaded; empty until it is on disk.
    /// GOG publishes no checksum, so it guards reuse and the `already
    /// downloaded` skip against corruption, not first-download tampering.
    pub sha256: String,
    /// `api.gog.com` URL the real download URL is resolved from.
    pub downlink: String,
}

/// The installer of one product (the base game or a DLC).
#[derive(Debug, Clone, PartialEq)]
pub struct Product {
    pub product_id: String,
    /// Installer version text when GOG provides it.
    pub version: String,
    pub parts: Vec<Part>,
}

impl Product {
    /// The `*.exe` of the parts: the program [`crate::setup`] runs.
    pub fn setup_exe(&self) -> Option<&str> {
        self.parts.iter().map(|p| p.name.as_str()).find(|n| n.to_ascii_lowercase().ends_with(".exe"))
    }
}

/// What [`download`] fetched (or found already there): the folder and the
/// products' parts, for the install record and `installers.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct Bundle {
    /// `<dir>/<folder>` the parts sit in.
    pub dir: PathBuf,
    pub products: Vec<Product>,
    /// Parts that arrived this run, downloaded or reused (`0` for one
    /// read from `installers.json`): an update that brought no new file
    /// needs no new setup run.
    pub fetched: usize,
}

impl Bundle {
    /// `installers.json` of the folder.
    pub fn manifest(&self) -> Value {
        json!({
            "products": self.products.iter().map(|p| json!({
                "product_id": p.product_id,
                "version": p.version,
                "files": p.parts.iter().map(|f| json!({"name": f.name, "size": f.size, "sha256": f.sha256})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>()
        })
    }

    /// Writes `installers.json` into [`Self::dir`].
    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        heronite_data::temps::write_atomic(&self.dir.join(MANIFEST), self.manifest().to_string().as_bytes(), None)?;
        Ok(())
    }

    /// The `installers.json` of `dir`, if any (product ids and file names;
    /// part sizes are not checked against GOG's again).
    pub fn load(dir: &Path) -> Option<Bundle> {
        let doc = json_store::read_value(&dir.join(MANIFEST)).ok()?;
        let products = doc.get("products")?.as_array()?;
        let mut out = Vec::new();
        for p in products {
            let product_id = p.get("product_id")?.as_str()?.to_string();
            let version = p.get("version").and_then(Value::as_str).unwrap_or_default().to_string();
            let mut parts = Vec::new();
            for f in p.get("files").and_then(Value::as_array).cloned().unwrap_or_default() {
                let name = f.get("name")?.as_str()?.to_string();
                if !safe_name(&name) {
                    continue;
                }
                let size = f.get("size").and_then(Value::as_u64).unwrap_or(0);
                parts.push(Part { name, size, sha256: text(f.get("sha256")), downlink: String::new() });
            }
            out.push(Product { product_id, version, parts });
        }
        Some(Bundle { dir: dir.to_path_buf(), products: out, fetched: 0 })
    }
}

/// `https://api.gog.com/products/{id}?expand=downloads`.
fn products_url(product_id: &str) -> String {
    format!("https://api.gog.com/products/{product_id}?expand=downloads")
}

/// `name` is one file's name, never a path: it lands under the backup
/// folder with [`Path::join`], so anything with separators or dots-only
/// segments would write outside of it.
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && std::path::Path::new(name).file_name().is_some_and(|f| f == name)
}

fn text(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default().to_string()
}

/// The `downloads.installers` entry of `doc` for `language`, or `None`
/// when there is none for `os`: an exact `language_code` match, then the
/// language's two-letter prefix, then English, then the first one.
fn pick_installer<'a>(doc: &'a Value, os: &str, language: &str) -> Option<&'a Value> {
    let list = doc.pointer("/downloads/installers")?.as_array()?;
    let for_os: Vec<&Value> = list.iter().filter(|i| text(i.get("os")).eq_ignore_ascii_case(os)).collect();
    let pick = |f: &dyn Fn(&Value) -> bool| for_os.iter().copied().find(|i| f(i));
    pick(&|i| text(i.get("language_code")).eq_ignore_ascii_case(language))
        .or_else(|| {
            pick(&|i| {
                let want = language.split('-').next().unwrap_or(language);
                let got = text(i.get("language_code")).replace('_', "-");
                let got = got.split('-').next().unwrap_or(&got);
                !want.is_empty() && got.eq_ignore_ascii_case(want)
            })
        })
        .or_else(|| pick(&|i| text(i.get("language")).eq_ignore_ascii_case("en")))
        .or_else(|| for_os.first().copied())
}

/// The installer of one product, or `None` when it has none for `os`.
fn product_installer(net: &dyn Net, token: &str, product_id: &str, os: &str, language: &str) -> Result<Option<Product>> {
    let doc = get_json(net, token, &products_url(product_id))?;
    let Some(installer) = pick_installer(&doc, os, language) else { return Ok(None) };
    let mut parts = Vec::new();
    for f in installer.get("files").and_then(Value::as_array).cloned().unwrap_or_default() {
        let name = text(f.get("name"));
        let downlink = text(f.get("downlink"));
        if !safe_name(&name) || downlink.is_empty() {
            continue;
        }
        let size = f.get("size").and_then(Value::as_u64).unwrap_or(0);
        parts.push(Part { name, size, sha256: String::new(), downlink });
    }
    if parts.is_empty() {
        return Ok(None);
    }
    Ok(Some(Product { product_id: product_id.to_string(), version: text(installer.get("version")), parts }))
}

/// `net.get_json_bearer` with GOG's errors mapped: a refused token is a
/// [`SessionExpired`].
fn get_json(net: &dyn Net, token: &str, url: &str) -> Result<Value> {
    match net.get_json_bearer(url, token) {
        Ok(doc) => Ok(doc),
        Err(e) if e.contains("HTTP 401") => Err(anyhow!(SessionExpired).context(format!("{url}: HTTP 401"))),
        Err(e) => Err(anyhow!(tr!("GOG could not list the offline installers: {error}", error = e)).context(e)),
    }
}

/// The download URL behind a file's `downlink` address, https only: the
/// installer runs it later, so a plain-http URL GOG would never hand out is
/// refused here.
fn resolve(net: &dyn Net, token: &str, downlink: &str) -> Result<String> {
    let doc = get_json(net, token, downlink)?;
    let url = text(doc.get("downlink"));
    if !url.starts_with("https://") {
        bail!(tr!("GOG did not give a download URL for an installer part"));
    }
    Ok(url)
}

/// `done` of the parts are already in `dir` at their expected size.
fn present(dir: &Path, part: &Part) -> bool {
    part.size > 0 && std::fs::metadata(dir.join(&part.name)).map(|m| m.len() == part.size).unwrap_or(false)
}

/// The recorded hash of `dir/name` still matches, `true` when it does.
/// `recorded` is what the manifest of `dir` wrote before; a file without a
/// recorded hash (an older manifest, or one copied in by hand) is trusted.
fn intact(dir: &Path, name: &str, recorded: &str) -> bool {
    if recorded.is_empty() {
        return true;
    }
    match heronite_launch::gfxmods::hash::sha256_file(&dir.join(name)) {
        Ok(got) if got == recorded => true,
        Ok(_) => {
            log::warn!("{name} is not what the manifest recorded; downloading it again");
            false
        }
        Err(e) => {
            log::warn!("{name} could not be hashed ({e}); downloading it again");
            false
        }
    }
}

/// A same-named file of the right size in another game's folder of the
/// backup root (DLC installers are shared between products): it is linked
/// into `dir` instead of downloaded again, with its recorded hash. A hard
/// link keeps one copy on disk; a copy is the fallback across filesystems.
fn reusable(root: &Path, dir: &Path, part: &Part) -> Option<(PathBuf, String)> {
    if part.size == 0 {
        return None;
    }
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let sibling = entry.path();
        let candidate = sibling.join(&part.name);
        if sibling.as_path() == dir || !candidate.is_file() {
            continue;
        }
        if !std::fs::metadata(&candidate).map(|m| m.len() == part.size).unwrap_or(false) {
            continue;
        }
        // What the sibling's manifest recorded for the file, if anything.
        let recorded = Bundle::load(&sibling)
            .and_then(|b| {
                b.products
                    .iter()
                    .flat_map(|p| p.parts.iter())
                    .find(|f| f.name == part.name && f.size == part.size)
                    .map(|f| f.sha256.clone())
            })
            .unwrap_or_default();
        if intact(&sibling, &part.name, &recorded) {
            let sha256 =
                if recorded.is_empty() { heronite_launch::gfxmods::hash::sha256_file(&candidate).unwrap_or_default() } else { recorded };
            return Some((candidate, sha256));
        }
    }
    None
}

/// Links `from` onto `to`, copying when the link fails (cross-device).
fn link_or_copy(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::hard_link(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(e),
        Err(_) => std::fs::copy(from, to).map(|_| ()),
    }
}

/// Bytes/second-derived [`Progress`] of the whole download.
fn report(done: u64, total: u64, started: Instant, on_progress: &mut dyn FnMut(Progress)) {
    const MIB: f64 = 1024. * 1024.;
    let secs = started.elapsed().as_secs_f64().max(0.001);
    let speed = done as f64 / secs;
    let eta = if total > done && speed > 0. {
        let left = ((total - done) as f64 / speed) as u64;
        format!("{:02}:{:02}:{:02}", left / 3600, left / 60 % 60, left % 60)
    } else {
        "00:00:00".into()
    };
    on_progress(Progress {
        percent: if total > 0 { (done as f64 / total as f64 * 100.).min(100.) } else { 0. },
        downloaded_mib: done as f64 / MIB,
        eta,
        down_speed: speed / MIB,
        disk_speed: speed / MIB,
    });
}

/// How [`download`] ended.
#[derive(Debug)]
pub enum Outcome {
    /// The base game has no offline installer: install the depot way.
    NoInstallers,
    /// Stopped before the end; finished parts stay for the retry.
    Cancelled,
    /// The parts are all in [`Bundle::dir`] and `installers.json` was written.
    Done(Bundle),
}

/// The `products`' installer parts in `dir/folder`: what is there and of
/// the right size is kept, the rest is downloaded. `on_progress` gets the
/// overall percent.
pub fn download(
    paths: &DataPaths,
    gogdl: &Gogdl,
    net: &dyn Net,
    app_name: &str,
    folder: &str,
    os: &str,
    language: &str,
    dlcs: &[String],
    cancel: &Cancel,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<Outcome> {
    let token = user::credentials(paths, gogdl)?.access_token;
    let dir = backup_dir(paths).join(folder);

    // Every asked-for product needs an installer: a missing or unreadable
    // one (a DLC without backup installers, a transient API error) sends
    // the whole install to the depot so nothing is skipped silently.
    let mut products = Vec::new();
    for product_id in std::iter::once(app_name).chain(dlcs.iter().map(String::as_str)) {
        match product_installer(net, &token, product_id, os, language) {
            Ok(Some(product)) => products.push(product),
            Ok(None) => {
                if product_id != app_name {
                    log::warn!("DLC {product_id} has no offline installer for {os}");
                }
                return Ok(Outcome::NoInstallers);
            }
            Err(e) => {
                log::warn!("the offline installers of {product_id} could not be listed: {e:#}");
                return Ok(Outcome::NoInstallers);
            }
        }
    }

    let total: u64 = products.iter().flat_map(|p| p.parts.iter().map(|f| f.size)).sum();
    if let Some(space) = heronite_epic::disk::check(&dir.display().to_string()) {
        if total > 0 && space.free < total {
            bail!(tr!("Not enough disk space for the offline installers ({need} needed)", need = file_size(total as f64)));
        }
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    let started = Instant::now();
    let root = backup_dir(paths);
    // The hashes the previous manifest recorded, by file name: the
    // `already downloaded` check trusts only a file that still matches.
    let prior: std::collections::BTreeMap<String, String> =
        Bundle::load(&dir).map(|b| b.products.into_iter().flat_map(|p| p.parts).map(|f| (f.name, f.sha256)).collect()).unwrap_or_default();
    let mut fetched = 0;
    let mut done: u64 = products.iter().flat_map(|p| p.parts.iter()).filter(|f| present(&dir, f)).map(|f| f.size).sum();
    for product in &mut products {
        for part in &mut product.parts {
            if cancel.is_requested() {
                return Ok(Outcome::Cancelled);
            }
            let dest = dir.join(&part.name);
            if present(&dir, part) {
                let recorded = prior.get(&part.name).map(String::as_str).unwrap_or("");
                if intact(&dir, &part.name, recorded) {
                    log::info!("keeping the already downloaded {}", part.name);
                    part.sha256 = if recorded.is_empty() {
                        heronite_launch::gfxmods::hash::sha256_file(&dest).unwrap_or_default()
                    } else {
                        recorded.to_string()
                    };
                    continue;
                }
                done -= part.size;
            }
            if let Some((sibling, sha256)) = reusable(&root, &dir, part) {
                match link_or_copy(&sibling, &dest) {
                    Ok(()) => {
                        log::info!("reused {} of {}", part.name, sibling.display());
                        part.sha256 = sha256;
                        done += part.size;
                        fetched += 1;
                        report(done, total, started, on_progress);
                        continue;
                    }
                    Err(e) => log::warn!("could not reuse {}: {e}", sibling.display()),
                }
            }
            let url = resolve(net, &token, &part.downlink)?;
            let tmp = PathBuf::from(format!("{}.part", dest.display()));
            let base = done;
            let result = net.download_progress(&url, &tmp, &|| cancel.is_requested(), &mut |got| {
                report(base + got, total, started, on_progress);
            });
            match result {
                Ok(()) => {
                    // A wrong size means a truncated download: it stays a
                    // .part candidate for the retry instead of a broken file.
                    let got = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
                    if part.size > 0 && got != part.size {
                        bail!(tr!("{name} downloaded incompletely ({got} of {size} bytes)", name = part.name, got, size = part.size));
                    }
                    std::fs::rename(&tmp, &dest).with_context(|| format!("renaming {}", tmp.display()))?;
                    part.sha256 = heronite_launch::gfxmods::hash::sha256_file(&dest).unwrap_or_default();
                    done += part.size;
                    fetched += 1;
                    report(done, total, started, on_progress);
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    if e == heronite_launch::net::CANCELLED {
                        return Ok(Outcome::Cancelled);
                    }
                    return Err(anyhow!(tr!("Could not download {name}: {error}", name = part.name, error = e)).context(e));
                }
            }
        }
    }

    let bundle = Bundle { dir, products, fetched };
    bundle.save()?;
    report(total, total, started, on_progress);
    Ok(Outcome::Done(bundle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::testing::*;
    use heronite_launch::net::FakeNet;
    use serde_json::json;

    const PRODUCTS: &str = "https://api.gog.com/products/42?expand=downloads";

    fn installer_doc(installers: Value) -> Value {
        json!({"downloads": {"installers": installers, "patches": [], "bonus_content": []}})
    }

    fn windows_installer() -> Value {
        json!({
            "id": "en1installer0", "name": "The Game", "os": "windows", "language": "en", "language_code": "en-US",
            "version": "1.5", "total_size": 60,
            "files": [
                {"id": "en1installer0", "size": 20, "name": "setup_the_game_1.5_(64bit)_(1).exe", "downlink": "https://api.gog.com/products/42/downlink/installer/en1installer0"},
                {"id": "en1installer1", "size": 40, "name": "setup_the_game_1.5_(64bit)_(1)-1.bin", "downlink": "https://api.gog.com/products/42/downlink/installer/en1installer1"},
            ]
        })
    }

    #[test]
    fn the_windows_installer_of_the_language_is_picked() {
        let doc = installer_doc(json!([
            json!({"id": "a", "os": "windows", "language": "en", "language_code": "en-US", "files": [{"name": "en.exe", "size": 1, "downlink": "u"}]}),
            json!({"id": "b", "os": "windows", "language": "de", "language_code": "de-DE", "files": [{"name": "de.exe", "size": 1, "downlink": "u"}]}),
            json!({"id": "c", "os": "mac", "language": "en", "language_code": "en-US", "files": [{"name": "mac.zip", "size": 1, "downlink": "u"}]}),
        ]));
        assert_eq!(text(pick_installer(&doc, "windows", "de-DE").unwrap().get("id")), "b");
        assert_eq!(text(pick_installer(&doc, "windows", "en-US").unwrap().get("id")), "a");
        // No mac installer is ever picked.
        assert_eq!(text(pick_installer(&doc, "windows", "fr-FR").unwrap().get("id")), "a");
        // Language prefix fallback: pt-BR for a pt installer.
        let doc = installer_doc(json!([
            json!({"id": "a", "os": "windows", "language": "en", "language_code": "en-US", "files": [{"name": "en.exe", "size": 1, "downlink": "u"}]}),
            json!({"id": "b", "os": "windows", "language": "pt", "language_code": "pt-BR", "files": [{"name": "pt.exe", "size": 1, "downlink": "u"}]}),
        ]));
        assert_eq!(text(pick_installer(&doc, "windows", "pt-PT").unwrap().get("id")), "b");
        let doc = installer_doc(json!([]));
        assert!(pick_installer(&doc, "windows", "en-US").is_none());
    }

    #[test]
    fn a_product_and_its_files_are_read() {
        let net = FakeNet::online().with_json(PRODUCTS, installer_doc(json!([windows_installer()])));
        let product = product_installer(&net, "tok", "42", "windows", "en-US").unwrap().unwrap();
        assert_eq!((product.product_id.as_str(), product.version.as_str()), ("42", "1.5"));
        assert_eq!(product.parts.len(), 2);
        assert_eq!(product.setup_exe(), Some("setup_the_game_1.5_(64bit)_(1).exe"));
        // A game without installers answers `None`.
        let net = FakeNet::online().with_json(PRODUCTS, installer_doc(json!([])));
        assert!(product_installer(&net, "tok", "42", "windows", "en-US").unwrap().is_none());
    }

    #[test]
    fn parts_download_into_the_backup_folder() {
        let (dir, paths) = setup();
        std::fs::create_dir_all(paths.gog_auth().parent().unwrap()).unwrap();
        let tool = fake_tool(dir.path()).otherwise(Reply::new().out(r#"{"access_token":"tok","user_id":"7"}"#));
        let g = gogdl(&tool, &paths);
        let doc = installer_doc(json!([windows_installer()]));
        let net = FakeNet::online()
            .with_json(PRODUCTS, doc)
            .with_json("https://api.gog.com/products/42/downlink/installer/en1installer0", json!({"downlink": "https://cdn.gog.com/f0"}))
            .with_json("https://api.gog.com/products/42/downlink/installer/en1installer1", json!({"downlink": "https://cdn.gog.com/f1"}))
            .with_file("https://cdn.gog.com/f0", vec![0u8; 20])
            .with_file("https://cdn.gog.com/f1", vec![0u8; 40]);
        // The backup folder is the settings' one.
        std::fs::create_dir_all(paths.global_config().parent().unwrap()).unwrap();
        std::fs::write(
            paths.global_config(),
            json!({"defaultSettings": {"gogOfflineInstallerDir": dir.path().join("backups").display().to_string()}}).to_string(),
        )
        .unwrap();
        let mut reports = Vec::new();
        let outcome =
            download(&paths, &g, &net, "42", "The Game", "windows", "en-US", &[], &Cancel::new(), &mut |p| reports.push(p)).unwrap();
        let Outcome::Done(bundle) = outcome else { panic!("expected Done, got {outcome:?}") };
        let game = dir.path().join("backups/The Game");
        assert_eq!(bundle.dir, game);
        assert_eq!(std::fs::metadata(game.join("setup_the_game_1.5_(64bit)_(1).exe")).unwrap().len(), 20);
        assert_eq!(std::fs::metadata(game.join("setup_the_game_1.5_(64bit)_(1)-1.bin")).unwrap().len(), 40);
        assert!(!game.join("setup_the_game_1.5_(64bit)_(1).exe.part").exists());
        let manifest = Bundle::load(&game).unwrap();
        assert_eq!(manifest.products[0].product_id, "42");
        assert_eq!(manifest.products[0].setup_exe(), Some("setup_the_game_1.5_(64bit)_(1).exe"));
        assert_eq!(reports.last().map(|p| p.percent), Some(100.));
        assert_eq!(reports.first().map(|p| p.percent), Some(20. / 60. * 100.));
    }

    #[test]
    fn complete_files_are_not_downloaded_again() {
        let (dir, paths) = setup();
        std::fs::create_dir_all(paths.gog_auth().parent().unwrap()).unwrap();
        let tool = fake_tool(dir.path()).otherwise(Reply::new().out(r#"{"access_token":"tok","user_id":"7"}"#));
        let g = gogdl(&tool, &paths);
        std::fs::create_dir_all(paths.global_config().parent().unwrap()).unwrap();
        std::fs::write(
            paths.global_config(),
            json!({"defaultSettings": {"gogOfflineInstallerDir": dir.path().join("backups").display().to_string()}}).to_string(),
        )
        .unwrap();
        let game = dir.path().join("backups/The Game");
        std::fs::create_dir_all(&game).unwrap();
        // The exe is already there at the right size; only the .bin comes.
        std::fs::write(game.join("setup_the_game_1.5_(64bit)_(1).exe"), vec![0u8; 20]).unwrap();
        let doc = installer_doc(json!([windows_installer()]));
        let net = FakeNet::online()
            .with_json(PRODUCTS, doc)
            .with_json("https://api.gog.com/products/42/downlink/installer/en1installer1", json!({"downlink": "https://cdn.gog.com/f1"}))
            .with_file("https://cdn.gog.com/f1", vec![0u8; 40]);
        let outcome = download(&paths, &g, &net, "42", "The Game", "windows", "en-US", &[], &Cancel::new(), &mut |_| {}).unwrap();
        assert!(matches!(outcome, Outcome::Done(_)));
        // The downlink of the kept part was never resolved, the exe untouched.
        assert!(!net.requests().iter().any(|r| r.contains("en1installer0")));
        assert_eq!(std::fs::metadata(game.join("setup_the_game_1.5_(64bit)_(1)-1.bin")).unwrap().len(), 40);
    }

    #[test]
    fn a_part_another_game_has_is_reused_without_downloading() {
        let (dir, paths) = setup();
        std::fs::create_dir_all(paths.gog_auth().parent().unwrap()).unwrap();
        let tool = fake_tool(dir.path()).otherwise(Reply::new().out(r#"{"access_token":"tok","user_id":"7"}"#));
        let g = gogdl(&tool, &paths);
        std::fs::create_dir_all(paths.global_config().parent().unwrap()).unwrap();
        std::fs::write(
            paths.global_config(),
            json!({"defaultSettings": {"gogOfflineInstallerDir": dir.path().join("backups").display().to_string()}}).to_string(),
        )
        .unwrap();
        // Another game's backup already holds the .bin at the right size.
        let other = dir.path().join("backups/Other Game");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("setup_the_game_1.5_(64bit)_(1)-1.bin"), vec![0u8; 40]).unwrap();
        let doc = installer_doc(json!([windows_installer()]));
        // Only the exe has a downlink answer: fetching the .bin would fail.
        let net = FakeNet::online()
            .with_json(PRODUCTS, doc)
            .with_json("https://api.gog.com/products/42/downlink/installer/en1installer0", json!({"downlink": "https://cdn.gog.com/f0"}))
            .with_file("https://cdn.gog.com/f0", vec![0u8; 20]);
        let outcome = download(&paths, &g, &net, "42", "The Game", "windows", "en-US", &[], &Cancel::new(), &mut |_| {}).unwrap();
        let Outcome::Done(bundle) = outcome else { panic!("expected Done, got {outcome:?}") };
        let game = dir.path().join("backups/The Game");
        assert_eq!(std::fs::metadata(game.join("setup_the_game_1.5_(64bit)_(1)-1.bin")).unwrap().len(), 40);
        // The shared part never hit the network; it counts as arrived.
        assert!(!net.requests().iter().any(|r| r.contains("en1installer1")));
        assert_eq!(bundle.fetched, 2);
        // The reuse recorded the sibling's hash in the manifest.
        let kept = Bundle::load(&game).unwrap();
        let bin = kept.products[0].parts.iter().find(|f| f.name.ends_with(".bin")).unwrap();
        assert_eq!(bin.sha256.len(), 64);
    }

    #[test]
    fn a_same_size_file_that_does_not_match_its_record_is_downloaded_again() {
        let (dir, paths) = setup();
        std::fs::create_dir_all(paths.gog_auth().parent().unwrap()).unwrap();
        let tool = fake_tool(dir.path()).otherwise(Reply::new().out(r#"{"access_token":"tok","user_id":"7"}"#));
        let g = gogdl(&tool, &paths);
        std::fs::create_dir_all(paths.global_config().parent().unwrap()).unwrap();
        std::fs::write(
            paths.global_config(),
            json!({"defaultSettings": {"gogOfflineInstallerDir": dir.path().join("backups").display().to_string()}}).to_string(),
        )
        .unwrap();
        let game = dir.path().join("backups/The Game");
        std::fs::create_dir_all(&game).unwrap();
        // Right size, wrong bytes, and the manifest says so.
        let exe = game.join("setup_the_game_1.5_(64bit)_(1).exe");
        std::fs::write(&exe, vec![9u8; 20]).unwrap();
        let stale = Bundle {
            dir: game.clone(),
            products: vec![Product {
                product_id: "42".into(),
                version: "1.5".into(),
                parts: vec![Part {
                    name: "setup_the_game_1.5_(64bit)_(1).exe".into(),
                    size: 20,
                    sha256: heronite_launch::gfxmods::hash::sha256(&vec![0u8; 20]),
                    downlink: String::new(),
                }],
            }],
            fetched: 0,
        };
        stale.save().unwrap();
        let net = FakeNet::online()
            .with_json(PRODUCTS, installer_doc(json!([windows_installer()])))
            .with_json("https://api.gog.com/products/42/downlink/installer/en1installer0", json!({"downlink": "https://cdn.gog.com/f0"}))
            .with_json("https://api.gog.com/products/42/downlink/installer/en1installer1", json!({"downlink": "https://cdn.gog.com/f1"}))
            .with_file("https://cdn.gog.com/f0", vec![0u8; 20])
            .with_file("https://cdn.gog.com/f1", vec![0u8; 40]);
        let outcome = download(&paths, &g, &net, "42", "The Game", "windows", "en-US", &[], &Cancel::new(), &mut |_| {}).unwrap();
        assert!(matches!(outcome, Outcome::Done(_)));
        // The corrupted exe was replaced by the download.
        assert_eq!(std::fs::read(&exe).unwrap(), vec![0u8; 20]);
        let kept = Bundle::load(&game).unwrap();
        assert_eq!(kept.products[0].parts.iter().find(|f| f.name.ends_with(".exe")).unwrap().sha256, stale.products[0].parts[0].sha256);
    }

    #[test]
    fn settings_default_on_and_use_the_setting_dir() {
        let (dir, paths) = setup();
        assert!(enabled(&paths));
        assert_eq!(backup_dir(&paths), PathBuf::from(game_config::default_gog_offline_dir(&heronite_epic::ops::home())));
        std::fs::create_dir_all(paths.global_config().parent().unwrap()).unwrap();
        std::fs::write(
            paths.global_config(),
            json!({"defaultSettings": {"gogOfflineInstallers": false, "gogOfflineInstallerDir": dir.path().join("x").display().to_string()}})
                .to_string(),
        )
        .unwrap();
        assert!(!enabled(&paths));
        assert_eq!(backup_dir(&paths), dir.path().join("x"));
    }
}
