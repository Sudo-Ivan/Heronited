//! The game a backup is for, and the folders it keeps its saves in on this PC.

use std::path::PathBuf;

use heronite_data::models::{LocalGameMeta, LocalMetaFile};
use heronite_data::{json_store, DataPaths};
use heronite_launch::probe::Probe;
use heronite_launch::settings::{load_game_settings, GlobalSettings};
use heronite_launch::steam::SteamLocator;
use heronite_saves::locations::{Target, WinDirs};
use serde::{Deserialize, Serialize};

use crate::pathutil::join;
use crate::sysfiles::HostEnv;

/// The slot every game's backups go to: one game, one history.
pub const SLOT: &str = "default";

/// Arguments of a backup: who the game is and where it lives.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavesGame {
    pub app_name: String,
    pub title: String,
    pub runner: String,
    #[serde(default)]
    pub steam_app_id: Option<String>,
    #[serde(default)]
    pub store_game_id: Option<String>,
    /// Id of the user's emulator that runs the game (an `emulators.json`
    /// entry, the meta's `emulatorId`); `None` when the game is not emulated.
    #[serde(default)]
    pub emulator_id: Option<String>,
    /// The ROM that runs the game (emulated games): the name the emulator
    /// manifests build their entries from.
    #[serde(default)]
    pub rom_path: Option<String>,
    /// The folder the game is installed in.
    #[serde(default)]
    pub install_dir: Option<PathBuf>,
}

impl SavesGame {
    /// A launched game, with the store ids kept in its overlay meta.
    pub fn of_session(paths: &DataPaths, app_name: &str, title: &str, runner: &str, install_dir: Option<PathBuf>) -> Self {
        let metas: LocalMetaFile = json_store::read(&paths.local_meta()).unwrap_or_default();
        let meta = metas.games.get(app_name);
        SavesGame {
            app_name: app_name.to_owned(),
            title: title.to_owned(),
            runner: runner.to_owned(),
            steam_app_id: meta.and_then(|m| m.steam_app_id.clone()),
            store_game_id: meta.and_then(|m| m.store_game_id.clone()),
            emulator_id: Self::emulator_of(meta),
            rom_path: Self::rom_path_of(meta),
            install_dir,
        }
    }

    /// The emulator of an emulated game. Emulated games are sideloaded
    /// (`runner` is `sideload`), so the emulator lives in the overlay meta.
    pub fn emulator_of(meta: Option<&LocalGameMeta>) -> Option<String> {
        meta.filter(|m| m.launch_kind == "emulator").and_then(|m| m.emulator_id.clone()).filter(|id| !id.is_empty())
    }

    /// The ROM a meta lists: the one selected for play, else the first
    /// non-empty `roms[].path`.
    pub fn rom_path_of(meta: Option<&LocalGameMeta>) -> Option<String> {
        let meta = meta?;
        if let Some(path) = meta.selected_rom_path.as_ref().filter(|path| !path.is_empty()) {
            return Some(path.clone());
        }
        meta.roms.as_ref()?.iter().map(|rom| rom.path.as_str()).find(|path| !path.is_empty()).map(str::to_owned)
    }

    /// The store the game comes from, as the game list names it.
    pub fn store(&self) -> Option<&'static str> {
        if self.steam_app_id.as_deref().is_some_and(|s| !s.is_empty()) {
            return Some("steam");
        }
        match self.runner.as_str() {
            "steam" => Some("steam"),
            "gog" => Some("gog"),
            "legendary" => Some("epic"),
            "nile" => Some("amazon"),
            _ => None,
        }
    }

    /// The id the game has on the server: the store's own id where there is
    /// one, so the same game from the same store is the same history on
    /// every PC. Other games are named by their title.
    pub fn app_id(&self) -> String {
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if let Some(id) = self.steam_app_id.as_deref().filter(|s| digits(s)) {
            return format!("steam:{id}");
        }
        if let Some(id) = self.store_game_id.as_deref().filter(|s| digits(s)).filter(|_| self.runner == "gog") {
            return format!("gog:{id}");
        }
        slug_id("game", &self.title, &self.app_name)
    }

    /// The names the game list may know the game by. An emulator with its own
    /// saves entry set (`UserEmulator::ludusavi_entry`) backs every game up
    /// under that one name.
    pub fn names(&self, paths: &DataPaths) -> Vec<String> {
        let emulator = super::user_emulator(paths, self.emulator_id.as_deref());
        if let Some(entry) = emulator.as_ref().and_then(|e| e.ludusavi_entry()) {
            return vec![entry.to_owned()];
        }
        let engine = emulator.map(|e| crate::save_states::engine_id_for(&e));
        let mut out = vec![self.title.clone()];
        out.extend(super::titles::emulator_names(engine.as_deref(), &self.title, self.rom_path.as_deref()));
        out
    }
}

/// `prefix:slug` of a title, at most 64 characters and valid as an app id.
/// A title with no letters or digits falls back to the game's own name.
pub fn slug_id(prefix: &str, title: &str, fallback: &str) -> String {
    let slug = |s: &str| {
        let mut out = String::new();
        for c in s.chars().flat_map(char::to_lowercase) {
            if c.is_ascii_alphanumeric() {
                out.push(c);
            } else if !out.ends_with('-') && !out.is_empty() {
                out.push('-');
            }
        }
        out.trim_end_matches('-').to_string()
    };
    let mut body = slug(title);
    if body.is_empty() {
        body = slug(fallback);
    }
    if body.is_empty() {
        body = "game".into();
    }
    let room = 64 - prefix.len() - 1;
    body.truncate(room);
    format!("{prefix}:{}", body.trim_end_matches('-'))
}

/// Everything the lookups read from the machine.
pub struct SavesEnv<'a> {
    pub paths: &'a DataPaths,
    pub probe: &'a dyn Probe,
}

/// The game's own Wine prefix (the folder holding `drive_c`), from its game
/// settings. `None` for Steam-launched games: their prefix is Steam's. `None`
/// on Windows, where games run as they are.
pub fn game_wine_prefix(env: &SavesEnv, app_name: &str) -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    let metas: LocalMetaFile = json_store::read(&env.paths.local_meta()).ok()?;
    if metas.games.get(app_name).is_some_and(|m| m.launch_kind == "steam-uri") {
        return None;
    }
    let global = GlobalSettings::load(env.paths);
    let game = load_game_settings(env.paths, app_name, &global, env.probe);
    let prefix = game.wine_prefix;
    if prefix.is_empty() {
        return None;
    }
    if env.probe.exists(&join(&prefix, "drive_c")) {
        return Some(PathBuf::from(prefix));
    }
    let pfx = join(&prefix, "pfx");
    env.probe.exists(&join(&pfx, "drive_c")).then(|| PathBuf::from(pfx))
}

/// The folders this PC gives the game's save rules to stand on.
pub fn target_for(env: &SavesEnv, game: &SavesGame) -> Target {
    let host = HostEnv::system();
    let login = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
    let steam_id = game.steam_app_id.as_deref().filter(|s| !s.is_empty());
    let mut target = Target {
        windows_host: cfg!(windows),
        store: game.store().map(str::to_string),
        install_dir: game.install_dir.clone(),
        home: Some(host.home.clone()),
        xdg_data: Some(host.xdg_data_home()),
        xdg_config: Some(host.xdg_config_home()),
        login,
        ..Default::default()
    };
    if cfg!(windows) {
        target.win = WinDirs {
            profile: Some(host.home.clone()),
            roaming: Some(host.appdata.clone()),
            local: Some(host.local_appdata.clone()),
            documents: Some(host.documents.clone()),
            public: std::env::var_os("PUBLIC").map(PathBuf::from),
            program_data: std::env::var_os("ProgramData").map(PathBuf::from),
        };
    }
    let locator = steam_id.map(|_| SteamLocator::detect(&GlobalSettings::load(env.paths), env.probe));
    if let (Some(id), Some(locator)) = (steam_id, &locator) {
        target.install_dir = locator.install_dir(id).or(target.install_dir.take());
        if let Some(userdata) = locator.userdata_dir(id) {
            target.roots.extend(userdata.parent().map(PathBuf::from));
        }
        target.roots.extend(locator.library_roots());
        if !cfg!(windows) {
            target.prefix =
                heronite_steam::files::prefix(locator, id).map(|p| p.compatdata.join("pfx")).filter(|p| p.join("drive_c").is_dir());
        }
    } else if let Some(dir) = &target.install_dir {
        target.roots.extend(dir.parent().map(PathBuf::from));
    }
    if target.prefix.is_none() {
        target.prefix = game_wine_prefix(env, &game.app_name);
    }
    if let Some(prefix) = &target.prefix {
        target.wine_user = crate::saves::wine_user(prefix, Some(&target.login)).or_else(|| Some("steamuser".into()));
    }
    if let Ok(s) = heronite_data::settings::load(env.paths) {
        target.custom = s.saves.manual_saves_folder_for(&game.runner, &game.app_name).map(PathBuf::from);
    }
    let mut seen = Vec::new();
    target.roots.retain(|r| {
        r.is_dir()
            && !{
                let dup = seen.contains(r);
                seen.push(r.clone());
                dup
            }
    });
    target
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(f: impl FnOnce(&mut SavesGame)) -> SavesGame {
        let mut g = SavesGame { app_name: "a".into(), title: "Hollow Knight".into(), runner: "sideload".into(), ..Default::default() };
        f(&mut g);
        g
    }

    #[test]
    fn app_ids_follow_the_store() {
        assert_eq!(game(|g| g.steam_app_id = Some("367520".into())).app_id(), "steam:367520");
        assert_eq!(game(|g| (g.runner, g.store_game_id) = ("gog".into(), Some("1207658924".into()))).app_id(), "gog:1207658924");
        assert_eq!(game(|_| {}).app_id(), "game:hollow-knight");
        assert_eq!(game(|g| g.steam_app_id = Some("abc".into())).app_id(), "game:hollow-knight");
        assert_eq!(game(|g| (g.runner, g.store_game_id) = ("legendary".into(), Some("12".into()))).app_id(), "game:hollow-knight");
    }

    #[test]
    fn slugs_are_valid_app_ids() {
        for title in ["Tony Hawk's Pro Skater™ 1 + 2", "ЭЛДЕН", "   ", &"x".repeat(200), "A", "!!Hades!!"] {
            let id = slug_id("game", title, "fallback-name");
            assert!(id.len() <= 64, "{id}");
            let valid = id.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
                && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b':' | b'-'));
            assert!(valid && !id.ends_with('-'), "{id}");
        }
        assert_eq!(slug_id("game", "Tony Hawk's Pro Skater", ""), "game:tony-hawk-s-pro-skater");
        assert_eq!(slug_id("game", "ЭЛДЕН", "Elden Ring"), "game:elden-ring");
    }

    #[test]
    fn stores() {
        assert_eq!(game(|g| g.runner = "legendary".into()).store(), Some("epic"));
        assert_eq!(game(|g| g.steam_app_id = Some("1".into())).store(), Some("steam"));
        assert_eq!(game(|_| {}).store(), None);
    }

    #[test]
    fn an_emulator_saves_entry_is_the_only_name() {
        let d = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(d.path());
        std::fs::create_dir_all(paths.emulators().parent().unwrap()).unwrap();
        std::fs::write(
            paths.emulators(),
            r#"{"emulators":[{"id":"my-pcsx2","definitionId":"pcsx2","name":"PCSX2","ludusaviEntry":"My PS2 games"}]}"#,
        )
        .unwrap();
        let with = game(|g| g.emulator_id = Some("my-pcsx2".into()));
        assert_eq!(with.names(&paths), vec!["My PS2 games".to_string()]);
        // Without the entry the game keeps its own names.
        std::fs::write(
            paths.emulators(),
            r#"{"emulators":[{"id":"my-pcsx2","definitionId":"pcsx2","name":"PCSX2"}]}"#,
        )
        .unwrap();
        let names = with.names(&paths);
        assert_eq!(names[0], "Hollow Knight");
        assert!(names.len() > 1, "pcsx2 suffix names: {names:?}");
    }
}
