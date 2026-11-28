//! A global shortcut grabbed on the X server itself (XGrabKey), for the
//! window managers that give apps nothing else: i3, bspwm, dwm and the
//! other bare X11 desktops have no GlobalShortcuts portal, and their users
//! bind keys in sxhkd or the manager's own config. Tried before the portal,
//! which a bare X11 session rarely answers.
//!
//! The grab covers every lock state (Caps Lock, Num Lock, Scroll Lock): X11
//! matches modifiers exactly, so one grab per combination. A press reaches
//! the reader thread as a KeyPress on the root window; the game never sees
//! the key (the grab takes it).
//!
//! Only the pure pieces are unit-tested (no X server): the modifier masks,
//! the lock combinations, the keysym table, the session detection.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use heronite_i18n::tr;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self, ConnectionExt, GrabMode, KeyPressEvent};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use super::accelerator::parse_accelerator;

// X11 modifier masks (`ShiftMask` &c of X.h).
pub const SHIFT: u16 = 1;
pub const LOCK: u16 = 2;
pub const CONTROL: u16 = 4;
pub const MOD1: u16 = 8;
pub const MOD2: u16 = 16;
pub const MOD3: u16 = 32;
pub const MOD4: u16 = 64;

/// The accelerator as the server wants it: the modifier mask and the keysym
/// of the key. `None` for a modifier or key X11 has no name for here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct X11Trigger {
    pub modifiers: u16,
    pub keysym: u32,
}

/// The X11 modifier mask of an accelerator's modifiers; `None` for one X11
/// does not know (Shift is bit 0, Ctrl 2, Alt = Mod1, Super = Mod4).
pub fn modifier_mask(accelerator: &str) -> Option<u16> {
    let acc = parse_accelerator(accelerator)?;
    let mut mask = 0;
    for m in &acc.modifiers {
        mask |= match m.to_ascii_lowercase().as_str() {
            "commandorcontrol" | "control" | "ctrl" => CONTROL,
            "command" | "super" | "meta" => MOD4,
            "alt" | "option" => MOD1,
            "shift" => SHIFT,
            _ => return None,
        };
    }
    Some(mask)
}

/// The keysym of the key of an accelerator, by its XKB name (what
/// `to_portal_trigger` and `keysym_name` yield). Letters and digits are
/// their ASCII codes, F1..F24 start at 0xFFBE, the named keys are the
/// usual set; anything else is `None`.
pub fn keysym_value(name: &str) -> Option<u32> {
    let b = name.as_bytes();
    if b.len() == 1 && (b[0].is_ascii_alphanumeric()) {
        return Some(u32::from(b[0].to_ascii_lowercase()));
    }
    if b.len() >= 2 && (b[0] == b'F') && b[1..].iter().all(u8::is_ascii_digit) {
        let n: u32 = name[1..].parse().ok()?;
        return (1..=24).contains(&n).then(|| 0xFFBE + n - 1);
    }
    // KP_0..KP_9 of the keypad.
    if let Some(d) = name.strip_prefix("KP_").filter(|d| d.len() == 1 && d.as_bytes()[0].is_ascii_digit()) {
        return Some(0xFFB0 + u32::from(d.as_bytes()[0] - b'0'));
    }
    Some(match name {
        "space" => 0x20,
        "plus" => 0x2B,
        "minus" => 0x2D,
        "Escape" => 0xFF1B,
        "Return" => 0xFF0D,
        "Tab" => 0xFF09,
        "BackSpace" => 0xFF08,
        "Delete" => 0xFFFF,
        "Insert" => 0xFF63,
        "Home" => 0xFF50,
        "End" => 0xFF57,
        "Page_Up" => 0xFF55,
        "Page_Down" => 0xFF56,
        "Left" => 0xFF51,
        "Up" => 0xFF52,
        "Right" => 0xFF53,
        "Down" => 0xFF54,
        "Print" => 0xFF61,
        "Pause" => 0xFF13,
        "Caps_Lock" => 0xFFE5,
        "Num_Lock" => 0xFF7F,
        "Scroll_Lock" => 0xFF14,
        "Menu" => 0xFF67,
        "KP_Add" => 0xFFAB,
        "KP_Subtract" => 0xFFAD,
        "KP_Multiply" => 0xFFAA,
        "KP_Divide" => 0xFFAF,
        "KP_Decimal" => 0xFFAE,
        _ => return None,
    })
}

/// The trigger of an accelerator, keysym name and all; `None` when the
/// shortcut cannot be grabbed.
pub fn to_x11_trigger(accelerator: &str) -> Option<X11Trigger> {
    let acc = parse_accelerator(accelerator)?;
    let modifiers = modifier_mask(accelerator)?;
    let keysym = keysym_value(&super::accelerator::keysym_name(&acc.key))?;
    Some(X11Trigger { modifiers, keysym })
}

/// The grabs one accelerator needs: X11 matches the modifiers exactly, so
/// the shortcut must be grabbed once per lock combination it should work
/// under. Num Lock is Mod2 and Caps Lock the Lock bit on every desktop;
/// Scroll Lock is Mod3 where the modifier mapping maps it (`scroll_lock`).
pub fn lock_combinations(base: u16, scroll_lock: Option<u16>) -> Vec<u16> {
    let mut extras = vec![LOCK, MOD2];
    if let Some(m) = scroll_lock {
        if !extras.contains(&m) && m != 0 {
            extras.push(m);
        }
    }
    let mut out = Vec::new();
    for bits in 0..(1u16 << extras.len()) {
        let mut mask = base;
        for (i, extra) in extras.iter().enumerate() {
            if bits & (1 << i) != 0 {
                mask |= extra;
            }
        }
        if !out.contains(&mask) {
            out.push(mask);
        }
    }
    out
}

/// Whether a session is a bare X11 one, from the environment's values:
/// DISPLAY answers and either it says x11 or Wayland is not there. (On an
/// XWayland desktop the portal or the compositor's own way is preferred.)
pub fn x11_session(display: Option<&str>, wayland_display: Option<&str>, session_type: Option<&str>) -> bool {
    if !display.is_some_and(|d| !d.is_empty()) {
        return false;
    }
    if session_type.is_some_and(|s| s.eq_ignore_ascii_case("x11")) {
        return true;
    }
    wayland_display.is_none_or(|w| w.is_empty())
}

/// This session's [`x11_session`]. Hyprland is never one (its binds win).
pub fn is_x11_session() -> bool {
    if super::hyprland::is_hyprland_session() {
        return false;
    }
    let var = |name| std::env::var(name).ok();
    x11_session(var("DISPLAY").as_deref(), var("WAYLAND_DISPLAY").as_deref(), var("XDG_SESSION_TYPE").as_deref())
}

/// The keycode of `keysym` in the server's keyboard mapping (`keysyms` of
/// GetKeyboardMapping, `per` per keycode starting at `first`).
pub fn keycode_of(keysyms: &[u32], per: usize, first: u8, keysym: u32) -> Option<u8> {
    for (i, group) in keysyms.chunks(per).enumerate() {
        if group.contains(&keysym) {
            return u8::try_from(usize::from(first) + i).ok();
        }
    }
    None
}

/// `true` when the modifier mapping (`keycodes` of GetModifierMapping, 8
/// per modifier) maps anything on Mod3: taken as Scroll Lock being real.
pub fn scroll_lock_mapped(keycodes: &[u8], per: usize) -> Option<u16> {
    let mod3 = keycodes.chunks(per).nth(3)?;
    (mod3.iter().any(|k| *k != 0)).then_some(MOD3)
}

struct Shared {
    stopped: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
}

/// The grabbed shortcut. Dropping it ungrabs the keys and stops the reader
/// thread, which it joins: drop it off the UI thread, as the siblings.
pub struct X11Grab {
    conn: Arc<RustConnection>,
    root: u32,
    grabs: Vec<(u16, u8)>,
    shared: Arc<Shared>,
}

impl X11Grab {
    /// Connects, resolves and grabs (blocking, so callers spawn it). On
    /// success the reader thread is already running; the error is
    /// user-readable.
    pub fn grab(trigger: X11Trigger, on_press: Arc<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        let (conn, screen) = RustConnection::connect(None)
            .map_err(|e| tr!("Could not connect to the X server: {error}", error = e.to_string()).to_string())?;
        let root = conn.setup().roots[screen].root;
        grab_on(conn, root, trigger, on_press)
    }

    fn wake(&self) {
        // A synthetic KeyPress of keycode 0, which no key has: the reader
        // wakes on it, sees `stopped` and ends. The grab delivery does not
        // need it selected, but the send does.
        let event = KeyPressEvent { detail: 0, event: self.root, root: self.root, ..Default::default() };
        let _ = xproto::send_event(&*self.conn, false, self.root, x11rb::protocol::xproto::EventMask::KEY_PRESS, event);
        let _ = self.conn.flush();
    }
}

fn grab_on(conn: RustConnection, root: u32, trigger: X11Trigger, on_press: Arc<dyn Fn() + Send + Sync>) -> Result<X11Grab, String> {
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let count = max - min + 1;
    let err = |e: String| tr!("Could not read the keyboard mapping: {error}", error = e).to_string();
    let mapping = xproto::get_keyboard_mapping(&conn, min, count)
        .map_err(|e| err(e.to_string()))
        .and_then(|c| c.reply().map_err(|e| err(e.to_string())))?;
    let keycode = keycode_of(&mapping.keysyms, mapping.keysyms_per_keycode as usize, min, trigger.keysym)
        .ok_or_else(|| tr!("The key has no keycode in the keyboard's layout").to_string())?;
    let scroll_lock = xproto::get_modifier_mapping(&conn)
        .ok()
        .and_then(|c| c.reply().ok())
        .and_then(|r| scroll_lock_mapped(&r.keycodes, r.keycodes_per_modifier() as usize));
    let masks = lock_combinations(trigger.modifiers, scroll_lock);
    // KeyPress events of the grabs come to the root window; selecting them
    // there also lets the wake-up event through. Another client may have
    // selected them already: a failure here is not fatal.
    let _ =
        xproto::change_window_attributes(&conn, root, &xproto::ChangeWindowAttributesAux::new().event_mask(xproto::EventMask::KEY_PRESS));
    let mut grabs: Vec<(u16, u8)> = Vec::new();
    for mask in &masks {
        let result = conn
            .grab_key(false, root, (*mask).into(), keycode, GrabMode::ASYNC, GrabMode::ASYNC)
            .map_err(x11rb::errors::ReplyError::ConnectionError)
            .and_then(|c| c.check());
        if let Err(e) = result {
            // Undo what was grabbed before reporting it.
            for (m, k) in &grabs {
                let _ = xproto::ungrab_key(&conn, *k, root, (*m).into());
            }
            let _ = conn.flush();
            let text = e.to_string();
            if text.contains("Access") {
                return Err(tr!(
                    "The keys are already taken by another program (sxhkd or the window manager?): bind it there to `heronite-rs` instead, or free them"
                )
                .to_string());
            }
            return Err(tr!("Could not grab the keys on the X server: {error}", error = text).to_string());
        }
        grabs.push((*mask, keycode));
    }
    let conn = Arc::new(conn);
    let shared = Arc::new(Shared { stopped: AtomicBool::new(false), thread: Mutex::new(None) });
    let reader_conn = conn.clone();
    let reader_shared = shared.clone();
    let thread = std::thread::Builder::new().name("heronite-x11-hotkey".into()).spawn(move || loop {
        if reader_shared.stopped.load(Ordering::SeqCst) {
            return;
        }
        match reader_conn.wait_for_event() {
            // Keycode 0 is the wake-up event `Drop` sends; no key has it.
            Ok(Event::KeyPress(ev)) if ev.detail != 0 => {
                if reader_shared.stopped.load(Ordering::SeqCst) {
                    return;
                }
                on_press();
            }
            Ok(_) if reader_shared.stopped.load(Ordering::SeqCst) => return,
            Ok(_) => {}
            Err(_) => return,
        }
    });
    match thread {
        Ok(t) => *shared.thread.lock().unwrap() = Some(t),
        Err(e) => {
            for (m, k) in &grabs {
                let _ = xproto::ungrab_key(&*conn, *k, root, (*m).into());
            }
            return Err(format!("could not start the reader thread: {e}"));
        }
    }
    Ok(X11Grab { conn, root, grabs, shared })
}

impl Drop for X11Grab {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        for (m, k) in &self.grabs {
            let _ = xproto::ungrab_key(&*self.conn, *k, self.root, (*m).into());
        }
        self.wake();
        if let Some(thread) = self.shared.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_detection() {
        assert!(x11_session(Some(":0"), None, None));
        assert!(x11_session(Some(":0"), Some(""), None));
        assert!(x11_session(Some(":0"), Some("wayland-0"), Some("x11")));
        assert!(!x11_session(Some(":0"), Some("wayland-0"), Some("wayland")));
        assert!(!x11_session(Some(":0"), Some("wayland-0"), None));
        assert!(!x11_session(None, None, None));
        assert!(!x11_session(Some(""), None, Some("x11")));
    }

    #[test]
    fn modifier_masks() {
        assert_eq!(modifier_mask("F10"), Some(0));
        assert_eq!(modifier_mask("Ctrl+A"), Some(CONTROL));
        assert_eq!(modifier_mask("CommandOrControl+Shift+F10"), Some(CONTROL | SHIFT));
        assert_eq!(modifier_mask("Super+G"), Some(MOD4));
        assert_eq!(modifier_mask("Alt+PrintScreen"), Some(MOD1));
        assert_eq!(modifier_mask("Control+Alt+Shift+Super+F1"), Some(CONTROL | MOD1 | SHIFT | MOD4));
        assert_eq!(modifier_mask("Hyper+F1"), None);
    }

    #[test]
    fn keysym_lookup() {
        assert_eq!(keysym_value("a"), Some(0x61));
        assert_eq!(keysym_value("5"), Some(0x35));
        assert_eq!(keysym_value("F1"), Some(0xFFBE));
        assert_eq!(keysym_value("F24"), Some(0xFFBE + 23));
        assert_eq!(keysym_value("Escape"), Some(0xFF1B));
        assert_eq!(keysym_value("Page_Up"), Some(0xFF55));
        assert_eq!(keysym_value("Return"), Some(0xFF0D));
        assert_eq!(keysym_value("space"), Some(0x20));
        assert_eq!(keysym_value("Print"), Some(0xFF61));
        assert_eq!(keysym_value("F25"), None);
        assert_eq!(keysym_value("Bogus"), None);
    }

    #[test]
    fn triggers() {
        assert_eq!(to_x11_trigger("Super+G"), Some(X11Trigger { modifiers: MOD4, keysym: u32::from(b'g') }));
        assert_eq!(to_x11_trigger("F12"), Some(X11Trigger { modifiers: 0, keysym: 0xFFBE + 11 }));
        assert_eq!(to_x11_trigger("Shift+Enter"), Some(X11Trigger { modifiers: SHIFT, keysym: 0xFF0D }));
        assert!(to_x11_trigger("Ctrl+?").is_none());
        assert!(to_x11_trigger("Hyper+F1").is_none());
    }

    #[test]
    fn lock_combinations_cover_every_subset() {
        let combos = lock_combinations(0, None);
        assert_eq!(combos.len(), 4);
        assert!(combos.contains(&0));
        assert!(combos.contains(&LOCK));
        assert!(combos.contains(&MOD2));
        assert!(combos.contains(&(LOCK | MOD2)));
        // The base modifiers survive in every combination.
        let combos = lock_combinations(CONTROL, Some(MOD3));
        assert_eq!(combos.len(), 8);
        assert!(combos.iter().all(|c| c & CONTROL != 0));
        assert!(combos.contains(&(CONTROL | LOCK | MOD2 | MOD3)));
        // No duplicates when Scroll Lock is already in the list.
        let combos = lock_combinations(0, Some(MOD2));
        assert_eq!(combos.len(), 4);
    }

    #[test]
    fn keycodes_from_a_mapping() {
        // keysyms per keycode: unshifted, shifted; keycode 8 is 'a', 9 'b'.
        let keysyms = [u32::from(b'a'), u32::from(b'A'), u32::from(b'b'), u32::from(b'B')];
        assert_eq!(keycode_of(&keysyms, 2, 8, u32::from(b'a')), Some(8));
        assert_eq!(keycode_of(&keysyms, 2, 8, u32::from(b'z')), None);
    }

    #[test]
    fn scroll_lock_detection() {
        // 8 modifiers, one keycode each: something on Mod3.
        let keycodes = [0, 0, 0, 77, 0, 0, 0, 0];
        assert_eq!(scroll_lock_mapped(&keycodes, 1), Some(MOD3));
        let none = [0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(scroll_lock_mapped(&none, 1), None);
        assert_eq!(scroll_lock_mapped(&[], 1), None);
    }
}
