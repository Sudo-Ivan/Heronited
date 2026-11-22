//! The saves HTTP API: key sign-in, usage, and snapshots of a slot.
//!
//! One blocking client per sign-in. The bearer token only travels over
//! `https` (plain `http` is accepted for a server on this PC), is never
//! logged, and every request has a timeout. Safe requests (reads, deletes,
//! chunk uploads) are retried after a network error or a 5xx; every request
//! is retried after a 429 or 503, which the server sends before changing
//! anything. Beginning and committing a snapshot are never retried after an
//! unknown outcome.

use std::str::FromStr;
use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Result, SavesError};
use crate::hash::sha256_hex;
use crate::key::SecureKey;

/// The server a new setup points at.
pub const DEFAULT_SERVER: &str = "https://saves.quad4.io";

const RETRIES: u32 = 3;
const MAX_WAIT: Duration = Duration::from_secs(10);
/// Most a single answer may hold: a chunk is at most the server's
/// `max_chunk_bytes` (a few MiB), so this only stops a runaway body.
const BODY_CAP: u64 = 64 << 20;

/// `server` as the base URL requests go to: `https://` is assumed without a
/// scheme, a trailing slash goes, and anything but a plain origin is refused
/// (no user name, query or fragment). `http` only for this PC.
pub fn normalize_server(server: &str) -> Result<String> {
    let s = server.trim();
    if s.is_empty() {
        return Ok(DEFAULT_SERVER.to_string());
    }
    let with_scheme = if s.contains("://") { s.to_string() } else { format!("https://{s}") };
    let uri = ureq::http::Uri::from_str(&with_scheme).map_err(|_| SavesError::local(bad_server()))?;
    let scheme = uri.scheme_str().unwrap_or_default();
    let authority = uri.authority().ok_or_else(|| SavesError::local(bad_server()))?;
    if authority.as_str().contains('@') || uri.query().is_some() || !matches!(uri.path(), "" | "/") {
        return Err(SavesError::local(bad_server()));
    }
    let host = authority.host().trim_matches(['[', ']']);
    let local = host == "localhost" || host == "127.0.0.1" || host == "::1";
    match scheme {
        "https" => {}
        "http" if local => {}
        "http" => return Err(SavesError::local(heronite_i18n::tr!("The saves server has to use https. Plain http is only for this PC."))),
        _ => return Err(SavesError::local(bad_server())),
    }
    Ok(format!("{scheme}://{authority}"))
}

fn bad_server() -> String {
    heronite_i18n::tr!("That is not a server address. Use a name like saves.example.com.").to_string()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub used_bytes: u64,
    #[serde(default)]
    pub reserved_bytes: u64,
    #[serde(default)]
    pub quota_bytes: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Limits {
    #[serde(default)]
    pub max_files: u64,
    #[serde(default)]
    pub max_file_bytes: u64,
    #[serde(default)]
    pub max_snapshot_bytes: u64,
    #[serde(default)]
    pub inline_max_bytes: u64,
    #[serde(default)]
    pub min_chunk_bytes: u64,
    #[serde(default)]
    pub max_chunk_bytes: u64,
    #[serde(default)]
    pub max_chunks_per_snapshot: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TierLimits {
    #[serde(default)]
    pub max_slots: u64,
    #[serde(default)]
    pub max_snapshots_per_slot: u64,
    #[serde(default)]
    pub quota_bytes: u64,
    #[serde(default)]
    pub max_files: u64,
    #[serde(default)]
    pub max_file_bytes: u64,
    #[serde(default)]
    pub max_snapshot_bytes: u64,
}

/// `GET /v1/me`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Me {
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub tier_limits: TierLimits,
    #[serde(default)]
    pub require_encryption: bool,
}

impl Me {
    /// What this account may keep, as the server reports it: the account's
    /// own quota, else the quota of its tier. 0 when the server names none.
    pub fn capacity(&self) -> u64 {
        [self.usage.quota_bytes, self.tier_limits.quota_bytes].into_iter().find(|q| *q > 0).unwrap_or(0)
    }

    /// Bytes this account holds, finished and still being uploaded.
    pub fn held(&self) -> u64 {
        self.usage.used_bytes + self.usage.reserved_bytes
    }

    /// The limits a push has to respect: the protocol's, narrowed by the
    /// tier's, with the protocol defaults for what the server leaves out.
    pub fn effective_limits(&self) -> Limits {
        let (l, t) = (&self.limits, &self.tier_limits);
        let pick = |v: u64, tier: u64, d: u64| {
            let v = if v == 0 { d } else { v };
            if tier == 0 {
                v
            } else {
                v.min(tier)
            }
        };
        Limits {
            max_files: pick(l.max_files, t.max_files, 1 << 20),
            max_file_bytes: pick(l.max_file_bytes, t.max_file_bytes, 64 << 30),
            max_snapshot_bytes: pick(l.max_snapshot_bytes, t.max_snapshot_bytes, 1 << 40),
            inline_max_bytes: pick(l.inline_max_bytes, 0, 64 << 10),
            min_chunk_bytes: pick(l.min_chunk_bytes, 0, 128 << 10),
            max_chunk_bytes: pick(l.max_chunk_bytes, 0, 2 << 20),
            max_chunks_per_snapshot: pick(l.max_chunks_per_snapshot, 0, 1 << 22),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Slot {
    #[serde(default, alias = "Name")]
    pub slot: String,
    #[serde(default, alias = "HeadSnapshotID")]
    pub head: Option<String>,
}

/// One saved state in the list of a slot. The server's own field names
/// (`ID`, `FileCount`) are read as well as the documented ones.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct SnapshotInfo {
    #[serde(alias = "ID")]
    pub id: String,
    #[serde(default, alias = "Label")]
    pub label: String,
    #[serde(default, alias = "Device")]
    pub device: String,
    #[serde(default, alias = "CreatedAt")]
    pub created_at: String,
    #[serde(default, alias = "FileCount", alias = "file_count")]
    pub files: u64,
    #[serde(default, alias = "TotalBytes", alias = "total_bytes")]
    pub bytes: u64,
    #[serde(default, alias = "State")]
    pub state: String,
    #[serde(default)]
    pub enc: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ChunkRef {
    pub h: String,
    pub n: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub size: i64,
    #[serde(default)]
    pub mode: i64,
    #[serde(default)]
    pub mtime_ns: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunks: Option<Vec<ChunkRef>>,
    /// Base64 of the bytes of a small file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Manifest {
    pub version: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enc: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub dirs: Vec<String>,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Begun {
    pub id: String,
    #[serde(default)]
    pub missing: Vec<String>,
}

/// A signed-in session token and who it belongs to.
#[derive(Debug, Clone)]
pub struct Login {
    pub token: String,
    pub user_id: String,
}

pub struct Client {
    base: String,
    token: String,
    agent: ureq::Agent,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("base", &self.base).field("token", &"<hidden>").finish()
    }
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .user_agent(concat!("Heronited/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Reads a problem document off a failed answer.
fn problem(status: u16, retry_after: Option<u64>, body: &[u8]) -> SavesError {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let code = Some(text("code")).filter(|c| !c.is_empty()).unwrap_or_else(|| {
        match status {
            401 => "unauthorized",
            403 => "forbidden",
            404 => "not_found",
            409 => "conflict",
            413 => "too_large",
            429 => "rate_limited",
            503 => "maintenance",
            _ => "internal",
        }
        .to_string()
    });
    SavesError::Api {
        status,
        code,
        title: text("title"),
        detail: text("detail"),
        retry_after,
        head: v.get("head").and_then(Value::as_str).map(str::to_string),
    }
}

struct Reply {
    status: u16,
    body: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verb {
    Get,
    Post,
    Put,
    Delete,
}

fn request(
    agent: &ureq::Agent,
    verb: Verb,
    url: &str,
    token: Option<&str>,
    extra: &[(&str, String)],
    body: Option<(&str, &[u8])>,
) -> Result<Reply> {
    let net = |e: ureq::Error| SavesError::Network(e.to_string());
    macro_rules! with_headers {
        ($req:expr) => {{
            let mut r = $req;
            if let Some(t) = token {
                r = r.header("Authorization", format!("Bearer {t}"));
            }
            for (k, v) in extra {
                r = r.header(*k, v.as_str());
            }
            r
        }};
    }
    let mut resp = match verb {
        Verb::Get => with_headers!(agent.get(url)).call(),
        Verb::Delete => with_headers!(agent.delete(url)).call(),
        Verb::Post => {
            let (ct, b) = body.unwrap_or(("application/json", &[]));
            with_headers!(agent.post(url).header("Content-Type", ct)).send(b)
        }
        Verb::Put => {
            let (ct, b) = body.unwrap_or(("application/octet-stream", &[]));
            with_headers!(agent.put(url).header("Content-Type", ct)).send(b)
        }
    }
    .map_err(net)?;
    let status = resp.status().as_u16();
    let retry_after = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    let body = resp.body_mut().with_config().limit(BODY_CAP).read_to_vec().map_err(net)?;
    if (200..300).contains(&status) {
        Ok(Reply { status, body })
    } else {
        Err(problem(status, retry_after, &body))
    }
}

/// Runs a request with the retry rules of the module docs.
fn call(
    agent: &ureq::Agent,
    verb: Verb,
    url: &str,
    token: Option<&str>,
    extra: &[(&str, String)],
    body: Option<(&str, &[u8])>,
    safe: bool,
) -> Result<Reply> {
    let mut attempt = 0u32;
    loop {
        let result = request(agent, verb, url, token, extra, body);
        let wait = match &result {
            Ok(_) => return result,
            Err(SavesError::Api { status, retry_after, .. }) if *status == 429 || *status == 503 => {
                Some(retry_after.map(Duration::from_secs).unwrap_or(Duration::from_millis(500 * u64::from(attempt + 1))))
            }
            Err(SavesError::Api { status, .. }) if safe && *status >= 500 => Some(Duration::from_millis(500 * u64::from(attempt + 1))),
            Err(SavesError::Network(_)) if safe => Some(Duration::from_millis(500 * u64::from(attempt + 1))),
            Err(_) => None,
        };
        match wait {
            Some(w) if attempt < RETRIES => {
                attempt += 1;
                std::thread::sleep(w.min(MAX_WAIT));
            }
            _ => return result,
        }
    }
}

fn parse<T: for<'de> Deserialize<'de>>(reply: &Reply) -> Result<T> {
    serde_json::from_slice(&reply.body).map_err(|e| SavesError::Network(format!("unreadable answer: {e}")))
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn quoted(id: &str) -> String {
    format!("\"{id}\"")
}

/// Signs in with the secure key: registers it (once, the server forgives
/// repeats) and trades the signed challenge for a token.
pub fn login(server: &str, key: &SecureKey, device: &str) -> Result<Login> {
    let base = normalize_server(server)?;
    let agent = agent(Duration::from_secs(30));
    let public = key.public();
    let body = serde_json::json!({ "public_key": public }).to_string();
    let reg = call(&agent, Verb::Post, &format!("{base}/v1/register"), None, &[], Some(("application/json", body.as_bytes())), false)?;
    if reg.status == 202 {
        return Err(SavesError::Api {
            status: 202,
            code: "pending_approval".into(),
            title: String::new(),
            detail: String::new(),
            retry_after: None,
            head: None,
        });
    }
    let reg: Value = parse(&reg)?;
    let challenge =
        reg.get("challenge").and_then(Value::as_str).ok_or_else(|| SavesError::Network("the server sent no challenge".into()))?;
    let body = serde_json::json!({
        "public_key": public,
        "challenge": challenge,
        "signature": key.sign_challenge(challenge),
        "device": device,
    })
    .to_string();
    let verified =
        call(&agent, Verb::Post, &format!("{base}/v1/auth/verify"), None, &[], Some(("application/json", body.as_bytes())), false)?;
    let v: Value = parse(&verified)?;
    let token = v
        .get("token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SavesError::Network("the server sent no token".into()))?;
    Ok(Login { token: token.to_string(), user_id: v.get("user_id").and_then(Value::as_str).unwrap_or_default().to_string() })
}

impl Client {
    pub fn new(server: &str, token: &str) -> Result<Self> {
        Ok(Self { base: normalize_server(server)?, token: token.to_string(), agent: agent(Duration::from_secs(60)) })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn get(&self, path: &str) -> Result<Reply> {
        call(&self.agent, Verb::Get, &self.url(path), Some(&self.token), &[], None, true)
    }

    pub fn me(&self) -> Result<Me> {
        parse(&self.get("/v1/me")?)
    }

    /// The slot, `None` when nothing was saved to it yet.
    pub fn slot(&self, app: &str, slot: &str) -> Result<Option<Slot>> {
        match self.get(&format!("/v1/apps/{}/slots/{}", enc(app), enc(slot))) {
            Ok(r) => parse(&r).map(Some),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Snapshots of a slot, newest first (a missing slot lists none).
    pub fn snapshots(&self, app: &str, slot: &str) -> Result<Vec<SnapshotInfo>> {
        let mut all = Vec::new();
        let mut cursor = String::new();
        for _ in 0..50 {
            let query = if cursor.is_empty() { "?limit=100".to_string() } else { format!("?limit=100&cursor={}", enc(&cursor)) };
            let reply = match self.get(&format!("/v1/apps/{}/slots/{}/snapshots{query}", enc(app), enc(slot))) {
                Ok(r) => r,
                Err(e) if e.is_not_found() => return Ok(all),
                Err(e) => return Err(e),
            };
            let v: Value = parse(&reply)?;
            let page: Vec<SnapshotInfo> = serde_json::from_value(v.get("snapshots").cloned().unwrap_or(Value::Null)).unwrap_or_default();
            all.extend(page.into_iter().filter(|s| s.state.is_empty() || s.state == "committed"));
            cursor = ["next", "cursor"].iter().find_map(|k| v.get(*k).and_then(Value::as_str)).unwrap_or_default().to_string();
            if cursor.is_empty() {
                break;
            }
        }
        Ok(all)
    }

    pub fn manifest(&self, app: &str, slot: &str, id: &str) -> Result<Manifest> {
        parse(&self.get(&format!("/v1/apps/{}/slots/{}/snapshots/{}", enc(app), enc(slot), enc(id)))?)
    }

    pub fn begin(&self, app: &str, slot: &str, parent: Option<&str>, manifest: &Manifest) -> Result<Begun> {
        let body = serde_json::to_vec(manifest).map_err(|e| SavesError::local(e.to_string()))?;
        let extra: Vec<(&str, String)> = parent.map(|p| ("If-Match", quoted(p))).into_iter().collect();
        let path = format!("/v1/apps/{}/slots/{}/snapshots", enc(app), enc(slot));
        parse(&call(&self.agent, Verb::Post, &self.url(&path), Some(&self.token), &extra, Some(("application/json", &body)), false)?)
    }

    /// Uploads one chunk. The server checks the hash of the body, and the
    /// body is hashed here too, so a buffer that does not match its name is
    /// never sent.
    pub fn put_chunk(&self, app: &str, slot: &str, snapshot: &str, hash: &str, data: &[u8]) -> Result<()> {
        if sha256_hex(data) != hash {
            return Err(SavesError::local("a chunk changed before it was sent"));
        }
        let path = format!("/v1/apps/{}/slots/{}/snapshots/{}/chunks/{}", enc(app), enc(slot), enc(snapshot), hash);
        call(&self.agent, Verb::Put, &self.url(&path), Some(&self.token), &[], Some(("application/octet-stream", data)), true)?;
        Ok(())
    }

    /// Publishes the snapshot as the new head. `expected` is the head this
    /// snapshot was made on top of.
    pub fn commit(&self, app: &str, slot: &str, snapshot: &str, expected: Option<&str>) -> Result<String> {
        let extra: Vec<(&str, String)> = expected.map(|p| ("If-Match", quoted(p))).into_iter().collect();
        let path = format!("/v1/apps/{}/slots/{}/snapshots/{}/commit", enc(app), enc(slot), enc(snapshot));
        let reply = call(&self.agent, Verb::Post, &self.url(&path), Some(&self.token), &extra, Some(("application/json", b"")), false)?;
        let v: Value = parse(&reply)?;
        Ok(["head", "ID", "id"].iter().find_map(|k| v.get(*k).and_then(Value::as_str)).unwrap_or(snapshot).to_string())
    }

    pub fn delete_snapshot(&self, app: &str, slot: &str, id: &str) -> Result<()> {
        let path = format!("/v1/apps/{}/slots/{}/snapshots/{}", enc(app), enc(slot), enc(id));
        call(&self.agent, Verb::Delete, &self.url(&path), Some(&self.token), &[], None, true)?;
        Ok(())
    }

    /// A chunk by hash, checked against it.
    pub fn chunk(&self, app: &str, hash: &str) -> Result<Vec<u8>> {
        let body = self.get(&format!("/v1/apps/{}/chunks/{}", enc(app), hash))?.body;
        if sha256_hex(&body) != hash {
            return Err(SavesError::local(heronite_i18n::tr!(
                "The saves server sent data that does not match its hash. Nothing was written."
            )));
        }
        Ok(body)
    }

    /// Writes a file of a manifest to `out`, checking every chunk against
    /// its hash and the whole against the size. Nothing is trusted before it is hashed: a chunk
    /// that does not match is an error before it reaches `out`.
    pub fn write_file(&self, app: &str, file: &FileEntry, out: &mut dyn std::io::Write) -> Result<()> {
        let damaged = || SavesError::local(heronite_i18n::tr!("The saves server sent a damaged file."));
        let mut written: i64 = 0;
        match (&file.inline, &file.chunks) {
            (Some(inline), _) => {
                let bytes = base64::engine::general_purpose::STANDARD.decode(inline).map_err(|_| damaged())?;
                written = bytes.len() as i64;
                out.write_all(&bytes)?;
            }
            (None, Some(chunks)) => {
                for c in chunks {
                    let data = self.chunk(app, &c.h)?;
                    if data.len() != c.n as usize {
                        return Err(damaged());
                    }
                    written += data.len() as i64;
                    out.write_all(&data)?;
                }
            }
            (None, None) => {}
        }
        if written != file.size {
            return Err(damaged());
        }
        Ok(())
    }

    /// [`Self::write_file`] into memory.
    pub fn read_file(&self, app: &str, file: &FileEntry) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(file.size.clamp(0, 64 << 20) as usize);
        self.write_file(app, file, &mut out)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_addresses() {
        assert_eq!(normalize_server("").unwrap(), DEFAULT_SERVER);
        assert_eq!(normalize_server("  saves.example.com/ ").unwrap(), "https://saves.example.com");
        assert_eq!(normalize_server("https://saves.example.com:8443").unwrap(), "https://saves.example.com:8443");
        assert_eq!(normalize_server("http://localhost:8080/").unwrap(), "http://localhost:8080");
        assert_eq!(normalize_server("http://127.0.0.1:9").unwrap(), "http://127.0.0.1:9");
    }

    #[test]
    fn refuses_unsafe_addresses() {
        for bad in [
            "http://saves.example.com",
            "ftp://x.example.com",
            "https://user:pw@x.example.com",
            "https://x.example.com/path",
            "https://x.example.com?a=1",
            "https://",
            "not a host",
        ] {
            assert!(normalize_server(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn escapes_path_segments() {
        assert_eq!(enc("steam:12"), "steam%3A12");
        assert_eq!(enc("a b/c"), "a%20b%2Fc");
        assert_eq!(enc("ok-._~"), "ok-._~");
    }

    #[test]
    fn problems_carry_codes() {
        let e = problem(413, None, br#"{"code":"quota_exceeded","title":"full"}"#);
        assert!(e.is_quota());
        let e = problem(409, None, br#"{"code":"conflict","head":"01H"}"#);
        assert!(e.is_conflict());
        assert!(matches!(e, SavesError::Api { head: Some(ref h), .. } if h == "01H"));
        assert_eq!(problem(401, None, b"").code(), "unauthorized");
        assert_eq!(problem(500, None, b"<html>").code(), "internal");
    }

    #[test]
    fn effective_limits_fill_gaps() {
        let l = Me::default().effective_limits();
        assert_eq!(l.inline_max_bytes, 64 << 10);
        assert_eq!(l.max_chunk_bytes, 2 << 20);
        let me = Me { limits: Limits { max_chunk_bytes: 1 << 20, ..Default::default() }, ..Default::default() };
        assert_eq!(me.effective_limits().max_chunk_bytes, 1 << 20);
    }

    #[test]
    fn debug_hides_the_token() {
        let c = Client::new("https://saves.example.com", "sv1_secret").unwrap();
        assert!(!format!("{c:?}").contains("secret"));
    }
}
