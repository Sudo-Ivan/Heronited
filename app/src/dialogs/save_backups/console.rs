//! Console mode of the Saves tab: the same steps as rows.

use gpui_kit::prelude::*;
use gpui_kit::*;
use heronite_emulation::saves::restore::FileKind;
use heronite_i18n::{tr, trn};

use super::render::{backup_line, group_facts, group_help, group_title, overwrite_line, short_path, target_line};
use super::{Load, SaveBackupsView, Stage};
use crate::console::list::{Outcome, Row, Value};
use crate::console::{hint, Btn, Hint, Pad};
use crate::icons::Icon;
use crate::pages::settings::format_time;

impl SaveBackupsView {
    fn console_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        if let Some((text, error)) = &self.notice {
            let r = Row::info("sb-notice", text.clone(), "");
            rows.push(if *error { r.danger() } else { r });
        }
        let l = match &self.load {
            Load::Idle | Load::Loading => {
                rows.push(Row::info("sb-loading", tr!("Looking for backups…"), ""));
                return rows;
            }
            Load::Off => {
                rows.push(Row::info(
                    "sb-off",
                    tr!("Save backups are off. Turn them on in Settings → Saves to keep this game's saves on a saves server or in a folder."),
                    "",
                ));
                return rows;
            }
            Load::Failed(e) => {
                rows.push(Row::info("sb-error", e.clone(), "").danger());
                rows.push(Row::action("sb-retry", tr!("Try again")).icon(Icon::Refresh));
                return rows;
            }
            Load::Ready(l) => l,
        };
        let (line, error) = target_line(l, self.installed);
        let target_row = Row::info("sb-target", line, "");
        rows.push(if error { target_row.danger() } else { target_row });
        let busy = self.busy();

        match &self.stage {
            Stage::List | Stage::Planning => {
                let folder = l.target.custom.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| tr!("Automatic").to_string());
                rows.push(Row::info("sb-folder", tr!("Saves folder"), folder));
                if l.saves_missing {
                    rows.push(Row::info(
                        "sb-folder-hint",
                        tr!("Heronited could not find this game's saves on this PC. If it keeps them in a folder of its own, choose that folder here."),
                        "",
                    ));
                }
                rows.push(Row::header(tr!("Backups")));
                if l.snapshots.is_empty() {
                    rows.push(Row::info("sb-none", tr!("No backup of this game on the saves server yet."), ""));
                } else if let Some(b) = self.console_backup.as_ref().and_then(|id| l.snapshots.iter().find(|s| &s.id == id)) {
                    let (title, facts) = backup_line(b);
                    rows.push(Row::info("sb-m-title", title, "").desc(facts));
                    rows.push(Row::action("sb-m-restore", tr!("Restore…")).icon(Icon::Replay).enabled(!busy && l.prefix_ok));
                    rows.push(if self.confirm_delete == self.console_backup {
                        Row::action("sb-m-delete-yes", tr!("Press A again to delete it. This cannot be undone."))
                            .icon(Icon::DeleteForever)
                            .danger()
                    } else {
                        Row::action("sb-m-delete", tr!("Delete")).icon(Icon::DeleteOutline).enabled(!busy)
                    });
                    rows.push(Row::action("sb-m-back", tr!("Back")).icon(Icon::ChevronLeft));
                    return rows;
                } else {
                    for (i, b) in l.snapshots.iter().enumerate() {
                        let (title, facts) = backup_line(b);
                        rows.push(Row::action(SharedString::from(format!("sb-b-{i}")), title).icon(Icon::History).desc(facts));
                    }
                }
                rows.push(Row::header(""));
                if matches!(self.stage, Stage::Planning) {
                    rows.push(Row::info("sb-planning", tr!("Reading the backup…"), ""));
                }
                rows.push(
                    Row::action("sb-backup-now", if self.working { tr!("Working…") } else { tr!("Back up now") })
                        .icon(Icon::Save)
                        .enabled(!busy),
                );
                if let Some(u) = &l.undo {
                    rows.push(
                        Row::action("sb-undo", tr!("Undo the last restore"))
                            .icon(Icon::History)
                            .enabled(!busy)
                            .desc(tr!("Last restore: {when}. A copy of what it replaced is kept.", when = format_time(&u.created_at))),
                    );
                }
                rows.push(Row::action("sb-refresh", tr!("Refresh")).icon(Icon::Refresh).enabled(!busy));
            }
            Stage::Plan(preview) => {
                let plan = &preview.plan;
                rows.push(Row::header(tr!("Files to restore")));
                for g in &preview.groups {
                    let mut desc = group_facts(g);
                    if let Some((dir, _)) = g.folders.first() {
                        desc = format!("{desc} \u{b7} {dir}");
                    }
                    if let Some(help) = group_help(g.kind) {
                        desc = format!("{desc} \u{2014} {help}");
                    }
                    rows.push(
                        Row::toggle(SharedString::from(format!("sb-g-{}", g.kind as u8)), group_title(g.kind), g.selected)
                            .enabled(g.kind != FileKind::Unmapped)
                            .desc(desc),
                    );
                }
                rows.push(Row::header(""));
                let selected = plan.selected().count();
                rows.push(
                    Row::action("sb-confirm", trn!(selected, "Restore {n} file", "Restore {n} files"))
                        .icon(Icon::Replay)
                        .enabled(selected > 0)
                        .desc(overwrite_line(plan)),
                );
                rows.push(Row::action("sb-back", tr!("Back")).icon(Icon::ChevronLeft));
            }
            Stage::Restoring => rows.push(Row::info("sb-restoring", tr!("Restoring the save…"), "")),
            Stage::Done(outcome) => {
                rows.push(Row::info("sb-written", trn!(outcome.written, "Restored {n} file.", "Restored {n} files."), ""));
                if outcome.unchanged > 0 {
                    rows.push(Row::info("sb-same", trn!(outcome.unchanged, "{n} was already the same.", "{n} were already the same."), ""));
                }
                for (i, (path, error)) in outcome.failed.iter().enumerate() {
                    rows.push(Row::info(SharedString::from(format!("sb-fail-{i}")), short_path(path, &l.target), error.clone()).danger());
                }
                rows.push(Row::action("sb-undo", tr!("Undo")).icon(Icon::History).enabled(outcome.undo.is_some() && !busy));
                rows.push(Row::action("sb-back", tr!("Done")).icon(Icon::Check));
            }
        }
        rows
    }

    pub(super) fn render_console(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> AnyElement {
        let rows = self.console_rows();
        div().size_full().child(self.console_list.render("console-save-backups", &rows, true).size_full()).into_any_element()
    }

    fn console_open_backup(&mut self, id: Option<String>) {
        self.console_backup = id;
        self.confirm_delete = None;
        self.console_list.reset();
    }

    pub fn console_reset(&mut self) {
        self.console_list.reset();
    }

    /// Whether the tab takes `pad` before the dialog does: B while a backup's
    /// actions are shown.
    pub fn captures(&self, pad: Pad) -> bool {
        pad == Pad::B && self.console_backup.is_some()
    }

    /// A pad press for the tab; `false` when it did nothing.
    pub fn on_pad(&mut self, pad: Pad, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        if pad == Pad::B && self.console_backup.is_some() {
            self.console_open_backup(None);
            cx.notify();
            return true;
        }
        let rows = self.console_rows();
        match self.console_list.on_pad(pad, &rows) {
            Outcome::Changed(id, Value::Toggle(_)) => {
                let kind = id
                    .strip_prefix("sb-g-")
                    .and_then(|n| n.parse::<u8>().ok())
                    .and_then(|n| [FileKind::Save, FileKind::Prefix, FileKind::Unmapped].into_iter().find(|k| *k as u8 == n));
                if let Some(kind) = kind {
                    self.toggle_kind(kind, cx);
                }
            }
            Outcome::Activate(id) => match id.as_ref() {
                "sb-retry" | "sb-refresh" => self.reload(cx),
                "sb-backup-now" => self.backup_now(cx),
                "sb-undo" => self.undo_last(cx),
                "sb-confirm" => self.confirm_restore(cx),
                "sb-back" => self.back_to_list(cx),
                "sb-m-back" => self.console_open_backup(None),
                "sb-m-restore" => {
                    if let Some(id) = self.console_backup.clone() {
                        self.console_open_backup(None);
                        self.pick_backup(id, cx);
                        self.start_restore(cx);
                    }
                }
                "sb-m-delete" => self.confirm_delete = self.console_backup.clone(),
                "sb-m-delete-yes" => {
                    self.delete_confirmed(cx);
                    self.console_list.reset();
                }
                id => {
                    // The row number is read against the rows just built, then
                    // the backup is kept by id.
                    let picked = id
                        .strip_prefix("sb-b-")
                        .and_then(|n| n.parse::<usize>().ok())
                        .and_then(|i| self.loaded().and_then(|l| l.snapshots.get(i)).map(|s| s.id.clone()));
                    if let Some(id) = picked {
                        self.pick_backup(id.clone(), cx);
                        self.console_open_backup(Some(id));
                    }
                }
            },
            Outcome::Ignored => return false,
            _ => {}
        }
        cx.notify();
        true
    }

    pub fn hints(&self, _cx: &App) -> Vec<Hint> {
        vec![hint(Btn::A, tr!("Select")), hint(Btn::LeftRight, tr!("Change"))]
    }
}
