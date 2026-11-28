//! A global keyboard shortcut besides the screenshot one (the in-game
//! overlay's), registered the same ways: a temporary Hyprland bind that
//! sends this process a signal, a grab on the X server of a bare X11
//! session (`x11`), else the XDG GlobalShortcuts portal.
//! Like the rest of the service, not exercised by the unit tests.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use heronite_i18n::tr;

use super::portal::PortalShortcut;
use super::{hyprland, to_hyprland_binding, to_portal_trigger, x11, HotkeySpec, ShortcutBackend};

#[derive(Default)]
struct Shared {
    stopped: AtomicBool,
    /// Keys of the Hyprland bind, once installed.
    hyprland: Mutex<Option<String>>,
    portal: Mutex<Option<PortalShortcut>>,
    signals: Mutex<Option<signal_hook::iterator::Handle>>,
    x11: Mutex<Option<x11::X11Grab>>,
}

/// A registered shortcut. Dropping it removes it, which blocks briefly
/// (`hyprctl`, the portal session): drop it off the UI thread.
pub struct GlobalHotkey {
    shared: Arc<Shared>,
}

type Ready = Box<dyn FnOnce(Result<ShortcutBackend, String>) + Send>;

impl GlobalHotkey {
    /// Registers `spec` in the background. `on_press` runs (on a thread of
    /// its own) for each press; `on_ready` gets the registration result once.
    pub fn start(spec: HotkeySpec, on_press: Arc<dyn Fn() + Send + Sync>, on_ready: Ready) -> Self {
        let shared = Arc::new(Shared::default());
        if hyprland::is_hyprland_session() {
            start_hyprland(&shared, spec, on_press, on_ready);
        } else if x11::is_x11_session() {
            start_x11(&shared, spec, on_press, on_ready);
        } else if let Some(trigger) = to_portal_trigger(&spec.accelerator) {
            let portal = PortalShortcut::start(
                spec.portal_app_id,
                (spec.id, spec.description),
                trigger,
                on_press,
                Box::new(move |r| on_ready(r.map(|_| ShortcutBackend::Portal))),
            );
            *shared.portal.lock().unwrap() = Some(portal);
        } else {
            on_ready(Err(tr!("The shortcut {accelerator} is unavailable", accelerator = &spec.accelerator)));
        }
        Self { shared }
    }
}

fn start_hyprland(shared: &Arc<Shared>, spec: HotkeySpec, on_press: Arc<dyn Fn() + Send + Sync>, on_ready: Ready) {
    let Some(binding) = to_hyprland_binding(&spec.accelerator) else {
        on_ready(Err(tr!("The shortcut {accelerator} is not supported by Hyprland", accelerator = &spec.accelerator)));
        return;
    };
    // In place before the bind exists: the default action of SIGUSR1 ends
    // the process. signal-hook keeps its handler after the iterator closes, so
    // a late signal is harmless too.
    let mut signals = match signal_hook::iterator::Signals::new([spec.signal]) {
        Ok(s) => s,
        Err(e) => {
            on_ready(Err(e.to_string()));
            return;
        }
    };
    *shared.signals.lock().unwrap() = Some(signals.handle());
    let weak = Arc::downgrade(shared);
    let pressed = on_press.clone();
    let listening = std::thread::Builder::new().name("heronite-hotkey-signal".into()).spawn(move || {
        for _ in signals.forever() {
            let Some(shared) = weak.upgrade() else { break };
            if !shared.stopped.load(Ordering::SeqCst) {
                pressed();
            }
        }
    });
    if let Err(e) = listening {
        on_ready(Err(e.to_string()));
        return;
    }
    let shared = shared.clone();
    let spawned = std::thread::Builder::new().name("heronite-hotkey-bind".into()).spawn(move || {
        let command = hyprland::signal_command(spec.signal);
        match hyprland::register_as(&binding, &spec.accelerator, &command, spec.description, spec.consume) {
            Ok(()) => {
                let mut keys = shared.hyprland.lock().unwrap();
                if shared.stopped.load(Ordering::SeqCst) {
                    // Dropped while it was being registered.
                    drop(keys);
                    let _ = hyprland::unregister(&binding.keys);
                } else {
                    *keys = Some(binding.keys.clone());
                }
                on_ready(Ok(ShortcutBackend::Hyprland));
            }
            Err(hyprland::Failure::Refused(e)) => on_ready(Err(e)),
            // Hyprland's portal may take the shortcut where `hyprctl` cannot.
            Err(hyprland::Failure::Unavailable(e)) => {
                log::warn!("Hyprland could not be asked for the shortcut {}; trying the desktop portal: {e}", spec.id);
                let Some(trigger) = to_portal_trigger(&spec.accelerator) else { return on_ready(Err(e)) };
                let mut portal = shared.portal.lock().unwrap();
                // Dropped meanwhile: `Drop` has already emptied this.
                if shared.stopped.load(Ordering::SeqCst) {
                    return;
                }
                *portal = Some(PortalShortcut::start(
                    spec.portal_app_id,
                    (spec.id, spec.description),
                    trigger,
                    on_press,
                    // Hyprland's reason first: that is the session's own way.
                    Box::new(move |r| on_ready(r.map(|_| ShortcutBackend::Portal).map_err(|_| e))),
                ));
                // `Drop` sets `stopped` before it takes this slot; if it ran
                // between the check above and the store, take it back here
                // (joining the portal thread outside the lock).
                let stale = if shared.stopped.load(Ordering::SeqCst) { portal.take() } else { None };
                drop(portal);
                drop(stale);
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("could not start the shortcut thread: {e}");
    }
}

/// X11 before the portal on a bare X11 session: the grab is answered at
/// once, the portal usually is not there at all. Failing both, the portal's
/// error is the one reported (it is the more general answer).
fn start_x11(shared: &Arc<Shared>, spec: HotkeySpec, on_press: Arc<dyn Fn() + Send + Sync>, on_ready: Ready) {
    let Some(trigger) = x11::to_x11_trigger(&spec.accelerator) else {
        // A key the grab table does not know: straight to the portal.
        if let Some(trigger) = to_portal_trigger(&spec.accelerator) {
            let portal = PortalShortcut::start(
                spec.portal_app_id,
                (spec.id, spec.description),
                trigger,
                on_press,
                Box::new(move |r| on_ready(r.map(|_| ShortcutBackend::Portal))),
            );
            *shared.portal.lock().unwrap() = Some(portal);
        } else {
            on_ready(Err(tr!("The shortcut {accelerator} is unavailable", accelerator = &spec.accelerator)));
        }
        return;
    };
    let weak = Arc::downgrade(shared);
    let spawned = std::thread::Builder::new().name("heronite-hotkey-x11".into()).spawn(move || {
        match x11::X11Grab::grab(trigger, on_press.clone()) {
            Ok(grab) => {
                let Some(shared) = weak.upgrade() else {
                    // Dropped while connecting: the grab dies with it here.
                    return;
                };
                let mut slot = shared.x11.lock().unwrap();
                if shared.stopped.load(Ordering::SeqCst) {
                    drop(slot);
                    drop(grab);
                    return;
                }
                *slot = Some(grab);
                drop(slot);
                on_ready(Ok(ShortcutBackend::X11));
            }
            Err(grab_error) => {
                log::warn!("X11 key grab failed ({grab_error}); trying the desktop portal");
                let Some(trigger) = to_portal_trigger(&spec.accelerator) else {
                    return on_ready(Err(grab_error));
                };
                let Some(shared) = weak.upgrade() else { return };
                if shared.stopped.load(Ordering::SeqCst) {
                    return;
                }
                let mut slot = shared.portal.lock().unwrap();
                *slot = Some(PortalShortcut::start(
                    spec.portal_app_id,
                    (spec.id, spec.description),
                    trigger,
                    on_press,
                    Box::new(move |r| {
                        on_ready(match r {
                            Ok(_) => Ok(ShortcutBackend::Portal),
                            Err(portal_error) => Err(format!("{grab_error}; {portal_error}")),
                        })
                    }),
                ));
                let stale = if shared.stopped.load(Ordering::SeqCst) { slot.take() } else { None };
                drop(slot);
                drop(stale);
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("could not start the shortcut thread: {e}");
    }
}

impl Drop for GlobalHotkey {
    fn drop(&mut self) {
        let keys = {
            let mut keys = self.shared.hyprland.lock().unwrap();
            self.shared.stopped.store(true, Ordering::SeqCst);
            keys.take()
        };
        if let Some(keys) = keys {
            if let Err(e) = hyprland::unregister(&keys) {
                log::warn!("Could not remove the Hyprland shortcut {keys}: {e}");
            }
        }
        // Ungrabs the keys and joins the reader thread.
        drop(self.shared.x11.lock().unwrap().take());
        // Joins the portal thread.
        drop(self.shared.portal.lock().unwrap().take());
        if let Some(handle) = self.shared.signals.lock().unwrap().take() {
            handle.close();
        }
    }
}
