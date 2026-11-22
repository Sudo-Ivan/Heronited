//! What this PC remembers of its backups, so the Collection can say when a
//! game was last saved without asking the server every time a game is
//! selected. The server stays the truth: this is only a note.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::Result;
use heronite_data::models::game_key;
use heronite_data::{json_store, DataPaths};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GameNote {
    /// ISO time of the backup.
    pub at: String,
    /// The game's name in the game list.
    pub name: String,
    pub app: String,
    pub snapshot: String,
    pub files: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    games: BTreeMap<String, GameNote>,
}

fn file(paths: &DataPaths) -> PathBuf {
    paths.root.join("saves-state.json")
}

pub(super) fn remember(paths: &DataPaths, runner: &str, app_name: &str, note: GameNote) -> Result<()> {
    json_store::update::<StateFile, _>(&file(paths), |f| {
        f.games.insert(game_key(runner, app_name), note);
        Ok(())
    })
}

/// The note of the last backup of a game on this PC.
pub fn note_of(paths: &DataPaths, runner: &str, app_name: &str) -> Option<GameNote> {
    let f: StateFile = json_store::read(&file(paths)).ok()?;
    f.games.get(&game_key(runner, app_name)).cloned()
}

/// The newest backup of a game.
#[derive(Debug, Clone, PartialEq)]
pub struct LastBackup {
    /// The game's name in the game list.
    pub game_name: String,
    pub at: SystemTime,
    pub files: usize,
    pub bytes: u64,
}

/// When the game was last backed up from this PC. `None` when the saves are
/// off or the game was never backed up here.
pub fn last_backup(paths: &DataPaths, runner: &str, app_name: &str) -> Option<LastBackup> {
    if !heronite_data::settings::load(paths).ok()?.saves.enabled {
        return None;
    }
    let note = note_of(paths, runner, app_name)?;
    let at = chrono::DateTime::parse_from_rfc3339(&note.at).ok()?.into();
    Some(LastBackup { game_name: note.name, at, files: note.files, bytes: note.bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_kept_per_game_and_read_only_while_the_saves_are_on() {
        let d = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(d.path());
        std::fs::create_dir_all(paths.local_library()).unwrap();
        let note = GameNote {
            at: "2026-01-02T03:04:05.000Z".into(),
            name: "Hades".into(),
            app: "steam:1".into(),
            snapshot: "s".into(),
            files: 3,
            bytes: 9,
        };
        remember(&paths, "steam", "a", note.clone()).unwrap();
        assert_eq!(note_of(&paths, "steam", "a"), Some(note));
        assert_eq!(note_of(&paths, "steam", "b"), None);
        assert_eq!(last_backup(&paths, "steam", "a"), None, "off");
        std::fs::write(paths.collection_settings(), r#"{"saves":{"enabled":true}}"#).unwrap();
        let last = last_backup(&paths, "steam", "a").unwrap();
        assert_eq!((last.game_name.as_str(), last.files), ("Hades", 3));
    }
}
