//! Widgets of the sharing target form.

use gpui_kit::component::input::InputState;
use gpui_kit::prelude::*;
use gpui_kit::*;
use heronite_i18n::tr;

use super::super::SettingsPage;
use super::forms::{
    form_to_target, ExtractForm, FormControls, FormKind, FtpTlsForm, HttpBodyForm, HttpMethodForm, SftpAuthForm, WebDavAuthForm,
};
use crate::theme;
use crate::ui::controls::{action_button, button_sm, caption, card_box, mini_checkbox, tabs, toggle, ButtonKind};
use crate::ui::fields::{input_box, labeled, meta_text};

fn update_kind(d: &mut SettingsPage, kind: FormKind, cx: &mut Context<SettingsPage>) {
    d.share.form.kind = kind;
    d.share.error = None;
    cx.notify();
}

fn small_input(input: &Entity<InputState>) -> Div {
    div().flex_1().min_w(px(90.)).child(input_box(input, false))
}

fn pair_rows(d: &SettingsPage, cx: &mut Context<SettingsPage>) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    for (i, row) in d.share.form.http_fields.iter().enumerate() {
        rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(small_input(&row.name))
                .child(small_input(&row.value))
                .child(
                    button_sm(SharedString::from(format!("share-field-remove-{i}")), ButtonKind::Glass, tr!("Remove"))
                        .cursor_pointer()
                        .on_click(cx.listener(move |d, _, _, cx| {
                            if i < d.share.form.http_fields.len() {
                                d.share.form.http_fields.remove(i);
                            }
                            cx.notify();
                        })),
                )
                .into_any_element(),
        );
    }
    rows
}

fn header_rows(d: &SettingsPage, cx: &mut Context<SettingsPage>) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    for (i, row) in d.share.form.http_headers.iter().enumerate() {
        let secret = row.secret;
        rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(small_input(&row.name))
                .child(small_input(&row.value))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .child(
                            mini_checkbox(SharedString::from(format!("share-header-secret-{i}")), secret, false).cursor_pointer().on_click(
                                cx.listener(move |d, _, window, cx| {
                                    if let Some(row) = d.share.form.http_headers.get_mut(i) {
                                        row.secret = !row.secret;
                                        let value = row.value.read(cx).value();
                                        let masked = row.secret;
                                        row.value = cx.new(|cx| InputState::new(window, cx).masked(masked));
                                        row.value.update(cx, |s, cx| s.set_value(value, window, cx));
                                    }
                                    cx.notify();
                                }),
                            ),
                        )
                        .child(div().text_size(px(12.)).text_color(theme::text_dim()).child(tr!("Secret"))),
                )
                .child(
                    button_sm(SharedString::from(format!("share-header-remove-{i}")), ButtonKind::Glass, tr!("Remove"))
                        .cursor_pointer()
                        .on_click(cx.listener(move |d, _, _, cx| {
                            if i < d.share.form.http_headers.len() {
                                d.share.form.http_headers.remove(i);
                            }
                            cx.notify();
                        })),
                )
                .into_any_element(),
        );
    }
    rows
}

fn host_key_lines(d: &SettingsPage, cx: &mut Context<SettingsPage>) -> Vec<AnyElement> {
    let mut out = Vec::new();
    let pinned = d.share.form.sftp_host_key.clone();
    if !pinned.is_empty() {
        out.push(
            div()
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(theme::text_dim())
                .child(tr!("Pinned: {fingerprint}", fingerprint = pinned.clone()))
                .into_any_element(),
        );
    }
    if let Some(key) = &d.share.form.host_key.fetched {
        let changed = !pinned.is_empty() && pinned != key.fingerprint;
        out.push(
            div()
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(if changed { theme::danger_text() } else { theme::text_dim() })
                .child(if changed {
                    tr!(
                        "The fetched key differs from the pinned one: {algorithm} {fingerprint}",
                        algorithm = key.algorithm.clone(),
                        fingerprint = key.fingerprint.clone()
                    )
                } else {
                    tr!("Fetched key: {algorithm} {fingerprint}", algorithm = key.algorithm.clone(), fingerprint = key.fingerprint.clone())
                })
                .into_any_element(),
        );
    }
    if let Some(error) = &d.share.form.host_key.error {
        out.push(div().text_size(px(12.)).line_height(px(16.)).text_color(theme::danger_text()).child(error.clone()).into_any_element());
    }
    let can_trust = d.share.form.host_key.fetched.is_some();
    let mut buttons = div().flex().items_center().gap(px(8.)).child(button_sm_enabled_for_host(cx));
    if can_trust {
        buttons = buttons.child(button_sm("share-hostkey-trust", ButtonKind::Glass, tr!("Trust this key")).cursor_pointer().on_click(
            cx.listener(|d, _, _, cx| {
                if let Some(key) = &d.share.form.host_key.fetched {
                    d.share.form.sftp_host_key = key.fingerprint.clone();
                }
                cx.notify();
            }),
        ));
    }
    out.push(buttons.into_any_element());
    out
}

fn button_sm_enabled_for_host(cx: &mut Context<SettingsPage>) -> Stateful<Div> {
    let checking = cx.entity().read(cx).share.form.host_key.checking;
    let label = if checking { tr!("Checking…") } else { tr!("Check host key") };
    let b = button_sm("share-hostkey-check", ButtonKind::Glass, label);
    if checking {
        b.opacity(0.45).cursor_not_allowed()
    } else {
        b.cursor_pointer().on_click(cx.listener(|d, _, _, cx| fetch_host_key(d, cx)))
    }
}

fn fetch_host_key(d: &mut SettingsPage, cx: &mut Context<SettingsPage>) {
    let host = d.share.form.sftp_host.read(cx).value().trim().to_string();
    let port = d.share.form.sftp_port.read(cx).value().trim().parse::<u16>().unwrap_or(22);
    if host.is_empty() {
        d.share.form.host_key.error = Some(tr!("Enter the host").to_string());
        cx.notify();
        return;
    }
    d.share.form.host_key.checking = true;
    d.share.form.host_key.error = None;
    cx.notify();
    cx.spawn(async move |this, cx| {
        let outcome = cx.background_executor().spawn(async move { heronite_share::sftp::fetch_host_key(&host, port) }).await;
        if let Some(this) = this.upgrade() {
            this.update(cx, |d, cx| {
                d.share.form.host_key.checking = false;
                match outcome {
                    Ok(key) => {
                        d.share.form.host_key.fetched = Some(key);
                        d.share.form.host_key.error = None;
                    }
                    Err(e) => d.share.form.host_key.error = Some(e.text()),
                }
                cx.notify();
            });
        }
    })
    .detach();
}

fn render_s3(d: &SettingsPage, cx: &mut Context<SettingsPage>) -> Vec<AnyElement> {
    let form = &d.share.form;
    let style_items = [(true, tr!("Path style")), (false, tr!("Virtual host"))];
    // A target that still uses the automatic style shows the choice the
    // crate will make. Picking a tab pins it.
    let active_style = form.s3_path_style.unwrap_or_else(|| !form.s3_endpoint.read(cx).value().trim().is_empty());
    let entity = cx.entity().downgrade();
    vec![
        labeled(tr!("Endpoint (empty for AWS)"), input_box(&form.s3_endpoint, false)).into_any_element(),
        labeled(tr!("Region"), input_box(&form.s3_region, false)).into_any_element(),
        labeled(tr!("Bucket"), input_box(&form.s3_bucket, false)).into_any_element(),
        labeled(tr!("Access key"), input_box(&form.s3_access_key, false)).into_any_element(),
        labeled(tr!("Secret key"), input_box(&form.s3_secret_key, false)).into_any_element(),
        labeled(tr!("Session token (optional)"), input_box(&form.s3_session_token, false)).into_any_element(),
        labeled(
            tr!("Addressing"),
            tabs("share-s3-style", &style_items, active_style, move |v, _, cx| {
                entity
                    .update(cx, |d, cx| {
                        d.share.form.s3_path_style = Some(v);
                        cx.notify();
                    })
                    .ok();
            }),
        )
        .into_any_element(),
        crate::ui::controls::row(tr!("Public-read ACL"), Some(tr!("Uploaded objects can be read by anyone who has their link")), {
            let on = form.s3_acl_public_read;
            toggle("share-s3-acl", on, false).cursor_pointer().on_click(cx.listener(move |d, _, _, cx| {
                d.share.form.s3_acl_public_read = !on;
                cx.notify();
            }))
        })
        .into_any_element(),
        labeled(tr!("Presign lifetime in seconds"), input_box(&form.s3_presign_secs, false)).into_any_element(),
        meta_text(tr!("Empty uses seven days. Presigned links matter only when the public link base is empty.")).into_any_element(),
    ]
}

fn render_sftp(d: &SettingsPage, cx: &mut Context<SettingsPage>) -> Vec<AnyElement> {
    let form = &d.share.form;
    let auth_items =
        [(SftpAuthForm::Agent, tr!("Agent")), (SftpAuthForm::Password, tr!("Password")), (SftpAuthForm::KeyFile, tr!("Key file"))];
    let entity = cx.entity().downgrade();
    let mut out = vec![
        labeled(tr!("Host"), input_box(&form.sftp_host, false)).into_any_element(),
        labeled(tr!("Port"), input_box(&form.sftp_port, false)).into_any_element(),
        labeled(tr!("User"), input_box(&form.sftp_user, false)).into_any_element(),
        labeled(
            tr!("Authentication"),
            tabs("share-sftp-auth", &auth_items, form.sftp_auth, move |v, _, cx| {
                entity
                    .update(cx, |d, cx| {
                        d.share.form.sftp_auth = v;
                        cx.notify();
                    })
                    .ok();
            }),
        )
        .into_any_element(),
    ];
    if form.sftp_auth == SftpAuthForm::Password {
        out.push(labeled(tr!("Password"), input_box(&form.sftp_password, false)).into_any_element());
    }
    if form.sftp_auth == SftpAuthForm::KeyFile {
        out.push(labeled(tr!("Key file"), input_box(&form.sftp_key_file, false)).into_any_element());
        out.push(labeled(tr!("Passphrase"), input_box(&form.sftp_passphrase, false)).into_any_element());
    }
    out.push(caption(tr!("Host key")).into_any_element());
    out.extend(host_key_lines(d, cx));
    out
}

fn render_webdav(d: &SettingsPage, cx: &mut Context<SettingsPage>) -> Vec<AnyElement> {
    let form = &d.share.form;
    let auth_items = [(WebDavAuthForm::Password, tr!("Password")), (WebDavAuthForm::Bearer, tr!("Bearer token"))];
    let entity = cx.entity().downgrade();
    let mut out = vec![
        labeled(tr!("URL"), input_box(&form.webdav_url, false)).into_any_element(),
        labeled(
            tr!("Authentication"),
            tabs("share-webdav-auth", &auth_items, form.webdav_auth, move |v, _, cx| {
                entity
                    .update(cx, |d, cx| {
                        d.share.form.webdav_auth = v;
                        cx.notify();
                    })
                    .ok();
            }),
        )
        .into_any_element(),
    ];
    if form.webdav_auth == WebDavAuthForm::Password {
        out.push(labeled(tr!("User"), input_box(&form.webdav_user, false)).into_any_element());
        out.push(labeled(tr!("Password"), input_box(&form.webdav_password, false)).into_any_element());
    } else {
        out.push(labeled(tr!("Bearer token"), input_box(&form.webdav_token, false)).into_any_element());
    }
    out.push(
        crate::ui::controls::row(tr!("Nextcloud/ownCloud share links"), None, {
            let on = form.webdav_nextcloud_share;
            toggle("share-webdav-nextcloud", on, false).cursor_pointer().on_click(cx.listener(move |d, _, _, cx| {
                d.share.form.webdav_nextcloud_share = !on;
                cx.notify();
            }))
        })
        .into_any_element(),
    );
    out.push(meta_text(tr!("Uses the OCS API. The URL needs the /remote.php/dav/files/<user>/ path of the account.")).into_any_element());
    out
}

fn render_ftp(d: &SettingsPage, cx: &mut Context<SettingsPage>) -> Vec<AnyElement> {
    let form = &d.share.form;
    let tls_items = [(FtpTlsForm::None, tr!("None")), (FtpTlsForm::Explicit, tr!("Explicit")), (FtpTlsForm::Implicit, tr!("Implicit"))];
    let entity = cx.entity().downgrade();
    vec![
        labeled(tr!("Host"), input_box(&form.ftp_host, false)).into_any_element(),
        labeled(tr!("Port"), input_box(&form.ftp_port, false)).into_any_element(),
        labeled(tr!("User (empty for anonymous)"), input_box(&form.ftp_user, false)).into_any_element(),
        labeled(tr!("Password"), input_box(&form.ftp_password, false)).into_any_element(),
        labeled(
            tr!("TLS"),
            tabs("share-ftp-tls", &tls_items, form.ftp_tls, move |v, _, cx| {
                entity
                    .update(cx, |d, cx| {
                        d.share.form.ftp_tls = v;
                        cx.notify();
                    })
                    .ok();
            }),
        )
        .into_any_element(),
    ]
}

fn render_http(d: &SettingsPage, cx: &mut Context<SettingsPage>) -> Vec<AnyElement> {
    let form = &d.share.form;
    let method_items = [(HttpMethodForm::Post, "POST"), (HttpMethodForm::Put, "PUT")];
    let body_items = [(HttpBodyForm::Multipart, tr!("Multipart")), (HttpBodyForm::Raw, tr!("Raw body"))];
    let extract_items =
        [(ExtractForm::Body, tr!("Body")), (ExtractForm::JsonPointer, tr!("JSON pointer")), (ExtractForm::Regex, tr!("Regex"))];
    let entity = cx.entity().downgrade();
    let entity_body = cx.entity().downgrade();
    let entity_extract = cx.entity().downgrade();
    let mut out = vec![
        labeled(tr!("URL"), input_box(&form.http_url, false)).into_any_element(),
        labeled(
            tr!("Method"),
            tabs("share-http-method", &method_items, form.http_method, move |v, _, cx| {
                entity
                    .update(cx, |d, cx| {
                        d.share.form.http_method = v;
                        cx.notify();
                    })
                    .ok();
            }),
        )
        .into_any_element(),
        labeled(
            tr!("Body"),
            tabs("share-http-body", &body_items, form.http_body, move |v, _, cx| {
                entity_body
                    .update(cx, |d, cx| {
                        d.share.form.http_body = v;
                        cx.notify();
                    })
                    .ok();
            }),
        )
        .into_any_element(),
    ];
    if form.http_body == HttpBodyForm::Multipart {
        out.push(labeled(tr!("Multipart file field"), input_box(&form.http_multipart_field, false)).into_any_element());
        out.push(caption(tr!("Extra form fields")).into_any_element());
        out.extend(pair_rows(d, cx));
        out.push(
            div()
                .flex()
                .child(button_sm("share-field-add", ButtonKind::Glass, tr!("Add field")).cursor_pointer().on_click(cx.listener(
                    |d, _, window, cx| {
                        d.share.form.http_fields.push(FormControls::pair_input(window, cx, "", ""));
                        cx.notify();
                    },
                )))
                .into_any_element(),
        );
    }
    out.push(caption(tr!("Headers")).into_any_element());
    out.extend(header_rows(d, cx));
    out.push(
        div()
            .flex()
            .child(button_sm("share-header-add", ButtonKind::Glass, tr!("Add header")).cursor_pointer().on_click(cx.listener(
                |d, _, window, cx| {
                    d.share.form.http_headers.push(FormControls::header_input(window, cx, "", "", false));
                    cx.notify();
                },
            )))
            .into_any_element(),
    );
    out.push(
        labeled(
            tr!("Read the link from"),
            tabs("share-http-extract", &extract_items, form.http_extract, move |v, _, cx| {
                entity_extract
                    .update(cx, |d, cx| {
                        d.share.form.http_extract = v;
                        cx.notify();
                    })
                    .ok();
            }),
        )
        .into_any_element(),
    );
    match form.http_extract {
        ExtractForm::Body => {}
        ExtractForm::JsonPointer => out.push(labeled(tr!("JSON pointer"), input_box(&form.http_json_pointer, false)).into_any_element()),
        ExtractForm::Regex => out.push(labeled(tr!("Regex with one capture group"), input_box(&form.http_regex, false)).into_any_element()),
    }
    out.push(labeled(tr!("Link template"), input_box(&form.http_link_template, false)).into_any_element());
    out.push(meta_text(tr!("Use {value} where the extracted value belongs. Empty uses the value as the link.")).into_any_element());
    out.push(labeled(tr!("Delete URL template (optional)"), input_box(&form.http_delete_url, false)).into_any_element());
    out.push(labeled(tr!("Delete method"), input_box(&form.http_delete_method, false)).into_any_element());
    out
}

fn save_form(d: &mut SettingsPage, window: &mut Window, cx: &mut Context<SettingsPage>) {
    let (form, existing) = {
        let svc = d.share.svc.read(cx);
        let editing = d.share.editing.clone();
        let existing = editing.as_deref().and_then(|id| svc.targets().get(id)).cloned();
        let id = editing.clone().unwrap_or_else(|| svc.targets().new_id());
        (d.share.form.form_state(&id, cx), existing)
    };
    match form_to_target(&form, existing.as_ref()) {
        Ok(target) => {
            let svc = d.share.svc.clone();
            match svc.update(cx, |s, cx| s.upsert_target(target, cx)) {
                Ok(()) => {
                    d.share.error = None;
                    super::clear_form(d, window, cx);
                }
                Err(e) => d.share.error = Some(e.to_string()),
            }
        }
        Err(e) => d.share.error = Some(e),
    }
    cx.notify();
}

/// The add/edit card. Kind switching keeps the common fields because they are
/// the same inputs for every kind.
pub fn render_form(d: &SettingsPage, cx: &mut Context<SettingsPage>) -> AnyElement {
    let form = &d.share.form;
    let editing = d.share.editing.is_some();
    let kind_items =
        [(FormKind::S3, "S3"), (FormKind::Sftp, "SFTP"), (FormKind::WebDav, "WebDAV"), (FormKind::Ftp, "FTP"), (FormKind::Http, "HTTP")];
    let entity = cx.entity().downgrade();
    // Collect erased children in a vector instead of chaining `.child` calls.
    // Every chained child becomes part of the element type here; with this many
    // fields the type grows beyond rustc's normal recursion limit.
    let mut fields = vec![
        meta_text(if editing {
            tr!("Change the target, then save it. Empty secret fields keep the values already stored.")
        } else {
            tr!("Add a server for uploads. Secrets stay sealed on this computer.")
        })
        .into_any_element(),
        labeled(
            tr!("Type"),
            tabs("share-kind", &kind_items, form.kind, move |v, _, cx| {
                entity.update(cx, |d, cx| update_kind(d, v, cx)).ok();
            }),
        )
        .into_any_element(),
        labeled(tr!("Name"), input_box(&form.name, false)).into_any_element(),
        labeled(tr!("Folder"), input_box(&form.folder, false)).into_any_element(),
        labeled(tr!("Public link base"), input_box(&form.public_url, false)).into_any_element(),
        meta_text(tr!("Use {name} for the uploaded file name and {ext} for its extension.")).into_any_element(),
        labeled(tr!("CA file"), input_box(&form.ca_file, false)).into_any_element(),
        meta_text(tr!("A PEM file with extra certificates. Leave it empty for the system roots.")).into_any_element(),
        crate::ui::controls::row(tr!("Do not verify certificates"), None, {
            let on = form.insecure;
            toggle("share-insecure", on, false).cursor_pointer().on_click(cx.listener(move |d, _, _, cx| {
                d.share.form.insecure = !on;
                cx.notify();
            }))
        })
        .into_any_element(),
        div()
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(theme::danger_text())
            .child(tr!("Only turn this on for a server you control. Anyone on the network can read or change the traffic."))
            .into_any_element(),
    ];
    let kind_fields = match form.kind {
        FormKind::S3 => render_s3(d, cx),
        FormKind::Sftp => render_sftp(d, cx),
        FormKind::WebDav => render_webdav(d, cx),
        FormKind::Ftp => render_ftp(d, cx),
        FormKind::Http => render_http(d, cx),
    };
    fields.extend(kind_fields);
    if let Some(error) = &d.share.error {
        fields.push(div().text_size(px(13.)).line_height(px(17.)).text_color(theme::danger_text()).child(error.clone()).into_any_element());
    }
    fields.push(
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(action_button(
                "share-save",
                ButtonKind::Primary,
                if editing { tr!("Save target") } else { tr!("Add target") },
                true,
                cx.listener(|d, _, window, cx| save_form(d, window, cx)),
            ))
            .children(editing.then(|| {
                action_button(
                    "share-cancel-edit",
                    ButtonKind::Glass,
                    tr!("Cancel"),
                    true,
                    cx.listener(|d, _, window, cx| super::clear_form(d, window, cx)),
                )
            }))
            .into_any_element(),
    );
    card_box().py(px(12.)).gap(px(12.)).children(fields).into_any_element()
}
