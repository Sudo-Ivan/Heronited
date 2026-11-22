//! Restoring a backup into the game's folders as they are on this PC.
//!
//! Flow: [`list_snapshots`] → [`plan_restore`] (reads the backup's file list
//! and compares each file with the one on this PC) → [`apply_restore`] (every
//! chosen file is downloaded and checked first, then a copy of what will be
//! replaced is kept, then the files go into place) → [`undo::undo`] if the
//! user changes their mind. A download that fails or does not match its
//! hashes changes nothing on this PC.

use std::fs;
use std::path::{Path, PathBuf};

use heronite_i18n::tr;
use heronite_saves::client::{FileEntry, Manifest};
use heronite_saves::locations::{self, Target};
use heronite_saves::snapshot::unescape_path;
use heronite_saves::{SavesError, SnapshotInfo};

use super::account;
use super::backup::InFlight;
use super::game::{target_for, SavesEnv, SavesGame, SLOT};
use super::undo::{self, UndoRecord};

/// How a file of the backup compares with the one on this PC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Nothing is there now.
    New,
    /// A different file is there and would be replaced.
    Different,
    /// The same bytes are there already.
    Same,
}

/// What a planned file is, for the groups of the preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// Goes to a folder of this PC.
    Save,
    /// Has no place here (no prefix, no Windows side, no Steam account).
    Unmapped,
}

#[derive(Debug, Clone)]
pub struct PlannedFile {
    /// Where the file is in the backup.
    pub path: String,
    /// Where it goes on this PC, as text (`None` when it has no place).
    pub target: Option<String>,
    pub bytes: u64,
    pub change: Change,
    pub kind: FileKind,
    pub selected: bool,
    pub entry: FileEntry,
}

/// Where a backup lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Server,
    /// The folder holding the backup's `files`.
    Local(PathBuf),
}

/// The id of a backup in the folder on this PC starts with this.
pub const LOCAL_PREFIX: &str = "local:";

/// A previewed restore. Toggle `files[i].selected`, then [`apply_restore`].
#[derive(Debug, Clone)]
pub struct RestorePlan {
    pub app: String,
    /// Where the files are read from.
    pub source: Source,
    pub snapshot: SnapshotInfo,
    pub files: Vec<PlannedFile>,
    /// Where files go: shown to the user as the places they land.
    pub target: Target,
}

impl RestorePlan {
    pub fn selected(&self) -> impl Iterator<Item = &PlannedFile> {
        self.files.iter().filter(|f| f.selected)
    }

    /// Files that would replace different ones on this PC.
    pub fn overwrites(&self) -> usize {
        self.selected().filter(|f| f.change == Change::Different).count()
    }
}

/// How a restore went.
#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    pub written: usize,
    pub unchanged: usize,
    /// `(path, error)` of files that could not be written.
    pub failed: Vec<(String, String)>,
    /// The copy to undo it with (`None` when nothing was written).
    pub undo: Option<UndoRecord>,
}

fn message(e: SavesError) -> String {
    e.to_string()
}

/// The backups of a game: the server's and the folder's, newest first.
#[derive(Debug, Clone, Default)]
pub struct Listing {
    pub snapshots: Vec<SnapshotInfo>,
    /// The server could not be asked (the folder's backups are still listed).
    pub server_error: Option<String>,
    /// Nothing was asked of anyone: no folder, and the server is off or has
    /// no account yet (asking would make one).
    pub off: bool,
}

fn local_root(env: &SavesEnv) -> Option<PathBuf> {
    let f = heronite_data::settings::load(env.paths).ok()?.saves.local_folder;
    Some(f.trim().to_string()).filter(|f| !f.is_empty()).map(PathBuf::from)
}

fn as_info(b: heronite_saves::local::LocalBackup) -> SnapshotInfo {
    SnapshotInfo {
        id: format!("{LOCAL_PREFIX}{}", b.id),
        label: "local".into(),
        device: b.device,
        created_at: b.created_at,
        files: b.files as u64,
        bytes: b.bytes,
        ..Default::default()
    }
}

/// The backups of a game.
pub fn list_snapshots(env: &SavesEnv, game: &SavesGame) -> Listing {
    let app = game.app_id();
    let cfg = heronite_data::settings::load(env.paths).map(|s| s.saves).unwrap_or_default();
    let mut out = Listing::default();
    let asks_server = !cfg.server_off && (cfg.enabled || matches!(account::load_key(env.paths), Ok(Some(_))));
    if asks_server {
        match account::with_connection(env.paths, |c| c.client.snapshots(&app, SLOT)) {
            Ok(list) => out.snapshots = list,
            Err(e) => out.server_error = Some(e.to_string()),
        }
    }
    let root = local_root(env);
    if let Some(root) = &root {
        out.snapshots.extend(heronite_saves::local::list(root, &app).into_iter().map(as_info));
        out.snapshots.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    }
    out.off = !asks_server && root.is_none();
    out
}

/// Where each file of `manifest` goes on this PC and what it would change.
pub fn plan_files(manifest: &Manifest, target: &Target) -> Vec<PlannedFile> {
    manifest
        .files
        .iter()
        .map(|entry| {
            let placed = locations::place(&entry.path, target);
            let (target_text, change, kind) = match &placed {
                Ok(p) => {
                    let change = if heronite_saves::snapshot::file_matches(p, entry) {
                        Change::Same
                    } else if p.exists() {
                        Change::Different
                    } else {
                        Change::New
                    };
                    (Some(p.to_string_lossy().into_owned()), change, FileKind::Save)
                }
                Err(_) => (None, Change::New, FileKind::Unmapped),
            };
            PlannedFile {
                path: entry.path.clone(),
                target: target_text,
                bytes: entry.size.max(0) as u64,
                change,
                kind,
                selected: kind == FileKind::Save && change != Change::Same,
                entry: entry.clone(),
            }
        })
        .collect()
}

/// Previews restoring one backup: where each file lands in `game`'s folders.
pub fn plan_restore(env: &SavesEnv, game: &SavesGame, snapshot: &SnapshotInfo) -> Result<RestorePlan, String> {
    let app = game.app_id();
    if snapshot.enc {
        return Err(tr!("This backup is encrypted. Heronited cannot read it.").into());
    }
    let (manifest, source) = match snapshot.id.strip_prefix(LOCAL_PREFIX) {
        Some(id) => {
            let root = local_root(env).ok_or_else(|| tr!("That backup is not in the folder any more.").to_string())?;
            let (m, files) = heronite_saves::local::open(&root, &app, id).map_err(message)?;
            (m, Source::Local(files))
        }
        None => (account::with_connection(env.paths, |c| c.client.manifest(&app, SLOT, &snapshot.id)).map_err(message)?, Source::Server),
    };
    if manifest.enc.is_some() {
        return Err(tr!("This backup is encrypted. Heronited cannot read it.").into());
    }
    let target = target_for(env, game);
    let files = plan_files(&manifest, &target);
    log::info!("saves restore plan for {} ({}): {} files", game.title, snapshot.id, files.len());
    Ok(RestorePlan { app, source, snapshot: snapshot.clone(), files, target })
}

fn temp_beside(target: &Path, n: usize) -> PathBuf {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    target.with_file_name(format!(".{name}.heronited-{}-{n}.tmp", std::process::id()))
}

/// Restores the selected files of `plan`: all of them are downloaded and
/// checked before anything on this PC changes, a copy of what they replace
/// goes to the undo folder (`undo_root`, under `app_name`), and only then are
/// they moved into place.
pub fn apply_restore(env: &SavesEnv, plan: &RestorePlan, undo_root: &Path, app_name: &str) -> Result<RestoreOutcome, String> {
    let Some(_guard) = InFlight::acquire() else {
        return Err(tr!("Another backup or restore is running. Try again in a moment.").into());
    };
    let chosen: Vec<&PlannedFile> = plan.selected().filter(|f| f.target.is_some()).collect();
    if chosen.is_empty() {
        return Err(tr!("Choose at least one file to restore.").into());
    }
    let (todo, same): (Vec<&PlannedFile>, Vec<&PlannedFile>) = chosen.into_iter().partition(|f| f.change != Change::Same);
    let mut outcome = RestoreOutcome { written: 0, unchanged: same.len(), failed: Vec::new(), undo: None };
    if todo.is_empty() {
        return Ok(outcome);
    }

    // 1. Download and check everything beside its destination.
    let mut conn = match plan.source {
        Source::Server => Some(account::connect(env.paths).map_err(message)?),
        Source::Local(_) => None,
    };
    let mut staged: Vec<(&PlannedFile, PathBuf, PathBuf)> = Vec::new();
    let cleanup = |staged: &[(&PlannedFile, PathBuf, PathBuf)]| staged.iter().for_each(|(_, tmp, _)| drop(fs::remove_file(tmp)));
    for (n, f) in todo.iter().enumerate() {
        let target = PathBuf::from(f.target.as_deref().unwrap_or_default());
        let tmp = temp_beside(&target, n);
        let result = (|| -> Result<(), String> {
            if let Some(dir) = target.parent() {
                fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            let mut out = fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
            match (&plan.source, conn.as_mut()) {
                (Source::Local(files), _) => {
                    if !heronite_saves::snapshot::is_safe_path(&f.path) {
                        return Err(tr!("That backup is not in the folder any more.").to_string());
                    }
                    let mut src = fs::File::open(files.join(&f.path)).map_err(|e| format!("{}: {e}", f.path))?;
                    std::io::copy(&mut src, &mut out).map_err(|e| e.to_string())?;
                }
                (Source::Server, Some(c)) => {
                    let mut write = |conn: &account::Connection| {
                        use std::io::Seek;
                        out.set_len(0)?;
                        out.rewind()?;
                        conn.client.write_file(&plan.app, &f.entry, &mut out)
                    };
                    match write(c) {
                        Err(e) if e.is_unauthorized() => {
                            account::forget_session();
                            *c = account::connect(env.paths).map_err(message)?;
                            write(c).map_err(message)?;
                        }
                        other => other.map_err(message)?,
                    }
                }
                (Source::Server, None) => unreachable!("a server restore has a connection"),
            }
            out.sync_all().map_err(|e| e.to_string())?;
            // Whatever the source, the bytes are checked against the hashes of the manifest.
            if !heronite_saves::snapshot::file_matches(&tmp, &f.entry) {
                return Err(tr!("The saves server sent a damaged file.").to_string());
            }
            if let Ok(md) = fs::metadata(&target) {
                drop(fs::set_permissions(&tmp, md.permissions()));
            }
            Ok(())
        })();
        staged.push((f, tmp, target));
        if let Err(e) = result {
            cleanup(&staged);
            return Err(format!("{} \u{2014} {e}\n{}", unescape_path(&f.path), tr!("Nothing on this PC was changed.")));
        }
    }

    // 2. A copy of what is about to be replaced.
    let targets: Vec<String> = staged.iter().map(|(_, _, t)| t.to_string_lossy().into_owned()).collect();
    let record = match undo::snapshot(undo_root, app_name, &plan.app, &plan.snapshot.id, &targets) {
        Ok(r) => r,
        Err(e) => {
            cleanup(&staged);
            return Err(format!("{e:#}"));
        }
    };

    // 3. Into place.
    for (f, tmp, target) in &staged {
        match fs::rename(tmp, target) {
            Ok(()) => outcome.written += 1,
            Err(e) => {
                drop(fs::remove_file(tmp));
                outcome.failed.push((target.to_string_lossy().into_owned(), format!("{e}")));
                log::warn!("could not restore {}: {e}", f.path);
            }
        }
    }
    if outcome.written > 0 {
        outcome.undo = Some(record);
    } else {
        undo::discard(&record);
    }
    Ok(outcome)
}

/// Deletes one backup from the server.
pub fn delete_snapshot(env: &SavesEnv, game: &SavesGame, id: &str) -> Result<(), String> {
    let app = game.app_id();
    if let Some(local) = id.strip_prefix(LOCAL_PREFIX) {
        let root = local_root(env).ok_or_else(|| tr!("That backup is not in the folder any more.").to_string())?;
        return heronite_saves::local::delete(&root, &app, local).map_err(message);
    }
    account::with_connection(env.paths, |c| c.client.delete_snapshot(&app, SLOT, id)).map_err(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use heronite_saves::client::ChunkRef;
    use heronite_saves::hash::sha256_hex;

    fn entry(path: &str, bytes: &[u8]) -> FileEntry {
        use base64::Engine;
        FileEntry {
            path: path.into(),
            size: bytes.len() as i64,
            inline: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
            ..Default::default()
        }
    }

    #[test]
    fn plans_compare_with_what_is_on_the_pc() {
        let d = tempfile::tempdir().unwrap();
        let install = d.path().join("install");
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join("same.sav"), "aaa").unwrap();
        fs::write(install.join("diff.sav"), "old").unwrap();
        let target = Target { install_dir: Some(install.clone()), ..Default::default() };
        let manifest = Manifest {
            version: 1,
            files: vec![
                entry("base/diff.sav", b"new"),
                entry("base/new.sav", b"n"),
                entry("base/same.sav", b"aaa"),
                entry("winAppData/x.sav", b"w"),
            ],
            ..Default::default()
        };
        let plan = plan_files(&manifest, &target);
        let by = |p: &str| plan.iter().find(|f| f.path == p).unwrap();
        assert_eq!(by("base/diff.sav").change, Change::Different);
        assert_eq!(by("base/new.sav").change, Change::New);
        assert_eq!(by("base/same.sav").change, Change::Same);
        assert!(!by("base/same.sav").selected, "what is the same is not picked");
        assert!(by("base/diff.sav").selected);
        assert_eq!(by("winAppData/x.sav").kind, FileKind::Unmapped);
        assert!(!by("winAppData/x.sav").selected && by("winAppData/x.sav").target.is_none());
    }

    #[test]
    fn chunked_files_are_compared_by_hash() {
        let d = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
        let path = d.path().join("big.bin");
        fs::write(&path, &data).unwrap();
        let chunks = vec![ChunkRef { h: sha256_hex(&data[..2000]), n: 2000 }, ChunkRef { h: sha256_hex(&data[2000..]), n: 1000 }];
        let e = FileEntry { path: "p".into(), size: 3000, chunks: Some(chunks.clone()), ..Default::default() };
        assert!(heronite_saves::snapshot::file_matches(&path, &e));
        let mut other = data.clone();
        other[2500] ^= 1;
        fs::write(&path, &other).unwrap();
        assert!(!heronite_saves::snapshot::file_matches(&path, &e));
        fs::write(&path, [data.clone(), vec![0]].concat()).unwrap();
        assert!(!heronite_saves::snapshot::file_matches(&path, &e), "a longer file is a different file");
        assert!(!heronite_saves::snapshot::file_matches(&d.path().join("missing"), &e));
    }

    #[test]
    fn temp_files_sit_beside_their_target_and_hide() {
        let t = temp_beside(Path::new("/a/b/save.dat"), 3);
        assert_eq!(t.parent(), Some(Path::new("/a/b")));
        let name = t.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(".save.dat.heronited-") && name.ends_with("-3.tmp"));
    }
}
