//! Plain target-form state and its conversion to a [`Target`].
//!
//! The desktop widgets live in the app; this pure half is here so editing a
//! target can be tested without compiling the interface test harness.

use heronite_i18n::tr;

use crate::config::seal_secret;
use crate::{Extract, Ftp, FtpTls, Http, HttpBody, HttpHeader, Kind, Sftp, SftpAuth, Target, TlsOptions, WebDav, S3};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FormKind {
    #[default]
    S3,
    Sftp,
    WebDav,
    Ftp,
    Http,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SftpAuthForm {
    #[default]
    Agent,
    Password,
    KeyFile,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WebDavAuthForm {
    #[default]
    Password,
    Bearer,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FtpTlsForm {
    #[default]
    None,
    Explicit,
    Implicit,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HttpMethodForm {
    #[default]
    Post,
    Put,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HttpBodyForm {
    #[default]
    Multipart,
    Raw,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExtractForm {
    #[default]
    Body,
    JsonPointer,
    Regex,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeaderForm {
    pub name: String,
    pub value: String,
    pub secret: bool,
}

/// Everything the form knows, as plain values. Secret fields are the newly
/// typed plain text; an empty one keeps the sealed value of `existing`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormState {
    pub id: String,
    pub kind: FormKind,
    pub name: String,
    pub folder: String,
    pub public_url: String,
    pub ca_file: String,
    pub insecure: bool,
    pub s3_endpoint: String,
    pub s3_region: String,
    pub s3_bucket: String,
    pub s3_access_key: String,
    pub s3_secret_key: String,
    pub s3_session_token: String,
    /// `None` is the crate's automatic style. The two tabs show the choice
    /// it resolves to and pin it once the user picks one.
    pub s3_path_style: Option<bool>,
    pub s3_acl_public_read: bool,
    pub s3_presign_secs: String,
    pub sftp_host: String,
    pub sftp_port: String,
    pub sftp_user: String,
    pub sftp_auth: SftpAuthForm,
    pub sftp_password: String,
    pub sftp_key_file: String,
    pub sftp_passphrase: String,
    pub sftp_host_key: String,
    pub webdav_url: String,
    pub webdav_user: String,
    pub webdav_auth: WebDavAuthForm,
    pub webdav_password: String,
    pub webdav_token: String,
    pub webdav_nextcloud_share: bool,
    pub ftp_host: String,
    pub ftp_port: String,
    pub ftp_user: String,
    pub ftp_password: String,
    pub ftp_tls: FtpTlsForm,
    pub http_url: String,
    pub http_method: HttpMethodForm,
    pub http_body: HttpBodyForm,
    pub http_multipart_field: String,
    pub http_fields: Vec<(String, String)>,
    pub http_headers: Vec<HeaderForm>,
    pub http_extract: ExtractForm,
    pub http_json_pointer: String,
    pub http_regex: String,
    pub http_link_template: String,
    pub http_delete_url: String,
    pub http_delete_method: String,
}

impl Default for FormState {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: FormKind::S3,
            name: String::new(),
            folder: String::new(),
            public_url: String::new(),
            ca_file: String::new(),
            insecure: false,
            s3_endpoint: String::new(),
            s3_region: String::new(),
            s3_bucket: String::new(),
            s3_access_key: String::new(),
            s3_secret_key: String::new(),
            s3_session_token: String::new(),
            s3_path_style: None,
            s3_acl_public_read: false,
            s3_presign_secs: String::new(),
            sftp_host: String::new(),
            sftp_port: "22".into(),
            sftp_user: String::new(),
            sftp_auth: SftpAuthForm::Agent,
            sftp_password: String::new(),
            sftp_key_file: String::new(),
            sftp_passphrase: String::new(),
            sftp_host_key: String::new(),
            webdav_url: String::new(),
            webdav_user: String::new(),
            webdav_auth: WebDavAuthForm::Password,
            webdav_password: String::new(),
            webdav_token: String::new(),
            webdav_nextcloud_share: false,
            ftp_host: String::new(),
            ftp_port: "21".into(),
            ftp_user: String::new(),
            ftp_password: String::new(),
            ftp_tls: FtpTlsForm::None,
            http_url: String::new(),
            http_method: HttpMethodForm::Post,
            http_body: HttpBodyForm::Multipart,
            http_multipart_field: "file".into(),
            http_fields: Vec::new(),
            http_headers: Vec::new(),
            http_extract: ExtractForm::Body,
            http_json_pointer: String::new(),
            http_regex: String::new(),
            http_link_template: String::new(),
            http_delete_url: String::new(),
            http_delete_method: "DELETE".into(),
        }
    }
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn sealed_or_kept(input: &str, existing: Option<&str>) -> Result<String, String> {
    if input.trim().is_empty() {
        Ok(existing.unwrap_or_default().to_string())
    } else {
        seal_secret(input).map_err(|e| e.text())
    }
}

fn parse_port(raw: &str, default: u16) -> Result<u16, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(default);
    }
    let port: u16 = raw.parse().map_err(|_| tr!("Enter a port from 1 to 65535").to_string())?;
    if port == 0 {
        return Err(tr!("Enter a port from 1 to 65535").to_string());
    }
    Ok(port)
}

/// Builds the target the form describes. New non-empty secrets are sealed,
/// empty secret inputs keep the sealed value of `existing`, and validation
/// failure means nothing is written.
pub fn form_to_target(form: &FormState, existing: Option<&Target>) -> Result<Target, String> {
    let tls = TlsOptions { ca_file: non_empty(&form.ca_file), insecure: form.insecure };
    let kind = match form.kind {
        FormKind::S3 => {
            let old = existing.and_then(|t| match &t.kind {
                Kind::S3(s) => Some(s),
                _ => None,
            });
            let presign_secs = match form.s3_presign_secs.trim() {
                "" => None,
                raw => Some(raw.parse::<u32>().map_err(|_| tr!("Enter the presign lifetime in seconds").to_string())?),
            };
            Kind::S3(S3 {
                endpoint: form.s3_endpoint.trim().into(),
                region: form.s3_region.trim().into(),
                bucket: form.s3_bucket.trim().into(),
                access_key: form.s3_access_key.trim().into(),
                sealed_secret_key: sealed_or_kept(&form.s3_secret_key, old.map(|s| s.sealed_secret_key.as_str()))?,
                sealed_session_token: sealed_or_kept(&form.s3_session_token, old.map(|s| s.sealed_session_token.as_str()))?,
                path_style: form.s3_path_style,
                acl_public_read: form.s3_acl_public_read,
                presign_secs,
            })
        }
        FormKind::Sftp => {
            let old = existing.and_then(|t| match &t.kind {
                Kind::Sftp(s) => Some(s),
                _ => None,
            });
            let auth = match form.sftp_auth {
                SftpAuthForm::Agent => SftpAuth::Agent,
                SftpAuthForm::Password => SftpAuth::Password,
                SftpAuthForm::KeyFile => SftpAuth::KeyFile,
            };
            Kind::Sftp(Sftp {
                host: form.sftp_host.trim().into(),
                port: parse_port(&form.sftp_port, 22)?,
                user: form.sftp_user.trim().into(),
                auth,
                sealed_password: if auth == SftpAuth::Password {
                    sealed_or_kept(&form.sftp_password, old.map(|s| s.sealed_password.as_str()))?
                } else {
                    String::new()
                },
                key_file: if auth == SftpAuth::KeyFile { form.sftp_key_file.trim().into() } else { String::new() },
                sealed_passphrase: if auth == SftpAuth::KeyFile {
                    sealed_or_kept(&form.sftp_passphrase, old.map(|s| s.sealed_passphrase.as_str()))?
                } else {
                    String::new()
                },
                host_key: form.sftp_host_key.trim().into(),
            })
        }
        FormKind::WebDav => {
            let old = existing.and_then(|t| match &t.kind {
                Kind::WebDav(w) => Some(w),
                _ => None,
            });
            Kind::WebDav(WebDav {
                url: form.webdav_url.trim().into(),
                user: form.webdav_user.trim().into(),
                sealed_password: if form.webdav_auth == WebDavAuthForm::Password {
                    sealed_or_kept(&form.webdav_password, old.map(|w| w.sealed_password.as_str()))?
                } else {
                    String::new()
                },
                sealed_token: if form.webdav_auth == WebDavAuthForm::Bearer {
                    sealed_or_kept(&form.webdav_token, old.map(|w| w.sealed_token.as_str()))?
                } else {
                    String::new()
                },
                nextcloud_share: form.webdav_nextcloud_share,
            })
        }
        FormKind::Ftp => {
            let old = existing.and_then(|t| match &t.kind {
                Kind::Ftp(f) => Some(f),
                _ => None,
            });
            Kind::Ftp(Ftp {
                host: form.ftp_host.trim().into(),
                port: parse_port(&form.ftp_port, 21)?,
                user: form.ftp_user.trim().into(),
                sealed_password: sealed_or_kept(&form.ftp_password, old.map(|f| f.sealed_password.as_str()))?,
                tls: match form.ftp_tls {
                    FtpTlsForm::None => FtpTls::None,
                    FtpTlsForm::Explicit => FtpTls::Explicit,
                    FtpTlsForm::Implicit => FtpTls::Implicit,
                },
            })
        }
        FormKind::Http => {
            let old = existing.and_then(|t| match &t.kind {
                Kind::Http(h) => Some(h),
                _ => None,
            });
            let mut headers = Vec::new();
            for h in &form.http_headers {
                if h.name.trim().is_empty() && h.value.trim().is_empty() {
                    continue;
                }
                let kept =
                    old.and_then(|old| old.headers.iter().find(|o| o.name == h.name.trim() && o.secret).map(|o| o.sealed_value.as_str()));
                headers.push(HttpHeader {
                    name: h.name.trim().into(),
                    value: if h.secret { String::new() } else { h.value.trim().into() },
                    secret: h.secret,
                    sealed_value: if h.secret { sealed_or_kept(&h.value, kept)? } else { String::new() },
                });
            }
            let body = match form.http_body {
                HttpBodyForm::Multipart => HttpBody::Multipart {
                    field: {
                        let field = form.http_multipart_field.trim();
                        if field.is_empty() {
                            "file".into()
                        } else {
                            field.into()
                        }
                    },
                    fields: form
                        .http_fields
                        .iter()
                        .filter(|(name, value)| !name.trim().is_empty() || !value.trim().is_empty())
                        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
                        .collect(),
                },
                HttpBodyForm::Raw => HttpBody::Raw,
            };
            let extract = match form.http_extract {
                ExtractForm::Body => Extract::Body,
                ExtractForm::JsonPointer => Extract::JsonPointer { pointer: form.http_json_pointer.trim().into() },
                ExtractForm::Regex => Extract::Regex { pattern: form.http_regex.trim().into() },
            };
            Kind::Http(Http {
                url: form.http_url.trim().into(),
                method: match form.http_method {
                    HttpMethodForm::Post => "POST".into(),
                    HttpMethodForm::Put => "PUT".into(),
                },
                headers,
                body,
                extract,
                link_template: form.http_link_template.trim().into(),
                delete_url: form.http_delete_url.trim().into(),
                delete_method: form.http_delete_method.trim().to_ascii_uppercase(),
            })
        }
    };
    let target = Target {
        id: form.id.trim().into(),
        name: form.name.trim().into(),
        enabled: existing.map(|t| t.enabled).unwrap_or(true),
        kind,
        public_url: form.public_url.trim().into(),
        folder: form.folder.trim().into(),
        tls,
    };
    target.validate().map_err(|e| e.text())?;
    Ok(target)
}

/// Edit state of a target. Secrets come back empty, which keeps them on save.
pub fn target_to_form(target: &Target) -> FormState {
    let mut form = FormState {
        id: target.id.clone(),
        name: target.name.clone(),
        folder: target.folder.clone(),
        public_url: target.public_url.clone(),
        ca_file: target.tls.ca_file.clone().unwrap_or_default(),
        insecure: target.tls.insecure,
        ..Default::default()
    };
    match &target.kind {
        Kind::S3(s) => {
            form.kind = FormKind::S3;
            form.s3_endpoint = s.endpoint.clone();
            form.s3_region = s.region.clone();
            form.s3_bucket = s.bucket.clone();
            form.s3_access_key = s.access_key.clone();
            form.s3_path_style = s.path_style;
            form.s3_acl_public_read = s.acl_public_read;
            form.s3_presign_secs = s.presign_secs.map(|n| n.to_string()).unwrap_or_default();
        }
        Kind::Sftp(s) => {
            form.kind = FormKind::Sftp;
            form.sftp_host = s.host.clone();
            form.sftp_port = s.port.to_string();
            form.sftp_user = s.user.clone();
            form.sftp_auth = match s.auth {
                SftpAuth::Agent => SftpAuthForm::Agent,
                SftpAuth::Password => SftpAuthForm::Password,
                SftpAuth::KeyFile => SftpAuthForm::KeyFile,
            };
            form.sftp_key_file = s.key_file.clone();
            form.sftp_host_key = s.host_key.clone();
        }
        Kind::WebDav(w) => {
            form.kind = FormKind::WebDav;
            form.webdav_url = w.url.clone();
            form.webdav_user = w.user.clone();
            form.webdav_auth = if w.sealed_token.is_empty() { WebDavAuthForm::Password } else { WebDavAuthForm::Bearer };
            form.webdav_nextcloud_share = w.nextcloud_share;
        }
        Kind::Ftp(f) => {
            form.kind = FormKind::Ftp;
            form.ftp_host = f.host.clone();
            form.ftp_port = f.port.to_string();
            form.ftp_user = f.user.clone();
            form.ftp_tls = match f.tls {
                FtpTls::None => FtpTlsForm::None,
                FtpTls::Explicit => FtpTlsForm::Explicit,
                FtpTls::Implicit => FtpTlsForm::Implicit,
            };
        }
        Kind::Http(h) => {
            form.kind = FormKind::Http;
            form.http_url = h.url.clone();
            form.http_method = if h.method.eq_ignore_ascii_case("PUT") { HttpMethodForm::Put } else { HttpMethodForm::Post };
            match &h.body {
                HttpBody::Multipart { field, fields } => {
                    form.http_body = HttpBodyForm::Multipart;
                    form.http_multipart_field = field.clone();
                    form.http_fields = fields.clone();
                }
                HttpBody::Raw => form.http_body = HttpBodyForm::Raw,
            }
            form.http_headers = h
                .headers
                .iter()
                .map(|header| HeaderForm {
                    name: header.name.clone(),
                    value: if header.secret { String::new() } else { header.value.clone() },
                    secret: header.secret,
                })
                .collect();
            match &h.extract {
                Extract::Body => form.http_extract = ExtractForm::Body,
                Extract::JsonPointer { pointer } => {
                    form.http_extract = ExtractForm::JsonPointer;
                    form.http_json_pointer = pointer.clone();
                }
                Extract::Regex { pattern } => {
                    form.http_extract = ExtractForm::Regex;
                    form.http_regex = pattern.clone();
                }
            }
            form.http_link_template = h.link_template.clone();
            form.http_delete_url = h.delete_url.clone();
            form.http_delete_method = h.delete_method.clone();
        }
    }
    form
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::unseal;
    use crate::seal::{use_seal, PlainSeal};
    use std::sync::{Arc, Mutex};

    static SEAL_TEST: Mutex<()> = Mutex::new(());

    fn http_form() -> FormState {
        FormState {
            id: "h".into(),
            kind: FormKind::Http,
            name: "Hook".into(),
            http_url: "https://up.example/upload".into(),
            ..Default::default()
        }
    }

    #[test]
    fn http_target_maps_every_plain_field() {
        let _guard = SEAL_TEST.lock().unwrap();
        use_seal(Arc::new(PlainSeal));
        let mut form = http_form();
        form.folder = "shots".into();
        form.public_url = "https://cdn.example/{name}".into();
        form.ca_file = "/tmp/ca.pem".into();
        form.insecure = true;
        form.http_method = HttpMethodForm::Put;
        form.http_body = HttpBodyForm::Multipart;
        form.http_multipart_field = "upload".into();
        form.http_fields = vec![("a".into(), "b".into())];
        form.http_headers = vec![HeaderForm { name: "X-One".into(), value: "plain".into(), secret: false }];
        form.http_extract = ExtractForm::JsonPointer;
        form.http_json_pointer = "/data/url".into();
        form.http_link_template = "https://x/{value}".into();
        form.http_delete_url = "https://up.example/{value}".into();
        form.http_delete_method = "post".into();
        let target = form_to_target(&form, None).unwrap();
        assert_eq!(target.id, "h");
        assert_eq!(target.folder, "shots");
        assert_eq!(target.tls.ca_file.as_deref(), Some("/tmp/ca.pem"));
        assert!(target.tls.insecure);
        let Kind::Http(h) = target.kind else { panic!("http") };
        assert_eq!(h.method, "PUT");
        assert_eq!(h.delete_method, "POST");
        assert!(matches!(h.extract, Extract::JsonPointer { .. }));
        match h.body {
            HttpBody::Multipart { field, fields } => {
                assert_eq!(field, "upload");
                assert_eq!(fields, vec![("a".to_string(), "b".to_string())]);
            }
            _ => panic!("multipart"),
        }
    }

    #[test]
    fn empty_secrets_keep_existing_and_new_secrets_are_sealed() {
        let _guard = SEAL_TEST.lock().unwrap();
        use_seal(Arc::new(PlainSeal));
        let mut form = FormState {
            id: "s".into(),
            kind: FormKind::S3,
            s3_bucket: "b".into(),
            s3_access_key: "a".into(),
            s3_region: "eu-test-1".into(),
            s3_secret_key: "first".into(),
            public_url: "https://cdn.example/{name}".into(),
            ..Default::default()
        };
        let first = form_to_target(&form, None).unwrap();
        let Kind::S3(s3) = &first.kind else { panic!("s3") };
        assert_eq!(unseal(&s3.sealed_secret_key).unwrap(), "first");
        assert_ne!(s3.sealed_secret_key, "first");
        form.s3_secret_key.clear();
        let kept = form_to_target(&form, Some(&first)).unwrap();
        let Kind::S3(s3) = &kept.kind else { panic!("s3") };
        assert_eq!(unseal(&s3.sealed_secret_key).unwrap(), "first");
        form.s3_secret_key = "second".into();
        let replaced = form_to_target(&form, Some(&first)).unwrap();
        let Kind::S3(s3) = &replaced.kind else { panic!("s3") };
        assert_eq!(unseal(&s3.sealed_secret_key).unwrap(), "second");
    }

    #[test]
    fn secret_headers_keep_the_matching_header_only() {
        let _guard = SEAL_TEST.lock().unwrap();
        use_seal(Arc::new(PlainSeal));
        let mut form = http_form();
        form.http_headers = vec![HeaderForm { name: "Authorization".into(), value: "one".into(), secret: true }];
        let first = form_to_target(&form, None).unwrap();
        form.http_headers = vec![
            HeaderForm { name: "Authorization".into(), value: String::new(), secret: true },
            HeaderForm { name: "X-New".into(), value: "two".into(), secret: true },
        ];
        let next = form_to_target(&form, Some(&first)).unwrap();
        let Kind::Http(h) = next.kind else { panic!("http") };
        assert_eq!(h.headers[0].value().unwrap(), "one");
        assert_eq!(h.headers[1].value().unwrap(), "two");
        assert!(h.headers.iter().all(|h| h.value.is_empty()));
    }

    #[test]
    fn edit_round_trip_returns_empty_secrets() {
        let _guard = SEAL_TEST.lock().unwrap();
        use_seal(Arc::new(PlainSeal));
        let mut form = FormState {
            id: "f".into(),
            kind: FormKind::Ftp,
            ftp_host: "files.example".into(),
            ftp_password: "secret".into(),
            public_url: "https://files.example/{name}".into(),
            ..Default::default()
        };
        let target = form_to_target(&form, None).unwrap();
        let edit = target_to_form(&target);
        assert!(edit.ftp_password.is_empty());
        assert_eq!(edit.ftp_host, "files.example");
        form.ftp_password.clear();
        let saved = form_to_target(&edit, Some(&target)).unwrap();
        let Kind::Ftp(f) = saved.kind else { panic!("ftp") };
        assert_eq!(unseal(&f.sealed_password).unwrap(), "secret");
    }

    #[test]
    fn validation_failure_returns_an_error() {
        let _guard = SEAL_TEST.lock().unwrap();
        use_seal(Arc::new(PlainSeal));
        let form = FormState { id: "bad".into(), kind: FormKind::Http, ..Default::default() };
        assert!(form_to_target(&form, None).is_err());
    }

    #[test]
    fn ports_parse_and_reject_zero() {
        assert_eq!(parse_port("", 22).unwrap(), 22);
        assert_eq!(parse_port("2200", 22).unwrap(), 2200);
        assert!(parse_port("0", 22).is_err());
        assert!(parse_port("many", 22).is_err());
    }
}
