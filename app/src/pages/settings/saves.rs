//! Settings → Saves, first card: save backups to a saves server after play
//! and, when set, every interval while a game runs.
//!
//! Debug: `HERONITE_DEBUG_PAGE=settings:saves`; `HERONITE_DEBUG_SAVES_INTERVAL_OPEN=1`
//! opens the "Also back up while playing" choice.

use gpui_kit::component::input::InputState;
use gpui_kit::prelude::*;
use gpui_kit::*;
use heronite_data::settings::EffectiveSettings;
use heronite_emulation::saves::account;
use heronite_emulation::settings_ext::{set_saves_settings, SavesSettingsPatch};
use heronite_i18n::{n_, tr};

use super::backup::format_time;
use super::SettingsPage;
use crate::pages::PageCtx;
use crate::theme;
use crate::ui::controls::{caption, card, card_box, help_text, toggle_row};
use crate::ui::fields::{choose_path, meta_text, new_input, on_commit, path_field, select_field, text_field};

/// `saves.intervalMinutes` choices; `None` = only after play.
const INTERVALS: [(Option<u32>, &str); 5] = [
    (None, n_!("Off")),
    (Some(15), n_!("Every 15 minutes")),
    (Some(30), n_!("Every 30 minutes")),
    (Some(60), n_!("Every hour")),
    (Some(120), n_!("Every 2 hours")),
];

/// The label of an interval, also of one typed into `settings.json`.
fn interval_label(minutes: Option<u32>) -> String {
    match INTERVALS.iter().find(|(m, _)| *m == minutes) {
        Some((_, l)) => tr(l).to_string(),
        // At least 5 (`SAVES_INTERVAL_RANGE`): never singular.
        None => tr!("Every {n} minutes", n = minutes.unwrap_or_default()),
    }
}

pub struct State {
    server: Entity<InputState>,
    folder: Entity<InputState>,
    interval_open: bool,
}

impl State {
    pub fn new(_ctx: &PageCtx, s: &EffectiveSettings, window: &mut Window, cx: &mut Context<SettingsPage>) -> Self {
        let server = new_input(window, cx, &s.saves.server, heronite_saves::DEFAULT_SERVER.trim_start_matches("https://"));
        on_commit(&server, window, cx, |d: &mut SettingsPage, v, window, cx| {
            if v == d.settings.saves.server {
                return;
            }
            save(d, SavesSettingsPatch { server: Some(v), ..Default::default() }, cx);
            // What is kept is the normalised address (or none for the default).
            let shown = d.settings.saves.server.clone();
            d.saves.server.update(cx, |s, cx| s.set_value(shown, window, cx));
            super::saves_restore::server_changed(d, cx);
        });
        let folder = new_input(window, cx, &s.saves.local_folder, "");
        on_commit(&folder, window, cx, |d: &mut SettingsPage, v, _, cx| {
            if v != d.settings.saves.local_folder {
                save(d, SavesSettingsPatch { local_folder: Some(v), ..Default::default() }, cx);
            }
        });
        Self { server, folder, interval_open: crate::env::flag("HERONITE_DEBUG_SAVES_INTERVAL_OPEN") }
    }

    /// Section shown: nothing to look for (the account is only contacted when
    /// the user asks, or when a backup runs).
    pub fn shown(_page: &mut SettingsPage, _window: &mut Window, _cx: &mut Context<SettingsPage>) {}
}

pub(super) fn save(d: &mut SettingsPage, patch: SavesSettingsPatch, cx: &mut Context<SettingsPage>) {
    let res = set_saves_settings(&d.ctx.data_paths, &patch);
    d.saved(res, cx);
}

/// Turning the backups on makes the secure key (once), so there is something
/// to keep a copy of right away.
pub(super) fn set_enabled(d: &mut SettingsPage, on: bool, cx: &mut Context<SettingsPage>) {
    save(d, SavesSettingsPatch { enabled: Some(on), ..Default::default() }, cx);
    if on {
        super::saves_restore::key_ready(d, cx);
    }
}

/// What a backup while playing is and is not.
const INTERVAL_HELP: &str = n_!("Starts with the next game you launch. The save files are read as they are at that moment: a game saving then can leave a mixed backup, so the backup after the game closes is the one to trust. Nothing is restored on its own.");

fn last_line(d: &SettingsPage) -> String {
    let l = &d.settings.saves;
    let mut last = match &l.last_backup_at {
        Some(at) => tr!("Last save backup: {when}", when = format_time(at)),
        None => tr!("Last save backup: Never").to_string(),
    };
    if let Some(g) = &l.last_backup_game {
        last.push_str(&format!(" \u{b7} {g}"));
    }
    last
}

pub fn render(d: &mut SettingsPage, _window: &mut Window, cx: &mut Context<SettingsPage>) -> AnyElement {
    let l = d.settings.saves.clone();
    let enable = card([toggle_row("saves-enabled", tr!("Back up saves when a game closes"), None, l.enabled)
        .on_click(cx.listener(move |d, _, _, cx| {
            let on = !d.settings.saves.enabled;
            set_enabled(d, on, cx);
        }))
        .into_any_element()]);

    let interval = card_box().py(px(12.)).gap(px(8.)).child(select_field(
        "saves-interval",
        Some(tr!("Also back up while playing")),
        interval_label(l.interval_minutes),
        !l.enabled,
        d.saves.interval_open && l.enabled,
        INTERVALS.iter().map(|(m, lbl)| (tr(lbl).into(), *m == l.interval_minutes)).collect(),
        cx,
        |d: &mut SettingsPage, _, cx| {
            d.saves.interval_open = !d.saves.interval_open;
            cx.notify();
        },
        |d: &mut SettingsPage, i, _, cx| {
            d.saves.interval_open = false;
            save(d, SavesSettingsPatch { interval_minutes: Some(INTERVALS[i].0), ..Default::default() }, cx);
        },
    ));
    let interval = interval.child(help_text(tr(INTERVAL_HELP)));

    let server_on = !l.server_off;
    let server = card_box()
        .py(px(12.))
        .gap(px(8.))
        .child(toggle_row("saves-server-on", tr!("Send saves to the saves server"), None, server_on).on_click(cx.listener(
            move |d, _, _, cx| {
                let off = !d.settings.saves.server_off;
                save(d, SavesSettingsPatch { server_off: Some(off), ..Default::default() }, cx);
            },
        )))
        .child(div().when(!server_on, |d| d.opacity(0.5)).child(text_field(tr!("Saves server"), &d.saves.server)))
        .child(help_text(tr!("Backups go to saves.quad4.io unless you run a server of your own. They are sent over https.")));

    let has_folder = !l.local_folder.is_empty();
    let folder = card_box().py(px(12.)).gap(px(8.)).child(path_field(
        "saves-folder-pick",
        Some(tr!("Backup folder on this PC")),
        &d.saves.folder,
        has_folder,
        false,
        cx.listener(move |d, _, window, cx| {
            if has_folder {
                d.saves.folder.update(cx, |s, cx| s.set_value("", window, cx));
                save(d, SavesSettingsPatch { local_folder: Some(String::new()), ..Default::default() }, cx);
            } else {
                choose_path(window, cx, true, tr!("Backup folder on this PC"), |d: &mut SettingsPage, p, window, cx| {
                    d.saves.folder.update(cx, |s, cx| s.set_value(p.clone(), window, cx));
                    save(d, SavesSettingsPatch { local_folder: Some(p), ..Default::default() }, cx);
                });
            }
        }),
    ));
    let folder = folder.child(help_text(tr!(
        "Every backup is also copied to this folder, without a server: the last 10 of each game are kept, and the Backups tab of a game can restore from them. Leave it empty for server backups only."
    )));

    div()
        .flex()
        .flex_col()
        .child(caption(tr!("Save backups")))
        .child(div().px(px(6.)).pb(px(8.)).child(meta_text(tr!(
            "When enabled, Heronited backs up the game save to a saves server after you close a game. Right-click a Collection card to back up that game now."
        ))))
        .child(enable)
        .child(div().mt(px(10.)).child(interval))
        .child(div().mt(px(10.)).child(server))
        .child(div().mt(px(10.)).child(folder))
        .child(div().mt(px(12.)).mx(px(6.)).child(meta_text(last_line(d))))
        .children(l.last_error.clone().map(|e| div().mt(px(8.)).mx(px(6.)).text_color(theme::danger_text()).text_size(px(13.)).child(e)))
        .into_any_element()
}

/// Console mode rows of Saves: the backups, then the account.
pub(super) fn console_rows(d: &SettingsPage, cx: &App) -> Vec<crate::console::list::Row> {
    use crate::console::list::Row;
    use crate::icons::Icon;
    let l = &d.settings.saves;
    let mut rows = vec![
        Row::header(tr!("Save backups")),
        Row::toggle("sv-enabled", tr!("Back up saves when a game closes"), l.enabled).desc(tr!("To a saves server")),
        Row::choice(
            "sv-interval",
            tr!("Also back up while playing"),
            INTERVALS.iter().map(|(_, x)| SharedString::from(tr(x))).collect(),
            INTERVALS.iter().position(|(m, _)| *m == l.interval_minutes).unwrap_or(0),
        )
        .enabled(l.enabled)
        .desc(tr(INTERVAL_HELP)),
        Row::toggle("sv-server-on", tr!("Send saves to the saves server"), !l.server_off),
        Row::text("sv-server", tr!("Saves server"), d.saves.server.read(cx).value().to_string())
            .icon(Icon::CloudSync)
            .enabled(!l.server_off),
        Row::text("sv-folder", tr!("Backup folder on this PC"), d.saves.folder.read(cx).value().to_string()).icon(Icon::FolderOpen),
        Row::info("sv-last", last_line(d), ""),
    ];
    if let Some(e) = &l.last_error {
        rows.push(Row::info("sv-error", e.clone(), "").danger());
    }
    rows.extend(super::saves_restore::console_rows(d));
    rows
}

pub(super) fn console_apply(
    d: &mut SettingsPage,
    outcome: crate::console::list::Outcome,
    window: &mut Window,
    cx: &mut Context<SettingsPage>,
) {
    use crate::console::list::{Outcome, Value};
    match &outcome {
        Outcome::Changed(id, Value::Toggle(on)) if id.as_ref() == "sv-enabled" => set_enabled(d, *on, cx),
        Outcome::Changed(id, Value::Toggle(on)) if id.as_ref() == "sv-server-on" => {
            save(d, SavesSettingsPatch { server_off: Some(!*on), ..Default::default() }, cx)
        }
        Outcome::Activate(id) if id.as_ref() == "sv-folder" => {
            let start = Some(std::path::PathBuf::from(d.saves.folder.read(cx).value().to_string()));
            d.console_browse(
                tr!("Backup folder on this PC"),
                crate::console::browser::PickKind::Folder,
                start,
                window,
                cx,
                |d, path, window, cx| {
                    d.saves.folder.update(cx, |s, cx| s.set_value(path.clone(), window, cx));
                    save(d, SavesSettingsPatch { local_folder: Some(path), ..Default::default() }, cx);
                },
            );
        }
        Outcome::Changed(id, Value::Choice { index, .. }) if id.as_ref() == "sv-interval" => {
            save(d, SavesSettingsPatch { interval_minutes: Some(INTERVALS[*index].0), ..Default::default() }, cx)
        }
        Outcome::Activate(id) if id.as_ref() == "sv-server" => {
            let current = d.saves.server.read(cx).value().to_string();
            d.console_osk(tr!("Saves server"), &current, window, cx, |d, text, window, cx| {
                d.saves.server.update(cx, |s, cx| s.set_value(text.clone(), window, cx));
                save(d, SavesSettingsPatch { server: Some(text), ..Default::default() }, cx);
                let shown = d.settings.saves.server.clone();
                d.saves.server.update(cx, |s, cx| s.set_value(shown, window, cx));
                super::saves_restore::server_changed(d, cx);
            });
        }
        _ => {
            super::saves_restore::console_apply(d, &outcome, window, cx);
        }
    }
    cx.notify();
}

/// The server in the settings, for display (the default when none is set).
pub(super) fn server_label(d: &SettingsPage) -> String {
    account::server_of(&d.ctx.data_paths)
        .map(|s| s.trim_start_matches("https://").to_string())
        .unwrap_or_else(|_| d.settings.saves.server.clone())
}
