//! Settings → Sharing: upload targets, naming and the history of shares.

mod form_render;
mod forms;
mod history;

use std::collections::HashMap;

use gpui_kit::component::input::InputState;
use gpui_kit::prelude::*;
use gpui_kit::*;
use heronite_data::maintenance::human_size;
use heronite_i18n::tr;
use heronite_share::{Kind, NameMode, Naming, Target};

use super::SettingsPage;
use crate::console::list::{Outcome, Row, Value};
use crate::icons::Icon;
use crate::services::share::{JobState, ShareService};
use crate::theme;
use crate::ui::controls::{button_sm, caption, card, row, tabs, toggle, ButtonKind};
use crate::ui::fields::{input_box, meta_text, on_commit};

use forms::FormControls;

#[derive(Clone, Debug, Default)]
struct TestState {
    testing: bool,
    /// Ok: the report lines joined. Err: the failure text.
    outcome: Option<Result<String, String>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum NamingChoice {
    #[default]
    Random,
    Original,
    Template,
}

pub struct State {
    svc: Entity<ShareService>,
    form: FormControls,
    naming_template: Entity<InputState>,
    naming_choice: NamingChoice,
    /// Target id being edited; `None` is a new target.
    editing: Option<String>,
    confirm_remove: Option<String>,
    confirm_delete: Option<String>,
    tests: HashMap<String, TestState>,
    /// Last form or settings write error.
    error: Option<String>,
}

impl State {
    pub fn new(window: &mut Window, cx: &mut Context<SettingsPage>) -> Self {
        let svc = crate::services::share(cx);
        cx.observe(&svc, |_, _, cx| cx.notify()).detach();
        let (choice, template) = match &svc.read(cx).targets().naming.mode {
            NameMode::Random => (NamingChoice::Random, String::new()),
            NameMode::Original => (NamingChoice::Original, String::new()),
            NameMode::Template(t) => (NamingChoice::Template, t.clone()),
        };
        let naming_template = cx.new(|cx| InputState::new(window, cx));
        naming_template.update(cx, |s, cx| s.set_value(template, window, cx));
        on_commit(&naming_template, window, cx, |d, value, _, cx| {
            if d.share.naming_choice == NamingChoice::Template {
                save_naming(d, NameMode::Template(value), cx);
            }
        });
        Self {
            svc,
            form: FormControls::new(window, cx),
            naming_template,
            naming_choice: choice,
            editing: None,
            confirm_remove: None,
            confirm_delete: None,
            tests: HashMap::new(),
            error: None,
        }
    }

    pub(super) fn dismiss(&mut self) -> bool {
        let remove = self.confirm_remove.take().is_some();
        let delete = self.confirm_delete.take().is_some();
        remove || delete
    }
}

pub fn shown(cx: &mut Context<SettingsPage>) {
    crate::services::share(cx).update(cx, |s, cx| {
        s.reload(cx);
        s.history(cx);
    });
}

pub(super) fn clear_form(d: &mut SettingsPage, window: &mut Window, cx: &mut Context<SettingsPage>) {
    let form = forms::FormState::default();
    d.share.form.fill(&form, window, cx);
    d.share.editing = None;
    d.share.error = None;
    cx.notify();
}

fn edit_target(d: &mut SettingsPage, target: Target, window: &mut Window, cx: &mut Context<SettingsPage>) {
    d.share.editing = Some(target.id.clone());
    let form = forms::target_to_form(&target);
    d.share.form.fill(&form, window, cx);
    d.share.error = None;
    cx.notify();
}

fn save_naming(d: &mut SettingsPage, mode: NameMode, cx: &mut Context<SettingsPage>) {
    let svc = d.share.svc.clone();
    if let Err(e) = svc.update(cx, |s, cx| s.set_naming(Naming { mode }, cx)) {
        d.share.error = Some(e.to_string());
    }
    cx.notify();
}

fn target_descriptor(target: &Target) -> String {
    match &target.kind {
        Kind::S3(s) => {
            let host = if s.endpoint.trim().is_empty() { "AWS".to_string() } else { s.endpoint.trim().to_string() };
            format!("{host}/{}", s.bucket)
        }
        Kind::Sftp(s) => format!("{}@{}", s.user, s.host),
        Kind::WebDav(w) => url_host(&w.url),
        Kind::Ftp(f) => f.host.clone(),
        Kind::Http(h) => url_host(&h.url),
    }
}

fn url_host(raw: &str) -> String {
    let after_scheme = raw.split_once("://").map(|(_, rest)| rest).unwrap_or(raw);
    after_scheme.split(['/', '?', '#']).next().unwrap_or(raw).to_string()
}

fn default_radio(id: SharedString, on: bool, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(18.))
        .rounded_full()
        .border_1()
        .border_color(if on { theme::accent().into() } else { theme::ink_a(0.35) })
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .on_click(on_click)
        .children(on.then(|| div().size(px(8.)).rounded_full().bg(theme::accent())))
}

fn test_target(d: &mut SettingsPage, target: Target, cx: &mut Context<SettingsPage>) {
    let id = target.id.clone();
    if d.share.tests.get(&id).is_some_and(|t| t.testing) {
        return;
    }
    d.share.tests.insert(id.clone(), TestState { testing: true, outcome: None });
    cx.notify();
    cx.spawn(async move |this, cx| {
        let outcome = cx
            .background_executor()
            .spawn(async move { heronite_share::check(&target).map(|report| report.steps.join("  ·  ")).map_err(|e| e.text()) })
            .await;
        if let Some(this) = this.upgrade() {
            this.update(cx, |d, cx| {
                d.share.tests.insert(id, TestState { testing: false, outcome: Some(outcome) });
                cx.notify();
            });
        }
    })
    .detach();
}

fn progress_for(service: &ShareService, target_id: &str) -> Option<String> {
    let mut sent = 0;
    let mut total = 0;
    let mut found = false;
    for job in service.jobs().iter().filter(|j| j.target_id == target_id && matches!(j.state, JobState::Queued | JobState::Uploading)) {
        found = true;
        sent += job.bytes;
        total += job.total;
    }
    found.then(|| tr!("Uploading: {sent} of {total}", sent = human_size(sent), total = human_size(total)))
}

fn target_row(d: &SettingsPage, target: Target, cx: &mut Context<SettingsPage>) -> AnyElement {
    let id = target.id.clone();
    let svc = d.share.svc.read(cx);
    let is_default = svc.targets().default.as_deref() == Some(id.as_str());
    let test = d.share.tests.get(&id).cloned().unwrap_or_default();
    let progress = progress_for(svc, &id);
    let confirming = d.share.confirm_remove.as_deref() == Some(id.as_str());
    let mut desc = format!("{}  ·  {}", target.kind.label(), target_descriptor(&target));
    if is_default {
        desc.push_str(&format!("  ·  {}", tr!("Default")));
    }
    if !target.enabled {
        desc.push_str(&format!("  ·  {}", tr!("Off")));
    }
    if let Some(progress) = progress {
        desc.push_str(&format!("  ·  {progress}"));
    }
    if test.testing {
        desc.push_str(&format!("  ·  {}", tr!("Checking connection…")));
    } else if let Some(Ok(ok)) = &test.outcome {
        if !ok.is_empty() {
            desc.push_str(&format!("  ·  {ok}"));
        }
    }
    let error = test.outcome.as_ref().and_then(|o| o.as_ref().err()).cloned();

    let enabled = target.enabled;
    let target_toggle = target.clone();
    let target_test = target.clone();
    let target_edit = target.clone();
    let id_remove = id.clone();
    let id_default = id.clone();
    let controls = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(default_radio(SharedString::from(format!("share-default-{id}")), is_default, {
            let id = id_default.clone();
            cx.listener(move |d, _, _, cx| {
                let svc = d.share.svc.clone();
                if let Err(e) = svc.update(cx, |s, cx| s.set_default(Some(id.clone()), cx)) {
                    d.share.error = Some(e.to_string());
                }
                cx.notify();
            })
        }))
        .child(toggle(SharedString::from(format!("share-enabled-{id}")), enabled, false).cursor_pointer().on_click(cx.listener(
            move |d, _, _, cx| {
                let mut next = target_toggle.clone();
                next.enabled = !enabled;
                let svc = d.share.svc.clone();
                if let Err(e) = svc.update(cx, |s, cx| s.upsert_target(next, cx)) {
                    d.share.error = Some(e.to_string());
                }
                cx.notify();
            },
        )))
        .child(
            button_sm(
                SharedString::from(format!("share-test-{id}")),
                ButtonKind::Glass,
                if test.testing { tr!("Testing…") } else { tr!("Test connection") },
            )
            .cursor_pointer()
            .on_click(cx.listener(move |d, _, _, cx| test_target(d, target_test.clone(), cx))),
        )
        .child(
            button_sm(SharedString::from(format!("share-edit-{id}")), ButtonKind::Glass, tr!("Edit"))
                .cursor_pointer()
                .on_click(cx.listener(move |d, _, window, cx| edit_target(d, target_edit.clone(), window, cx))),
        )
        .child(
            button_sm(
                SharedString::from(format!("share-remove-{id}")),
                if confirming { ButtonKind::Danger } else { ButtonKind::Glass },
                if confirming { tr!("Remove?") } else { tr!("Remove") },
            )
            .cursor_pointer()
            .on_click(cx.listener(move |d, _, _, cx| {
                if d.share.confirm_remove.as_deref() == Some(id_remove.as_str()) {
                    let svc = d.share.svc.clone();
                    if let Err(e) = svc.update(cx, |s, cx| s.remove_target(&id_remove, cx)) {
                        d.share.error = Some(e.to_string());
                    }
                    if d.share.editing.as_deref() == Some(id_remove.as_str()) {
                        d.share.editing = None;
                    }
                    d.share.confirm_remove = None;
                } else {
                    d.share.confirm_remove = Some(id_remove.clone());
                }
                cx.notify();
            })),
        );
    let label = if target.name.trim().is_empty() { target.kind.label().to_string() } else { target.label() };
    let mut out = div().flex().flex_col().child(row(label, Some(&desc), controls));
    if let Some(error) = error {
        out = out.child(div().px(px(4.)).pb(px(8.)).text_size(px(12.)).line_height(px(16.)).text_color(theme::danger_text()).child(error));
    }
    out.into_any_element()
}

fn targets_card(d: &SettingsPage, cx: &mut Context<SettingsPage>) -> AnyElement {
    let targets = d.share.svc.read(cx).targets().targets.clone();
    let mut out = div().flex().flex_col().child(caption(tr!("Targets")));
    if targets.is_empty() {
        out = out.child(meta_text(tr!("No targets yet. Add one below.")));
    } else {
        out = out.child(card(targets.into_iter().map(|target| target_row(d, target, cx))));
    }
    out.into_any_element()
}

fn default_picker(d: &SettingsPage, cx: &mut Context<SettingsPage>) -> Div {
    let svc = d.share.svc.read(cx);
    let current = svc.targets().default.clone();
    let mut controls = div().flex().flex_wrap().items_center().gap(px(6.));
    let automatic = current.is_none();
    controls = controls.child(
        button_sm("share-default-auto", if automatic { ButtonKind::Primary } else { ButtonKind::Glass }, tr!("First enabled"))
            .cursor_pointer()
            .on_click(cx.listener(|d, _, _, cx| {
                let svc = d.share.svc.clone();
                if let Err(e) = svc.update(cx, |s, cx| s.set_default(None, cx)) {
                    d.share.error = Some(e.to_string());
                }
                cx.notify();
            })),
    );
    for target in svc.targets().targets.iter().filter(|t| t.enabled) {
        let id = target.id.clone();
        let active = current.as_deref() == Some(id.as_str());
        controls = controls.child(
            button_sm(
                SharedString::from(format!("share-default-pick-{}", target.id)),
                if active { ButtonKind::Primary } else { ButtonKind::Glass },
                target.label(),
            )
            .cursor_pointer()
            .on_click(cx.listener(move |d, _, _, cx| {
                let svc = d.share.svc.clone();
                if let Err(e) = svc.update(cx, |s, cx| s.set_default(Some(id.clone()), cx)) {
                    d.share.error = Some(e.to_string());
                }
                cx.notify();
            })),
        );
    }
    controls
}

fn options_card(d: &SettingsPage, cx: &mut Context<SettingsPage>) -> AnyElement {
    let naming_items =
        [(NamingChoice::Random, tr!("Random")), (NamingChoice::Original, tr!("Keep file name")), (NamingChoice::Template, tr!("Template"))];
    let entity = cx.entity().downgrade();
    let naming = row(
        tr!("File names"),
        Some(tr!("How uploads are named on the server")),
        tabs("share-naming", &naming_items, d.share.naming_choice, move |choice, window, cx| {
            entity
                .update(cx, |d, cx| {
                    d.share.naming_choice = choice;
                    let mode = match choice {
                        NamingChoice::Random => NameMode::Random,
                        NamingChoice::Original => NameMode::Original,
                        NamingChoice::Template => {
                            if d.share.naming_template.read(cx).value().trim().is_empty() {
                                d.share.naming_template.update(cx, |s, cx| s.set_value("{game}/{date}/{stem}.{ext}", window, cx));
                            }
                            NameMode::Template(d.share.naming_template.read(cx).value().trim().to_string())
                        }
                    };
                    save_naming(d, mode, cx);
                })
                .ok();
        }),
    );
    let mut content = div().flex().flex_col().child(caption(tr!("Sharing options"))).child(card([
        row(tr!("Default target"), Some(tr!("Shares without a chosen target go here")), default_picker(d, cx)).into_any_element(),
        naming.into_any_element(),
    ]));
    if d.share.naming_choice == NamingChoice::Template {
        content = content.child(card([row(
            tr!("Name template"),
            None,
            div().w(px(420.)).max_w_full().child(input_box(&d.share.naming_template, false)),
        )
        .into_any_element()]));
    }
    content
        .child(meta_text(tr!("Template placeholders: {game} {date} {time} {name} {stem} {ext} {rand}")))
        .child(meta_text(tr!("Links are unlisted, not private. Anyone who has a link can open the file.")))
        .into_any_element()
}

/// Console rows: the forms stay desktop-only, but targets can be enabled,
/// tested and chosen here, and recent links copied.
pub(super) fn console_rows(d: &SettingsPage, cx: &App) -> Vec<Row> {
    let svc = d.share.svc.read(cx);
    let targets = svc.targets();
    let mut rows = vec![Row::header(tr!("Sharing"))];
    rows.push(Row::info("desktop", tr!("Targets"), tr!("Add and edit targets on the desktop")).icon(Icon::Share));
    for target in &targets.targets {
        let mut desc = format!("{} · {}", target.kind.label(), target_descriptor(target));
        if targets.default.as_deref() == Some(target.id.as_str()) {
            desc.push_str(" · ");
            desc.push_str(tr!("Default target"));
        }
        if let Some(test) = d.share.tests.get(&target.id) {
            desc.push_str(" · ");
            if test.testing {
                desc.push_str(tr!("Testing…"));
            } else if let Some(outcome) = &test.outcome {
                match outcome {
                    Ok(text) => desc.push_str(text),
                    Err(text) => desc.push_str(text),
                }
            }
        }
        rows.push(Row::info(format!("target:{}", target.id), target.label(), desc));
        rows.push(Row::action(format!("test:{}", target.id), tr!("Test connection")).icon(Icon::Refresh));
        rows.push(Row::toggle(format!("enabled:{}", target.id), tr!("Use {target}", target = target.label()), target.enabled));
        rows.push(Row::action(format!("default:{}", target.id), tr!("Set as default")));
    }
    let naming_options = vec![tr!("Random").into(), tr!("Keep file name").into(), tr!("Template").into()];
    let (naming_index, naming_desc) = match &targets.naming.mode {
        NameMode::Random => (0, tr!("Random").to_string()),
        NameMode::Original => (1, tr!("Keep file name").to_string()),
        NameMode::Template(template) => (2, template.clone()),
    };
    rows.push(Row::choice("naming", tr!("File names"), naming_options, naming_index).desc(naming_desc));
    rows.push(Row::header(tr!("Recent shares")));
    for record in svc.history_snapshot().iter().take(5) {
        rows.push(Row::info(format!("share:{}", record.id), record.file_name.clone(), record.url.clone()));
        rows.push(Row::action(format!("copy:{}", record.id), tr!("Copy link")).icon(Icon::ContentCopy));
    }
    rows
}

pub(super) fn console_apply(d: &mut SettingsPage, outcome: Outcome, cx: &mut Context<SettingsPage>) {
    match outcome {
        Outcome::Activate(id) if id.as_ref().starts_with("test:") => {
            let tid = id.trim_start_matches("test:").to_string();
            if let Some(target) = d.share.svc.read(cx).targets().get(&tid).cloned() {
                test_target(d, target, cx);
            }
        }
        Outcome::Activate(id) if id.as_ref().starts_with("default:") => {
            let tid = id.trim_start_matches("default:").to_string();
            let svc = d.share.svc.clone();
            let _ = svc.update(cx, |s, cx| s.set_default(Some(tid), cx));
        }
        Outcome::Activate(id) if id.as_ref().starts_with("copy:") => {
            let rid = id.trim_start_matches("copy:").to_string();
            if let Some(url) = d.share.svc.read(cx).history_snapshot().iter().find(|r| r.id == rid).map(|r| r.url.clone()) {
                crate::services::share::copy_link(cx, &url);
            }
        }
        Outcome::Changed(id, Value::Toggle(on)) if id.as_ref().starts_with("enabled:") => {
            let tid = id.trim_start_matches("enabled:").to_string();
            let svc = d.share.svc.clone();
            if let Some(mut target) = svc.read(cx).targets().get(&tid).cloned() {
                target.enabled = on;
                let _ = svc.update(cx, |s, cx| s.upsert_target(target, cx));
            }
        }
        Outcome::Changed(id, Value::Choice { index, .. }) if id.as_ref() == "naming" => {
            let mode = match index {
                0 => NameMode::Random,
                1 => NameMode::Original,
                _ => match &d.share.svc.read(cx).targets().naming.mode {
                    NameMode::Template(template) => NameMode::Template(template.clone()),
                    _ => NameMode::Template("{game}-{stem}-{rand}.{ext}".into()),
                },
            };
            let svc = d.share.svc.clone();
            let _ = svc.update(cx, |s, cx| s.set_naming(Naming { mode }, cx));
        }
        _ => {}
    }
}

pub fn render(d: &mut SettingsPage, _window: &mut Window, cx: &mut Context<SettingsPage>) -> AnyElement {
    let mut out = div().flex().flex_col().gap(px(4.)).max_w(px(760.));
    out = out.child(meta_text(tr!(
        "Upload screenshots and clips to your own server. Test a target before relying on it, and pin the key of SFTP servers."
    )));
    out = out.child(targets_card(d, cx));
    out = out.child(caption(if d.share.editing.is_some() { tr!("Edit target") } else { tr!("Add a target") }));
    out = out.child(form_render::render_form(d, cx));
    out = out.child(options_card(d, cx));
    out = out.child(history::render_history(d, cx));
    out.into_any_element()
}
