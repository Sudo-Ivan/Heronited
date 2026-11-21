//! The overlay's surfaces: Wayland layer-shell, X11 override-redirect windows
//! or Windows topmost tool windows, drawn over everything (fullscreen games
//! included) without keyboard focus, so the game keeps it; only a panel
//! opened from the keyboard takes it (until it closes). On Windows every panel
//! takes the foreground, so the games that ignore the controller in the
//! background ignore the overlay's navigation (XInput has no grab). Sizes
//! follow the display's height, like console mode, smaller since the panel
//! floats over a game (1440 px tall → 1.16).

#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use gpui_kit::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellNotSupportedError, LayerShellOptions};
use gpui_kit::*;
use heronite_data::overlay_prefs::Side;

/// Size of the panel in design px (console mode's 1280×800 scale). The
/// height fits the tallest tab (Home with the FPS limit and "Mark this
/// moment" rows and its recent screenshots); every tab
/// fills it, so the panel never changes size while it loads or between tabs.
pub const PANEL_W: f32 = 440.;
pub const PANEL_H: f32 = 912.;
/// Gap between the surfaces and the screen edges, in design px.
pub const EDGE: f32 = 16.;
/// Size of a notice, in design px.
pub const NOTICE_W: f32 = 360.;
pub const NOTICE_H: f32 = 76.;

/// Display height (px) per unit of the console scale.
const PX_PER_SCALE: f32 = 1240.;

/// The display one open is for, read once so the size and the place agree
/// (on Windows the foreground window may move to another monitor between
/// two reads).
pub struct Display {
    height: f32,
    /// X11: the X screen the surfaces are placed on (its origin and size, in
    /// logical px). There is no layer-shell to anchor them, so they are put
    /// at the edges by hand.
    #[cfg(target_os = "linux")]
    x_screen: Option<Bounds<Pixels>>,
    #[cfg(windows)]
    screen: std::io::Result<heronite_platform::overlay_window::Screen>,
}

impl Display {
    /// Height of the tallest display. Wayland has no primary display (GPUI
    /// returns none), and the compositor puts the surface on the focused one,
    /// which this cannot know: one display is the usual case. Windows uses
    /// the target monitor (`target_screen`).
    /// `HERONITE_DEBUG_OVERLAY_DISPLAY=<px>` stands in for it (sizes of other
    /// screens in debug captures).
    pub fn current(cx: &App) -> Self {
        #[cfg(windows)]
        let screen = target_screen();
        // GPUI lists the displays at the interface zoom; the overlay's
        // windows are not zoomed (`gpui-zoom`), so it takes their own size.
        let zoom = gpui_zoom::zoom();
        #[cfg(target_os = "linux")]
        let x_screen = if on_x11() {
            cx.primary_display().or_else(|| cx.displays().into_iter().next()).map(|display| {
                let b = display.bounds();
                Bounds {
                    origin: point(px(f32::from(b.origin.x) * zoom), px(f32::from(b.origin.y) * zoom)),
                    size: size(px(f32::from(b.size.width) * zoom), px(f32::from(b.size.height) * zoom)),
                }
            })
        } else {
            None
        };
        let height = crate::env::parse::<f32>("HERONITE_DEBUG_OVERLAY_DISPLAY").unwrap_or_else(|| {
            #[cfg(windows)]
            match &screen {
                Ok(screen) => return screen.logical_height(),
                Err(e) => log::warn!("overlay: cannot read the foreground monitor: {e}"),
            }
            cx.displays().iter().map(|d| f32::from(d.bounds().size.height) * zoom).fold(0., f32::max).max(720.)
        });
        Self {
            height,
            #[cfg(target_os = "linux")]
            x_screen,
            #[cfg(windows)]
            screen,
        }
    }

    /// The console scale for the display the overlay shows on.
    pub fn scale(&self) -> f32 {
        (self.height / PX_PER_SCALE).clamp(1., 2.)
    }

    /// The tallest the panel may be: the display minus the edge gaps (a Steam
    /// Deck's 800 px is a little short of the full height).
    fn panel_max_height(&self) -> f32 {
        (self.height - 2. * EDGE * self.scale()).max(400.)
    }

    /// The monitor the native surfaces go on, when it could be read.
    #[cfg(windows)]
    pub fn screen(&self) -> Option<heronite_platform::overlay_window::Screen> {
        self.screen.as_ref().ok().copied()
    }
}

/// Windows: a Direct3D game in exclusive fullscreen, where a topmost window
/// would minimise it or switch its display mode, so nothing opens. Logged
/// once per stretch of it.
#[cfg(windows)]
pub fn exclusive_fullscreen() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static LOGGED: AtomicBool = AtomicBool::new(false);
    let on = heronite_platform::overlay_window::exclusive_fullscreen();
    if !on {
        LOGGED.store(false, Ordering::Relaxed);
    } else if !LOGGED.swap(true, Ordering::Relaxed) {
        log::info!("overlay: a game is in exclusive fullscreen; the overlay does not open over it");
    }
    on
}

/// Only Windows has an exclusive fullscreen to stay out of.
#[cfg(not(windows))]
pub fn exclusive_fullscreen() -> bool {
    false
}

/// The monitor of the foreground window (the game's). In debug captures,
/// `HERONITE_DEBUG_OVERLAY_MONITOR=<x>,<y>` picks the one holding that desktop
/// point (physical px) instead, so another monitor is tested without moving
/// the focus.
#[cfg(windows)]
pub(crate) fn target_screen() -> std::io::Result<heronite_platform::overlay_window::Screen> {
    use heronite_platform::overlay_window;
    let point = crate::env::value("HERONITE_DEBUG_OVERLAY_MONITOR").and_then(|v| {
        let (x, y) = v.split_once(',')?;
        Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
    });
    match point {
        Some((x, y)) => overlay_window::screen_at(x, y),
        None => overlay_window::foreground_screen(),
    }
}

#[cfg(target_os = "linux")]
fn side_anchor(side: Side) -> Anchor {
    match side {
        Side::Left => Anchor::LEFT,
        Side::Right => Anchor::RIGHT,
    }
}

/// The session is X11 (no Wayland): there is no layer-shell, so the surfaces
/// are override-redirect windows placed at the screen's edges by hand.
#[cfg(target_os = "linux")]
pub fn on_x11() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_none()
}

#[cfg(not(target_os = "linux"))]
pub fn on_x11() -> bool {
    false
}

#[cfg(target_os = "linux")]
impl Display {
    /// Where a window of `size` goes on the X screen: `margin` from the edge
    /// `side` names, and from the top (or the bottom, `bottom`).
    fn x11_origin(&self, size: Size<Pixels>, side: Side, bottom: bool, margin: Pixels) -> Option<Point<Pixels>> {
        let screen = self.x_screen?;
        let x = match side {
            Side::Left => screen.origin.x + margin,
            Side::Right => screen.origin.x + screen.size.width - size.width - margin,
        };
        let y = if bottom { screen.origin.y + screen.size.height - size.height - margin } else { screen.origin.y + margin };
        Some(point(x, y))
    }
}

/// The window of a surface. `layer`: on Linux a layer-shell surface at the
/// top (`bottom`: at the bottom), along `side`, or on X11 an override-redirect
/// window at `origin`; on Windows a hidden topmost pop-up that `windows::open`
/// places and shows. Otherwise a normal window (debug captures).
#[allow(clippy::too_many_arguments)]
fn options(
    namespace: &str,
    size: Size<Pixels>,
    side: Side,
    bottom: bool,
    margin: Pixels,
    layer: bool,
    keyboard: bool,
    origin: Option<Point<Pixels>>,
) -> WindowOptions {
    #[cfg(windows)]
    if layer {
        return WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size })),
            app_id: Some(format!("io.github.sudoivan.{namespace}")),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::PopUp,
            focus: false,
            show: false,
            is_resizable: false,
            ..Default::default()
        };
    }
    #[cfg(target_os = "linux")]
    let kind = match (layer, on_x11()) {
        // X11 has no layer-shell: an override-redirect window sits over the
        // game (fullscreen too), never gets the keyboard unless it asks for
        // it, and is placed where `origin` says.
        (true, true) => WindowKind::PopUp,
        (true, false) => layer_shell(namespace, side, bottom, margin, keyboard),
        _ => WindowKind::Normal,
    };
    #[cfg(not(target_os = "linux"))]
    let kind = if layer { layer_shell(namespace, side, bottom, margin, keyboard) } else { WindowKind::Normal };
    WindowOptions {
        titlebar: None,
        window_bounds: Some(WindowBounds::Windowed(Bounds { origin: origin.unwrap_or_else(|| point(px(0.), px(0.))), size })),
        app_id: Some(if layer { "io.github.sudoivan.Heronited".into() } else { format!("io.github.sudoivan.{namespace}") }),
        window_background: WindowBackgroundAppearance::Transparent,
        kind,
        focus: !layer || keyboard,
        ..Default::default()
    }
}

#[cfg(target_os = "linux")]
fn layer_shell(namespace: &str, side: Side, bottom: bool, margin: Pixels, keyboard: bool) -> WindowKind {
    WindowKind::LayerShell(LayerShellOptions {
        namespace: namespace.to_string(),
        layer: Layer::Overlay,
        anchor: side_anchor(side) | if bottom { Anchor::BOTTOM } else { Anchor::TOP },
        margin: Some((margin, margin, margin, margin)),
        keyboard_interactivity: if keyboard { KeyboardInteractivity::Exclusive } else { KeyboardInteractivity::None },
        ..Default::default()
    })
}

/// Layer-shell is Wayland's, so it is never asked for off Linux: Windows
/// returns its own pop-up from [`options`] first, and other systems have no
/// overlay ([`supported`]).
#[cfg(not(target_os = "linux"))]
fn layer_shell(_namespace: &str, _side: Side, _bottom: bool, _margin: Pixels, _keyboard: bool) -> WindowKind {
    WindowKind::Normal
}

/// A panel [`open_panel`] opened.
pub struct OpenedPanel<V: 'static> {
    pub handle: WindowHandle<V>,
    /// Windows: places and shows the native window, then gives what hands the
    /// foreground back to the game when the panel closes. `None` for a
    /// normal window.
    #[cfg(windows)]
    pub shown: Option<Task<anyhow::Result<Option<heronite_platform::overlay_window::FocusReturn>>>>,
}

/// Opens the panel along one side of the screen. `layer = false` draws it in
/// a normal window (debug captures); `keyboard`: it takes the keyboard.
pub fn open_panel<V: Render>(
    cx: &mut App,
    display: Display,
    side: Side,
    layer: bool,
    keyboard: bool,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> anyhow::Result<OpenedPanel<V>> {
    let s = display.scale();
    let height = (PANEL_H * s).min(display.panel_max_height());
    let size = size(px(PANEL_W * s), px(height));
    #[cfg(windows)]
    if layer {
        let (handle, shown) = windows::open(cx, display.screen, "overlay", size, side, false, EDGE * s, build)?;
        return Ok(OpenedPanel { handle, shown: Some(shown) });
    }
    #[cfg(target_os = "linux")]
    let origin = if layer { display.x11_origin(size, side, false, px(EDGE * s)) } else { None };
    #[cfg(not(target_os = "linux"))]
    let origin = None;
    let handle = cx.open_window(options("overlay", size, side, false, px(EDGE * s), layer, keyboard, origin), build)?;
    Ok(OpenedPanel {
        handle,
        #[cfg(windows)]
        shown: None,
    })
}

/// Opens a notice `height` design px tall in the top corner away from `side`.
pub fn open_notice<V: Render>(
    cx: &mut App,
    display: Display,
    side: Side,
    layer: bool,
    height: f32,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> anyhow::Result<WindowHandle<V>> {
    let s = display.scale();
    let size = size(px(NOTICE_W * s), px(height * s));
    // Away from the panel's side, so both can show.
    let corner = match side {
        Side::Left => Side::Right,
        Side::Right => Side::Left,
    };
    #[cfg(windows)]
    if layer {
        return open_native_notice(cx, display, "overlay-notice", size, corner, false, 24. * s, build);
    }
    #[cfg(target_os = "linux")]
    let origin = if layer { display.x11_origin(size, corner, false, px(24. * s)) } else { None };
    #[cfg(not(target_os = "linux"))]
    let origin = None;
    cx.open_window(options("overlay-notice", size, corner, false, px(24. * s), layer, false, origin), build)
}

/// Opens a notice `size` design px in the bottom right corner (the one
/// asking to enter console mode: `console::summon`).
pub fn open_corner_notice<V: Render>(
    cx: &mut App,
    display: Display,
    layer: bool,
    size: (f32, f32),
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> anyhow::Result<WindowHandle<V>> {
    let s = display.scale();
    let px_size = gpui_kit::size(px(size.0 * s), px(size.1 * s));
    #[cfg(windows)]
    if layer {
        return open_native_notice(cx, display, "console-notice", px_size, Side::Right, true, 24. * s, build);
    }
    #[cfg(target_os = "linux")]
    let origin = if layer { display.x11_origin(px_size, Side::Right, true, px(24. * s)) } else { None };
    #[cfg(not(target_os = "linux"))]
    let origin = None;
    cx.open_window(options("console-notice", px_size, Side::Right, true, px(24. * s), layer, false, origin), build)
}

/// A notice in a Windows topmost window, placed once it shows (closed if it
/// cannot be).
#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn open_native_notice<V: Render>(
    cx: &mut App,
    display: Display,
    namespace: &str,
    size: Size<Pixels>,
    side: Side,
    bottom: bool,
    margin: f32,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> anyhow::Result<WindowHandle<V>> {
    let (handle, shown) = windows::open(cx, display.screen, namespace, size, side, bottom, margin, build)?;
    cx.spawn(async move |cx| {
        if let Err(error) = shown.await {
            log::warn!("overlay: cannot show the native notice: {error:#}");
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    })
    .detach();
    Ok(handle)
}

/// The compositor refused a layer-shell surface (GNOME's has none).
#[cfg(target_os = "linux")]
pub fn is_layer_shell_missing(e: &anyhow::Error) -> bool {
    e.downcast_ref::<LayerShellNotSupportedError>().is_some()
}

/// No layer-shell surface is asked for off Linux.
#[cfg(not(target_os = "linux"))]
pub fn is_layer_shell_missing(_e: &anyhow::Error) -> bool {
    false
}

/// Windows has native topmost windows; Linux draws layer-shell surfaces on
/// Wayland (desktops without it, GNOME, are found out when one is asked for)
/// and override-redirect windows on X11.
pub fn supported() -> bool {
    cfg!(windows) || cfg!(target_os = "linux")
}
