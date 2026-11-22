//! Settings → Saves, the account card under the backup options: whether the
//! server answers and how much room is left, the secure key (the account has
//! no password: the key is it), and the game list.

use gpui_kit::component::input::InputState;
use gpui_kit::prelude::*;
use gpui_kit::*;
use heronite_data::maintenance::human_size;
use heronite_emulation::saves::account;
use heronite_i18n::tr;
use heronite_saves::manifest::{self, Updated};
use heronite_saves::SecureKey;

use super::SettingsPage;
use crate::theme;
use crate::ui::controls::{action_button, caption, card_box, help_text, ButtonKind};
use crate::ui::fields::{input_box, meta_text, new_input};

/// What the server said about the account.
#[derive(Clone)]
struct Info {
    server: String,
    tier: String,
    used: u64,
    capacity: u64,
}

/// What the key area shows.
enum Key {
    Idle,
    /// Pasting another key: the field.
    Importing(Entity<InputState>),
    /// Asked to make a new key.
    ConfirmNew,
}

pub struct State {
    /// The last check of the server (`Err` = what went wrong).
    info: Option<Result<Info, String>>,
    checking: bool,
    key: Key,
    /// What the last key action did (`true` = a problem).
    notice: Option<(String, bool)>,
    manifest_busy: bool,
    manifest: Option<Result<String, String>>,
}

impl Default for State {
    fn default() -> Self {
        Self { info: None, checking: false, key: Key::Idle, notice: None, manifest_busy: false, manifest: None }
    }
}

/// The server changed: what was known about the old one no longer holds.
pub(super) fn server_changed(d: &mut SettingsPage, cx: &mut Context<SettingsPage>) {
    d.saves_restore.info = None;
    d.saves_restore.notice = None;
    cx.notify();
}

/// The backups were just turned on: the key is made (once) and its copy is
/// asked for.
pub(super) fn key_ready(d: &mut SettingsPage, cx: &mut Context<SettingsPage>) {
    let existed = matches!(account::load_key(&d.ctx.data_paths), Ok(Some(_)));
    match account::ensure_key(&d.ctx.data_paths) {
        Ok(_) if !existed => {
            d.saves_restore.notice = Some((
                tr!("Your secure key was made. It is your account: copy it and keep it somewhere safe. Without it your saves cannot be brought back on another PC.")
                    .to_string(),
                false,
            ));
        }
        Ok(_) => {}
        Err(e) => d.saves_restore.notice = Some((e, true)),
    }
    cx.notify();
}

fn check(d: &mut SettingsPage, cx: &mut Context<SettingsPage>) {
    if d.saves_restore.checking {
        return;
    }
    d.saves_restore.checking = true;
    d.saves_restore.info = None;
    let paths = d.ctx.data_paths.clone();
    cx.notify();
    cx.spawn(async move |this, cx| {
        let result = cx
            .background_spawn(async move {
                account::connect(&paths)
                    .map(|c| Info {
                        server: c.server.trim_start_matches("https://").to_string(),
                        tier: c.me.tier.clone(),
                        used: c.me.held(),
                        capacity: c.me.capacity(),
                    })
                    .map_err(|e| e.to_string())
            })
            .await;
        this.update(cx, |d, cx| {
            d.saves_restore.checking = false;
            d.saves_restore.info = Some(result);
            cx.notify();
        })
        .ok();
    })
    .detach();
}

fn copy_key(d: &mut SettingsPage, cx: &mut Context<SettingsPage>) {
    match account::ensure_key(&d.ctx.data_paths) {
        Ok(key) => {
            cx.write_to_clipboard(ClipboardItem::new_string(key.export()));
            d.saves_restore.notice = Some((tr!("The secure key is on the clipboard. Paste it somewhere safe.").to_string(), false));
        }
        Err(e) => d.saves_restore.notice = Some((e, true)),
    }
    cx.notify();
}

fn start_import(d: &mut SettingsPage, window: &mut Window, cx: &mut Context<SettingsPage>) {
    let input = new_input(window, cx, "", tr!("Paste the secure key"));
    input.update(cx, |s, cx| s.focus(window, cx));
    d.saves_restore.key = Key::Importing(input);
    d.saves_restore.notice = None;
    cx.notify();
}

fn apply_import(d: &mut SettingsPage, text: &str, cx: &mut Context<SettingsPage>) {
    d.saves_restore.notice =
        Some(match SecureKey::parse(text).map_err(|e| e.to_string()).and_then(|k| account::replace_key(&d.ctx.data_paths, &k)) {
            Ok(()) => {
                d.saves_restore.key = Key::Idle;
                d.saves_restore.info = None;
                (
                    tr!("This PC now uses that key. The key it used before is kept in the data folder as saves-key.previous.json.")
                        .to_string(),
                    false,
                )
            }
            Err(e) => (e, true),
        });
    cx.notify();
}

fn make_new_key(d: &mut SettingsPage, cx: &mut Context<SettingsPage>) {
    d.saves_restore.key = Key::Idle;
    d.saves_restore.info = None;
    d.saves_restore.notice = Some(match account::regenerate_key(&d.ctx.data_paths) {
        Ok(_) => (
            tr!("A new key was made, with a new empty account. The key it used before is kept in the data folder as saves-key.previous.json.").to_string(),
            false,
        ),
        Err(e) => (e, true),
    });
    cx.notify();
}

fn update_game_list(d: &mut SettingsPage, cx: &mut Context<SettingsPage>) {
    if d.saves_restore.manifest_busy {
        return;
    }
    d.saves_restore.manifest_busy = true;
    d.saves_restore.manifest = None;
    let folder = manifest::dir(&d.ctx.data_paths.root);
    cx.notify();
    cx.spawn(async move |this, cx| {
        let result = cx
            .background_spawn(async move {
                manifest::update(&folder, manifest::MANIFEST_URL)
                    .map(|u| match u {
                        Updated::Downloaded(n) => {
                            heronite_i18n::trn!(n, "The game list was updated: {n} game.", "The game list was updated: {n} games.")
                        }
                        Updated::UpToDate => tr!("The game list is up to date.").to_string(),
                    })
                    .map_err(|e| e.to_string())
            })
            .await;
        this.update(cx, |d, cx| {
            d.saves_restore.manifest_busy = false;
            d.saves_restore.manifest = Some(result);
            cx.notify();
        })
        .ok();
    })
    .detach();
}

fn status_line(d: &SettingsPage) -> (String, bool) {
    let st = &d.saves_restore;
    match &st.info {
        _ if st.checking => (tr!("Contacting the saves server…").to_string(), false),
        Some(Ok(i)) => {
            let tier = if i.tier.is_empty() { String::new() } else { format!(" ({})", i.tier) };
            (tr!("Connected to {server}{tier}.", server = i.server.clone(), tier), false)
        }
        Some(Err(e)) => (e.clone(), true),
        None => (tr!("Not checked yet: {server}", server = super::saves::server_label(d)), false),
    }
}

fn fraction(i: &Info) -> f32 {
    if i.capacity == 0 {
        0.
    } else {
        (i.used as f32 / i.capacity as f32).clamp(0., 1.)
    }
}

fn usage_text(i: &Info) -> String {
    if i.capacity == 0 {
        return tr!("{used} used", used = human_size(i.used)).to_string();
    }
    tr!("{used} of {limit} used", used = human_size(i.used), limit = human_size(i.capacity)).to_string()
}

fn key_line(d: &SettingsPage) -> String {
    match account::load_key(&d.ctx.data_paths) {
        Ok(Some(k)) => tr!("Secure key: {id}", id = k.fingerprint()).to_string(),
        Ok(None) => tr!("No secure key yet. It is made when you turn on the backups.").to_string(),
        Err(e) => e,
    }
}

fn manifest_line(d: &SettingsPage) -> Option<(String, bool)> {
    let st = &d.saves_restore;
    match &st.manifest {
        _ if st.manifest_busy => Some((tr!("Updating the game list…").to_string(), false)),
        Some(Ok(text)) => Some((text.clone(), false)),
        Some(Err(e)) => Some((e.clone(), true)),
        None => None,
    }
}

pub fn render(d: &mut SettingsPage, _window: &mut Window, cx: &mut Context<SettingsPage>) -> AnyElement {
    let (status, failed) = status_line(d);
    let st = &d.saves_restore;
    let busy = st.checking;
    let usage = st.info.as_ref().and_then(|r| r.as_ref().ok()).cloned().map(|i| {
        div().flex().flex_col().gap(px(6.)).child(div().text_size(px(13.)).text_color(theme::text_primary()).child(usage_text(&i))).child(
            div()
                .h(px(6.))
                .w_full()
                .rounded_full()
                .bg(theme::ink_a(0.1))
                .child(div().h_full().w(relative(fraction(&i))).rounded_full().bg(theme::accent())),
        )
    });
    let account_card = card_box()
        .py(px(12.))
        .gap(px(10.))
        .child(
            div()
                .text_size(px(13.))
                .line_height(px(18.))
                .text_color(if failed { theme::danger_text() } else { theme::text_primary() })
                .child(status),
        )
        .children(usage)
        .child(div().flex().child(action_button(
            "saves-check",
            ButtonKind::Glass,
            if busy { tr!("Checking…") } else { tr!("Check connection") },
            !busy,
            cx.listener(|d, _, _, cx| check(d, cx)),
        )));

    let key_area: AnyElement = match &st.key {
        Key::Idle => div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(action_button("saves-key-copy", ButtonKind::Primary, tr!("Copy secure key"), true, cx.listener(|d, _, _, cx| copy_key(d, cx))))
            .child(action_button(
                "saves-key-use",
                ButtonKind::Glass,
                tr!("Use another key…"),
                true,
                cx.listener(|d, _, window, cx| start_import(d, window, cx)),
            ))
            .child(action_button(
                "saves-key-new",
                ButtonKind::Ghost,
                tr!("Make a new key…"),
                true,
                cx.listener(|d, _, _, cx| {
                    d.saves_restore.key = Key::ConfirmNew;
                    d.saves_restore.notice = None;
                    cx.notify();
                }),
            ))
            .into_any_element(),
        Key::Importing(input) => {
            let input = input.clone();
            let read = input.clone();
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(input_box(&input, false))
                .child(help_text(tr!(
                    "This PC will use that key and reach the saves of its account. The key it uses now is kept in the data folder as saves-key.previous.json."
                )))
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(action_button(
                            "saves-key-import",
                            ButtonKind::Primary,
                            tr!("Use this key"),
                            true,
                            cx.listener(move |d, _, _, cx| {
                                let text = read.read(cx).value().to_string();
                                apply_import(d, &text, cx);
                            }),
                        ))
                        .child(action_button(
                            "saves-key-import-cancel",
                            ButtonKind::Ghost,
                            tr!("Cancel"),
                            true,
                            cx.listener(|d, _, _, cx| {
                                d.saves_restore.key = Key::Idle;
                                cx.notify();
                            }),
                        )),
                )
                .into_any_element()
        }
        Key::ConfirmNew => div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(div().text_size(px(13.)).line_height(px(18.)).text_color(theme::danger_text()).child(tr!(
                "A new key is a new, empty account. The saves of the current key stay on the server, and only the old key reaches them again. Make a new key?"
            )))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(action_button("saves-key-new-yes", ButtonKind::Danger, tr!("Make a new key"), true, cx.listener(|d, _, _, cx| make_new_key(d, cx))))
                    .child(action_button(
                        "saves-key-new-no",
                        ButtonKind::Ghost,
                        tr!("Cancel"),
                        true,
                        cx.listener(|d, _, _, cx| {
                            d.saves_restore.key = Key::Idle;
                            cx.notify();
                        }),
                    )),
            )
            .into_any_element(),
    };
    let notice = st.notice.clone();
    let key_card = card_box()
        .py(px(12.))
        .gap(px(10.))
        .child(div().text_size(px(13.)).text_color(theme::text_primary()).child(key_line(d)))
        .child(help_text(tr!(
            "The secure key is your account: there is no password and no email. Anyone who has it can read and replace your saves, and nobody can bring it back for you. Keep a copy somewhere safe."
        )))
        .child(key_area)
        .children(notice.map(|(text, error)| {
            div().text_size(px(13.)).line_height(px(18.)).text_color(if error { theme::danger_text() } else { theme::success() }).child(text)
        }));

    let manifest = div()
        .flex()
        .items_center()
        .gap(px(12.))
        .mt(px(12.))
        .child(action_button(
            "saves-manifest-update",
            ButtonKind::Glass,
            tr!("Update game list"),
            !d.saves_restore.manifest_busy,
            cx.listener(|d, _, _, cx| update_game_list(d, cx)),
        ))
        .children(manifest_line(d).map(|(text, error)| {
            div().text_size(px(13.)).text_color(if error { theme::danger_text() } else { theme::text_muted() }).child(text)
        }));

    div()
        .flex()
        .flex_col()
        .child(caption(tr!("Account")))
        .child(account_card)
        .child(caption(tr!("Secure key")))
        .child(key_card)
        .child(caption(tr!("Game list")))
        .child(div().px(px(6.)).pb(px(8.)).child(meta_text(tr!(
            "Where each game keeps its saves comes from a community list (Ludusavi's), downloaded on its own once a day. Update it now when a new game is not found."
        ))))
        .child(manifest)
        .into_any_element()
}

/// Console mode rows, after the backup options.
pub(super) fn console_rows(d: &SettingsPage) -> Vec<crate::console::list::Row> {
    use crate::console::list::Row;
    use crate::icons::Icon;
    let st = &d.saves_restore;
    let (status, failed) = status_line(d);
    let usage = st.info.as_ref().and_then(|r| r.as_ref().ok()).map(usage_text).unwrap_or_default();
    let status_row = Row::info("ac-status", status, usage);
    let mut rows = vec![
        Row::header(tr!("Account")),
        if failed { status_row.danger() } else { status_row },
        Row::action("ac-check", if st.checking { tr!("Checking…") } else { tr!("Check connection") })
            .icon(Icon::Refresh)
            .enabled(!st.checking),
        Row::info("ac-key", key_line(d), tr!("The secure key is your account. Keep a copy somewhere safe.")),
        Row::action("ac-copy", tr!("Copy secure key")).icon(Icon::ContentCopy),
    ];
    if let Some((text, error)) = &st.notice {
        let r = Row::info("ac-notice", text.clone(), "");
        rows.push(if *error { r.danger() } else { r });
    }
    rows.push(Row::header(tr!("Game list")));
    let desc = match manifest_line(d) {
        Some((text, _)) => text,
        None => tr!("Updates by itself once a day").to_string(),
    };
    rows.push(Row::action("ac-manifest", tr!("Update game list")).icon(Icon::Refresh).enabled(!st.manifest_busy).desc(desc));
    rows
}

/// Handles a console row of [`console_rows`]; `false` when it is not one.
pub(super) fn console_apply(
    d: &mut SettingsPage,
    outcome: &crate::console::list::Outcome,
    _window: &mut Window,
    cx: &mut Context<SettingsPage>,
) -> bool {
    use crate::console::list::Outcome;
    match outcome {
        Outcome::Activate(id) if id.as_ref() == "ac-check" => check(d, cx),
        Outcome::Activate(id) if id.as_ref() == "ac-copy" => copy_key(d, cx),
        Outcome::Activate(id) if id.as_ref() == "ac-manifest" => update_game_list(d, cx),
        _ => return false,
    }
    true
}
