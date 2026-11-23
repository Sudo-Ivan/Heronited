//! Settings writers for the saves section of `local_library/settings.json`
//! and its "last backup" status fields. Read-modify-write like the other
//! settings, so keys written by other parts survive.

use anyhow::Result;
use heronite_data::models::{CollectionSettingsFile, SavesSettings};
use heronite_data::{json_store, settings, DataPaths};

/// A change to the saves settings (`None` leaves a field alone).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavesSettingsPatch {
    #[serde(default)]
    pub enabled: Option<bool>,
    /// The server address, as typed; `Some("")` goes back to the default.
    #[serde(default)]
    pub server: Option<String>,
    /// Stop sending saves to the server (they go to the local folder only).
    #[serde(default)]
    pub server_off: Option<bool>,
    /// Also back up the Wine prefix data of games that have one.
    #[serde(default)]
    pub prefix_backup: Option<bool>,
    /// The folder of local backups; `Some("")` turns them off.
    #[serde(default)]
    pub local_folder: Option<String>,
    /// Minutes between backups while a game runs; `Some(None)` turns them off.
    #[serde(default)]
    pub interval_minutes: Option<Option<u32>>,
}

fn current(paths: &DataPaths) -> Result<SavesSettings> {
    Ok(settings::load(paths)?.saves)
}

fn write(paths: &DataPaths, next: SavesSettings) -> Result<()> {
    json_store::update::<CollectionSettingsFile, _>(&paths.collection_settings(), |f| {
        f.saves = Some(next);
        Ok(())
    })
}

/// Applies `patch`; returns the effective settings. A server address that is
/// not acceptable is refused and nothing is written.
pub fn set_saves_settings(paths: &DataPaths, patch: &SavesSettingsPatch) -> Result<settings::EffectiveSettings> {
    let mut next = current(paths)?;
    if let Some(v) = patch.enabled {
        next.enabled = v;
    }
    if let Some(v) = &patch.server {
        let before = next.server.clone();
        let v = v.trim();
        // The default is kept as no address at all, so it follows the app.
        next.server = if v.is_empty() {
            String::new()
        } else {
            let norm = heronite_saves::normalize_server(v).map_err(|e| anyhow::anyhow!("{e}"))?;
            if norm == heronite_saves::DEFAULT_SERVER {
                String::new()
            } else {
                norm
            }
        };
        // Only a real change drops the session; re-saving the same address
        // must not sign the account out.
        if next.server != before {
            crate::saves::account::forget_session();
        }
    }
    if let Some(v) = patch.server_off {
        next.server_off = v;
    }
    if let Some(v) = patch.prefix_backup {
        next.prefix_backup = v;
    }
    if let Some(v) = &patch.local_folder {
        let v = v.trim();
        if !v.is_empty() && !std::path::Path::new(v).is_absolute() {
            anyhow::bail!(heronite_i18n::tr!("The backup folder has to be a full path."));
        }
        next.local_folder = v.to_string();
    }
    if let Some(v) = patch.interval_minutes {
        next.interval_minutes = settings::as_saves_interval(v);
    }
    write(paths, next)?;
    settings::load(paths)
}

/// Leaves one game out of the after-play backup (`on: false`) or puts it
/// back in.
pub fn set_auto_backup(paths: &DataPaths, runner: &str, app_name: &str, on: bool) -> Result<()> {
    let mut next = current(paths)?;
    let key = heronite_data::models::game_key(runner, app_name);
    next.auto_backup_off.retain(|k| *k != key);
    if !on {
        next.auto_backup_off.push(key);
    }
    write(paths, next)
}

/// Names the folder one game's saves are in (`None`/empty clears it), for
/// games whose saves the rules cannot find.
pub fn set_manual_saves_folder(paths: &DataPaths, runner: &str, app_name: &str, folder: Option<&str>) -> Result<()> {
    let mut next = current(paths)?;
    let key = heronite_data::models::game_key(runner, app_name);
    let folder = folder.map(str::trim).filter(|f| !f.is_empty());
    match folder {
        Some(f) => {
            if !std::path::Path::new(f).is_absolute() {
                anyhow::bail!(heronite_i18n::tr!("The saves folder has to be a full path."));
            }
            next.manual_saves_folders.insert(key, f.to_string());
        }
        None => {
            next.manual_saves_folders.shift_remove(&key);
        }
    }
    write(paths, next)
}

/// Outcome of a save backup to record.
#[derive(Debug, Clone, Default)]
pub struct SavesRecord {
    pub at: Option<String>,
    pub game: Option<String>,
    pub error: Option<String>,
}

/// An error keeps the previous success fields and sets `lastError`; a
/// success replaces them and clears `lastError`.
pub fn record_saves_backup(paths: &DataPaths, rec: &SavesRecord) -> Result<()> {
    let mut next = current(paths)?;
    if rec.error.is_none() {
        next.last_backup_at = rec.at.clone();
        next.last_backup_game = rec.game.clone();
    }
    next.last_error = rec.error.clone();
    write(paths, next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(json: &str) -> (tempfile::TempDir, DataPaths) {
        let d = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(d.path());
        std::fs::create_dir_all(paths.local_library()).unwrap();
        std::fs::write(paths.collection_settings(), json).unwrap();
        (d, paths)
    }

    #[test]
    fn patches_and_records() {
        let (_d, paths) = setup(r#"{"greyUninstalledGames":false}"#);
        let eff = set_saves_settings(
            &paths,
            &SavesSettingsPatch { enabled: Some(true), server: Some("  saves.example.com/ ".into()), ..Default::default() },
        )
        .unwrap();
        assert!(eff.saves.enabled);
        assert_eq!(eff.saves.server, "https://saves.example.com");
        let eff = set_saves_settings(&paths, &SavesSettingsPatch { server: Some("saves.quad4.io".into()), ..Default::default() }).unwrap();
        assert_eq!(eff.saves.server, "", "the default is stored as no server");
        assert!(eff.saves.enabled);
        assert_eq!(json_store::read_value(&paths.collection_settings()).unwrap()["greyUninstalledGames"], false);

        record_saves_backup(&paths, &SavesRecord { at: Some("t1".into()), game: Some("Hades".into()), error: None }).unwrap();
        record_saves_backup(&paths, &SavesRecord { error: Some("boom".into()), ..Default::default() }).unwrap();
        let s = settings::load(&paths).unwrap().saves;
        assert_eq!((s.last_backup_game.as_deref(), s.last_error.as_deref()), (Some("Hades"), Some("boom")));
        record_saves_backup(&paths, &SavesRecord { at: Some("t2".into()), game: Some("G".into()), error: None }).unwrap();
        let s = settings::load(&paths).unwrap().saves;
        assert_eq!((s.last_error, s.last_backup_at.as_deref()), (None, Some("t2")));
    }

    #[test]
    fn the_local_folder_is_kept_and_has_to_be_a_full_path() {
        let (d, paths) = setup(r#"{"saves":{"enabled":true}}"#);
        let folder = d.path().join("saves").to_string_lossy().into_owned();
        let eff = set_saves_settings(
            &paths,
            &SavesSettingsPatch {
                local_folder: Some(format!(" {folder} ")),
                server_off: Some(true),
                prefix_backup: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!((eff.saves.local_folder.as_str(), eff.saves.server_off, eff.saves.prefix_backup), (folder.as_str(), true, true));
        assert!(set_saves_settings(&paths, &SavesSettingsPatch { local_folder: Some("relative/x".into()), ..Default::default() }).is_err());
        assert_eq!(settings::load(&paths).unwrap().saves.local_folder, folder);
        let eff = set_saves_settings(&paths, &SavesSettingsPatch { local_folder: Some(String::new()), ..Default::default() }).unwrap();
        assert!(eff.saves.local_folder.is_empty());
    }

    #[test]
    fn a_bad_server_is_refused_and_nothing_changes() {
        let (_d, paths) = setup(r#"{"saves":{"enabled":true,"server":"https://a.example.com"}}"#);
        for bad in ["http://a.example.com", "https://u:p@a.example.com", "ftp://x"] {
            assert!(set_saves_settings(&paths, &SavesSettingsPatch { server: Some(bad.into()), ..Default::default() }).is_err(), "{bad}");
        }
        assert_eq!(settings::load(&paths).unwrap().saves.server, "https://a.example.com");
    }

    #[test]
    fn interval_patch_round_trips() {
        let (_d, paths) = setup(r#"{"greyUninstalledGames":true,"saves":{"enabled":true}}"#);
        let eff = set_saves_settings(&paths, &SavesSettingsPatch { interval_minutes: Some(Some(30)), ..Default::default() }).unwrap();
        assert_eq!(eff.saves.interval_minutes, Some(30));
        assert_eq!(json_store::read_value(&paths.collection_settings()).unwrap()["saves"]["intervalMinutes"], 30);
        record_saves_backup(&paths, &SavesRecord { at: Some("t".into()), ..Default::default() }).unwrap();
        assert_eq!(settings::load(&paths).unwrap().saves.interval_minutes, Some(30));
        let eff = set_saves_settings(&paths, &SavesSettingsPatch { interval_minutes: Some(Some(2)), ..Default::default() }).unwrap();
        assert_eq!(eff.saves.interval_minutes, Some(5));
        let eff = set_saves_settings(&paths, &SavesSettingsPatch { interval_minutes: Some(None), ..Default::default() }).unwrap();
        assert_eq!(eff.saves.interval_minutes, None);
        let raw = json_store::read_value(&paths.collection_settings()).unwrap();
        assert!(raw["saves"].get("intervalMinutes").is_none(), "{raw}");
        assert_eq!(raw["greyUninstalledGames"], true);
    }

    #[test]
    fn a_game_can_name_its_saves_folder() {
        let (d, paths) = setup(r#"{"saves":{"enabled":true}}"#);
        let folder = d.path().join("saves").to_string_lossy().into_owned();
        let other = d.path().join("other").to_string_lossy().into_owned();
        assert!(set_manual_saves_folder(&paths, "gog", "1", Some("relative/x")).is_err());
        set_manual_saves_folder(&paths, "gog", "1", Some(&folder)).unwrap();
        set_manual_saves_folder(&paths, "sideload", "2", Some(&other)).unwrap();
        let s = settings::load(&paths).unwrap().saves;
        assert_eq!(s.manual_saves_folder_for("gog", "1"), Some(folder.as_str()));
        assert_eq!(s.manual_saves_folder_for("sideload", "2"), Some(other.as_str()));
        assert_eq!(s.manual_saves_folder_for("gog", "2"), None);
        set_manual_saves_folder(&paths, "gog", "1", None).unwrap();
        let s = settings::load(&paths).unwrap().saves;
        assert_eq!(s.manual_saves_folder_for("gog", "1"), None);
        assert_eq!(s.manual_saves_folder_for("sideload", "2"), Some(other.as_str()), "other games keep theirs");
        let raw = json_store::read_value(&paths.collection_settings()).unwrap();
        assert!(raw["saves"]["manualSavesFolders"].get("gog:1").is_none(), "{raw}");
    }

    #[test]
    fn auto_backup_can_be_off_for_one_game() {
        let (_d, paths) = setup(r#"{"saves":{"enabled":true}}"#);
        set_auto_backup(&paths, "gog", "123", false).unwrap();
        set_auto_backup(&paths, "gog", "123", false).unwrap();
        let s = settings::load(&paths).unwrap().saves;
        assert_eq!(s.auto_backup_off, ["gog:123"]);
        assert!(!s.auto_backup_for("gog", "123") && s.auto_backup_for("legendary", "123"));
        record_saves_backup(&paths, &SavesRecord { error: Some("x".into()), ..Default::default() }).unwrap();
        assert_eq!(settings::load(&paths).unwrap().saves.auto_backup_off, ["gog:123"]);
        set_auto_backup(&paths, "gog", "123", true).unwrap();
        let raw = json_store::read_value(&paths.collection_settings()).unwrap();
        assert!(raw["saves"].get("autoBackupOff").is_none());
    }
}
