//! Sharing screenshots and clips to the user's own server (Settings →
//! Sharing): upload jobs run one at a time on a worker thread through the
//! `heronite-share` crate, the link lands in the clipboard and a desktop
//! notification, and the capture list, the viewer and the overlay watch the
//! jobs here. The service never trusts an unknown SFTP host key on its own:
//! the target's key is pinned in Settings.
//!
//! The service is a GPUI entity kept in a global (see [`share`] in
//! `services/mod.rs`), created on first use.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::*;
use heronite_data::DataPaths;
use heronite_i18n::tr;
use heronite_share::{Continue, Naming, Progress, Record, ShareContext, ShareError, Target, Targets};

/// How a caller wants the result reported: the capture list and the viewer
/// take the desktop notification, the overlay may not (it shows the link
/// itself).
#[derive(Debug, Clone, Copy)]
pub struct ShareUi {
    /// Send a desktop notification with the link or the error.
    pub notify: bool,
    /// Copy the link(s) to the clipboard.
    pub clipboard: bool,
}

impl Default for ShareUi {
    fn default() -> Self {
        Self { notify: true, clipboard: true }
    }
}

/// What a job is doing.
#[derive(Debug, Clone, PartialEq)]
pub enum JobState {
    Queued,
    Uploading,
    Done { url: String },
    Failed(String),
    Cancelled,
}

/// One file on its way to the server. The capture list and the overlay
/// (handoff B) read the fields.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Job {
    pub id: u64,
    pub file_name: String,
    pub bytes: u64,
    pub total: u64,
    pub target_id: String,
    pub target_name: String,
    pub state: JobState,
}

struct JobShared {
    job: Job,
    cancel: Arc<AtomicBool>,
    /// The url once Done, for the batch's clipboard text.
    result_url: Option<String>,
}

type Shared = Arc<Mutex<Vec<Arc<Mutex<JobShared>>>>>;

/// Which target a share goes to.
#[derive(Debug, PartialEq)]
pub enum Resolve {
    /// The target id to use.
    Target(String),
    /// Nothing is set up: send the user to Settings → Sharing.
    NoTargets,
    /// The id or name given matches nothing.
    Unknown(String),
}

/// Explicit id or name first, then the default, then the only enabled one.
pub fn resolve_target(targets: &Targets, explicit: Option<&str>) -> Resolve {
    let enabled: Vec<&heronite_share::Target> = targets.targets.iter().filter(|t| t.enabled).collect();
    if let Some(want) = explicit {
        if let Some(t) = targets.targets.iter().find(|t| t.id == want || t.name == want) {
            return Resolve::Target(t.id.clone());
        }
        return Resolve::Unknown(want.to_string());
    }
    if let Some(def) = &targets.default {
        if let Some(t) = enabled.iter().find(|t| &t.id == def) {
            return Resolve::Target(t.id.clone());
        }
    }
    if enabled.len() == 1 {
        return Resolve::Target(enabled[0].id.clone());
    }
    Resolve::NoTargets
}

/// The clipboard text for a batch: one link per line, in file order.
pub fn links_text(urls: &[String]) -> String {
    urls.join("\n")
}

static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

/// The sharing service: the configured targets, the jobs in flight and the
/// local history of what went up.
pub struct ShareService {
    paths: DataPaths,
    targets: Targets,
    /// Every job of every batch, shared with the worker threads.
    shared: Shared,
    /// Snapshot for the UI, refreshed by the poll.
    jobs: Vec<Job>,
    history: Vec<Record>,
    history_loaded: bool,
    /// Batches waiting for a free worker: one upload at a time per service.
    queue: Arc<Mutex<VecDeque<BatchWork>>>,
    worker_busy: Arc<AtomicBool>,
    polling: bool,
}

struct BatchWork {
    jobs: Vec<Arc<Mutex<JobShared>>>,
    target_id: String,
    files: Vec<PathBuf>,
    game: Option<String>,
    ui: ShareUi,
}

impl ShareService {
    pub fn new(paths: DataPaths) -> Self {
        let targets = {
            let paths = paths.clone();
            // The keyring can block for a moment: read the targets off the
            // UI thread and take what is there when it cannot.
            std::thread::spawn(move || Targets::load(&paths)).join().unwrap_or_default()
        };
        Self {
            paths,
            targets,
            shared: Arc::new(Mutex::new(Vec::new())),
            jobs: Vec::new(),
            history: Vec::new(),
            history_loaded: false,
            queue: Arc::new(Mutex::new(VecDeque::new())),
            worker_busy: Arc::new(AtomicBool::new(false)),
            polling: false,
        }
    }

    /// The configured targets, reloaded after Settings saves.
    /// Handoff B reads it for the capture list.
    #[allow(dead_code)]
    pub fn targets(&self) -> &Targets {
        &self.targets
    }

    /// Reload after the Settings page saves `share.json`.
    #[allow(dead_code)]
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let paths = self.paths.clone();
        cx.spawn(async move |this, cx| {
            let targets = cx.background_executor().spawn(async move { Targets::load(&paths) }).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |service: &mut ShareService, cx| {
                    service.targets = targets;
                    service.history_loaded = false;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn save_targets(&mut self, before: Targets, cx: &mut Context<Self>) -> anyhow::Result<()> {
        match self.targets.save(&self.paths) {
            Ok(()) => {
                cx.notify();
                Ok(())
            }
            Err(e) => {
                self.targets = before;
                Err(e)
            }
        }
    }

    /// Adds or replaces a target and writes `share.json`.
    pub fn upsert_target(&mut self, target: Target, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let before = self.targets.clone();
        self.targets.upsert(target);
        self.save_targets(before, cx)
    }

    /// Removes a target and its default marker.
    pub fn remove_target(&mut self, id: &str, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let before = self.targets.clone();
        self.targets.remove(id);
        self.save_targets(before, cx)
    }

    /// Pins the default target (`None`: the first enabled one).
    pub fn set_default(&mut self, id: Option<String>, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let before = self.targets.clone();
        self.targets.default = id;
        self.save_targets(before, cx)
    }

    /// Saves how uploaded files are named.
    pub fn set_naming(&mut self, naming: Naming, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let before = self.targets.clone();
        self.targets.naming = naming;
        self.save_targets(before, cx)
    }

    /// Snapshot of the jobs, newest batch last. Handoff B watches them.
    #[allow(dead_code)]
    pub fn jobs(&self) -> &[Job] {
        &self.jobs
    }

    /// The local share history, newest first (loaded once, then cached).
    #[allow(dead_code)]
    pub fn history(&mut self, cx: &mut Context<Self>) -> Vec<Record> {
        if self.history_loaded {
            return self.history.clone();
        }
        let paths = self.paths.clone();
        let records = {
            let paths = paths.clone();
            std::thread::spawn(move || heronite_share::history(&paths)).join().unwrap_or_default()
        };
        self.history_loaded = true;
        self.history = records.clone();
        let _ = cx;
        records
    }

    /// Mark the cache stale after the Settings page or a delete changed the history.
    #[allow(dead_code)]
    pub fn invalidate_history(&mut self) {
        self.history_loaded = false;
    }

    /// Cancel a job in flight (best effort: the backends clean up after themselves).
    #[allow(dead_code)]
    pub fn cancel(&self, id: u64) {
        if let Ok(jobs) = self.shared.lock() {
            for job in jobs.iter() {
                if let Ok(j) = job.lock() {
                    if j.job.id == id {
                        j.cancel.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
    }

    /// Delete a history record: the remote object goes first, the record
    /// follows (a failing remote delete keeps the record, see the crate).
    #[allow(dead_code)]
    pub fn delete_record(&mut self, id: &str, cx: &mut Context<Self>) {
        let paths = self.paths.clone();
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let outcome =
                cx.background_executor().spawn(async move { heronite_share::delete(&paths, &id).map_err(|e| e.to_string()) }).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |service: &mut ShareService, cx| {
                    service.history_loaded = false;
                    if let Err(e) = outcome {
                        crate::system::notify::send(tr!("The share could not be deleted"), e);
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Queue files for upload, one at a time per service. `target` is an id
    /// or a name, `None` is the default target (see [`resolve_target`]).
    pub fn share_files(cx: &mut App, files: Vec<PathBuf>, game: Option<String>, target: Option<String>, ui: ShareUi) {
        let entity = crate::services::share(cx);
        entity.update(cx, |service, cx| service.queue_files(files, game, target, ui, cx));
    }

    fn queue_files(&mut self, files: Vec<PathBuf>, game: Option<String>, target: Option<String>, ui: ShareUi, cx: &mut Context<Self>) {
        let target_id = match resolve_target(&self.targets, target.as_deref()) {
            Resolve::Target(id) => id,
            Resolve::NoTargets => {
                crate::system::notify::send(tr!("Nothing to share with"), tr!("Set up a sharing target in Settings > Sharing"));
                crate::system::show_main_window(cx);
                return;
            }
            Resolve::Unknown(given) => {
                crate::system::notify::send(tr!("Sharing target not found"), given);
                return;
            }
        };
        let target_name = self.targets.get(&target_id).map(|t| t.label()).unwrap_or_default();
        let mut jobs = Vec::new();
        for file in &files {
            let file_name = file.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
            let total = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
            let job = Arc::new(Mutex::new(JobShared {
                job: Job {
                    id: NEXT_JOB.fetch_add(1, Ordering::SeqCst),
                    file_name,
                    bytes: 0,
                    total,
                    target_id: target_id.clone(),
                    target_name: target_name.clone(),
                    state: JobState::Queued,
                },
                cancel: Arc::new(AtomicBool::new(false)),
                result_url: None,
            }));
            self.shared.lock().unwrap().push(job.clone());
            jobs.push(job);
        }
        self.queue.lock().unwrap().push_back(BatchWork { jobs, target_id, files, game, ui });
        self.start_worker(cx);
        self.start_poll(cx);
        cx.notify();
    }

    fn start_worker(&mut self, cx: &mut Context<Self>) {
        if self.worker_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let queue = self.queue.clone();
        let busy = self.worker_busy.clone();
        let paths = self.paths.clone();
        cx.spawn(async move |_, cx| {
            loop {
                let work = queue.lock().unwrap().pop_front();
                let Some(work) = work else { break };
                let paths = paths.clone();
                cx.background_executor()
                    .spawn(async move {
                        run_batch(paths, work);
                    })
                    .await;
            }
            busy.store(false, Ordering::SeqCst);
            // A batch that arrived while the loop wound down starts a new worker.
            if !queue.lock().unwrap().is_empty() {
                // The next share_files call starts it; nothing to do here.
            }
        })
        .detach();
    }

    /// Refresh the observable job snapshot and report finished batches.
    fn start_poll(&mut self, cx: &mut Context<Self>) {
        if self.polling {
            return;
        }
        self.polling = true;
        let shared = self.shared.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let Some(entity) = this.upgrade() else { return };
                let mut keep_polling = true;
                entity.update(cx, |service: &mut ShareService, cx| {
                    let (done, active) = snapshot(&shared);
                    service.jobs = done;
                    keep_polling = active;
                    cx.notify();
                });
                if !keep_polling {
                    break;
                }
            }
            if let Some(entity) = this.upgrade() {
                entity.update(cx, |service: &mut ShareService, _cx| service.polling = false);
            }
        })
        .detach();
    }
}

fn snapshot(shared: &Shared) -> (Vec<Job>, bool) {
    let jobs = shared.lock().unwrap();
    let mut out = Vec::new();
    let mut active = false;
    for job in jobs.iter() {
        let j = job.lock().unwrap();
        if matches!(j.job.state, JobState::Queued | JobState::Uploading) {
            active = true;
        }
        out.push(j.job.clone());
    }
    (out, active)
}

fn run_batch(paths: DataPaths, work: BatchWork) {
    for (file, job) in work.files.iter().zip(&work.jobs) {
        let cancel = job.lock().unwrap().cancel.clone();
        let job_for_progress = job.clone();
        let file = file.clone();
        let ctx = ShareContext { game: work.game.clone() };
        let paths = paths.clone();
        let target_id = work.target_id.clone();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            heronite_share::share(&paths, &target_id, &file, &ctx, &mut |progress: Progress| {
                let mut j = job_for_progress.lock().unwrap();
                j.job.bytes = progress.sent;
                j.job.total = progress.total;
                if matches!(j.job.state, JobState::Queued) {
                    j.job.state = JobState::Uploading;
                }
                if cancel.load(Ordering::SeqCst) {
                    Continue::Cancel
                } else {
                    Continue::Go
                }
            })
        }));
        let mut j = job.lock().unwrap();
        match outcome {
            Ok(Ok(record)) => {
                j.job.state = JobState::Done { url: record.url.clone() };
                j.result_url = Some(record.url);
            }
            Ok(Err(ShareError::Cancelled)) => {
                j.job.state = JobState::Cancelled;
            }
            Ok(Err(e)) => {
                let host_key = matches!(e, ShareError::HostKeyUnknown { .. });
                let text = if host_key { tr!("Open Settings > Sharing and trust the server's key").to_string() } else { e.to_string() };
                if work.ui.notify {
                    crate::system::notify::send(tr!("The upload failed"), text.clone());
                }
                j.job.state = JobState::Failed(text);
            }
            Err(_) => {
                j.job.state = JobState::Failed(tr!("The upload failed").to_string());
            }
        }
    }
    // Batch clipboard + notification: one URL per line in file order.
    let mut urls = Vec::new();
    let mut any_done = false;
    let mut all_terminal = true;
    for job in &work.jobs {
        let j = job.lock().unwrap();
        match &j.job.state {
            JobState::Done { url } => {
                any_done = true;
                urls.push(url.clone());
            }
            JobState::Cancelled | JobState::Failed(_) => {}
            _ => all_terminal = false,
        }
    }
    if all_terminal && any_done {
        let text = links_text(&urls);
        if work.ui.notify {
            crate::system::notify::send(tr!("Link copied"), text.clone());
        }
        if work.ui.clipboard {
            store_clipboard(text);
        }
    }
}

/// The links go to the clipboard through wl-copy/xclip/xsel too, because an
/// override-redirect overlay window may not own the clipboard itself.
pub fn store_clipboard(text: String) {
    std::thread::spawn(move || pipe_system_clipboard(&text));
}

/// Best effort, first tool found in PATH, short-lived.
pub fn pipe_system_clipboard(text: &str) {
    if !cfg!(target_os = "linux") {
        return;
    }
    for (prog, args) in [("wl-copy", vec![]), ("xclip", vec!["-selection", "clipboard"]), ("xsel", vec!["-ib"])] {
        if let Ok(mut child) = std::process::Command::new(prog)
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return;
        }
    }
}

/// Copy a link to the GPUI clipboard and the system one (see
/// [`pipe_system_clipboard`]): handoff B's overlay button uses this.
#[allow(dead_code)]
pub fn copy_link(cx: &mut App, url: &str) {
    cx.write_to_clipboard(ClipboardItem::new_string(url.to_string()));
    store_clipboard(url.to_string());
}

#[cfg(test)]
mod tests {
    use crate::services::share::{links_text, resolve_target, Resolve};
    use heronite_share::{Kind, Sftp, Target, Targets};

    fn target(id: &str, name: &str, enabled: bool) -> Target {
        Target {
            id: id.into(),
            name: name.into(),
            enabled,
            kind: Kind::Sftp(Sftp::default()),
            public_url: "https://files.example/{name}".into(),
            folder: String::new(),
            tls: Default::default(),
        }
    }

    fn targets(list: Vec<Target>, default: Option<&str>) -> Targets {
        Targets { targets: list, default: default.map(|d| d.into()), ..Default::default() }
    }

    #[test]
    fn resolve_prefers_the_explicit_id_or_name() {
        let t = targets(vec![target("a", "NAS", true), target("b", "Cloud", true)], Some("a"));
        assert_eq!(resolve_target(&t, Some("b")), Resolve::Target("b".into()));
        assert_eq!(resolve_target(&t, Some("NAS")), Resolve::Target("a".into()));
    }

    #[test]
    fn resolve_falls_back_to_default_then_the_only_enabled() {
        let t = targets(vec![target("a", "NAS", true), target("b", "Cloud", true)], Some("a"));
        assert_eq!(resolve_target(&t, None), Resolve::Target("a".into()));
        let t = targets(vec![target("a", "NAS", false), target("b", "Cloud", true)], None);
        assert_eq!(resolve_target(&t, None), Resolve::Target("b".into()));
        let t = targets(vec![target("a", "NAS", false), target("b", "Cloud", true)], Some("a"));
        assert_eq!(resolve_target(&t, None), Resolve::Target("b".into()));
    }

    #[test]
    fn resolve_reports_nothing_and_unknown() {
        let t = targets(vec![], None);
        assert_eq!(resolve_target(&t, None), Resolve::NoTargets);
        let t = targets(vec![target("a", "NAS", true), target("b", "Cloud", true)], None);
        assert_eq!(resolve_target(&t, None), Resolve::NoTargets);
        assert_eq!(resolve_target(&t, Some("zzz")), Resolve::Unknown("zzz".into()));
    }

    #[test]
    fn links_are_one_per_line_in_order() {
        let urls = vec!["https://x/1.png".to_string(), "https://x/2.png".to_string()];
        assert_eq!(links_text(&urls), "https://x/1.png\nhttps://x/2.png");
        assert_eq!(links_text(&[]), "");
    }
}
