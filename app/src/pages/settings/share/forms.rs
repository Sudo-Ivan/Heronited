//! Input entities and widget state of the sharing target form. The plain
//! form state and conversion live in `heronite_share::form`, where their
//! tests do not have to compile the interface test harness.

use gpui_kit::component::input::InputState;
use gpui_kit::prelude::*;
use gpui_kit::*;

pub use heronite_share::form::{
    form_to_target, target_to_form, ExtractForm, FormKind, FormState, FtpTlsForm, HeaderForm, HttpBodyForm, HttpMethodForm, SftpAuthForm,
    WebDavAuthForm,
};

use super::super::SettingsPage;

/// Input row of the multipart extra fields list.
pub struct PairInputs {
    pub name: Entity<InputState>,
    pub value: Entity<InputState>,
}

/// Input row of the headers list.
pub struct HeaderInputs {
    pub name: Entity<InputState>,
    pub value: Entity<InputState>,
    pub secret: bool,
}

/// Fetched SFTP host key state of the form.
#[derive(Default)]
pub struct HostKeyState {
    pub checking: bool,
    pub fetched: Option<heronite_share::sftp::HostKey>,
    pub error: Option<String>,
}

/// Every input and choice of the target form.
pub struct FormControls {
    pub kind: FormKind,
    pub insecure: bool,
    pub s3_path_style: Option<bool>,
    pub s3_acl_public_read: bool,
    pub sftp_auth: SftpAuthForm,
    pub webdav_auth: WebDavAuthForm,
    pub webdav_nextcloud_share: bool,
    pub ftp_tls: FtpTlsForm,
    pub http_method: HttpMethodForm,
    pub http_body: HttpBodyForm,
    pub http_extract: ExtractForm,
    pub sftp_host_key: String,
    pub host_key: HostKeyState,
    pub name: Entity<InputState>,
    pub folder: Entity<InputState>,
    pub public_url: Entity<InputState>,
    pub ca_file: Entity<InputState>,
    pub s3_endpoint: Entity<InputState>,
    pub s3_region: Entity<InputState>,
    pub s3_bucket: Entity<InputState>,
    pub s3_access_key: Entity<InputState>,
    pub s3_secret_key: Entity<InputState>,
    pub s3_session_token: Entity<InputState>,
    pub s3_presign_secs: Entity<InputState>,
    pub sftp_host: Entity<InputState>,
    pub sftp_port: Entity<InputState>,
    pub sftp_user: Entity<InputState>,
    pub sftp_password: Entity<InputState>,
    pub sftp_key_file: Entity<InputState>,
    pub sftp_passphrase: Entity<InputState>,
    pub webdav_url: Entity<InputState>,
    pub webdav_user: Entity<InputState>,
    pub webdav_password: Entity<InputState>,
    pub webdav_token: Entity<InputState>,
    pub ftp_host: Entity<InputState>,
    pub ftp_port: Entity<InputState>,
    pub ftp_user: Entity<InputState>,
    pub ftp_password: Entity<InputState>,
    pub http_url: Entity<InputState>,
    pub http_multipart_field: Entity<InputState>,
    pub http_json_pointer: Entity<InputState>,
    pub http_regex: Entity<InputState>,
    pub http_link_template: Entity<InputState>,
    pub http_delete_url: Entity<InputState>,
    pub http_delete_method: Entity<InputState>,
    pub http_fields: Vec<PairInputs>,
    pub http_headers: Vec<HeaderInputs>,
}

fn input(window: &mut Window, cx: &mut Context<SettingsPage>, masked: bool) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).masked(masked))
}

impl FormControls {
    pub fn new(window: &mut Window, cx: &mut Context<SettingsPage>) -> Self {
        Self {
            kind: FormKind::S3,
            insecure: false,
            s3_path_style: None,
            s3_acl_public_read: false,
            sftp_auth: SftpAuthForm::Agent,
            webdav_auth: WebDavAuthForm::Password,
            webdav_nextcloud_share: false,
            ftp_tls: FtpTlsForm::None,
            http_method: HttpMethodForm::Post,
            http_body: HttpBodyForm::Multipart,
            http_extract: ExtractForm::Body,
            sftp_host_key: String::new(),
            host_key: HostKeyState::default(),
            name: input(window, cx, false),
            folder: input(window, cx, false),
            public_url: input(window, cx, false),
            ca_file: input(window, cx, false),
            s3_endpoint: input(window, cx, false),
            s3_region: input(window, cx, false),
            s3_bucket: input(window, cx, false),
            s3_access_key: input(window, cx, false),
            s3_secret_key: input(window, cx, true),
            s3_session_token: input(window, cx, true),
            s3_presign_secs: input(window, cx, false),
            sftp_host: input(window, cx, false),
            sftp_port: input(window, cx, false),
            sftp_user: input(window, cx, false),
            sftp_password: input(window, cx, true),
            sftp_key_file: input(window, cx, false),
            sftp_passphrase: input(window, cx, true),
            webdav_url: input(window, cx, false),
            webdav_user: input(window, cx, false),
            webdav_password: input(window, cx, true),
            webdav_token: input(window, cx, true),
            ftp_host: input(window, cx, false),
            ftp_port: input(window, cx, false),
            ftp_user: input(window, cx, false),
            ftp_password: input(window, cx, true),
            http_url: input(window, cx, false),
            http_multipart_field: input(window, cx, false),
            http_json_pointer: input(window, cx, false),
            http_regex: input(window, cx, false),
            http_link_template: input(window, cx, false),
            http_delete_url: input(window, cx, false),
            http_delete_method: input(window, cx, false),
            http_fields: Vec::new(),
            http_headers: Vec::new(),
        }
    }

    pub fn pair_input(window: &mut Window, cx: &mut Context<SettingsPage>, name: &str, value: &str) -> PairInputs {
        let name = name.to_string();
        let value = value.to_string();
        let name_input = input(window, cx, false);
        name_input.update(cx, |s, cx| s.set_value(name, window, cx));
        let value_input = input(window, cx, false);
        value_input.update(cx, |s, cx| s.set_value(value, window, cx));
        PairInputs { name: name_input, value: value_input }
    }

    pub fn header_input(window: &mut Window, cx: &mut Context<SettingsPage>, name: &str, value: &str, secret: bool) -> HeaderInputs {
        let name = name.to_string();
        let value = value.to_string();
        let name_input = input(window, cx, false);
        name_input.update(cx, |s, cx| s.set_value(name, window, cx));
        let value_input = input(window, cx, secret);
        value_input.update(cx, |s, cx| s.set_value(value, window, cx));
        HeaderInputs { name: name_input, value: value_input, secret }
    }

    fn set(input: &Entity<InputState>, value: String, window: &mut Window, cx: &mut Context<SettingsPage>) {
        input.update(cx, |s, cx| s.set_value(value, window, cx));
    }

    /// Fills every input from plain form state. Empty dynamic rows first go
    /// away, then the state's rows are recreated.
    pub fn fill(&mut self, form: &FormState, window: &mut Window, cx: &mut Context<SettingsPage>) {
        self.kind = form.kind;
        self.insecure = form.insecure;
        self.s3_path_style = form.s3_path_style;
        self.s3_acl_public_read = form.s3_acl_public_read;
        self.sftp_auth = form.sftp_auth;
        self.webdav_auth = form.webdav_auth;
        self.webdav_nextcloud_share = form.webdav_nextcloud_share;
        self.ftp_tls = form.ftp_tls;
        self.http_method = form.http_method;
        self.http_body = form.http_body;
        self.http_extract = form.http_extract;
        self.sftp_host_key = form.sftp_host_key.clone();
        self.host_key = HostKeyState::default();
        Self::set(&self.name, form.name.clone(), window, cx);
        Self::set(&self.folder, form.folder.clone(), window, cx);
        Self::set(&self.public_url, form.public_url.clone(), window, cx);
        Self::set(&self.ca_file, form.ca_file.clone(), window, cx);
        Self::set(&self.s3_endpoint, form.s3_endpoint.clone(), window, cx);
        Self::set(&self.s3_region, form.s3_region.clone(), window, cx);
        Self::set(&self.s3_bucket, form.s3_bucket.clone(), window, cx);
        Self::set(&self.s3_access_key, form.s3_access_key.clone(), window, cx);
        Self::set(&self.s3_secret_key, form.s3_secret_key.clone(), window, cx);
        Self::set(&self.s3_session_token, form.s3_session_token.clone(), window, cx);
        Self::set(&self.s3_presign_secs, form.s3_presign_secs.clone(), window, cx);
        Self::set(&self.sftp_host, form.sftp_host.clone(), window, cx);
        Self::set(&self.sftp_port, form.sftp_port.clone(), window, cx);
        Self::set(&self.sftp_user, form.sftp_user.clone(), window, cx);
        Self::set(&self.sftp_password, form.sftp_password.clone(), window, cx);
        Self::set(&self.sftp_key_file, form.sftp_key_file.clone(), window, cx);
        Self::set(&self.sftp_passphrase, form.sftp_passphrase.clone(), window, cx);
        Self::set(&self.webdav_url, form.webdav_url.clone(), window, cx);
        Self::set(&self.webdav_user, form.webdav_user.clone(), window, cx);
        Self::set(&self.webdav_password, form.webdav_password.clone(), window, cx);
        Self::set(&self.webdav_token, form.webdav_token.clone(), window, cx);
        Self::set(&self.ftp_host, form.ftp_host.clone(), window, cx);
        Self::set(&self.ftp_port, form.ftp_port.clone(), window, cx);
        Self::set(&self.ftp_user, form.ftp_user.clone(), window, cx);
        Self::set(&self.ftp_password, form.ftp_password.clone(), window, cx);
        Self::set(&self.http_url, form.http_url.clone(), window, cx);
        Self::set(&self.http_multipart_field, form.http_multipart_field.clone(), window, cx);
        Self::set(&self.http_json_pointer, form.http_json_pointer.clone(), window, cx);
        Self::set(&self.http_regex, form.http_regex.clone(), window, cx);
        Self::set(&self.http_link_template, form.http_link_template.clone(), window, cx);
        Self::set(&self.http_delete_url, form.http_delete_url.clone(), window, cx);
        Self::set(&self.http_delete_method, form.http_delete_method.clone(), window, cx);
        self.http_fields.clear();
        for (name, value) in &form.http_fields {
            self.http_fields.push(Self::pair_input(window, cx, name, value));
        }
        self.http_headers.clear();
        for h in &form.http_headers {
            self.http_headers.push(Self::header_input(window, cx, &h.name, &h.value, h.secret));
        }
    }

    fn value(input: &Entity<InputState>, cx: &App) -> String {
        input.read(cx).value().trim().to_string()
    }

    /// Reads the widgets into the plain state the pure conversion consumes.
    pub fn form_state(&self, id: &str, cx: &App) -> FormState {
        FormState {
            id: id.into(),
            kind: self.kind,
            name: Self::value(&self.name, cx),
            folder: Self::value(&self.folder, cx),
            public_url: Self::value(&self.public_url, cx),
            ca_file: Self::value(&self.ca_file, cx),
            insecure: self.insecure,
            s3_endpoint: Self::value(&self.s3_endpoint, cx),
            s3_region: Self::value(&self.s3_region, cx),
            s3_bucket: Self::value(&self.s3_bucket, cx),
            s3_access_key: Self::value(&self.s3_access_key, cx),
            s3_secret_key: Self::value(&self.s3_secret_key, cx),
            s3_session_token: Self::value(&self.s3_session_token, cx),
            s3_path_style: self.s3_path_style,
            s3_acl_public_read: self.s3_acl_public_read,
            s3_presign_secs: Self::value(&self.s3_presign_secs, cx),
            sftp_host: Self::value(&self.sftp_host, cx),
            sftp_port: Self::value(&self.sftp_port, cx),
            sftp_user: Self::value(&self.sftp_user, cx),
            sftp_auth: self.sftp_auth,
            sftp_password: Self::value(&self.sftp_password, cx),
            sftp_key_file: Self::value(&self.sftp_key_file, cx),
            sftp_passphrase: Self::value(&self.sftp_passphrase, cx),
            sftp_host_key: self.sftp_host_key.clone(),
            webdav_url: Self::value(&self.webdav_url, cx),
            webdav_user: Self::value(&self.webdav_user, cx),
            webdav_auth: self.webdav_auth,
            webdav_password: Self::value(&self.webdav_password, cx),
            webdav_token: Self::value(&self.webdav_token, cx),
            webdav_nextcloud_share: self.webdav_nextcloud_share,
            ftp_host: Self::value(&self.ftp_host, cx),
            ftp_port: Self::value(&self.ftp_port, cx),
            ftp_user: Self::value(&self.ftp_user, cx),
            ftp_password: Self::value(&self.ftp_password, cx),
            ftp_tls: self.ftp_tls,
            http_url: Self::value(&self.http_url, cx),
            http_method: self.http_method,
            http_body: self.http_body,
            http_multipart_field: Self::value(&self.http_multipart_field, cx),
            http_fields: self.http_fields.iter().map(|row| (Self::value(&row.name, cx), Self::value(&row.value, cx))).collect(),
            http_headers: self
                .http_headers
                .iter()
                .map(|row| HeaderForm { name: Self::value(&row.name, cx), value: Self::value(&row.value, cx), secret: row.secret })
                .collect(),
            http_extract: self.http_extract,
            http_json_pointer: Self::value(&self.http_json_pointer, cx),
            http_regex: Self::value(&self.http_regex, cx),
            http_link_template: Self::value(&self.http_link_template, cx),
            http_delete_url: Self::value(&self.http_delete_url, cx),
            http_delete_method: Self::value(&self.http_delete_method, cx),
        }
    }
}
