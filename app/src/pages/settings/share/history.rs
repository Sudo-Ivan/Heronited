//! The share history card: what went up, copy/open actions and remote delete.

use gpui_kit::prelude::*;
use gpui_kit::*;
use heronite_data::maintenance::human_size;
use heronite_i18n::tr;
use heronite_share::Record;

use super::super::SettingsPage;
use crate::ui::controls::{button_sm, caption, card, row, ButtonKind};
use crate::ui::fields::meta_text;

fn short_date(at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(at)
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| at.chars().take(16).collect())
}

fn short_url(url: &str) -> String {
    let mut out: String = url.chars().take(54).collect();
    if url.chars().count() > 54 {
        out.push('…');
    }
    out
}

fn record_row(d: &SettingsPage, record: Record, cx: &mut Context<SettingsPage>) -> AnyElement {
    let id = record.id.clone();
    let confirming = d.share.confirm_delete.as_deref() == Some(id.as_str());
    let mut parts = vec![record.target_name.clone(), short_date(&record.at), human_size(record.size), short_url(&record.url)];
    if let Some(game) = record.game.as_ref().filter(|g| !g.trim().is_empty()) {
        parts.insert(0, game.clone());
    }
    let desc = parts.join("  ·  ");
    let url_copy = record.url.clone();
    let url_open = record.url.clone();
    let id_delete = id.clone();
    let buttons = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            button_sm(SharedString::from(format!("share-copy-{id}")), ButtonKind::Glass, tr!("Copy link"))
                .cursor_pointer()
                .on_click(cx.listener(move |_, _, _, cx| crate::services::share::copy_link(cx, &url_copy))),
        )
        .child(
            button_sm(SharedString::from(format!("share-open-{id}")), ButtonKind::Glass, tr!("Open"))
                .cursor_pointer()
                .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url_open))),
        )
        .child(
            button_sm(
                SharedString::from(format!("share-delete-{id}")),
                if confirming { ButtonKind::Danger } else { ButtonKind::Glass },
                if confirming { tr!("Delete?") } else { tr!("Delete") },
            )
            .cursor_pointer()
            .on_click(cx.listener(move |d, _, _, cx| {
                if d.share.confirm_delete.as_deref() == Some(id_delete.as_str()) {
                    let svc = d.share.svc.clone();
                    svc.update(cx, |s, cx| s.delete_record(&id_delete, cx));
                    d.share.confirm_delete = None;
                } else {
                    d.share.confirm_delete = Some(id_delete.clone());
                }
                cx.notify();
            })),
        );
    row(record.file_name.clone(), Some(&desc), buttons).into_any_element()
}

/// Newest first, with an empty state when nothing was shared yet.
pub fn render_history(d: &SettingsPage, cx: &mut Context<SettingsPage>) -> AnyElement {
    let svc = d.share.svc.clone();
    let records = svc.update(cx, |s, cx| s.history(cx));
    let mut out = div().flex().flex_col().child(caption(tr!("History")));
    if records.is_empty() {
        out = out.child(meta_text(tr!("Nothing shared yet.")));
    } else {
        out = out.child(card(records.into_iter().map(|record| record_row(d, record, cx))));
    }
    out.into_any_element()
}
