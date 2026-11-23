//! XDG desktop portals through `ashpd`: GlobalShortcuts and Screenshot.
//! Only the pure parts are unit-tested; `live_portal` talks to the session's
//! portal and runs on request (`cargo test -p heronite-media -- --ignored`).
//!
//! # The app id
//! xdg-desktop-portal 1.21 and later refuse a GlobalShortcuts session from an
//! app they cannot name ("An app id is required"). An app outside a sandbox
//! is named by its systemd unit (`app-<id>-*.scope`, which GNOME and KDE put
//! the apps of their menus in, and little else does) or by
//! `Registry.Register`, which must be the first portal call of its D-Bus
//! connection. ashpd's shared connection has long been used by then (GPUI
//! reads the Settings portal at startup), so the portals here have a
//! connection of their own, registered when it is made.

use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use ashpd::desktop::screenshot::Screenshot;
use ashpd::zbus;
use futures_util::future::{select, Either};
use futures_util::StreamExt;
use heronite_i18n::tr;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Id of the screenshot shortcut in its portal session.
pub const SHORTCUT_ID: &str = "game-screenshot";

/// How long the portal gets to name the app, and to close a session.
const REGISTER_TIMEOUT: Duration = Duration::from_secs(10);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a screenshot may take. The first one of an app can wait for the
/// user's permission, in a dialog that may open behind the game: without a
/// deadline that capture would never end, and no other could start.
const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(20);

/// Hears how the registration went. A desktop behind the portal picks the
/// keys itself (the trigger asked for is a suggestion, and one it remembers
/// from an earlier time wins): `Ok` carries what it says triggers the
/// shortcut, `None` when it names no key.
pub type Ready = Box<dyn FnOnce(Result<Option<String>, String>) + Send>;

/// The connection every portal call here goes through, once the portal
/// knows it as the app.
static CONNECTION: Mutex<Option<zbus::Connection>> = Mutex::new(None);

/// The portals' connection, registered as `app_id` (every caller passes the
/// same one) and kept once it is. Blocking. When the registration fails the
/// connection is still handed out, since the desktop may know the app by its
/// systemd unit and portals older than the registry ask for no app id, but
/// it is not kept: the next caller tries again on a new one, as the menu
/// entry the portal wants may have been installed since.
fn connection(app_id: Option<&str>) -> Result<zbus::Connection, String> {
    let mut shared = CONNECTION.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(connection) = shared.as_ref() {
        return Ok(connection.clone());
    }
    let (connection, registered) = pollster::block_on(async {
        let connection = zbus::Connection::session().await.map_err(|e| e.to_string())?;
        let registered = match app_id {
            Some(id) => register(&connection, id).await,
            None => true,
        };
        Ok::<_, String>((connection, registered))
    })?;
    if registered {
        *shared = Some(connection.clone());
    }
    Ok(connection)
}

/// Names the app behind `connection` to the portal; false when it did not
/// take the name.
async fn register(connection: &zbus::Connection, id: &str) -> bool {
    let parsed = match id.parse::<ashpd::AppID>() {
        Ok(parsed) => parsed,
        Err(e) => {
            log::warn!("invalid portal app id {id:?}: {e}");
            return false;
        }
    };
    let register = ashpd::register_host_app_with_connection(connection.clone(), parsed);
    match unless(register, async_io::Timer::after(REGISTER_TIMEOUT)).await {
        Some(Ok(())) => {
            log::info!("registered with the desktop portal as {id}");
            true
        }
        Some(Err(e)) => {
            log::warn!("the desktop portal did not register the app as {id}: {e}");
            false
        }
        None => {
            log::warn!("the desktop portal did not answer the registration as {id} in time");
            false
        }
    }
}

/// What to tell the user when the portal refuses a shortcut.
fn shortcut_error(error: &ashpd::Error) -> String {
    use ashpd::desktop::ResponseError;
    use ashpd::PortalError;
    match error {
        e if no_shortcuts_service(e) => {
            tr!("This desktop has no global shortcuts service. Install or enable a portal backend that supports global shortcuts.").into()
        }
        ashpd::Error::Portal(PortalError::NotAllowed(reason)) if is_missing_app_id(reason) => {
            tr!("The desktop refused the shortcut: it could not tell that Heronited asked for it").into()
        }
        ashpd::Error::Response(ResponseError::Cancelled) => tr!("The shortcut was not approved in the desktop's dialog").into(),
        other => other.to_string(),
    }
}

/// Whether the error means this desktop cannot do global shortcuts at all:
/// no portal frontend, or one without the interface or a backend for it.
/// Everything else (a refused bind, a cancelled dialog, a timeout) keeps the
/// portal's own words.
fn no_shortcuts_service(error: &ashpd::Error) -> bool {
    match error {
        ashpd::Error::PortalNotFound(_) => true,
        ashpd::Error::Zbus(e) => zbus_no_shortcuts(e),
        _ => false,
    }
}

fn zbus_no_shortcuts(e: &zbus::Error) -> bool {
    use zbus::fdo::Error as Fdo;
    match e {
        zbus::Error::InterfaceNotFound => true,
        zbus::Error::FDO(fdo) => match &**fdo {
            Fdo::UnknownInterface(_) | Fdo::UnknownMethod(_) | Fdo::UnknownObject(_) | Fdo::ServiceUnknown(_) | Fdo::NameHasNoOwner(_) => {
                true
            }
            // The same reply ashpd maps to `PortalNotFound` when the details
            // name the interface; the probe sees it raw.
            Fdo::InvalidArgs(d) => missing_interface(d),
            _ => false,
        },
        // Error names outside the D-Bus core set arrive as `MethodError`;
        // the ones a portal without the interface or its backend answers.
        zbus::Error::MethodError(name, detail, _) => {
            matches!(
                name.as_str(),
                "org.freedesktop.portal.Error.NotFound"
                    | "org.freedesktop.DBus.Error.UnknownInterface"
                    | "org.freedesktop.DBus.Error.UnknownMethod"
                    | "org.freedesktop.DBus.Error.ServiceUnknown"
            ) || detail.as_deref().is_some_and(missing_interface)
        }
        _ => false,
    }
}

/// The portal's "No such interface" reply, however it is phrased.
fn missing_interface(detail: &str) -> bool {
    detail.contains("org.freedesktop.portal.GlobalShortcuts") || detail.to_ascii_lowercase().contains("no such interface")
}

/// The `version` property of `org.freedesktop.portal.GlobalShortcuts` on the
/// portal's object. ashpd reads it too but shrugs off every failure but one,
/// so without this probe a missing interface only surfaces at `CreateSession`,
/// further from the cause. `CacheProperties::No`: nothing to keep updated.
async fn shortcuts_served(connection: &zbus::Connection) -> Result<(), ashpd::Error> {
    let proxy = zbus::proxy::Builder::<zbus::Proxy>::new(connection)
        .destination("org.freedesktop.portal.Desktop")
        .path("/org/freedesktop/portal/desktop")
        .interface("org.freedesktop.portal.GlobalShortcuts")
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?;
    proxy.get_property::<u32>("version").await?;
    Ok(())
}

/// The portal's "An app id is required".
fn is_missing_app_id(reason: &str) -> bool {
    reason.to_ascii_lowercase().contains("app id")
}

/// A live GlobalShortcuts session on its own thread.
pub struct PortalShortcut {
    stop: Option<futures_channel::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl PortalShortcut {
    /// Binds `trigger` as the shortcut `id` and calls `on_activated` for
    /// each press. `on_ready` receives the registration result once, or
    /// never when the shortcut is stopped before the desktop answered.
    pub fn start(
        app_id: Option<String>,
        (id, description): (&'static str, &'static str),
        trigger: String,
        on_activated: Arc<dyn Fn() + Send + Sync>,
        on_ready: Ready,
    ) -> Self {
        let (stop_tx, stop_rx) = futures_channel::oneshot::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("heronite-shortcut-portal".into())
            .spawn(move || match connection(app_id.as_deref()) {
                Ok(connection) => pollster::block_on(run(connection, id, description, trigger, on_activated, on_ready, stop_rx)),
                Err(e) => on_ready(Err(e)),
            })
            .ok();
        Self { stop: Some(stop_tx), thread }
    }

    /// Closes the portal session and joins the thread.
    pub fn stop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for PortalShortcut {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What the desktop says triggers the shortcut `id`, among the
/// `(id, trigger description)` it bound.
fn bound_trigger<'a>(bound: impl IntoIterator<Item = (&'a str, &'a str)>, id: &str) -> Option<String> {
    bound.into_iter().find(|(bound_id, _)| *bound_id == id).map(|(_, trigger)| trigger.trim().to_owned()).filter(|t| !t.is_empty())
}

/// What `future` gives, unless `other` finishes first.
async fn unless<T, O>(future: impl Future<Output = T>, other: impl Future<Output = O> + Unpin) -> Option<T> {
    futures_util::pin_mut!(future);
    match select(future, other).await {
        Either::Left((value, _)) => Some(value),
        Either::Right(_) => None,
    }
}

fn refused(id: &str, on_ready: Ready, error: &ashpd::Error) {
    log::warn!("the desktop portal refused the shortcut {id}: {error}");
    on_ready(Err(shortcut_error(error)));
}

/// Whoever drops the shortcut joins this thread, so nothing here waits on
/// the desktop alone: `stop` ends every step (a desktop may hold the bind
/// until the user approves it in a dialog, which can open behind the game)
/// and closing the session has a deadline.
async fn run(
    connection: zbus::Connection,
    id: &'static str,
    description: &'static str,
    trigger: String,
    on_activated: Arc<dyn Fn() + Send + Sync>,
    on_ready: Ready,
    mut stop: futures_channel::oneshot::Receiver<()>,
) {
    let setup = async {
        shortcuts_served(&connection).await?;
        let shortcuts = GlobalShortcuts::with_connection(connection).await?;
        let session = shortcuts.create_session(Default::default()).await?;
        Ok::<_, ashpd::Error>((shortcuts, session))
    };
    let (shortcuts, session) = match unless(setup, &mut stop).await {
        Some(Ok(v)) => v,
        Some(Err(e)) => return refused(id, on_ready, &e),
        None => return,
    };
    let bind = async {
        let activated = shortcuts.receive_activated().await?;
        let request = shortcuts
            .bind_shortcuts(
                &session,
                &[NewShortcut::new(id, description).preferred_trigger(trigger.as_str())],
                None,
                BindShortcutsOptions::default(),
            )
            .await?;
        let trigger = bound_trigger(request.response()?.shortcuts().iter().map(|s| (s.id(), s.trigger_description())), id);
        Ok::<_, ashpd::Error>((activated, trigger))
    };
    match unless(bind, &mut stop).await {
        Some(Ok((mut activated, trigger))) => {
            on_ready(Ok(trigger));
            while let Either::Left((Some(a), _)) = select(activated.next(), &mut stop).await {
                if a.shortcut_id() == id {
                    on_activated();
                }
            }
        }
        Some(Err(e)) => refused(id, on_ready, &e),
        // Stopped while the desktop still held the bind.
        None => {}
    }
    match unless(session.close(), async_io::Timer::after(CLOSE_TIMEOUT)).await {
        Some(Ok(())) => {}
        Some(Err(e)) => log::debug!("closing the session of the shortcut {id}: {e}"),
        None => log::warn!("the desktop portal did not close the session of the shortcut {id} in time"),
    }
}

/// `org.freedesktop.portal.Screenshot` (non-interactive): path of the PNG the
/// portal wrote. `app_id`: see [`connection`] (the desktop keeps the
/// permission to capture under it). Gives up after [`SCREENSHOT_TIMEOUT`].
pub fn screenshot(app_id: Option<&str>) -> Result<PathBuf, String> {
    let connection = connection(app_id)?;
    pollster::block_on(async {
        let request = async { Screenshot::request().interactive(false).connection(Some(connection)).send().await?.response() };
        let response = match unless(request, async_io::Timer::after(SCREENSHOT_TIMEOUT)).await {
            Some(response) => response.map_err(|e| e.to_string())?,
            None => return Err(tr!("The desktop did not answer in time (it may be asking for permission in a dialog)").into()),
        };
        file_uri_to_path(response.uri().as_str()).ok_or_else(|| format!("unsupported screenshot uri {}", response.uri()))
    })
}

/// `file:///a%20b.png` -> `/a b.png`.
pub fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let decoded = (bytes[i] == b'%' && i + 2 < bytes.len())
            .then(|| std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()))
            .flatten();
        if let Some(b) = decoded {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_file_uris() {
        assert_eq!(file_uri_to_path("file:///tmp/a%20b.png"), Some(PathBuf::from("/tmp/a b.png")));
        assert_eq!(file_uri_to_path("file://localhost/tmp/x.png"), Some(PathBuf::from("/tmp/x.png")));
        assert_eq!(file_uri_to_path("file:///tmp/%E2%9C%93.png"), Some(PathBuf::from("/tmp/✓.png")));
        assert_eq!(file_uri_to_path("file:///tmp/100%"), Some(PathBuf::from("/tmp/100%")));
        assert!(file_uri_to_path("https://x/y.png").is_none());
    }

    /// The whole way through the session's portal: a connection registered
    /// as Heronited gets a GlobalShortcuts session and its shortcut bound.
    /// Needs xdg-desktop-portal with a GlobalShortcuts backend and
    /// `io.github.sudoivan.Heronited.desktop` installed; a desktop that asks the
    /// user to approve shortcuts shows its dialog.
    #[test]
    #[ignore = "talks to the desktop's portal"]
    fn live_portal_binds_a_shortcut() {
        // ashpd's shared connection is in use before any shortcut is wanted,
        // as it is in the app (GPUI reads the Settings portal at startup):
        // registering on it afterwards is refused.
        let shared = pollster::block_on(async { GlobalShortcuts::new().await?.create_session(Default::default()).await.map(|_| ()) });
        eprintln!("a session on ashpd's shared connection: {shared:?}");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut shortcut = PortalShortcut::start(
            Some("io.github.sudoivan.Heronited".into()),
            ("heronite-live-test", "Heronited portal test"),
            "CTRL+ALT+SHIFT+F10".into(),
            Arc::new(|| {}),
            Box::new(move |result| {
                let _ = tx.send(result);
            }),
        );
        let result = rx.recv_timeout(std::time::Duration::from_secs(60)).expect("the portal answers");
        shortcut.stop();
        eprintln!("the desktop's trigger: {:?}", result.expect("the shortcut is bound"));
    }

    #[test]
    fn the_trigger_the_desktop_names() {
        let bound = [("overlay", "Meta+G"), ("game-screenshot", " Ctrl+F10 "), ("replay", "")];
        assert_eq!(bound_trigger(bound, "game-screenshot").as_deref(), Some("Ctrl+F10"));
        // Bound without a key, or not bound at all.
        assert_eq!(bound_trigger(bound, "replay"), None);
        assert_eq!(bound_trigger(bound, "other"), None);
    }

    #[test]
    fn a_step_ends_when_it_is_stopped_or_late() {
        let never = std::future::pending::<u8>;
        let (stop, mut stopped) = futures_channel::oneshot::channel::<()>();
        stop.send(()).unwrap();
        assert_eq!(pollster::block_on(unless(never(), &mut stopped)), None);
        // A dropped shortcut stops it too.
        let (stop, mut stopped) = futures_channel::oneshot::channel::<()>();
        drop(stop);
        assert_eq!(pollster::block_on(unless(never(), &mut stopped)), None);
        assert_eq!(pollster::block_on(unless(never(), async_io::Timer::after(Duration::from_millis(20)))), None);
        assert_eq!(pollster::block_on(unless(async { 7 }, async_io::Timer::after(Duration::from_secs(30)))), Some(7));
    }

    /// The `zbus::Error` a D-Bus error reply named `name`, with a text body,
    /// becomes: core names land in `fdo::Error`, the rest in `MethodError`.
    fn error_reply(name: &str) -> zbus::Error {
        error_reply_with(name, "no backend")
    }

    fn error_reply_with(name: &str, body: &str) -> zbus::Error {
        let serial = std::num::NonZeroU32::new(1).unwrap();
        zbus::Message::error_for_serial(":1.1", serial, name).build(&body).unwrap().into()
    }

    #[test]
    fn portal_refusals_are_explained() {
        use ashpd::desktop::ResponseError;
        use ashpd::PortalError;
        let friendly = "This desktop has no global shortcuts service. Install or enable a portal backend that supports global shortcuts.";
        let missing = ashpd::Error::Portal(PortalError::NotAllowed("An app id is required".into()));
        assert_eq!(shortcut_error(&missing), "The desktop refused the shortcut: it could not tell that Heronited asked for it");
        // Another refusal keeps the portal's own words.
        let other = ashpd::Error::Portal(PortalError::NotAllowed("Not on this seat".into()));
        assert!(shortcut_error(&other).contains("Not on this seat"), "{}", shortcut_error(&other));
        assert_eq!(
            shortcut_error(&ashpd::Error::Response(ResponseError::Cancelled)),
            "The shortcut was not approved in the desktop's dialog"
        );
        let name = zbus::names::OwnedInterfaceName::try_from("org.freedesktop.portal.GlobalShortcuts").unwrap();
        assert_eq!(shortcut_error(&ashpd::Error::PortalNotFound(name)), friendly);
    }

    #[test]
    fn a_desktop_without_shortcuts_is_explained() {
        let friendly = "This desktop has no global shortcuts service. Install or enable a portal backend that supports global shortcuts.";
        for e in [
            ashpd::Error::Zbus(zbus::Error::InterfaceNotFound),
            ashpd::Error::Zbus(error_reply("org.freedesktop.DBus.Error.UnknownInterface")),
            ashpd::Error::Zbus(error_reply("org.freedesktop.DBus.Error.UnknownMethod")),
            ashpd::Error::Zbus(error_reply("org.freedesktop.DBus.Error.UnknownObject")),
            ashpd::Error::Zbus(error_reply("org.freedesktop.DBus.Error.ServiceUnknown")),
            ashpd::Error::Zbus(error_reply("org.freedesktop.DBus.Error.NameHasNoOwner")),
            ashpd::Error::Zbus(error_reply_with(
                "org.freedesktop.DBus.Error.InvalidArgs",
                "No such interface “org.freedesktop.portal.GlobalShortcuts”",
            )),
            ashpd::Error::Zbus(error_reply("org.freedesktop.portal.Error.NotFound")),
        ] {
            assert_eq!(shortcut_error(&e), friendly, "{e}");
        }
        // A failure that is not "no service" keeps the portal's own words.
        let failed = ashpd::Error::Zbus(error_reply("org.freedesktop.portal.Error.Failed"));
        assert!(shortcut_error(&failed).contains("Failed"), "{}", shortcut_error(&failed));
    }

    /// A private session bus with no portal on it: the probe must end in the
    /// friendly error, not raw zbus text. Runs its own `dbus-daemon` and
    /// points the process at it, so it runs alone (`--ignored`) and needs
    /// `dbus-daemon` installed.
    #[test]
    #[ignore = "starts its own session bus"]
    fn a_bus_without_the_portal_is_explained() {
        use std::io::{BufRead, BufReader};
        let mut daemon = match std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(d) => d,
            Err(e) => return eprintln!("dbus-daemon not installed, skipped: {e}"),
        };
        let stdout = daemon.stdout.take().unwrap();
        let address = BufReader::new(stdout).lines().next().unwrap().unwrap();
        let old = std::env::var_os("DBUS_SESSION_BUS_ADDRESS");
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address);

        let (tx, rx) = std::sync::mpsc::channel();
        let mut shortcut = PortalShortcut::start(
            None,
            ("heronite-bus-test", "Heronited bus test"),
            "CTRL+ALT+F12".into(),
            Arc::new(|| {}),
            Box::new(move |result| {
                let _ = tx.send(result);
            }),
        );
        let result = rx.recv_timeout(std::time::Duration::from_secs(30));
        shortcut.stop();
        match old {
            Some(v) => std::env::set_var("DBUS_SESSION_BUS_ADDRESS", v),
            None => std::env::remove_var("DBUS_SESSION_BUS_ADDRESS"),
        }
        let _ = daemon.kill();
        let _ = daemon.wait();

        let err = result.expect("the bus answers").expect_err("a bus without the portal refuses the shortcut");
        assert!(err.contains("no global shortcuts service"), "{err}");
    }
}
