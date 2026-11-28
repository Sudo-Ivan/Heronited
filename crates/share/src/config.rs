//! Targets (`local_library/share.json`): where uploads go and how the link
//! back is built. Secrets are sealed for this user, base64 in `sealed_*`
//! fields, never plain on disk; Debug redacts them.

use serde::{Deserialize, Serialize};

use heronite_data::{json_store, DataPaths};
use heronite_i18n::tr;

use crate::error::{Result, ShareError};
use crate::naming::Naming;
use crate::seal::seal;
use crate::tls::TlsOptions;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Targets {
    #[serde(default)]
    pub targets: Vec<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default)]
    pub naming: Naming,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

pub fn file(paths: &DataPaths) -> std::path::PathBuf {
    paths.local_library().join("share.json")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(flatten)]
    pub kind: Kind,
    /// Link base template: `{name}` is the uploaded file's remote name (with
    /// the folder prefix), `{ext}` its extension. May be empty where the
    /// backend makes links itself (S3 presign, Nextcloud shares).
    #[serde(default)]
    pub public_url: String,
    /// Key prefix or remote directory the uploads live under.
    #[serde(default)]
    pub folder: String,
    #[serde(default)]
    pub tls: TlsOptions,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum Kind {
    S3(S3),
    Sftp(Sftp),
    WebDav(WebDav),
    Ftp(Ftp),
    Http(Http),
}

impl Kind {
    pub fn label(&self) -> &'static str {
        match self {
            Kind::S3(_) => "S3",
            Kind::Sftp(_) => "SFTP",
            Kind::WebDav(_) => "WebDAV",
            Kind::Ftp(_) => "FTP",
            Kind::Http(_) => "HTTP",
        }
    }
}

fn redact(sealed: &str) -> &'static str {
    if sealed.is_empty() {
        "(none)"
    } else {
        "(sealed)"
    }
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S3 {
    /// Empty means AWS: `s3.<region>.amazonaws.com`.
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub bucket: String,
    #[serde(default)]
    pub access_key: String,
    #[serde(default)]
    pub sealed_secret_key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sealed_session_token: String,
    /// Path-style (`endpoint/bucket/key`); the default when a custom endpoint
    /// is set. Virtual-host style is `bucket.endpoint/key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_style: Option<bool>,
    #[serde(default)]
    pub acl_public_read: bool,
    /// Seconds presigned links live when `public_url` is empty (max a week).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presign_secs: Option<u32>,
}

impl std::fmt::Debug for S3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key", &self.access_key)
            .field("sealed_secret_key", &redact(&self.sealed_secret_key))
            .field("sealed_session_token", &redact(&self.sealed_session_token))
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum SftpAuth {
    #[default]
    Agent,
    Password,
    KeyFile,
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sftp {
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub auth: SftpAuth,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sealed_password: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key_file: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sealed_passphrase: String,
    /// Pinned host key fingerprint, `SHA256:<base64>` OpenSSH style.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host_key: String,
}

fn default_ssh_port() -> u16 {
    22
}

impl std::fmt::Debug for Sftp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sftp")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("auth", &self.auth)
            .field("sealed_password", &redact(&self.sealed_password))
            .field("key_file", &self.key_file)
            .field("sealed_passphrase", &redact(&self.sealed_passphrase))
            .finish()
    }
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDav {
    /// The collection URL uploads go under (`.../remote.php/dav/files/me/`).
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub user: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sealed_password: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sealed_token: String,
    /// Make a public share link through the Nextcloud/ownCloud OCS API.
    #[serde(default)]
    pub nextcloud_share: bool,
}

impl std::fmt::Debug for WebDav {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebDav")
            .field("url", &self.url)
            .field("user", &self.user)
            .field("sealed_password", &redact(&self.sealed_password))
            .field("sealed_token", &redact(&self.sealed_token))
            .field("nextcloud_share", &self.nextcloud_share)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum FtpTls {
    #[default]
    None,
    /// AUTH TLS before login.
    Explicit,
    /// TLS from the first byte (port 990 usually).
    Implicit,
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ftp {
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_ftp_port")]
    pub port: u16,
    /// Empty user means anonymous.
    #[serde(default)]
    pub user: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sealed_password: String,
    // "tlsMode": plain `tls` is the target's shared TlsOptions already.
    #[serde(default, rename = "tlsMode")]
    pub tls: FtpTls,
}

fn default_ftp_port() -> u16 {
    21
}

impl std::fmt::Debug for Ftp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ftp")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("sealed_password", &redact(&self.sealed_password))
            .field("tls", &self.tls)
            .finish()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Http {
    #[serde(default)]
    pub url: String,
    /// POST or PUT.
    #[serde(default = "default_post")]
    pub method: String,
    #[serde(default)]
    pub headers: Vec<HttpHeader>,
    #[serde(default)]
    pub body: HttpBody,
    #[serde(default)]
    pub extract: Extract,
    /// `{value}` is the extracted piece; empty uses it as the link directly.
    #[serde(default)]
    pub link_template: String,
    /// Optional delete endpoint: `{value}` template, DELETE by default.
    #[serde(default)]
    pub delete_url: String,
    #[serde(default = "default_delete")]
    pub delete_method: String,
}

fn default_post() -> String {
    "POST".into()
}

fn default_delete() -> String {
    "DELETE".into()
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpHeader {
    pub name: String,
    /// Plain value, or empty when the secret field holds it.
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub secret: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sealed_value: String,
}

impl std::fmt::Debug for HttpHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpHeader")
            .field("name", &self.name)
            .field("value", &if self.secret { redact(&self.sealed_value) } else { self.value.as_str() })
            .finish()
    }
}

impl HttpHeader {
    pub fn value(&self) -> Result<String> {
        if self.secret {
            seal().unprotect(&self.sealed_value)
        } else {
            Ok(self.value.clone())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum HttpBody {
    /// multipart/form-data, the file in `field` (default `file`).
    Multipart {
        #[serde(default = "default_field")]
        field: String,
        #[serde(default)]
        fields: Vec<(String, String)>,
    },
    /// The file's bytes are the whole body.
    Raw,
}

fn default_field() -> String {
    "file".into()
}

impl Default for HttpBody {
    fn default() -> Self {
        HttpBody::Multipart { field: default_field(), fields: Vec::new() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
#[derive(Default)]
pub enum Extract {
    /// The trimmed response body is the link.
    #[default]
    Body,
    /// RFC 6901 pointer into the JSON body (`/data/url`).
    JsonPointer(String),
    /// First capture group of the pattern.
    Regex(String),
}

// ---- secrets ----

pub fn seal_secret(plain: &str) -> Result<String> {
    seal().protect(plain.trim())
}

pub fn unseal(sealed: &str) -> Result<String> {
    seal().unprotect(sealed)
}

impl Target {
    /// The link for an uploaded remote name (folder prefix included).
    pub fn link_of(&self, remote: &str) -> String {
        self.public_url.replace("{name}", remote).replace("{ext}", crate::naming::extension_of(remote).as_str())
    }

    pub fn label(&self) -> String {
        if !self.name.trim().is_empty() {
            self.name.trim().into()
        } else {
            format!("{} target", self.kind.label())
        }
    }

    /// Settings wrong or incomplete. The texts say what to fix.
    pub fn validate(&self) -> Result<()> {
        if self.folder.contains("..") {
            return Err(ShareError::config(tr!("The folder must not contain \"..\"")));
        }
        if !self.public_url.is_empty() && url::Url::parse(&self.public_url.replace("{name}", "x").replace("{ext}", "png")).is_err() {
            return Err(ShareError::config(tr!("The public link base is not a valid URL")));
        }
        match &self.kind {
            Kind::S3(s3) => {
                if s3.bucket.trim().is_empty() {
                    return Err(ShareError::config(tr!("Enter the bucket")));
                }
                if s3.access_key.trim().is_empty() {
                    return Err(ShareError::config(tr!("Enter the access key")));
                }
                if s3.sealed_secret_key.is_empty() {
                    return Err(ShareError::config(tr!("Enter the secret key")));
                }
                if !s3.endpoint.is_empty() {
                    check_http_url(&s3.endpoint, "endpoint")?;
                } else if s3.region.trim().is_empty() {
                    return Err(ShareError::config(tr!("Enter the region for AWS")));
                }
                if let Some(secs) = s3.presign_secs {
                    if secs == 0 || secs > 604800 {
                        return Err(ShareError::config(tr!("Presigned links can live one second to one week")));
                    }
                }
                if self.public_url.is_empty() && !s3.acl_public_read && s3.presign_secs.is_none() {
                    // Presigning is the fallback: fine, the default applies.
                }
            }
            Kind::Sftp(sftp) => {
                if sftp.host.trim().is_empty() {
                    return Err(ShareError::config(tr!("Enter the host")));
                }
                if sftp.user.trim().is_empty() {
                    return Err(ShareError::config(tr!("Enter the user")));
                }
                match sftp.auth {
                    SftpAuth::Password if sftp.sealed_password.is_empty() => return Err(ShareError::config(tr!("Enter the password"))),
                    SftpAuth::KeyFile if sftp.key_file.trim().is_empty() => return Err(ShareError::config(tr!("Choose the key file"))),
                    _ => {}
                }
                if self.public_url.is_empty() {
                    return Err(ShareError::config(tr!(
                        "SFTP cannot make links itself; enter the public link base (where the folder is served)"
                    )));
                }
            }
            Kind::WebDav(w) => {
                check_http_url(&w.url, "URL")?;
                if w.sealed_token.is_empty() {
                    if w.user.trim().is_empty() {
                        return Err(ShareError::config(tr!("Enter the user")));
                    }
                    if w.sealed_password.is_empty() {
                        return Err(ShareError::config(tr!("Enter the password")));
                    }
                }
                if self.public_url.is_empty() && !w.nextcloud_share {
                    return Err(ShareError::config(tr!("Enter the public link base, or turn on Nextcloud sharing")));
                }
                if w.nextcloud_share {
                    crate::webdav::nextcloud_root(w)?;
                }
            }
            Kind::Ftp(f) => {
                if f.host.trim().is_empty() {
                    return Err(ShareError::config(tr!("Enter the host")));
                }
                if self.public_url.is_empty() {
                    return Err(ShareError::config(tr!(
                        "FTP cannot make links itself; enter the public link base (where the folder is served)"
                    )));
                }
            }
            Kind::Http(h) => {
                check_http_url(&h.url, "URL")?;
                let m = h.method.to_ascii_uppercase();
                if m != "POST" && m != "PUT" {
                    return Err(ShareError::config(tr!("The method must be POST or PUT")));
                }
                if let Extract::Regex(p) = &h.extract {
                    if regex::Regex::new(p).is_err() {
                        return Err(ShareError::config(tr!("The extraction regex does not compile")));
                    }
                }
                for header in &h.headers {
                    if header.name.trim().is_empty() {
                        return Err(ShareError::config(tr!("A header has no name")));
                    }
                    if header.secret && header.sealed_value.is_empty() {
                        return Err(ShareError::config(tr!("The secret header {name} has no value", name = &header.name)));
                    }
                }
                if !h.delete_url.is_empty() {
                    check_http_url(&h.delete_url.replace("{value}", "x"), "delete URL")?;
                }
            }
        }
        Ok(())
    }
}

fn check_http_url(raw: &str, what: &str) -> Result<()> {
    let parsed = url::Url::parse(raw).map_err(|_| ShareError::config(tr!("The {what} is not a valid URL", what = what)))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ShareError::config(tr!("The {what} must be http or https", what = what)));
    }
    if parsed.host_str().is_none() {
        return Err(ShareError::config(tr!("The {what} has no host", what = what)));
    }
    Ok(())
}

impl Targets {
    pub fn load(paths: &DataPaths) -> Self {
        json_store::read(&file(paths)).unwrap_or_default()
    }

    pub fn save(&self, paths: &DataPaths) -> anyhow::Result<()> {
        json_store::write(&file(paths), self)
    }

    pub fn get(&self, id: &str) -> Option<&Target> {
        self.targets.iter().find(|t| t.id == id)
    }

    pub fn upsert(&mut self, target: Target) {
        match self.targets.iter_mut().find(|t| t.id == target.id) {
            Some(t) => *t = target,
            None => self.targets.push(target),
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.targets.retain(|t| t.id != id);
        if self.default.as_deref() == Some(id) {
            self.default = None;
        }
    }

    /// A new id that does not collide (`target`, `target-2`...).
    pub fn new_id(&self) -> String {
        for n in 1..1000 {
            let id = if n == 1 { "target".into() } else { format!("target-{n}") };
            if self.get(&id).is_none() {
                return id;
            }
        }
        format!("target-{}", chrono::Utc::now().timestamp())
    }

    /// The default target, or the only enabled one.
    pub fn default_target(&self) -> Option<&Target> {
        if let Some(id) = &self.default {
            if let Some(t) = self.get(id).filter(|t| t.enabled) {
                return Some(t);
            }
        }
        self.targets.iter().find(|t| t.enabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal::{use_seal, PlainSeal};
    use std::sync::Arc;

    fn s3_target() -> Target {
        Target {
            id: "t".into(),
            name: "Mine".into(),
            enabled: true,
            kind: Kind::S3(S3 {
                endpoint: "https://minio.lan:9000".into(),
                region: "us-east-1".into(),
                bucket: "shots".into(),
                access_key: "AKIA".into(),
                sealed_secret_key: "x".into(),
                ..Default::default()
            }),
            public_url: "https://cdn.lan/{name}".into(),
            folder: "games".into(),
            tls: TlsOptions::default(),
        }
    }

    #[test]
    fn round_trip_keeps_everything() {
        use_seal(Arc::new(PlainSeal));
        let mut targets = Targets::default();
        targets.upsert(s3_target());
        targets.default = Some("t".into());
        targets.naming.mode = crate::naming::NameMode::Template("{game}.{ext}".into());
        let json = serde_json::to_string_pretty(&targets).unwrap();
        let back: Targets = serde_json::from_str(&json).unwrap();
        assert_eq!(back.targets.len(), 1);
        assert_eq!(back.default.as_deref(), Some("t"));
        assert!(!json.contains("secretplaintext"));
    }

    #[test]
    fn debug_redacts_every_secret() {
        use_seal(Arc::new(PlainSeal));
        let secret = "hunter2hunter2";
        let sealed = seal_secret(secret).unwrap();
        assert!(!sealed.contains(secret));
        let kinds = vec![
            format!("{:?}", S3 { sealed_secret_key: sealed.clone(), sealed_session_token: sealed.clone(), ..Default::default() }),
            format!("{:?}", Sftp { sealed_password: sealed.clone(), sealed_passphrase: sealed.clone(), ..Default::default() }),
            format!("{:?}", WebDav { sealed_password: sealed.clone(), sealed_token: sealed.clone(), ..Default::default() }),
            format!("{:?}", Ftp { sealed_password: sealed.clone(), ..Default::default() }),
            format!("{:?}", HttpHeader { name: "x".into(), value: String::new(), secret: true, sealed_value: sealed.clone() }),
        ];
        for debug in kinds {
            assert!(!debug.contains(secret), "{debug}");
            assert!(debug.contains("(sealed)"), "{debug}");
        }
    }

    #[test]
    fn validation_catches_missing_pieces() {
        use_seal(Arc::new(PlainSeal));
        let mut t = s3_target();
        assert!(t.validate().is_ok());
        if let Kind::S3(s3) = &mut t.kind {
            s3.bucket.clear();
        }
        assert!(t.validate().is_err());
        let mut t = s3_target();
        if let Kind::S3(s3) = &mut t.kind {
            s3.endpoint = "ftp://x".into();
        }
        assert!(t.validate().is_err());
        let mut t = s3_target();
        t.folder = "../up".into();
        assert!(t.validate().is_err());
        let mut t = s3_target();
        t.public_url = "not a url".into();
        assert!(t.validate().is_err());
    }

    #[test]
    fn sftp_needs_a_link_base() {
        use_seal(Arc::new(PlainSeal));
        let t = Target {
            id: "s".into(),
            name: String::new(),
            enabled: true,
            kind: Kind::Sftp(Sftp { host: "h.lan".into(), user: "u".into(), auth: SftpAuth::Agent, ..Default::default() }),
            public_url: String::new(),
            folder: String::new(),
            tls: TlsOptions::default(),
        };
        assert!(t.validate().is_err());
    }

    #[test]
    fn ids_and_defaults() {
        let mut targets = Targets::default();
        assert_eq!(targets.new_id(), "target");
        targets.upsert(s3_target());
        assert_eq!(targets.new_id(), "target"); // "target" still free; id was "t"
        assert!(targets.default_target().is_some());
        targets.remove("t");
        assert!(targets.default_target().is_none());
    }
}
