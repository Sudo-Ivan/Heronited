//! Game save backups to a saves server (`saves.quad4.io` unless another is
//! set), in place of the Ludusavi program Heronited used before.
//!
//! * [`account`]: the secure key and the signed-in session.
//! * [`backup`]: the backup flow of one game, after a play session, and by
//!   hand ([`periodic`] runs it while a game runs).
//! * [`restore`]: listing, previewing and applying a backup, with an
//!   [`undo`] copy of what it replaces.
//! * [`state`]: the note of each game's last backup.
//!
//! The wire protocol, hashing, the game list and the save paths are in
//! `heronite-saves`. Results are written back to `settings.json`
//! (`saves.lastBackup*`/`lastError`).

pub mod account;
mod backup;
mod game;
mod periodic;
pub mod restore;
mod state;
mod target;
mod titles;
pub mod undo;

use heronite_data::{json_store, DataPaths};
use heronite_launch::emulators::EmulatorsFile;

pub use backup::{
    backup_after_play, backup_for_game, game_list, notification, AfterPlay, BackupReason, BackupResult, AFTER_PLAY_WAIT,
    SKIP_SESSION_SECONDS,
};
pub use game::{game_wine_prefix, slug_id, target_for, SavesEnv, SavesGame, SLOT};
pub use periodic::{periodic_backup, PeriodicBackups, PeriodicTick};
pub use restore::{Listing, Source, LOCAL_PREFIX};
pub use state::{last_backup, note_of, GameNote, LastBackup};
pub use target::{steam_game_running, wine_user};

/// The user's emulator `emulator_id` (`emulators.json`); `None` when the id
/// is empty or unknown. Its catalog engine (`retroarch`, `pcsx2`) comes from
/// [`crate::save_states::engine_id_for`].
pub(crate) fn user_emulator(paths: &DataPaths, emulator_id: Option<&str>) -> Option<heronite_launch::emulators::UserEmulator> {
    let id = emulator_id.filter(|id| !id.is_empty())?;
    let emulators: EmulatorsFile = json_store::read(&paths.emulators()).ok()?;
    emulators.get(id).cloned()
}
