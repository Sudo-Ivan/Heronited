//! The "Backups" tab of a game's settings: its backups on the saves server,
//! and restoring one into the game's folders as they are now
//! (`heronite_emulation::saves::restore`), with an undo.
//!
//! Layout: this file holds the state and the background work, `render` the
//! desktop view and `console` the console mode rows.

mod console;
mod render;

use gpui_kit::prelude::*;
use gpui_kit::*;
use heronite_data::collection::Game;
use heronite_data::DataPaths;
use heronite_emulation::saves::restore::{
    apply_restore, delete_snapshot, list_snapshots, plan_restore, FileKind, RestoreOutcome, RestorePlan,
};
use heronite_emulation::saves::undo::{self, UndoRecord};
use heronite_emulation::saves::{backup_for_game, steam_game_running, target_for, BackupReason, SavesEnv, SavesGame};
use heronite_i18n::tr;
use heronite_launch::SystemProbe;
use heronite_saves::locations::Target;
use heronite_saves::SnapshotInfo;

use super::saves_backup::{describe, Outcome};
use super::settings_panel::logic::{GameFacts, RunKind};
use render::{groups, Group};

/// What the tab knows once loaded.
pub(crate) struct Loaded {
    target: Target,
    /// The game has a Wine prefix to restore into on this PC, or needs none.
    prefix_ok: bool,
    /// Newest first.
    snapshots: Vec<SnapshotInfo>,
    /// The copy of what the last restore replaced.
    undo: Option<UndoRecord>,
}

/// A previewed restore and its groups, summed up once (not on every frame).
pub(crate) struct Preview {
    plan: RestorePlan,
    groups: Vec<Group>,
}

impl Preview {
    fn new(plan: RestorePlan) -> Self {
        let groups = groups(&plan, &plan.target);
        Self { plan, groups }
    }
}

enum Load {
    Idle,
    Loading,
    /// The saves are off and this PC has no key: nothing is asked of the
    /// server (asking would make an account), the tab says how to turn them on.
    Off,
    Failed(String),
    Ready(Box<Loaded>),
}

/// Where the restore is.
enum Stage {
    List,
    Planning,
    /// Preview: the user picks the files.
    Plan(Box<Preview>),
    Restoring,
    Done(RestoreOutcome),
}

pub struct SaveBackupsView {
    paths: DataPaths,
    game: SavesGame,
    run_kind: RunKind,
    /// Not installed: backups can be listed and deleted, and restored only
    /// while a prefix of the game is still on this PC.
    installed: bool,
    steam_app_id: Option<String>,
    load: Load,
    /// The id of the backup picked (`None` or gone: the newest).
    backup: Option<String>,
    stage: Stage,
    /// Background job besides the restore (backup, delete, undo).
    working: bool,
    notice: Option<(String, bool)>,
    /// The backup asking "delete?".
    confirm_delete: Option<String>,
    /// Console mode: the backup whose actions are listed.
    console_backup: Option<String>,
    console_list: crate::console::list::ListNav,
}

impl EventEmitter<()> for SaveBackupsView {}

/// How the game runs, for its saves: installed games as their settings
/// say, others from their store data. `None` (no tab) for emulated ROMs,
/// browser games and macOS builds.
fn save_kind(game: &Game) -> Option<RunKind> {
    let g = &game.game;
    let launch_kind = game.launch_kind.as_deref().unwrap_or("");
    let platform = g.install.platform.clone().unwrap_or_default().to_ascii_lowercase();
    if launch_kind == "emulator" || platform == "browser" || g.rest.get("browserUrl").is_some_and(|v| !v.is_null() && v != "") {
        return None;
    }
    if cfg!(windows) {
        return windows_save_kind(game, &platform);
    }
    if game.installed {
        return Some(GameFacts::of(game).run_kind).filter(|k| *k != RunKind::None);
    }
    Some(if game.steam_launch_id.as_deref().is_some_and(|s| !s.is_empty()) {
        RunKind::Steam
    } else if platform.contains("linux") || (platform.is_empty() && g.is_linux_native == Some(true)) {
        RunKind::Native
    } else if platform.contains("mac") || platform == "osx" {
        return None;
    } else {
        RunKind::Wine
    })
}

/// Windows: games run as they are. Steam games (installed or not, their
/// settings are Steam's), installed games that run here (a Windows
/// program), and games not installed unless they are Linux or macOS builds.
fn windows_save_kind(game: &Game, platform: &str) -> Option<RunKind> {
    if game.steam_launch_id.as_deref().is_some_and(|s| !s.is_empty()) {
        return Some(RunKind::Steam);
    }
    if game.installed {
        return Some(GameFacts::of(game).run_kind).filter(|k| *k == RunKind::Native);
    }
    let other_system = platform.contains("linux")
        || platform.contains("mac")
        || platform == "osx"
        || (platform.is_empty() && game.game.is_linux_native == Some(true));
    (!other_system).then_some(RunKind::Native)
}

/// Whether a game gets the tab (installed or not).
pub fn has_tab(game: &Game) -> bool {
    heronite_platform::supported(heronite_platform::Feature::SaveBackups) && save_kind(game).is_some()
}

impl SaveBackupsView {
    pub fn new(paths: DataPaths, game: &Game) -> Self {
        let run_kind = save_kind(game).unwrap_or(RunKind::None);
        let steam_app_id = if run_kind == RunKind::Steam { game.steam_launch_id.clone().filter(|s| !s.is_empty()) } else { None };
        let install_dir = game.game.install.install_path.as_deref().filter(|p| !p.is_empty()).map(std::path::PathBuf::from);
        Self {
            paths,
            game: SavesGame {
                app_name: game.app_name.clone(),
                title: game.title.clone(),
                runner: game.runner.clone(),
                steam_app_id: game.steam_launch_id.clone(),
                store_game_id: game.meta.as_ref().and_then(|m| m.store_game_id.clone()),
                emulator_id: SavesGame::emulator_of(game.meta.as_deref()),
                rom_path: SavesGame::rom_path_of(game.meta.as_deref()),
                install_dir,
            },
            run_kind,
            installed: game.installed,
            steam_app_id,
            load: Load::Idle,
            backup: None,
            stage: Stage::List,
            working: false,
            notice: None,
            confirm_delete: None,
            console_backup: None,
            console_list: Default::default(),
        }
    }

    /// The tab became visible: reads the backups the first time.
    pub fn shown(&mut self, cx: &mut Context<Self>) {
        if matches!(self.load, Load::Idle) {
            self.reload(cx);
        }
    }

    fn loaded(&self) -> Option<&Loaded> {
        match &self.load {
            Load::Ready(l) => Some(l),
            _ => None,
        }
    }

    /// The backup picked, else the newest.
    pub(crate) fn selected_id(&self) -> Option<String> {
        let l = self.loaded()?;
        let picked = self.backup.as_ref().filter(|id| l.snapshots.iter().any(|s| &s.id == *id));
        picked.cloned().or_else(|| l.snapshots.first().map(|s| s.id.clone()))
    }

    fn busy(&self) -> bool {
        self.working || matches!(self.stage, Stage::Planning | Stage::Restoring)
    }

    /// Runs `job` with an environment off the UI thread.
    fn background<R: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        job: impl FnOnce(&SavesEnv) -> R + Send + 'static,
        done: impl FnOnce(&mut Self, R, &mut Context<Self>) + 'static,
    ) {
        let paths = self.paths.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let env = SavesEnv { paths: &paths, probe: &SystemProbe };
                    job(&env)
                })
                .await;
            this.update(cx, |v, cx| {
                done(v, result, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Reads the backups and the folders again.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        self.load = Load::Loading;
        let game = self.game.clone();
        let (kind, installed) = (self.run_kind, self.installed);
        cx.notify();
        self.background(
            cx,
            move |env| -> Result<Option<(Loaded, Option<String>)>, String> {
                let listing = list_snapshots(env, &game);
                if listing.off {
                    return Ok(None);
                }
                if listing.snapshots.is_empty() {
                    if let Some(e) = listing.server_error {
                        return Err(e);
                    }
                }
                let snapshots = listing.snapshots;
                let target = target_for(env, &game);
                let prefix_ok =
                    cfg!(windows) || target.prefix.is_some() || !(kind == RunKind::Wine || (kind == RunKind::Steam && !installed));
                let undo = undo::latest(&env.paths.save_restore_undo(), &game.app_name);
                Ok(Some((Loaded { target, prefix_ok, snapshots, undo }, listing.server_error)))
            },
            move |v, result, cx| {
                v.load = match result {
                    Ok(None) => Load::Off,
                    Ok(Some((l, server_error))) => {
                        // The folder's backups show even when the server cannot be asked.
                        v.notice = server_error.map(|e| (e, true));
                        // Drop what points at a backup that is gone.
                        let gone = |id: &Option<String>| id.as_ref().is_some_and(|id| !l.snapshots.iter().any(|s| &s.id == id));
                        if gone(&v.backup) {
                            v.backup = None;
                        }
                        if gone(&v.confirm_delete) {
                            v.confirm_delete = None;
                        }
                        if gone(&v.console_backup) {
                            v.console_backup = None;
                        }
                        Load::Ready(Box::new(l))
                    }
                    Err(e) => Load::Failed(e),
                };
                v.apply_debug_env(cx);
            },
        );
    }

    /// `HERONITE_DEBUG_BACKUPS=plan`: once loaded, previews the selected
    /// backup (only reads it; for screenshots).
    fn apply_debug_env(&mut self, cx: &mut Context<Self>) {
        let Some(value) = crate::env::value("HERONITE_DEBUG_BACKUPS") else { return };
        if value == "plan" && matches!(self.stage, Stage::List) {
            self.start_restore(cx);
        }
    }

    pub(crate) fn pick_backup(&mut self, id: String, cx: &mut Context<Self>) {
        self.backup = Some(id);
        cx.notify();
    }

    fn running_now(&self, cx: &App) -> bool {
        crate::services::session::is_running(cx, &self.game.app_name)
    }

    /// Previews the restore of the selected backup.
    pub(crate) fn start_restore(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        if self.running_now(cx) {
            self.notice = Some((tr!("Close the game before restoring a save.").into(), true));
            cx.notify();
            return;
        }
        let Some(id) = self.selected_id() else { return };
        let Some(snapshot) = self.loaded().and_then(|l| l.snapshots.iter().find(|s| s.id == id).cloned()) else { return };
        let game = self.game.clone();
        self.stage = Stage::Planning;
        self.notice = None;
        cx.notify();
        self.background(
            cx,
            move |env| plan_restore(env, &game, &snapshot),
            |v, result, _| match result {
                Ok(plan) => v.stage = Stage::Plan(Box::new(Preview::new(plan))),
                Err(e) => {
                    v.stage = Stage::List;
                    v.notice = Some((e, true));
                }
            },
        );
        self.console_list.reset();
    }

    /// Picks or leaves out every file of one kind (the preview's groups).
    pub(crate) fn toggle_kind(&mut self, kind: FileKind, cx: &mut Context<Self>) {
        if kind == FileKind::Unmapped {
            return;
        }
        if let Stage::Plan(preview) = &mut self.stage {
            let on = !preview.plan.files.iter().any(|f| f.kind == kind && f.selected);
            for f in preview.plan.files.iter_mut().filter(|f| f.kind == kind) {
                f.selected = on;
            }
            for g in preview.groups.iter_mut().filter(|g| g.kind == kind) {
                g.selected = on;
            }
        }
        cx.notify();
    }

    pub(crate) fn back_to_list(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.stage, Stage::Planning | Stage::Restoring) {
            self.stage = Stage::List;
            self.console_list.reset();
            cx.notify();
        }
    }

    /// Restores the files picked in the preview.
    pub(crate) fn confirm_restore(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.stage, Stage::Plan(_)) {
            return;
        }
        if self.running_now(cx) {
            self.notice = Some((tr!("Close the game before restoring a save.").into(), true));
            cx.notify();
            return;
        }
        let Stage::Plan(preview) = std::mem::replace(&mut self.stage, Stage::Restoring) else { return };
        let plan = preview.plan.clone();
        let app_name = self.game.app_name.clone();
        let steam = self.steam_app_id.clone();
        self.notice = None;
        cx.notify();
        self.background(
            cx,
            move |env| {
                if steam.as_deref().is_some_and(steam_game_running) {
                    return Err(tr!("Close the game before restoring a save.").to_string());
                }
                apply_restore(env, &plan, &env.paths.save_restore_undo(), &app_name)
            },
            move |v, result, _| match result {
                Ok(outcome) => {
                    if let (Some(record), Load::Ready(l)) = (&outcome.undo, &mut v.load) {
                        l.undo = Some(record.clone());
                    }
                    v.stage = Stage::Done(outcome);
                }
                Err(e) => {
                    log::error!("saves restore failed: {e}");
                    v.notice = Some((e, true));
                    // A restore stopped part-way keeps its undo copy: back to
                    // the list, where Undo is.
                    let latest = undo::latest(&v.paths.save_restore_undo(), &v.game.app_name);
                    let mut kept = false;
                    if let Load::Ready(l) = &mut v.load {
                        kept = latest.as_ref().map(|r| &r.dir) != l.undo.as_ref().map(|r| &r.dir);
                        l.undo = latest;
                    }
                    v.stage = if kept { Stage::List } else { Stage::Plan(preview) };
                }
            },
        );
        self.console_list.reset();
    }

    /// Puts back what the last restore replaced.
    pub(crate) fn undo_last(&mut self, cx: &mut Context<Self>) {
        let Some(record) = self.loaded().and_then(|l| l.undo.clone()) else { return };
        if self.busy() {
            return;
        }
        if self.running_now(cx) {
            self.notice = Some((tr!("Close the game before restoring a save.").into(), true));
            cx.notify();
            return;
        }
        let steam = self.steam_app_id.clone();
        self.working = true;
        cx.notify();
        self.background(
            cx,
            move |_| {
                if steam.as_deref().is_some_and(steam_game_running) {
                    return Err(tr!("Close the game before restoring a save.").to_string());
                }
                undo::undo(&record).map_err(|e| format!("{e:#}"))
            },
            |v, result, _| {
                v.working = false;
                v.notice = Some(match result {
                    Ok(n) => {
                        (heronite_i18n::trn!(n, "Undone: {n} file is back as it was.", "Undone: {n} files are back as they were."), false)
                    }
                    Err(e) => (e, true),
                });
                if let Load::Ready(l) = &mut v.load {
                    l.undo = undo::latest(&v.paths.save_restore_undo(), &v.game.app_name);
                }
                if matches!(v.stage, Stage::Done(_)) {
                    v.stage = Stage::List;
                }
            },
        );
    }

    pub(crate) fn ask_delete(&mut self, id: String, cx: &mut Context<Self>) {
        self.confirm_delete = Some(id);
        cx.notify();
    }

    pub(crate) fn cancel_delete(&mut self, cx: &mut Context<Self>) {
        self.confirm_delete = None;
        cx.notify();
    }

    /// Deletes the backup asked about from the server, then reads the list again.
    pub(crate) fn delete_confirmed(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let Some(id) = self.confirm_delete.take() else { return };
        let game = self.game.clone();
        self.working = true;
        self.console_backup = None;
        cx.notify();
        self.background(
            cx,
            move |env| delete_snapshot(env, &game, &id),
            |v, result, cx| {
                v.working = false;
                match result {
                    Ok(()) => {
                        v.notice = Some((tr!("Backup deleted.").into(), false));
                        v.reload(cx);
                    }
                    Err(e) => v.notice = Some((e, true)),
                }
            },
        );
    }

    /// Backs the saves up now (the same as the card's menu item).
    pub(crate) fn backup_now(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let game = self.game.clone();
        let title = game.title.clone();
        self.working = true;
        self.notice = None;
        cx.notify();
        self.background(
            cx,
            move |env| backup_for_game(env, &game, BackupReason::Manual),
            move |v, result, cx| {
                v.working = false;
                v.notice = Some(match describe(&result, &title) {
                    Outcome::Done(message) | Outcome::Message(message) => (message, false),
                    Outcome::Error(e) => (e, true),
                    Outcome::Running => (String::new(), false),
                });
                if result.ran {
                    v.backup = None;
                    v.reload(cx);
                }
            },
        );
    }
}

impl Render for SaveBackupsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if crate::console::active() {
            return self.render_console(window, cx);
        }
        self.render_desktop(window, cx)
    }
}
