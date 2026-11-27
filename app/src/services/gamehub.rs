//! GameHub server profiles, library syncs and game installs. Network and
//! file work run off the UI thread; the Collection reloads after a sync or
//! install.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::*;
use heronite_data::{game_config, json_store, DataPaths};
use heronite_gamehub::{Client, Server, Servers, GAME_KEY, SERVER_KEY};
use heronite_i18n::tr;
use serde_json::Value;

#[derive(Clone, Debug)]
pub enum GameHubEvent {
    /// Profiles, sync state or installed entries changed.
    Changed,
    /// A sync or install ended.
    Toast { title: String, text: String, error: bool },
    /// Read the Collection again.
    LibraryChanged,
    /// A GameHub download started or ended.
    InstallState { app_name: String, active: bool },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerState {
    pub testing: bool,
    pub syncing: bool,
    pub error: String,
    pub games: usize,
}

pub struct GameHub {
    paths: DataPaths,
    pub servers: Servers,
    pub states: HashMap<String, ServerState>,
    /// Collection app names currently downloading.
    pub installing: HashSet<String>,
    periodic_started: bool,
}

impl EventEmitter<GameHubEvent> for GameHub {}

impl GameHub {
    pub fn new(paths: DataPaths) -> Self {
        Self { servers: Servers::load(&paths), states: HashMap::new(), installing: HashSet::new(), periodic_started: false, paths }
    }

    pub fn save(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = self.servers.save(&self.paths) {
            cx.emit(GameHubEvent::Toast { title: "GameHub".into(), text: e.to_string(), error: true });
        }
        cx.emit(GameHubEvent::Changed);
        cx.notify();
    }

    /// Adds or updates a profile, then tests it.
    pub fn upsert(&mut self, id: String, name: String, url: String, enabled: bool, cx: &mut Context<Self>) -> bool {
        if let Err(e) = self.servers.upsert(id.clone(), name, url, enabled) {
            cx.emit(GameHubEvent::Toast { title: "GameHub".into(), text: e, error: true });
            cx.notify();
            return false;
        }
        self.save(cx);
        if enabled {
            self.test(&id, true, cx);
        }
        true
    }

    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        match heronite_gamehub::remove_server_entries(&self.paths, id) {
            Ok(_) => {
                self.servers.remove(id);
                self.states.remove(id);
                self.save(cx);
                cx.emit(GameHubEvent::LibraryChanged);
            }
            Err(e) => {
                cx.emit(GameHubEvent::Toast { title: "GameHub".into(), text: e.to_string(), error: true });
                cx.notify();
            }
        }
    }

    fn server(&self, id: &str) -> Option<Server> {
        self.servers.get(id).cloned()
    }

    fn client(server: &Server) -> Result<Client, String> {
        Client::new(&server.url)
    }

    /// Checks the server. `then_sync` imports its games when it answers.
    pub fn test(&mut self, id: &str, then_sync: bool, cx: &mut Context<Self>) {
        let id = id.to_string();
        let Some(server) = self.server(&id) else { return };
        if self.states.get(&id).is_some_and(|s| s.testing) {
            return;
        }
        self.states.entry(id.clone()).or_default().testing = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { Self::client(&server)?.stats() }).await;
            this.update(cx, |this, cx| {
                let st = this.states.entry(id.clone()).or_default();
                st.testing = false;
                match result {
                    Ok(_) => {
                        st.error.clear();
                        if then_sync {
                            this.sync(&id, cx);
                        }
                    }
                    Err(e) => st.error = e,
                }
                cx.emit(GameHubEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Keeps enabled servers in sync while the app runs.
    pub fn start_periodic(&mut self, cx: &mut Context<Self>) {
        if self.periodic_started {
            return;
        }
        self.periodic_started = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(8)).await;
            loop {
                if this.update(cx, |s, cx| s.sync_all(cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(15 * 60)).await;
            }
        })
        .detach();
    }

    /// Imports one server's games.
    pub fn sync(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = id.to_string();
        let Some(server) = self.server(&id) else { return };
        if self.states.get(&id).is_some_and(|s| s.syncing) {
            return;
        }
        self.states.entry(id.clone()).or_default().syncing = true;
        cx.notify();
        let paths = self.paths.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let client = Self::client(&server)?;
                    let games: Vec<_> = client.games()?.into_iter().filter(heronite_gamehub::Game::is_pc).collect();
                    let report = heronite_gamehub::sync(&paths, &server, &client, &games).map_err(|e| e.to_string())?;
                    Ok::<_, String>((games.len(), report))
                })
                .await;
            this.update(cx, |this, cx| {
                let label = this.servers.get(&id).map(Server::label).unwrap_or_else(|| id.clone());
                let st = this.states.entry(id.clone()).or_default();
                st.syncing = false;
                match result {
                    Ok((n, report)) => {
                        st.games = n;
                        st.error.clear();
                        if let Some(s) = this.servers.servers.iter_mut().find(|s| s.id == id) {
                            s.last_sync = chrono::Utc::now().to_rfc3339();
                        }
                        let _ = this.servers.save(&this.paths);
                        cx.emit(GameHubEvent::Toast {
                            title: "GameHub".into(),
                            text: tr!(
                                "{server}: {count} games in your Collection ({added} new)",
                                server = label,
                                count = n,
                                added = report.added
                            ),
                            error: false,
                        });
                        cx.emit(GameHubEvent::LibraryChanged);
                    }
                    Err(e) => {
                        st.error = e.clone();
                        cx.emit(GameHubEvent::Toast { title: "GameHub".into(), text: e, error: true });
                    }
                }
                cx.emit(GameHubEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn sync_all(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<String> = self.servers.servers.iter().filter(|s| s.enabled).map(|s| s.id.clone()).collect();
        for id in ids {
            self.sync(&id, cx);
        }
    }

    /// Downloads and installs the GameHub game behind a Collection entry.
    pub fn install(&mut self, app_name: &str, cx: &mut Context<Self>) -> Result<(), String> {
        if self.installing.contains(app_name) {
            return Err(tr!("This game is already being installed").into());
        }
        let metas: heronite_data::models::LocalMetaFile = json_store::read(&self.paths.local_meta()).map_err(|e| e.to_string())?;
        let meta = metas.games.get(app_name).cloned().ok_or_else(|| tr!("The GameHub game was not found").to_string())?;
        let server_id = meta
            .extra
            .get(SERVER_KEY)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| tr!("The GameHub server was not found").to_string())?;
        let game_id = meta
            .extra
            .get(GAME_KEY)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| tr!("The GameHub game was not found").to_string())?;
        let server = self.server(&server_id).ok_or_else(|| tr!("The GameHub server is no longer configured").to_string())?;
        let paths = self.paths.clone();
        let global = json_store::read_value(&self.paths.global_config()).unwrap_or(Value::Null);
        let home = heronite_platform::dirs::home().unwrap_or_default().to_string_lossy().into_owned();
        let base = global.pointer("/defaultSettings/defaultInstallPath").and_then(Value::as_str).unwrap_or_default().to_string();
        let fallback = game_config::default_install_dir(&home);
        let base = game_config::expand_tilde(if base.trim().is_empty() { &fallback } else { &base }, &home);
        let app = app_name.to_string();
        self.installing.insert(app.clone());
        cx.emit(GameHubEvent::InstallState { app_name: app.clone(), active: true });
        cx.emit(GameHubEvent::Toast {
            title: "GameHub".into(),
            text: tr!("Downloading and installing {title}\u{2026}", title = meta.title.clone()),
            error: false,
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let client = Self::client(&server)?;
                    let game: heronite_gamehub::Game = client.game(&game_id)?;
                    let progress = |_, _| {};
                    let cancel = || false;
                    let base = PathBuf::from(base);
                    heronite_gamehub::install(&paths, &client, &server.id, &game, &base, &progress, &cancel)
                })
                .await;
            this.update(cx, |this, cx| {
                this.installing.remove(&app);
                cx.emit(GameHubEvent::InstallState { app_name: app.clone(), active: false });
                match result {
                    Ok(out) => {
                        cx.emit(GameHubEvent::Toast {
                            title: "GameHub".into(),
                            text: tr!("{title} is installed", title = meta.title.clone()),
                            error: false,
                        });
                        cx.emit(GameHubEvent::LibraryChanged);
                        log::info!("GameHub install: {} at {}", out.app_name, out.folder.display());
                    }
                    Err(e) => cx.emit(GameHubEvent::Toast { title: "GameHub".into(), text: e, error: true }),
                }
                cx.emit(GameHubEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
        Ok(())
    }
}
