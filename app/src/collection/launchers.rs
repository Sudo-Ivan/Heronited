//! The Collection's side of the other launchers' import
//! (`services::launcher_imports`): the service starts with the Collection
//! (and keeps the launchers turned on in sync), and the Collection reloads
//! when their games come in or go. A game found in the Collection twice gets
//! a toast whose Review opens Settings → Other launchers, where it is merged.

use gpui_kit::*;
use heronite_i18n::{tr, trn};

use super::{CollectionView, ToastAction};
use crate::services::launcher_imports::LauncherImportsEvent;

impl CollectionView {
    pub(super) fn watch_launcher_imports(&mut self, cx: &mut Context<Self>) {
        let svc = crate::services::launcher_imports(cx);
        cx.subscribe(&svc, |this, _, ev: &LauncherImportsEvent, cx| match ev {
            LauncherImportsEvent::Imported(launcher, report) => {
                if report.added > 0 {
                    let text =
                        trn!(report.added, "Added {n} game from {launcher}", "Added {n} games from {launcher}", launcher = launcher.name());
                    this.show_toast(text, false, cx);
                }
                this.reload(cx);
            }
            LauncherImportsEvent::Duplicates(new) => {
                let text = match new.as_slice() {
                    [one] => tr!(
                        "{title} is in your Collection twice: added by you and from {launcher}.",
                        title = one.theirs.title,
                        launcher = one.launcher.name()
                    ),
                    _ => trn!(new.len(), "{n} game is in your Collection twice.", "{n} games are in your Collection twice."),
                };
                this.show_toast(text, false, cx);
                this.toast_action = Some(ToastAction::ReviewDuplicates);
            }
            LauncherImportsEvent::Resolved => this.reload(cx),
        })
        .detach();
        cx.observe(&svc, |_, _, cx| cx.notify()).detach();
    }

    pub(super) fn watch_gameyfin(&mut self, cx: &mut Context<Self>) {
        let svc = crate::services::gameyfin(cx);
        svc.update(cx, |s, cx| s.start_periodic(cx));
        cx.subscribe(&svc, |this, _, ev: &crate::services::gameyfin::GameyfinEvent, cx| match ev {
            crate::services::gameyfin::GameyfinEvent::LibraryChanged => this.reload(cx),
            crate::services::gameyfin::GameyfinEvent::Toast { title, text, error } => {
                let text = if title == "Gameyfin" { text.clone() } else { format!("{title}: {text}") };
                this.show_toast(text, *error, cx);
            }
            crate::services::gameyfin::GameyfinEvent::InstallState { app_name, active } => {
                if *active {
                    this.preparing.insert(
                        app_name.clone(),
                        super::launch_progress::LaunchStep {
                            message: tr!("Downloading and installing...").into(),
                            cancellable: false,
                            cancelling: false,
                        },
                    );
                } else {
                    this.preparing.remove(app_name);
                }
                this.refresh_groups();
            }
            crate::services::gameyfin::GameyfinEvent::Changed => cx.notify(),
        })
        .detach();
        cx.observe(&svc, |_, _, cx| cx.notify()).detach();
    }

    pub(super) fn watch_drop(&mut self, cx: &mut Context<Self>) {
        let svc = crate::services::drop(cx);
        svc.update(cx, |s, cx| s.start_periodic(cx));
        cx.subscribe(&svc, |this, _, ev: &crate::services::drop::DropEvent, cx| match ev {
            crate::services::drop::DropEvent::LibraryChanged => this.reload(cx),
            crate::services::drop::DropEvent::Toast { title, text, error } => {
                let text = if title == "Drop" { text.clone() } else { format!("{title}: {text}") };
                this.show_toast(text, *error, cx);
            }
            crate::services::drop::DropEvent::InstallState { app_name, active } => {
                if *active {
                    this.preparing.insert(
                        app_name.clone(),
                        super::launch_progress::LaunchStep {
                            message: tr!("Downloading and installing...").into(),
                            cancellable: true,
                            cancelling: false,
                        },
                    );
                } else {
                    this.preparing.remove(app_name);
                }
                this.refresh_groups();
            }
            crate::services::drop::DropEvent::InstallProgress { app_name, done, total } => {
                if this.preparing.contains_key(app_name) {
                    let message = if *total > 1 {
                        let percent = ((*done as f64 / *total as f64) * 100.).round() as u32;
                        tr!("Downloading and installing... {percent}%", percent = percent).into()
                    } else {
                        tr!("Downloading and installing...").into()
                    };
                    this.set_launch_step(app_name.clone(), message, true);
                    cx.notify();
                }
            }
            crate::services::drop::DropEvent::Changed => cx.notify(),
        })
        .detach();
        cx.observe(&svc, |_, _, cx| cx.notify()).detach();
    }
}
