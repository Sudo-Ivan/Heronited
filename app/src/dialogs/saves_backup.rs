//! Manual "Back up saves" of one game: runs the same backup as the after-play
//! one (`heronite_emulation::saves`) and tells how it went.

use gpui_kit::prelude::*;
use gpui_kit::*;
use heronite_emulation::saves::{backup_for_game, BackupReason, BackupResult, SavesEnv, SavesGame};
use heronite_i18n::{tr, trn};
use heronite_launch::SystemProbe;

use super::{DialogCtx, DialogEvent};
use crate::theme;
use crate::ui::controls::{button, ButtonKind};
use crate::ui::dialog::{dialog, DialogProps};
use crate::ui::fields::paragraph;

fn title() -> &'static str {
    tr!("Save backup")
}

/// What the dialog says when the backup finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Running,
    /// Plain information (busy, no saves matched).
    Message(String),
    Error(String),
    Done(String),
}

pub fn describe(result: &BackupResult, title: &str) -> Outcome {
    if result.skipped_reason.as_deref() == Some("already-running") {
        return Outcome::Message(tr!("Another backup is running. Wait a moment and try again.").into());
    }
    if let (Some(error), false) = (&result.error, result.ran) {
        return if matches!(result.skipped_reason.as_deref(), Some("no-match" | "no-saves")) {
            Outcome::Message(error.clone())
        } else {
            Outcome::Error(error.clone())
        };
    }
    let name = result.game_name.as_deref().filter(|n| !n.is_empty()).unwrap_or(title);
    let files = result.files.unwrap_or_default();
    let folder = result.local_path.clone().unwrap_or_default();
    let mut text = match (result.server, result.unchanged) {
        (true, true) => tr!("The saves server already has the current saves of {name}. Nothing needed to be sent.", name).to_string(),
        (true, false) => {
            trn!(files, "Saved {n} file of {name} to the saves server.", "Saved {n} files of {name} to the saves server.", name)
        }
        (false, true) => tr!("The backup folder already has the current saves of {name}. Nothing needed to be copied.", name).to_string(),
        (false, false) => trn!(files, "Saved {n} file of {name} to {folder}.", "Saved {n} files of {name} to {folder}.", name, folder),
    };
    if result.server && !folder.is_empty() {
        text.push(' ');
        text.push_str(&tr!("A copy is in {folder}.", folder));
    }
    if result.left_out > 0 {
        text.push(' ');
        text.push_str(&trn!(result.left_out, "{n} file could not be included.", "{n} files could not be included."));
    }
    Outcome::Done(text)
}

pub struct SavesBackupDialog {
    outcome: Outcome,
    console_list: crate::console::list::ListNav,
}

impl EventEmitter<DialogEvent> for SavesBackupDialog {}

impl SavesBackupDialog {
    /// A dialog that already shows `outcome` (screenshots).
    pub fn showing(outcome: Outcome) -> Self {
        Self { outcome, console_list: Default::default() }
    }

    /// Starts the backup right away on a worker thread.
    pub fn new(ctx: DialogCtx, game: SavesGame, cx: &mut Context<Self>) -> Self {
        let title = game.title.clone();
        let paths = ctx.data_paths.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let env = SavesEnv { paths: &paths, probe: &SystemProbe };
                    backup_for_game(&env, &game, BackupReason::Manual)
                })
                .await;
            this.update(cx, |this, cx| {
                this.outcome = describe(&result, &title);
                // The backup is recorded in settings.json (last backup/error).
                cx.emit(DialogEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
        Self { outcome: Outcome::Running, console_list: Default::default() }
    }
}

impl Render for SavesBackupDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (text, is_error) = match &self.outcome {
            Outcome::Running => (tr!("Backing up saves…").to_string(), false),
            Outcome::Message(m) | Outcome::Done(m) => (m.clone(), false),
            Outcome::Error(m) => (m.clone(), true),
        };
        if crate::console::active() {
            let rows = vec![crate::console::list::Row::action("close", tr!("Close"))];
            let error = is_error.then(|| SharedString::from(text.clone()));
            let message = if is_error { String::new() } else { text };
            return crate::console::list::simple_dialog(&mut self.console_list, "console-saves-backup", title(), &message, error, &rows);
        }
        let body = div().flex().flex_col().child(paragraph(text).when(is_error, |d| d.text_color(theme::danger_text())));
        let footer = div().flex().gap(px(10.)).child(
            button("saves-close", ButtonKind::Primary, tr!("Close")).on_click(cx.listener(|_, _, _, cx| cx.emit(DialogEvent::Close))),
        );
        let entity = cx.entity().downgrade();
        dialog(
            DialogProps { id: "saves-backup".into(), title: title().into(), width: 560., height: None },
            body.into_any_element(),
            Some(footer.into_any_element()),
            move |_, cx| {
                entity.update(cx, |_, cx| cx.emit(DialogEvent::Close)).ok();
            },
        )
        .into_any_element()
    }
}

impl crate::console::PadTarget for SavesBackupDialog {
    fn on_pad(&mut self, pad: crate::console::Pad, _: &mut Window, cx: &mut Context<Self>) -> bool {
        if pad == crate::console::Pad::A {
            cx.emit(DialogEvent::Close);
            return true;
        }
        false
    }

    fn hints(&self, _: &App) -> Vec<crate::console::Hint> {
        vec![crate::console::hint(crate::console::Btn::A, tr!("Close"))]
    }
}

#[cfg(test)]
mod tests {
    use super::{describe, BackupResult, Outcome};

    fn result(f: impl FnOnce(&mut BackupResult)) -> BackupResult {
        let mut r = BackupResult::default();
        f(&mut r);
        r
    }

    #[test]
    fn busy_no_match_and_errors() {
        let busy = result(|r| r.skipped_reason = Some("already-running".into()));
        assert_eq!(describe(&busy, "G"), Outcome::Message("Another backup is running. Wait a moment and try again.".into()));
        for reason in ["no-match", "no-saves"] {
            let r = result(|r| {
                r.skipped_reason = Some(reason.into());
                r.error = Some("nothing".into());
            });
            assert_eq!(describe(&r, "G"), Outcome::Message("nothing".into()));
        }
        let failed = result(|r| r.error = Some("no network".into()));
        assert_eq!(describe(&failed, "G"), Outcome::Error("no network".into()));
    }

    #[test]
    fn saved_to_a_folder_only_or_both() {
        let folder = result(|r| {
            r.ran = true;
            r.game_name = Some("Hades".into());
            r.files = Some(2);
            r.local_path = Some("/b/hades".into());
        });
        assert_eq!(describe(&folder, "G"), Outcome::Done("Saved 2 files of Hades to /b/hades.".into()));
        let same = result(|r| {
            r.ran = true;
            r.unchanged = true;
            r.local_path = Some("/b".into());
        });
        assert!(matches!(describe(&same, "G"), Outcome::Done(m) if m.contains("backup folder already has")));
        let both = result(|r| {
            r.ran = true;
            r.server = true;
            r.files = Some(1);
            r.local_path = Some("/b/g".into());
        });
        assert!(matches!(describe(&both, "G"), Outcome::Done(m) if m.ends_with("A copy is in /b/g.")));
    }

    #[test]
    fn saved_unchanged_and_partial() {
        let saved = result(|r| {
            r.ran = true;
            r.server = true;
            r.game_name = Some("Portal 2".into());
            r.files = Some(3);
        });
        assert_eq!(describe(&saved, "Fallback"), Outcome::Done("Saved 3 files of Portal 2 to the saves server.".into()));
        let one = result(|r| {
            r.ran = true;
            r.server = true;
            r.files = Some(1);
        });
        assert_eq!(describe(&one, "Fallback"), Outcome::Done("Saved 1 file of Fallback to the saves server.".into()));
        let same = result(|r| {
            r.ran = true;
            r.server = true;
            r.unchanged = true;
        });
        assert!(matches!(describe(&same, "G"), Outcome::Done(m) if m.contains("already has")));
        let partial = result(|r| {
            r.ran = true;
            r.server = true;
            r.files = Some(2);
            r.left_out = 2;
        });
        assert!(matches!(describe(&partial, "G"), Outcome::Done(m) if m.ends_with("2 files could not be included.")));
    }
}
