//! Backs up, restores and undoes through the code the app runs, against a
//! real saves server when `SAVES_E2E_SERVER` is set (for example
//! `http://127.0.0.1:18787` with open registration). The data folder, the
//! install folder, the key and the game list are all temporary.

use std::fs;

use heronite_data::DataPaths;
use heronite_emulation::saves::restore::{apply_restore, list_snapshots, plan_restore, Change};
use heronite_emulation::saves::undo;
use heronite_emulation::saves::{backup_for_game, BackupReason, SavesEnv, SavesGame};
use heronite_launch::SystemProbe;
use heronite_saves::manifest::{self, Game, Rule};

/// Backup and restore take one in-flight guard for the whole process, so the
/// tests cannot overlap.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn backup_list_restore_and_undo() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(server) = std::env::var("SAVES_E2E_SERVER") else { return };
    let tmp = tempfile::tempdir().unwrap();
    let paths = DataPaths::at(tmp.path().join("data"));
    fs::create_dir_all(paths.local_library()).unwrap();
    fs::write(paths.collection_settings(), format!(r#"{{"saves":{{"enabled":true,"server":"{server}"}}}}"#)).unwrap();
    let game_entry = Game {
        title: "E2E Game".into(),
        files: vec![Rule { path: "<base>/saves".into(), tags: "s".into(), when: vec![] }],
        ..Default::default()
    };
    manifest::store(&manifest::dir(&paths.root), vec![game_entry], "").unwrap();

    let install = tmp.path().join("install");
    fs::create_dir_all(install.join("saves/sub")).unwrap();
    fs::write(install.join("saves/slot1.sav"), "one").unwrap();
    fs::write(install.join("saves/sub/slot2.sav"), "two").unwrap();
    fs::write(install.join("other.txt"), "not a save").unwrap();
    let game = SavesGame {
        app_name: "e2e".into(),
        title: "E2E Game".into(),
        runner: "sideload".into(),
        install_dir: Some(install.clone()),
        ..Default::default()
    };
    let env = SavesEnv { paths: &paths, probe: &SystemProbe };

    let first = backup_for_game(&env, &game, BackupReason::Manual);
    assert!(first.ran && !first.unchanged, "{first:?}");
    assert_eq!(first.files, Some(2));
    let again = backup_for_game(&env, &game, BackupReason::Manual);
    assert!(again.ran && again.unchanged, "identical saves are not sent twice: {again:?}");

    fs::write(install.join("saves/slot1.sav"), "ONE CHANGED").unwrap();
    let second = backup_for_game(&env, &game, BackupReason::Manual);
    assert!(second.ran && !second.unchanged, "{second:?}");

    let listing = list_snapshots(&env, &game);
    assert!(listing.server_error.is_none(), "{:?}", listing.server_error);
    let list = listing.snapshots;
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].id, second.snapshot.clone().unwrap());

    // Restore the first backup over the changed file and a deleted one.
    fs::remove_file(install.join("saves/sub/slot2.sav")).unwrap();
    let plan = plan_restore(&env, &game, &list[1]).unwrap();
    let change = |name: &str| plan.files.iter().find(|f| f.path.ends_with(name)).unwrap().change;
    assert_eq!(change("slot1.sav"), Change::Different);
    assert_eq!(change("slot2.sav"), Change::New);
    let undo_root = paths.save_restore_undo();
    let outcome = apply_restore(&env, &plan, &undo_root, &game.app_name).unwrap();
    assert_eq!((outcome.written, outcome.failed.len()), (2, 0));
    assert_eq!(fs::read_to_string(install.join("saves/slot1.sav")).unwrap(), "one");
    assert_eq!(fs::read_to_string(install.join("saves/sub/slot2.sav")).unwrap(), "two");
    assert!(
        fs::read_dir(install.join("saves")).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".tmp")),
        "no temp files are left"
    );

    // Undo puts the changed file back and removes the restored one.
    assert_eq!(undo::undo(&outcome.undo.unwrap()).unwrap(), 2);
    assert_eq!(fs::read_to_string(install.join("saves/slot1.sav")).unwrap(), "ONE CHANGED");
    assert!(!install.join("saves/sub/slot2.sav").exists());

    // A file the plan leaves out is never written.
    let mut plan = plan_restore(&env, &game, &list[1]).unwrap();
    plan.files.iter_mut().for_each(|f| f.selected = f.path.ends_with("slot2.sav"));
    let outcome = apply_restore(&env, &plan, &undo_root, &game.app_name).unwrap();
    assert_eq!(outcome.written, 1);
    assert_eq!(fs::read_to_string(install.join("saves/slot1.sav")).unwrap(), "ONE CHANGED");
}

#[test]
fn local_folder_backup_and_restore_without_a_server() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let paths = DataPaths::at(tmp.path().join("data"));
    fs::create_dir_all(paths.local_library()).unwrap();
    let folder = tmp.path().join("my backups");
    fs::write(
        paths.collection_settings(),
        format!(
            r#"{{"saves":{{"enabled":true,"serverOff":true,"localFolder":{}}}}}"#,
            serde_json::to_string(&folder.to_string_lossy()).unwrap()
        ),
    )
    .unwrap();
    let game_entry = Game {
        title: "Local Game".into(),
        files: vec![Rule { path: "<base>/saves".into(), tags: "s".into(), when: vec![] }],
        ..Default::default()
    };
    manifest::store(&manifest::dir(&paths.root), vec![game_entry], "").unwrap();
    let install = tmp.path().join("install");
    fs::create_dir_all(install.join("saves")).unwrap();
    fs::write(install.join("saves/a.sav"), "one").unwrap();
    let game = SavesGame {
        app_name: "lg".into(),
        title: "Local Game".into(),
        runner: "sideload".into(),
        install_dir: Some(install.clone()),
        ..Default::default()
    };
    let env = SavesEnv { paths: &paths, probe: &SystemProbe };

    let first = backup_for_game(&env, &game, BackupReason::Manual);
    assert!(first.ran && !first.server && !first.unchanged && first.local_path.is_some(), "{first:?}");
    assert!(backup_for_game(&env, &game, BackupReason::Manual).unchanged, "same bytes are not copied twice");
    assert!(!paths.root.join("saves-key.json").exists(), "no account is made when the server is off");

    fs::write(install.join("saves/a.sav"), "two").unwrap();
    assert!(!backup_for_game(&env, &game, BackupReason::Manual).unchanged);
    let listing = list_snapshots(&env, &game);
    assert!(listing.server_error.is_none() && !listing.off);
    assert_eq!(listing.snapshots.len(), 2);
    assert!(listing.snapshots[0].id.starts_with("local:"));

    let plan = plan_restore(&env, &game, &listing.snapshots[1]).unwrap();
    let outcome = apply_restore(&env, &plan, &paths.save_restore_undo(), &game.app_name).unwrap();
    assert_eq!(outcome.written, 1);
    assert_eq!(fs::read_to_string(install.join("saves/a.sav")).unwrap(), "one");
    assert_eq!(undo::undo(&outcome.undo.unwrap()).unwrap(), 1);
    assert_eq!(fs::read_to_string(install.join("saves/a.sav")).unwrap(), "two");

    // A damaged copy in the folder is refused and changes nothing.
    let stamp = listing.snapshots[1].id.trim_start_matches("local:").to_string();
    let copy = folder.join("game_local-game/").join(&stamp).join("files/base/saves/a.sav");
    fs::write(copy, "tampered").unwrap();
    let plan = plan_restore(&env, &game, &listing.snapshots[1]).unwrap();
    assert!(apply_restore(&env, &plan, &paths.save_restore_undo(), &game.app_name).is_err());
    assert_eq!(fs::read_to_string(install.join("saves/a.sav")).unwrap(), "two");
}
