//! Settings → Drop: the Drop OSS servers this Collection uses. Several
//! servers can be connected at once. Each signs in with Drop's code flow;
//! the client certificate stays sealed on this computer.

use gpui_kit::component::input::InputState;
use gpui_kit::prelude::*;
use gpui_kit::*;
use heronite_drop::Server;
use heronite_i18n::tr;

use super::SettingsPage;
use crate::console::list::{Outcome, Row, Value};
use crate::icons::Icon;
use crate::services::drop::{Drop, DropEvent};
use crate::ui::controls::{action_button, button_sm, caption, card, card_box, row, toggle, ButtonKind};
use crate::ui::fields::{input_box, labeled, meta_text};

pub struct State {
    svc: Entity<Drop>,
    name: Entity<InputState>,
    url: Entity<InputState>,
    /// Server id being edited; `None` is a new server.
    editing: Option<String>,
    /// Server id whose Remove is waiting for confirmation.
    confirm_remove: Option<String>,
}

impl State {
    pub fn new(window: &mut Window, cx: &mut Context<SettingsPage>) -> Self {
        let svc = crate::services::drop(cx);
        cx.observe(&svc, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&svc, |_d: &mut SettingsPage, _, ev: &DropEvent, cx| {
            if matches!(ev, DropEvent::LibraryChanged) {
                cx.emit(crate::pages::PageEvent::LibraryChanged);
            }
            cx.notify();
        })
        .detach();
        Self {
            svc,
            name: cx.new(|cx| InputState::new(window, cx)),
            url: cx.new(|cx| InputState::new(window, cx)),
            editing: None,
            confirm_remove: None,
        }
    }

    pub(super) fn dismiss(&mut self) -> bool {
        self.confirm_remove.take().is_some()
    }
}

pub fn shown(cx: &mut Context<SettingsPage>) {
    crate::services::drop(cx).update(cx, |s, cx| {
        let ids: Vec<String> = s.servers.servers.iter().filter(|s| s.enabled).map(|s| s.id.clone()).collect();
        for id in ids {
            if !s.states.get(&id).is_some_and(|st| st.games > 0 || st.syncing || st.testing) {
                s.test(&id, true, cx);
            }
        }
    });
}

fn clear_form(d: &mut SettingsPage, window: &mut Window, cx: &mut Context<SettingsPage>) {
    for input in [&d.drop.name, &d.drop.url] {
        input.update(cx, |s, cx| s.set_value("", window, cx));
    }
    d.drop.editing = None;
}

fn save_form(d: &mut SettingsPage, window: &mut Window, cx: &mut Context<SettingsPage>) {
    let st = &d.drop;
    let name = st.name.read(cx).value().trim().to_string();
    let url = st.url.read(cx).value().trim().to_string();
    let svc = st.svc.clone();
    let (id, enabled) = svc
        .read(cx)
        .servers
        .get(st.editing.as_deref().unwrap_or(""))
        .map(|s| (s.id.clone(), s.enabled))
        .unwrap_or_else(|| (svc.read(cx).servers.new_id(), true));
    if svc.update(cx, |s, cx| s.upsert(id, name, url, enabled, cx)) {
        clear_form(d, window, cx);
    }
}

fn edit(d: &mut SettingsPage, server: Server, window: &mut Window, cx: &mut Context<SettingsPage>) {
    d.drop.editing = Some(server.id.clone());
    d.drop.name.update(cx, |s, cx| s.set_value(server.name, window, cx));
    d.drop.url.update(cx, |s, cx| s.set_value(server.url, window, cx));
    cx.notify();
}

fn form_card(d: &SettingsPage, cx: &mut Context<SettingsPage>) -> Div {
    let editing = d.drop.editing.is_some();
    let can_save = !d.drop.url.read(cx).value().trim().is_empty();
    card_box()
        .py(px(12.))
        .gap(px(12.))
        .child(meta_text(if editing {
            tr!("Change the server, then save it. Its sign-in is kept.")
        } else {
            tr!("Add a Drop server, then sign in with the short code it gives you. The client certificate stays sealed on this computer.")
        }))
        .child(labeled(tr!("Name"), input_box(&d.drop.name, false)))
        .child(labeled(tr!("Server address"), input_box(&d.drop.url, false)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(action_button(
                    "dr-save",
                    ButtonKind::Primary,
                    if editing { tr!("Save server") } else { tr!("Add server") },
                    can_save,
                    cx.listener(|d, _, window, cx| save_form(d, window, cx)),
                ))
                .children(editing.then(|| {
                    action_button(
                        "dr-cancel",
                        ButtonKind::Glass,
                        tr!("Cancel"),
                        true,
                        cx.listener(|d, _, window, cx| clear_form(d, window, cx)),
                    )
                })),
        )
}

fn server_status(svc: &Drop, server: &Server) -> (String, bool) {
    let st = svc.states.get(&server.id).cloned().unwrap_or_default();
    if let Some(code) = svc.sign_in_codes.get(&server.id) {
        return (tr!("Sign-in code: {code}. Approve it in the Drop web interface", code = code).into(), true);
    }
    if st.testing {
        return (tr!("Checking connection…").into(), true);
    }
    if st.syncing {
        return (tr!("Syncing games…").into(), true);
    }
    if !st.error.is_empty() {
        return (st.error, false);
    }
    let mut parts = Vec::new();
    if st.games > 0 {
        parts.push(tr!("{count} games", count = st.games));
    }
    if !st.version.is_empty() {
        parts.push(tr!("Signed in as {user}", user = st.version));
    }
    if !server.last_sync.is_empty() {
        parts.push(tr!("Synced").into());
    }
    if parts.is_empty() {
        (tr!("Not signed in").into(), true)
    } else {
        (parts.join("  ·  "), true)
    }
}

fn server_card(d: &SettingsPage, server: Server, cx: &mut Context<SettingsPage>) -> AnyElement {
    let svc = d.drop.svc.read(cx);
    let (status, _) = server_status(svc, &server);
    let id = server.id.clone();
    let enabled = server.enabled;
    let busy = svc.states.get(&id).is_some_and(|s| s.syncing || s.testing);
    let confirming = d.drop.confirm_remove.as_deref() == Some(id.as_str());
    let mut buttons = div().flex().items_center().gap(px(8.));
    let toggle_id = id.clone();
    buttons = buttons.child(toggle(SharedString::from(format!("dr-on-{id}")), enabled, false).cursor_pointer().on_click(cx.listener(
        move |d, _, _, cx| {
            let svc = d.drop.svc.clone();
            let mut next = svc.read(cx).servers.get(&toggle_id).cloned().unwrap();
            next.enabled = !enabled;
            svc.update(cx, |s, cx| s.upsert(next.id.clone(), next.name.clone(), next.url.clone(), next.enabled, cx));
            if next.enabled {
                svc.update(cx, |s, cx| s.test(&next.id, true, cx));
            }
        },
    )));
    let signed = svc.servers.signed_in(&id);
    buttons = buttons.child(
        button_sm(
            SharedString::from(format!("dr-sign-{id}")),
            ButtonKind::Glass,
            if signed { tr!("Sign in again") } else { tr!("Sign in") },
        )
        .cursor_pointer()
        .on_click(cx.listener({
            let id = id.clone();
            move |d, _, _, cx| {
                let svc = d.drop.svc.clone();
                svc.update(cx, |s, cx| s.sign_in(&id, cx));
            }
        })),
    );
    buttons =
        buttons.child(
            button_sm(SharedString::from(format!("dr-sync-{id}")), ButtonKind::Glass, if busy { tr!("Working…") } else { tr!("Sync") })
                .when(!busy, |b| {
                    let id = id.clone();
                    b.cursor_pointer().on_click(cx.listener(move |d, _, _, cx| {
                        let svc = d.drop.svc.clone();
                        svc.update(cx, |s, cx| s.sync(&id, cx));
                    }))
                }),
        );
    buttons = buttons.child(
        button_sm(SharedString::from(format!("dr-edit-{id}")), ButtonKind::Glass, tr!("Edit")).cursor_pointer().on_click(cx.listener({
            let server = server.clone();
            move |d, _, window, cx| edit(d, server.clone(), window, cx)
        })),
    );
    buttons = buttons.child(
        button_sm(
            SharedString::from(format!("dr-remove-{id}")),
            ButtonKind::Glass,
            if confirming { tr!("Remove?") } else { tr!("Remove") },
        )
        .cursor_pointer()
        .on_click(cx.listener({
            let id = id.clone();
            move |d, _, _, cx| {
                if d.drop.confirm_remove.as_deref() == Some(id.as_str()) {
                    let svc = d.drop.svc.clone();
                    svc.update(cx, |s, cx| s.remove(&id, cx));
                    d.drop.confirm_remove = None;
                } else {
                    d.drop.confirm_remove = Some(id.clone());
                }
                cx.notify();
            }
        })),
    );
    let label = if server.enabled { server.label() } else { format!("{} · {}", server.label(), tr!("Off")) };
    row(label, Some(&format!("{}  ·  {}", server.url, status)), buttons).into_any_element()
}

pub fn render(d: &mut SettingsPage, _window: &mut Window, cx: &mut Context<SettingsPage>) -> AnyElement {
    let servers: Vec<Server> = d.drop.svc.read(cx).servers.servers.clone();
    let mut out = div().flex().flex_col().gap(px(12.)).max_w(px(760.));
    out = out.child(meta_text(tr!(
        "Connect one or more Drop servers. Their games show in your Collection and can be downloaded, installed, updated and started here."
    )));
    if !servers.is_empty() {
        out = out.child(caption(tr!("Servers"))).child(card(servers.into_iter().map(|s| server_card(d, s, cx))));
    }
    out.child(caption(if d.drop.editing.is_some() { tr!("Edit server") } else { tr!("Add a server") }))
        .child(form_card(d, cx))
        .into_any_element()
}

pub(super) fn console_rows(d: &SettingsPage, cx: &App) -> Vec<Row> {
    let svc = d.drop.svc.read(cx);
    let mut rows = vec![Row::header("Drop")];
    for s in &svc.servers.servers {
        let (status, _) = server_status(svc, s);
        rows.push(Row::info(format!("server:{}", s.id), s.label(), format!("{} · {}", s.url, status)));
        rows.push(Row::action(format!("sync:{}", s.id), tr!("Sync {server}", server = s.label())).icon(Icon::Refresh));
        rows.push(Row::toggle(format!("enabled:{}", s.id), tr!("Use {server}", server = s.label()), s.enabled));
    }
    rows.push(Row::action("add", tr!("Add a server")).desc(tr!("Use the desktop settings to enter the server address and sign in")));
    rows
}

pub(super) fn console_apply(d: &mut SettingsPage, outcome: Outcome, cx: &mut Context<SettingsPage>) {
    match outcome {
        Outcome::Activate(id) if id.as_ref().starts_with("sync:") => {
            let id = id.trim_start_matches("sync:").to_string();
            d.drop.svc.clone().update(cx, |s, cx| s.sync(&id, cx));
        }
        Outcome::Changed(id, Value::Toggle(on)) if id.as_ref().starts_with("enabled:") => {
            let sid = id.trim_start_matches("enabled:").to_string();
            let svc = d.drop.svc.clone();
            if let Some(server) = svc.read(cx).servers.get(&sid).cloned() {
                svc.update(cx, |s, cx| s.upsert(server.id, server.name, server.url, on, cx));
            }
        }
        _ => {}
    }
}
