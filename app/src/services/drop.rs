//! Drop OSS server profiles, code sign-in, library syncs and game installs.
//! Network and file work run off the UI thread; the Collection reloads after
//! a sync or install.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use gpui_kit::*;
use heronite_data::{game_config, json_store, DataPaths};
use heronite_drop::{Client, Server, Servers, GAME_KEY, LATEST_KEY, SERVER_KEY, VERSION_KEY};
use heronite_i18n::tr;
use serde_json::Value;

#[derive(Clone, Debug)]
pub enum DropEvent {
    /// Profiles, sync state or installed entries changed.
    Changed,
    /// A sync or install ended.
    Toast { title: String, text: String, error: bool },
    /// Read the Collection again.
    LibraryChanged,
    /// A Drop download started or ended.
    InstallState { app_name: String, active: bool },
    /// Byte progress of a Drop download.
    InstallProgress { app_name: String, done: u64, total: u64 },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerState {
    pub testing: bool,
    pub syncing: bool,
    pub version: String,
    pub error: String,
    pub games: usize,
}

#[derive(Default)]
struct InstallProgress {
    done: AtomicU64,
    total: AtomicU64,
    cancel: AtomicBool,
}

pub struct Drop {
    paths: DataPaths,
    pub servers: Servers,
    pub states: HashMap<String, ServerState>,
    /// Collection app names currently downloading.
    pub installing: HashSet<String>,
    install_progress: HashMap<String, Arc<InstallProgress>>,
    /// The one-time sign-in code being shown for each server.
    pub sign_in_codes: HashMap<String, String>,
    periodic_started: bool,
}

impl EventEmitter<DropEvent> for Drop {}

impl Drop {
    pub fn new(paths: DataPaths) -> Self {
        Self {
            servers: Servers::load(&paths),
            states: HashMap::new(),
            installing: HashSet::new(),
            install_progress: HashMap::new(),
            sign_in_codes: HashMap::new(),
            periodic_started: false,
            paths,
        }
    }

    pub fn save(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = self.servers.save(&self.paths) {
            cx.emit(DropEvent::Toast { title: "Drop".into(), text: e.to_string(), error: true });
        }
        cx.emit(DropEvent::Changed);
        cx.notify();
    }

    /// Adds or updates a profile, then tests it.
    pub fn upsert(&mut self, id: String, name: String, url: String, enabled: bool, cx: &mut Context<Self>) -> bool {
        if let Err(e) = self.servers.upsert(id.clone(), name, url, None, enabled) {
            cx.emit(DropEvent::Toast { title: "Drop".into(), text: e, error: true });
            cx.notify();
            return false;
        }
        self.save(cx);
        if enabled && self.servers.signed_in(&id) {
            self.test(&id, true, cx);
        }
        true
    }

    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        match heronite_drop::remove_server_entries(&self.paths, id) {
            Ok(_) => {
                self.servers.remove(id);
                self.states.remove(id);
                self.save(cx);
                cx.emit(DropEvent::LibraryChanged);
            }
            Err(e) => {
                cx.emit(DropEvent::Toast { title: "Drop".into(), text: e.to_string(), error: true });
                cx.notify();
            }
        }
    }

    fn server(&self, id: &str) -> Option<Server> {
        self.servers.get(id).cloned()
    }

    fn client(server: &Server) -> Result<Client, String> {
        Client::new(&server.url, server.auth()?)
    }

    /// Starts Drop's code sign-in. The code is kept in state while the user
    /// approves it in the server's web interface; the completed certificate is
    /// sealed into the profile.
    pub fn sign_in(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = id.to_string();
        let Some(server) = self.server(&id) else { return };
        if self.sign_in_codes.contains_key(&id) {
            return;
        }
        self.states.entry(id.clone()).or_default().testing = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let code = heronite_drop::client::initiate(&server.url, "Heronited")?;
                    Ok::<_, String>((code, server.clone()))
                })
                .await;
            let mut waiting = None;
            if let Ok((code, server)) = &result {
                waiting = Some((code.clone(), server.clone()));
                this.update(cx, |this, cx| {
                    this.sign_in_codes.insert(id.clone(), code.clone());
                    let page = format!("{}/client/code", server.url.trim_end_matches('/'));
                    if let Err(e) = heronite_platform::shell::open(&page) {
                        log::warn!("could not open Drop sign-in page {page}: {e}");
                    }
                    cx.emit(DropEvent::Toast {
                        title: "Drop".into(),
                        text: tr!("Enter {code} on {server} to sign in", code = code.clone(), server = server.label()),
                        error: false,
                    });
                    cx.notify();
                })
                .ok();
            }
            let result = if let Some((code, server)) = waiting {
                cx.background_spawn(async move { heronite_drop::client::complete_code(&server.url, &code) }).await
            } else {
                Err(result.err().unwrap_or_else(|| tr!("Drop sign-in could not start").into()))
            };
            this.update(cx, |this, cx| {
                this.sign_in_codes.remove(&id);
                let st = this.states.entry(id.clone()).or_default();
                st.testing = false;
                match result {
                    Ok(auth) => {
                        if let Err(e) = this.servers.set_auth(&id, &auth) {
                            st.error = e;
                        } else {
                            st.error.clear();
                            let _ = this.servers.save(&this.paths);
                            this.test(&id, true, cx);
                        }
                    }
                    Err(e) => st.error = e,
                }
                cx.emit(DropEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Checks the server. `then_sync` imports its games when the key works.
    pub fn test(&mut self, id: &str, then_sync: bool, cx: &mut Context<Self>) {
        let id = id.to_string();
        let Some(server) = self.server(&id) else { return };
        if self.states.get(&id).is_some_and(|s| s.testing) {
            return;
        }
        self.states.entry(id.clone()).or_default().testing = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { Self::client(&server)?.user() }).await;
            this.update(cx, |this, cx| {
                let st = this.states.entry(id.clone()).or_default();
                st.testing = false;
                match result {
                    Ok(v) => {
                        st.version = v.get("displayName").or_else(|| v.get("username")).and_then(Value::as_str).unwrap_or_default().into();
                        st.error.clear();
                        if then_sync {
                            this.sync(&id, cx);
                        }
                    }
                    Err(e) => st.error = e,
                }
                cx.emit(DropEvent::Changed);
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
                    let games = client.library()?;
                    let report = heronite_drop::sync(&paths, &server, &client, &games).map_err(|e| e.to_string())?;
                    // Remember whether an installed copy is behind the server. Checking the
                    // newest version here keeps the Collection's click path simple later.
                    let mut metas: heronite_data::models::LocalMetaFile = json_store::read(&paths.local_meta()).unwrap_or_default();
                    let mut changed = false;
                    for game in &games {
                        let app = heronite_drop::app_name(&server.id, &game.id);
                        let Some(meta) = metas.games.get(&app).cloned() else { continue };
                        let current = meta.extra.get(VERSION_KEY).and_then(Value::as_str).unwrap_or_default().to_string();
                        if current.is_empty() {
                            continue;
                        }
                        let latest = client
                            .versions(&game.id, Some(&current))
                            .ok()
                            .and_then(|versions| {
                                versions
                                    .iter()
                                    .filter(|v| v.platform.eq_ignore_ascii_case("linux") || v.platform.eq_ignore_ascii_case("windows"))
                                    .min_by_key(|v| if v.platform.eq_ignore_ascii_case("linux") { 0 } else { 1 })
                                    .map(|v| v.version_id.clone())
                            })
                            .unwrap_or_default();
                        if let Some(meta) = metas.games.get_mut(&app) {
                            meta.extra.insert(LATEST_KEY.into(), Value::String(latest));
                            changed = true;
                        }
                    }
                    if changed {
                        let _ = json_store::write(&paths.local_meta(), &metas);
                    }
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
                        cx.emit(DropEvent::Toast {
                            title: "Drop".into(),
                            text: tr!(
                                "{server}: {count} games in your Collection ({added} new)",
                                server = label,
                                count = n,
                                added = report.added
                            ),
                            error: false,
                        });
                        cx.emit(DropEvent::LibraryChanged);
                    }
                    Err(e) => {
                        st.error = e.clone();
                        cx.emit(DropEvent::Toast { title: "Drop".into(), text: e, error: true });
                    }
                }
                cx.emit(DropEvent::Changed);
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

    /// Asks a running download to stop. It fails the install, which removes a
    /// new folder and leaves an update's old files in place.
    pub fn cancel_install(&mut self, app_name: &str) -> bool {
        let Some(progress) = self.install_progress.get(app_name) else { return false };
        progress.cancel.store(true, Ordering::Relaxed);
        true
    }

    /// Downloads and installs the Drop game behind a Collection entry.
    pub fn install(&mut self, app_name: &str, cx: &mut Context<Self>) -> Result<(), String> {
        if self.installing.contains(app_name) {
            return Err(tr!("This game is already being installed").into());
        }
        let metas: heronite_data::models::LocalMetaFile = json_store::read(&self.paths.local_meta()).map_err(|e| e.to_string())?;
        let meta = metas.games.get(app_name).cloned().ok_or_else(|| tr!("The Drop game was not found").to_string())?;
        let server_id = meta
            .extra
            .get(SERVER_KEY)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| tr!("The Drop server was not found").to_string())?;
        let game_id = meta
            .extra
            .get(GAME_KEY)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| tr!("The Drop game was not found").to_string())?;
        let installed_version = meta.extra.get(VERSION_KEY).and_then(Value::as_str).map(str::to_owned);
        let server = self.server(&server_id).ok_or_else(|| tr!("The Drop server is no longer configured").to_string())?;
        let paths = self.paths.clone();
        let global = json_store::read_value(&self.paths.global_config()).unwrap_or(Value::Null);
        let home = heronite_platform::dirs::home().unwrap_or_default().to_string_lossy().into_owned();
        let base = global.pointer("/defaultSettings/defaultInstallPath").and_then(Value::as_str).unwrap_or_default().to_string();
        let fallback = game_config::default_install_dir(&home);
        let base = game_config::expand_tilde(if base.trim().is_empty() { &fallback } else { &base }, &home);
        let app = app_name.to_string();
        let progress_state = Arc::new(InstallProgress::default());
        self.install_progress.insert(app.clone(), progress_state.clone());
        self.installing.insert(app.clone());
        cx.emit(DropEvent::InstallState { app_name: app.clone(), active: true });
        cx.emit(DropEvent::Toast {
            title: "Drop".into(),
            text: tr!("Downloading and installing {title}\u{2026}", title = meta.title.clone()),
            error: false,
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let install_app = app.clone();
            let (send, receive) = mpsc::channel();
            let worker_progress = progress_state.clone();
            std::thread::spawn(move || {
                let result = (|| {
                    let client = Self::client(&server)?;
                    let game = client
                        .library()?
                        .into_iter()
                        .find(|g| g.id == game_id)
                        .ok_or_else(|| tr!("The Drop game is no longer in your library").to_string())?;
                    let progress = |done, total| {
                        worker_progress.done.store(done, Ordering::Relaxed);
                        worker_progress.total.store(total, Ordering::Relaxed);
                    };
                    let cancel = || worker_progress.cancel.load(Ordering::Relaxed);
                    // A version only means an update when the sideload entry is still installed.
                    // After an uninstall the metadata can remember the old version, but the new
                    // install must start in an empty folder.
                    let installed = heronite_data::sideload::find_game(&paths, &install_app)
                        .ok()
                        .flatten()
                        .and_then(|g| g.get("is_installed").and_then(Value::as_bool))
                        .unwrap_or(false);
                    let previous = installed.then_some(installed_version.as_deref()).flatten();
                    heronite_drop::install(&paths, &server, &client, &game, previous, Some(&base), progress, cancel)
                })();
                let _ = send.send(result);
            });
            let result = loop {
                match receive.try_recv() {
                    Ok(result) => break result,
                    Err(mpsc::TryRecvError::Empty) => {
                        let done = progress_state.done.load(Ordering::Relaxed);
                        let total = progress_state.total.load(Ordering::Relaxed);
                        this.update(cx, |_, cx| cx.emit(DropEvent::InstallProgress { app_name: app.clone(), done, total })).ok();
                        cx.background_executor().timer(Duration::from_millis(300)).await;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => break Err(tr!("The Drop download stopped unexpectedly").into()),
                }
            };
            this.update(cx, |this, cx| {
                this.installing.remove(&app);
                this.install_progress.remove(&app);
                cx.emit(DropEvent::InstallState { app_name: app.clone(), active: false });
                match result {
                    Ok(out) => {
                        cx.emit(DropEvent::Toast {
                            title: "Drop".into(),
                            text: tr!("{title} is installed", title = meta.title.clone()),
                            error: false,
                        });
                        cx.emit(DropEvent::LibraryChanged);
                        log::info!("Drop install: {} at {}", out.app_name, out.folder);
                    }
                    Err(e) => cx.emit(DropEvent::Toast { title: "Drop".into(), text: e, error: true }),
                }
                cx.emit(DropEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
        Ok(())
    }
}
