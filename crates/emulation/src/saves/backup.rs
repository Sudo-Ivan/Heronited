//! The backup flow: one game, or after a play session.
//!
//! The game list says where the game keeps its saves, those files are read
//! and hashed, and the snapshot goes to the server. Nothing is sent when no
//! save file exists (an empty state never becomes the newest backup), when it
//! would not fit in the account, or when the server already holds the same
//! bytes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use heronite_data::{settings, DataPaths};
use heronite_i18n::{tr, trn};
use heronite_saves::locations;
use heronite_saves::manifest;
use heronite_saves::snapshot::{self, SkipReason};
use heronite_saves::SavesError;
use serde::{Deserialize, Serialize};

use super::account;
use super::game::{target_for, SavesEnv, SavesGame, SLOT};
use super::state::{remember, GameNote};
use crate::iso::iso_now;
use crate::settings_ext::{record_saves_backup, SavesRecord};

pub const SKIP_SESSION_SECONDS: u64 = 5;

/// How long the after-play backup waits for one already running, so the last
/// backup of a session is not lost to a periodic one still sending.
pub const AFTER_PLAY_WAIT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupReason {
    /// After a play session: waits up to [`AFTER_PLAY_WAIT`] for a backup
    /// already running.
    Auto,
    /// While the game runs: skipped when another backup is running.
    Periodic,
    Manual,
}

impl BackupReason {
    fn automatic(self) -> bool {
        self != BackupReason::Manual
    }

    fn label(self) -> &'static str {
        match self {
            BackupReason::Auto => "after play",
            BackupReason::Periodic => "while playing",
            BackupReason::Manual => "by hand",
        }
    }
}

/// What a backup did.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BackupResult {
    /// A backup was made, or the server already had these exact saves.
    pub ran: bool,
    /// 'disabled' | 'off-for-game' | 'no-match' | 'no-saves' | 'no-destination' | 'already-running'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
    /// The game's name in the game list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Nothing new anywhere: the server and the folder already held these exact saves.
    #[serde(default)]
    pub unchanged: bool,
    /// The saves went (or were checked) on the server.
    #[serde(default)]
    pub server: bool,
    /// The folder of this game's local backups, when one was made there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    /// Files left out (names or size the server cannot take, unreadable).
    #[serde(default)]
    pub left_out: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// One backup or restore at a time.
pub(super) struct InFlight;

impl InFlight {
    pub(super) fn acquire() -> Option<Self> {
        IN_FLIGHT.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).ok().map(|_| InFlight)
    }

    pub(super) fn acquire_within(wait: Duration) -> Option<Self> {
        let deadline = Instant::now() + wait;
        loop {
            if let Some(g) = Self::acquire() {
                return Some(g);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            std::thread::sleep(left.min(Duration::from_millis(100)));
        }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.store(false, Ordering::SeqCst);
    }
}

/// Tests that take the in-flight guard run one at a time.
#[cfg(test)]
pub(super) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
    L.lock().unwrap_or_else(|e| e.into_inner())
}

fn record_error(paths: &DataPaths, error: &str) {
    if let Err(e) = record_saves_backup(paths, &SavesRecord { error: Some(error.to_string()), ..Default::default() }) {
        log::error!("could not record the saves error: {e:#}");
    }
}

/// How old the game list may get before a backup looks for a newer one.
const LIST_MAX_AGE: Duration = Duration::from_secs(24 * 3600);

/// The game list, downloaded when there is none or it is a day old (a failed
/// refresh keeps the old list).
pub fn game_list(paths: &DataPaths) -> Result<std::sync::Arc<manifest::Index>, SavesError> {
    let folder = manifest::dir(&paths.root);
    let stale = manifest::age(&folder).is_none_or(|a| a > LIST_MAX_AGE);
    if stale {
        match manifest::update(&folder, manifest::MANIFEST_URL) {
            Ok(_) => {}
            Err(e) if manifest::load(&folder).is_some() => log::warn!("the game list could not be refreshed: {e}"),
            Err(e) => return Err(e),
        }
    }
    manifest::load(&folder)
        .ok_or_else(|| SavesError::local(tr!("The game list is not on this PC yet. Check the connection and try again.")))
}

/// The backup of one game. `Auto` waits for one already running.
pub fn backup_for_game(env: &SavesEnv, game: &SavesGame, reason: BackupReason) -> BackupResult {
    backup_for_game_within(env, game, reason, AFTER_PLAY_WAIT)
}

pub(super) fn backup_for_game_within(env: &SavesEnv, game: &SavesGame, reason: BackupReason, wait: Duration) -> BackupResult {
    log::info!("saves {reason:?} backup requested for {}", game.title);
    let guard = match reason {
        BackupReason::Auto => InFlight::acquire_within(wait),
        BackupReason::Periodic | BackupReason::Manual => InFlight::acquire(),
    };
    let Some(_guard) = guard else {
        log::info!("saves {reason:?} backup of {} skipped: another backup is running", game.title);
        return BackupResult { skipped_reason: Some("already-running".into()), ..Default::default() };
    };
    match backup_inner(env, game, reason) {
        Ok(r) => r,
        Err(e) => {
            let message = e.to_string();
            record_error(env.paths, &message);
            log::error!("saves backup failed: {message}");
            BackupResult { error: Some(message), ..Default::default() }
        }
    }
}

fn skipped(reason: &str, name: Option<String>, error: Option<String>) -> BackupResult {
    BackupResult { skipped_reason: Some(reason.into()), game_name: name, error, ..Default::default() }
}

fn backup_inner(env: &SavesEnv, game: &SavesGame, reason: BackupReason) -> Result<BackupResult, SavesError> {
    let cfg = settings::load(env.paths).map_err(|e| SavesError::local(format!("{e:#}")))?.saves;
    if reason.automatic() && !cfg.enabled {
        return Ok(skipped("disabled", None, None));
    }
    if reason.automatic() && !cfg.auto_backup_for(&game.runner, &game.app_name) {
        log::info!("saves auto backup is off for {}", game.title);
        return Ok(skipped("off-for-game", None, None));
    }
    let target = target_for(env, game);
    let custom_found = locations::find_custom(&target);
    let listed = match game_list(env.paths) {
        Ok(index) => {
            let names = game.names(env.paths);
            index
                .find(game.steam_app_id.as_deref(), game.store_game_id.as_deref().filter(|_| game.runner == "gog"), &names)
                .map(|entry| (entry.title.clone(), locations::find(entry, &target)))
        }
        Err(e) if target.custom.is_none() => return Err(e),
        Err(e) => {
            log::warn!("the game list could not be read, so only the saves folder named for {} is backed up: {e}", game.title);
            None
        }
    };
    if listed.is_none() && target.custom.is_none() {
        let error = tr!("{title} is not in the game list, so its saves cannot be found.", title = game.title).to_string();
        record_error(env.paths, &error);
        return Ok(skipped("no-match", None, Some(error)));
    }
    let (entry_title, mut found) = listed.unwrap_or_else(|| (game.title.clone(), Vec::new()));
    found.extend(custom_found);
    found.sort_by(|a, b| a.path.cmp(&b.path));
    // Saves only: prefix data goes along when there are saves, never alone.
    if found.is_empty() {
        let error = tr!("No save files of {title} were found on this PC.", title = game.title).to_string();
        record_error(env.paths, &error);
        return Ok(skipped("no-saves", Some(entry_title.clone()), Some(error)));
    }
    let use_server = !cfg.server_off;
    let local_root = Some(cfg.local_folder.trim()).filter(|f| !f.is_empty()).map(std::path::PathBuf::from);
    if !use_server && local_root.is_none() {
        let error =
            tr!("Saves are not going anywhere: turn on the saves server or choose a backup folder in Settings → Saves.").to_string();
        record_error(env.paths, &error);
        return Ok(skipped("no-destination", None, Some(error)));
    }
    let app = game.app_id();
    let files = || -> Vec<snapshot::LocalFile> {
        let mut all = found.clone();
        if cfg.prefix_backup {
            all.extend(locations::find_prefix(&target));
        }
        all.into_iter().map(Into::into).collect()
    };
    let mut conn = if use_server { Some(account::connect(env.paths)?) } else { None };
    let limits = |c: &Option<account::Connection>| {
        c.as_ref().map_or_else(|| heronite_saves::Me::default().effective_limits(), |c| c.me.effective_limits())
    };
    let prepared = snapshot::prepare(files(), &limits(&conn), &device(), reason.label(), None)?;
    let a_save_was_read = prepared.manifest.files.iter().any(|f| f.path.split('/').next() != Some(locations::PREFIX_KEY));
    if !a_save_was_read {
        let error = tr!("No save file of {title} could be read.", title = game.title).to_string();
        record_error(env.paths, &error);
        return Ok(skipped("no-saves", Some(entry_title.clone()), Some(error)));
    }
    let at = iso_now();
    let mut local_path = None;
    let mut local_id = None;
    if let Some(root) = &local_root {
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ").to_string();
        match heronite_saves::local::write(root, &app, &entry_title, &device(), &stamp, &at, &prepared) {
            Ok(made) => {
                local_path = Some(heronite_saves::local::app_dir(root, &app).to_string_lossy().into_owned());
                local_id = made.map(|b| format!("local:{}", b.id));
            }
            // With a server too, its copy still goes; alone, this is the error.
            Err(e) if use_server => log::error!("the local backup of {} failed: {e}", game.title),
            Err(e) => return Err(e),
        }
    }
    let (files_n, bytes_n) = (prepared.files(), prepared.total_bytes);
    // Files left out count whichever destination took the rest: the server's
    // outcome only exists when there is one.
    let left_out = prepared.skipped.iter().filter(|s| !matches!(s.reason, SkipReason::CaseCollision)).count();
    if left_out > 0 {
        log::warn!("{left_out} files of {} were left out of the backup", game.title);
    }
    let mut server_out = None;
    if let Some(c) = conn.take() {
        let outcome = snapshot::push(&c.client, &c.me, &app, SLOT, prepared, &|_, _| {}).or_else(|e| {
            if !e.is_unauthorized() {
                return Err(e);
            }
            account::forget_session();
            let c = account::connect(env.paths)?;
            let again = snapshot::prepare(files(), &c.me.effective_limits(), &device(), reason.label(), None)?;
            snapshot::push(&c.client, &c.me, &app, SLOT, again, &|_, _| {})
        })?;
        server_out = Some(outcome);
    }
    let snapshot_id = server_out.as_ref().and_then(|o| o.snapshot.clone()).or(local_id.clone());
    if let Some(id) = &snapshot_id {
        remember(
            env.paths,
            &game.runner,
            &game.app_name,
            GameNote { at: at.clone(), name: entry_title.clone(), app: app.clone(), snapshot: id.clone(), files: files_n, bytes: bytes_n },
        )
        .map_err(|e| SavesError::local(format!("{e:#}")))?;
    }
    record_saves_backup(env.paths, &SavesRecord { at: Some(at), game: Some(entry_title.clone()), error: None })
        .map_err(|e| SavesError::local(format!("{e:#}")))?;
    let unchanged = !server_out.as_ref().is_some_and(|o| o.snapshot.is_some()) && local_id.is_none();
    Ok(BackupResult {
        ran: true,
        game_name: Some(entry_title.clone()),
        snapshot: snapshot_id,
        files: Some(files_n),
        bytes: Some(bytes_n),
        unchanged,
        left_out,
        server: use_server,
        local_path,
        ..Default::default()
    })
}

fn device() -> String {
    let host = if cfg!(windows) { std::env::var("COMPUTERNAME").ok() } else { std::fs::read_to_string("/proc/sys/kernel/hostname").ok() };
    host.map(|h| h.trim().to_string()).filter(|h| !h.is_empty()).unwrap_or_else(|| "pc".into())
}

/// Arguments of an after-play backup.
#[derive(Debug, Clone)]
pub struct AfterPlay {
    pub app_name: String,
    pub title: String,
    pub runner: String,
    pub install_dir: Option<std::path::PathBuf>,
    pub elapsed_seconds: u64,
}

/// Skips sessions shorter than [`SKIP_SESSION_SECONDS`] (returns `None`),
/// otherwise runs an automatic backup with the ids kept in the game's meta.
pub fn backup_after_play(env: &SavesEnv, play: &AfterPlay) -> Option<BackupResult> {
    if play.elapsed_seconds < SKIP_SESSION_SECONDS {
        log::info!("Skipping the saves auto backup; session was only {}s", play.elapsed_seconds);
        return None;
    }
    let game = SavesGame::of_session(env.paths, &play.app_name, &play.title, &play.runner, play.install_dir.clone());
    let result = backup_for_game(env, &game, BackupReason::Auto);
    if !result.ran {
        if let Some(e) = &result.error {
            log::error!("saves backup after play failed: {e}");
        }
    }
    Some(result)
}

/// The desktop notification `(title, body)` for a finished after-play backup.
pub fn notification(result: &BackupResult, title: &str) -> Option<(String, String)> {
    if !result.ran || result.unchanged {
        return None;
    }
    let name = result.game_name.as_deref().unwrap_or(title);
    let body = match (&result.local_path, result.server, result.files) {
        (Some(path), false, Some(files)) => {
            trn!(files, "Saved {n} file of {name} to {folder}.", "Saved {n} files of {name} to {folder}.", name, folder = path.clone())
        }
        (Some(path), false, None) => tr!("Saved {name} to {path}", name, path = path.clone()),
        (_, _, Some(files)) => {
            trn!(files, "Saved {n} file of {name} to the saves server.", "Saved {n} files of {name} to the saves server.", name)
        }
        _ => tr!("Saved {name} to the saves server", name),
    };
    let title = if result.server { tr!("Cloud saves") } else { tr!("Saves") };
    Some((title.into(), body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(settings_json: &str) -> (tempfile::TempDir, DataPaths) {
        let d = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(d.path());
        std::fs::create_dir_all(paths.local_library()).unwrap();
        std::fs::write(paths.collection_settings(), settings_json).unwrap();
        (d, paths)
    }

    fn game() -> SavesGame {
        SavesGame { app_name: "g".into(), title: "G".into(), runner: "sideload".into(), ..Default::default() }
    }

    #[test]
    fn disabled_and_left_out_games_are_skipped_but_a_manual_backup_goes_on() {
        let _l = test_lock();
        let probe = heronite_launch::probe::FakeProbe::new("/h");
        let (_d, paths) = setup(r#"{"saves":{"enabled":false}}"#);
        let env = SavesEnv { paths: &paths, probe: &probe };
        assert_eq!(backup_for_game(&env, &game(), BackupReason::Auto).skipped_reason.as_deref(), Some("disabled"));
        let (_d1, paths1) = setup(r#"{"saves":{"enabled":true,"autoBackupOff":["sideload:g"]}}"#);
        let env1 = SavesEnv { paths: &paths1, probe: &probe };
        assert_eq!(backup_for_game(&env1, &game(), BackupReason::Auto).skipped_reason.as_deref(), Some("off-for-game"));
        // A game list with no such game: a manual backup runs and says so.
        manifest::store(&manifest::dir(&paths1.root), vec![], "").unwrap();
        let r = backup_for_game(&env1, &game(), BackupReason::Manual);
        assert_eq!(r.skipped_reason.as_deref(), Some("no-match"), "{r:?}");
        assert!(r.error.as_deref().is_some_and(|e| e.contains("not in the game list")));
        let after = settings::load(&paths1).unwrap().saves;
        assert!(after.last_error.is_some() && after.last_backup_at.is_none());
    }

    #[test]
    fn a_running_backup_makes_the_next_one_wait_or_skip() {
        let _l = test_lock();
        let probe = heronite_launch::probe::FakeProbe::new("/h");
        let (_d, paths) = setup(r#"{"saves":{"enabled":true}}"#);
        let env = SavesEnv { paths: &paths, probe: &probe };
        let _held = InFlight::acquire().unwrap();
        for reason in [BackupReason::Periodic, BackupReason::Manual] {
            let r = backup_for_game(&env, &game(), reason);
            assert_eq!(r.skipped_reason.as_deref(), Some("already-running"));
            assert!(!r.ran && r.error.is_none());
        }
        let r = backup_for_game_within(&env, &game(), BackupReason::Auto, Duration::from_millis(30));
        assert_eq!(r.skipped_reason.as_deref(), Some("already-running"));
    }

    #[test]
    fn short_sessions_are_not_backed_up() {
        let probe = heronite_launch::probe::FakeProbe::new("/h");
        let (_d, paths) = setup(r#"{"saves":{"enabled":true}}"#);
        let env = SavesEnv { paths: &paths, probe: &probe };
        let play = AfterPlay { app_name: "g".into(), title: "G".into(), runner: "sideload".into(), install_dir: None, elapsed_seconds: 2 };
        assert!(backup_after_play(&env, &play).is_none());
    }

    #[test]
    fn notices_only_for_saves_that_went_up() {
        let ran = BackupResult { ran: true, game_name: Some("Hades".into()), files: Some(3), server: true, ..Default::default() };
        assert_eq!(notification(&ran, "T"), Some(("Cloud saves".into(), "Saved 3 files of Hades to the saves server.".into())));
        let local = BackupResult {
            ran: true,
            game_name: Some("Hades".into()),
            files: Some(1),
            local_path: Some("/backups/hades".into()),
            ..Default::default()
        };
        assert_eq!(notification(&local, "T"), Some(("Saves".into(), "Saved 1 file of Hades to /backups/hades.".into())));
        assert_eq!(notification(&BackupResult { unchanged: true, ..ran.clone() }, "T"), None);
        assert_eq!(notification(&BackupResult::default(), "T"), None);
    }
}
