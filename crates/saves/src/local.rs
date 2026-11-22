//! Backups in a folder on this PC, with no server: each backup is a folder
//! `<root>/<game>/<time>/` holding the files as they are named in a snapshot
//! (`files/`) and `manifest.json` with the sizes and hashes taken when they
//! were copied. A backup is built beside its final name and renamed into
//! place once every copied file matches its hash, and a restore checks the
//! same hashes again before it writes anything.

use std::fs;
use std::path::{Path, PathBuf};

use heronite_i18n::tr;
use serde::{Deserialize, Serialize};

use crate::client::Manifest;
use crate::error::{Result, SavesError};
use crate::snapshot::{changed, file_matches, is_safe_path, same_content, Prepared};

/// Backups kept per game; older ones are removed after a new one is made.
pub const KEEP: usize = 10;

const MANIFEST: &str = "manifest.json";
const FILES: &str = "files";

#[derive(Serialize, Deserialize)]
struct Stored {
    v: u32,
    title: String,
    created_at: String,
    device: String,
    manifest: Manifest,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalBackup {
    /// The folder's name (the time it was made).
    pub id: String,
    pub dir: PathBuf,
    pub created_at: String,
    pub device: String,
    pub files: usize,
    pub bytes: u64,
}

/// The folder of one game's backups.
pub fn app_dir(root: &Path, app: &str) -> PathBuf {
    root.join(app.replace([':', '/', '\\'], "_"))
}

fn plain_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40 && id.bytes().all(|b| b.is_ascii_alphanumeric()) && !id.ends_with("partial")
}

fn read_stored(dir: &Path) -> Option<Stored> {
    serde_json::from_slice(&fs::read(dir.join(MANIFEST)).ok()?).ok()
}

fn backup_of(id: &str, dir: PathBuf, s: &Stored) -> LocalBackup {
    LocalBackup {
        id: id.to_string(),
        dir,
        created_at: s.created_at.clone(),
        device: s.device.clone(),
        files: s.manifest.files.len(),
        bytes: s.manifest.files.iter().map(|f| f.size.max(0) as u64).sum(),
    }
}

/// The backups of `app` in `root`, newest first. Folders without a valid
/// `manifest.json` (not ours, or unfinished) are not backups.
pub fn list(root: &Path, app: &str) -> Vec<LocalBackup> {
    let base = app_dir(root, app);
    let mut out: Vec<LocalBackup> = fs::read_dir(&base)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            let stored = plain_id(&id).then(|| read_stored(&e.path())).flatten()?;
            Some(backup_of(&id, e.path(), &stored))
        })
        .collect();
    out.sort_by(|a, b| b.id.cmp(&a.id));
    out
}

/// Makes a backup of `prepared` named `stamp`. `None` when the newest backup
/// already holds the same bytes. Copies are checked against their hashes and
/// a mismatch (a file changed meanwhile) removes the unfinished backup.
pub fn write(
    root: &Path,
    app: &str,
    title: &str,
    device: &str,
    stamp: &str,
    created_at: &str,
    prepared: &Prepared,
) -> Result<Option<LocalBackup>> {
    if !root.is_absolute() {
        return Err(SavesError::local(tr!("The backup folder has to be a full path.")));
    }
    if !plain_id(stamp) {
        return Err(SavesError::local("bad backup name"));
    }
    let base = app_dir(root, app);
    fs::create_dir_all(&base)?;
    if let Some(newest) = list(root, app).first().and_then(|b| read_stored(&b.dir)) {
        if same_content(&newest.manifest, &prepared.manifest) {
            return Ok(None);
        }
    }
    for e in fs::read_dir(&base)?.flatten() {
        if e.file_name().to_string_lossy().ends_with(".partial") {
            drop(fs::remove_dir_all(e.path()));
        }
    }
    let tmp = base.join(format!("{stamp}.partial"));
    let built = (|| -> Result<()> {
        for (entry, src) in prepared.manifest.files.iter().zip(prepared.sources()) {
            if !is_safe_path(&entry.path) {
                return Err(SavesError::local("unsafe path"));
            }
            let dest = tmp.join(FILES).join(&entry.path);
            if let Some(dir) = dest.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::copy(src, &dest)?;
            if !file_matches(&dest, entry) {
                return Err(changed(&entry.path));
            }
        }
        let stored =
            Stored { v: 1, title: title.into(), created_at: created_at.into(), device: device.into(), manifest: prepared.manifest.clone() };
        fs::write(tmp.join(MANIFEST), serde_json::to_vec_pretty(&stored).map_err(|e| SavesError::local(e.to_string()))?)?;
        Ok(())
    })();
    if let Err(e) = built {
        drop(fs::remove_dir_all(&tmp));
        return Err(e);
    }
    let dir = base.join(stamp);
    fs::rename(&tmp, &dir).inspect_err(|_| drop(fs::remove_dir_all(&tmp)))?;
    for old in list(root, app).into_iter().skip(KEEP) {
        if let Err(e) = fs::remove_dir_all(&old.dir) {
            log::warn!("could not remove the old backup {}: {e}", old.dir.display());
        }
    }
    let stored = read_stored(&dir).ok_or_else(|| SavesError::local("the backup could not be read back"))?;
    Ok(Some(backup_of(stamp, dir, &stored)))
}

/// A backup's manifest and the folder its files are in.
pub fn open(root: &Path, app: &str, id: &str) -> Result<(Manifest, PathBuf)> {
    let gone = || SavesError::local(tr!("That backup is not in the folder any more."));
    if !plain_id(id) {
        return Err(gone());
    }
    let dir = app_dir(root, app).join(id);
    let stored = read_stored(&dir).ok_or_else(gone)?;
    Ok((stored.manifest, dir.join(FILES)))
}

/// Deletes one backup (only a folder with a valid `manifest.json`).
pub fn delete(root: &Path, app: &str, id: &str) -> Result<()> {
    open(root, app, id)?;
    fs::remove_dir_all(app_dir(root, app).join(id))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Me;
    use crate::snapshot::{prepare, LocalFile};

    fn prepared(dir: &Path, text: &str) -> Prepared {
        let full = dir.join("src/slot.sav");
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(&full, text).unwrap();
        prepare(vec![LocalFile { path: "base/slot.sav".into(), full }], &Me::default().effective_limits(), "pc", "", None).unwrap()
    }

    #[test]
    fn backs_up_dedupes_lists_opens_and_prunes() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("backups");
        let first =
            write(&root, "steam:1", "G", "pc", "20260101T000000000Z", "2026-01-01T00:00:00Z", &prepared(d.path(), "one")).unwrap().unwrap();
        assert_eq!((first.files, first.bytes), (1, 3));
        assert!(
            write(&root, "steam:1", "G", "pc", "20260102T000000000Z", "t", &prepared(d.path(), "one")).unwrap().is_none(),
            "same bytes"
        );
        for n in 0..KEEP + 2 {
            write(&root, "steam:1", "G", "pc", &format!("202602{n:02}T000000000Z"), "t", &prepared(d.path(), &format!("v{n}"))).unwrap();
        }
        let all = list(&root, "steam:1");
        assert_eq!(all.len(), KEEP, "the oldest are pruned");
        assert!(all[0].id > all[1].id);
        let (m, files) = open(&root, "steam:1", &all[0].id).unwrap();
        assert!(file_matches(&files.join(&m.files[0].path), &m.files[0]));
        assert!(app_dir(&root, "steam:1").ends_with("steam_1"));
        delete(&root, "steam:1", &all[0].id).unwrap();
        assert_eq!(list(&root, "steam:1").len(), KEEP - 1);
    }

    #[test]
    fn refuses_odd_names_and_relative_folders() {
        let d = tempfile::tempdir().unwrap();
        assert!(open(d.path(), "a", "../x").is_err());
        assert!(open(d.path(), "a", "").is_err());
        assert!(delete(d.path(), "a", "nothing").is_err());
        assert!(write(Path::new("rel"), "a", "G", "pc", "20260101T000000000Z", "t", &prepared(d.path(), "x")).is_err());
        assert!(write(d.path(), "a", "G", "pc", "../evil", "t", &prepared(d.path(), "x")).is_err());
    }

    #[test]
    fn a_folder_that_is_not_a_backup_is_left_alone() {
        let d = tempfile::tempdir().unwrap();
        let junk = app_dir(d.path(), "a").join("20250101T000000000Z");
        fs::create_dir_all(&junk).unwrap();
        fs::write(junk.join("mine.txt"), "x").unwrap();
        assert!(list(d.path(), "a").is_empty());
        assert!(delete(d.path(), "a", "20250101T000000000Z").is_err());
        assert!(junk.join("mine.txt").exists());
    }
}
