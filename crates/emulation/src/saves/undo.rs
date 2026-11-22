//! A copy of what a restore overwrites, to put it back.
//!
//! The server's own history is no safety net for the current saves: with a small
//! retention it would push out the very backup being restored. So before a
//! restore the files it will write are copied to
//! `save-restore-undo/<game>/<time>/` in the data folder, with `undo.json`
//! listing them; files the restore creates are listed without a copy and are
//! deleted on undo. The newest [`KEEP`] copies of each game are kept.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::iso::iso_now;

/// Undo copies kept per game.
pub const KEEP: usize = 3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoEntry {
    pub target: String,
    /// File name of the copy in `files/`; `None`: the restore created it.
    pub stored: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoRecord {
    /// The folder of this copy.
    #[serde(skip)]
    pub dir: PathBuf,
    pub created_at: String,
    /// The app id of the game and the backup restored.
    pub game_name: String,
    pub backup_id: String,
    pub entries: Vec<UndoEntry>,
}

/// `app_name` as a folder name.
fn game_key(app_name: &str) -> String {
    let key: String = app_name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    if key.is_empty() {
        "_".into()
    } else {
        key
    }
}

/// Copies the files at `targets` that exist and records the others.
pub fn snapshot(root: &Path, app_name: &str, game_name: &str, backup_id: &str, targets: &[String]) -> Result<UndoRecord> {
    let game_dir = root.join(game_key(app_name));
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ").to_string();
    let dir = game_dir.join(&stamp);
    let written = write_snapshot(&dir, targets).and_then(|entries| {
        let record =
            UndoRecord { dir: dir.clone(), created_at: iso_now(), game_name: game_name.into(), backup_id: backup_id.into(), entries };
        fs::write(dir.join("undo.json"), serde_json::to_string_pretty(&record)?)?;
        Ok(record)
    });
    match written {
        Ok(record) => {
            prune(&game_dir);
            Ok(record)
        }
        // Without `undo.json` a partial copy is never listed nor pruned.
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

fn write_snapshot(dir: &Path, targets: &[String]) -> Result<Vec<UndoEntry>> {
    let files = dir.join("files");
    fs::create_dir_all(&files).with_context(|| format!("creating {}", files.display()))?;
    let mut entries = Vec::new();
    for (i, target) in targets.iter().enumerate() {
        let src = Path::new(target);
        let stored = if src.is_file() {
            let name = i.to_string();
            let copy = files.join(&name);
            fs::copy(src, &copy).with_context(|| format!("copying {target}"))?;
            // `fs::copy` keeps the permissions but not the time, and a save's
            // time is part of it.
            if let Ok(m) = fs::metadata(src).and_then(|m| m.modified()) {
                if let Ok(f) = fs::File::options().write(true).open(&copy) {
                    drop(f.set_modified(m));
                }
            }
            Some(name)
        } else {
            None
        };
        entries.push(UndoEntry { target: target.clone(), stored });
    }
    Ok(entries)
}

fn copies(game_dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(game_dir)
        .map(|it| it.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.join("undo.json").is_file()).collect())
        .unwrap_or_default();
    dirs.sort();
    dirs
}

fn prune(game_dir: &Path) {
    let dirs = copies(game_dir);
    for old in dirs.iter().take(dirs.len().saturating_sub(KEEP)) {
        if let Err(e) = fs::remove_dir_all(old) {
            log::warn!("could not remove the undo copy {}: {e}", old.display());
        }
    }
}

/// The newest undo copy of a game.
pub fn latest(root: &Path, app_name: &str) -> Option<UndoRecord> {
    let dir = copies(&root.join(game_key(app_name))).pop()?;
    let mut record: UndoRecord = serde_json::from_str(&fs::read_to_string(dir.join("undo.json")).ok()?).ok()?;
    record.dir = dir;
    Some(record)
}

/// Puts the copied files back, deletes the ones the restore created, then
/// drops the copy. Returns how many files were put back or removed.
pub fn undo(record: &UndoRecord) -> Result<usize> {
    let mut done = 0;
    for e in &record.entries {
        let target = Path::new(&e.target);
        match &e.stored {
            Some(name) => {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                let copy = record.dir.join("files").join(name);
                fs::copy(&copy, target).with_context(|| format!("restoring {}", e.target))?;
                if let Ok(m) = fs::metadata(&copy).and_then(|m| m.modified()) {
                    if let Ok(f) = fs::File::options().write(true).open(target) {
                        drop(f.set_modified(m));
                    }
                }
                done += 1;
            }
            None if target.is_file() => {
                fs::remove_file(target).with_context(|| format!("removing {}", e.target))?;
                done += 1;
            }
            None => {}
        }
    }
    discard(record);
    Ok(done)
}

/// Deletes an undo copy.
pub fn discard(record: &UndoRecord) {
    if let Err(e) = fs::remove_dir_all(&record.dir) {
        log::warn!("could not remove the undo copy {}: {e}", record.dir.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_and_undo() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("undo");
        let saves = d.path().join("saves");
        fs::create_dir_all(&saves).unwrap();
        let old = saves.join("slot1.sav");
        let new = saves.join("deep/slot2.sav");
        fs::write(&old, "old").unwrap();
        let targets = vec![old.to_string_lossy().into_owned(), new.to_string_lossy().into_owned()];
        let rec = snapshot(&root, "app/1", "Game", "backup-1.zip", &targets).unwrap();
        assert_eq!(rec.entries[0].stored.as_deref(), Some("0"));
        assert_eq!(rec.entries[1].stored, None);

        // The restore overwrites one file and creates the other.
        fs::write(&old, "restored").unwrap();
        fs::create_dir_all(new.parent().unwrap()).unwrap();
        fs::write(&new, "restored").unwrap();

        let found = latest(&root, "app/1").unwrap();
        assert_eq!(found, rec);
        assert_eq!(undo(&found).unwrap(), 2);
        assert_eq!(fs::read_to_string(&old).unwrap(), "old");
        assert!(!new.exists());
        assert!(latest(&root, "app/1").is_none());
    }

    #[test]
    #[cfg(unix)]
    fn undo_keeps_the_time_of_the_file() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("undo");
        let saves = d.path().join("saves");
        fs::create_dir_all(&saves).unwrap();
        let f = saves.join("slot.sav");
        fs::write(&f, "old").unwrap();
        let when = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        fs::File::options().write(true).open(&f).unwrap().set_modified(when).unwrap();
        let targets = vec![f.to_string_lossy().into_owned()];
        snapshot(&root, "g", "G", "b", &targets).unwrap();

        fs::write(&f, "restored").unwrap();
        undo(&latest(&root, "g").unwrap()).unwrap();
        let back = fs::metadata(&f).unwrap().modified().unwrap();
        let diff = back.duration_since(when).map(|d| d.as_secs()).unwrap_or_else(|e| e.duration().as_secs());
        assert!(diff < 2, "mtime is {back:?}, wanted {when:?}");
        assert_eq!(fs::read_to_string(&f).unwrap(), "old");
    }

    #[test]
    fn keeps_the_newest_copies() {
        let d = tempfile::tempdir().unwrap();
        for _ in 0..KEEP + 2 {
            snapshot(d.path(), "g", "G", "b", &[]).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(copies(&d.path().join("g")).len(), KEEP);
        assert_eq!(game_key(""), "_");
        assert_eq!(game_key("a b/c"), "a_b_c");
    }
}
