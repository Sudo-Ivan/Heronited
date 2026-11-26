//! Blocking Drop `/api/v1` client. Authentication is a client certificate
//! plus a fresh ES384 JWT on every request.

use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;

use tungstenite::client::IntoClientRequest;

use serde::Deserialize;
use serde_json::Value;

use heronite_i18n::tr;

use crate::auth::{initiate_body, Auth, HandshakeResponse};

#[derive(Debug, Clone)]
pub struct Client {
    base: String,
    auth: Auth,
    agent: ureq::Agent,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Game {
    pub id: String,
    #[serde(rename = "type", default)]
    pub game_type: String,
    #[serde(default)]
    pub m_name: String,
    #[serde(default)]
    pub m_short_description: String,
    #[serde(default)]
    pub m_description: String,
    #[serde(default)]
    pub m_icon_object_id: String,
    #[serde(default)]
    pub m_banner_object_id: String,
    #[serde(default)]
    pub m_cover_object_id: String,
    #[serde(default)]
    pub library_path: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionOption {
    pub game_id: String,
    pub version_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub version_path: Option<String>,
    pub platform: String,
    #[serde(default)]
    pub size: GameSize,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameSize {
    #[serde(default)]
    pub install_size: u64,
    #[serde(default)]
    pub download_size: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionDetails {
    pub game_id: String,
    pub version_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub version_path: Option<String>,
    #[serde(default)]
    pub only_setup: bool,
    #[serde(default)]
    pub version_index: u64,
    #[serde(default)]
    pub delta: bool,
    #[serde(default)]
    pub launches: Vec<Launch>,
    #[serde(default)]
    pub setups: Vec<Launch>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Launch {
    #[serde(default)]
    pub launch_id: String,
    #[serde(default)]
    pub setup_id: String,
    #[serde(default)]
    pub name: String,
    pub command: String,
    pub platform: String,
    #[serde(default)]
    pub emulator: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestDetails {
    pub file_list: HashMap<String, String>,
    pub manifests: HashMap<String, Manifest>,
    #[serde(default)]
    pub install_size: u64,
    #[serde(default)]
    pub download_size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub version: String,
    pub chunks: HashMap<String, ChunkData>,
    #[serde(default)]
    pub size: u64,
    pub key: [u8; 16],
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChunkData {
    pub files: Vec<FileEntry>,
    pub checksum: String,
    pub iv: [u8; 16],
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileEntry {
    pub filename: String,
    pub start: u64,
    pub length: u64,
    #[serde(default)]
    pub permissions: u32,
}

#[derive(Debug, Deserialize)]
struct Depot {
    endpoint: String,
}

impl Client {
    pub fn new(url: &str, auth: Auth) -> Result<Self, String> {
        let agent = ureq::Agent::config_builder().timeout_connect(Some(Duration::from_secs(10))).user_agent("Heronited").build().into();
        Ok(Self { base: normalize_url(url)?, auth, agent })
    }

    pub fn unsigned(base: &str) -> Result<ureq::Agent, String> {
        normalize_url(base)?;
        Ok(ureq::Agent::config_builder().timeout_connect(Some(Duration::from_secs(10))).user_agent("Heronited").build().into())
    }

    fn request(&self, method: &str, path: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
        let url = format!("{}{}", self.base, path);
        let auth = self.auth.header()?;
        let req = match method {
            "GET" => self.agent.get(&url),
            _ => unreachable!(),
        };
        req.header("Authorization", auth).call().map_err(error)
    }

    fn json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T, String> {
        let text =
            self.request("GET", path)?.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string().map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| format!("Drop response was not understood: {e}"))
    }

    /// Validates the client certificate and returns the account object.
    pub fn user(&self) -> Result<Value, String> {
        self.json("/api/v1/client/user")
    }

    pub fn library(&self) -> Result<Vec<Game>, String> {
        self.json("/api/v1/client/user/library")
    }

    pub fn object(&self, id: &str) -> Result<Vec<u8>, String> {
        let mut r = self.request("GET", &format!("/api/v1/client/object/{}", enc(id)))?;
        r.body_mut().with_config().limit(32 * 1024 * 1024).read_to_vec().map_err(|e| e.to_string())
    }

    pub fn object_content_type(&self, id: &str) -> Result<(Vec<u8>, String), String> {
        let mut r = self.request("GET", &format!("/api/v1/client/object/{}", enc(id)))?;
        let ctype = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let bytes = r.body_mut().with_config().limit(32 * 1024 * 1024).read_to_vec().map_err(|e| e.to_string())?;
        Ok((bytes, ctype))
    }

    pub fn versions(&self, game: &str, previous: Option<&str>) -> Result<Vec<VersionOption>, String> {
        let previous = previous.unwrap_or("");
        self.json(&format!("/api/v1/client/game/{}/versions?previous={}", enc(game), enc(previous)))
    }

    pub fn version(&self, game: &str, version: &str) -> Result<VersionDetails, String> {
        self.json(&format!("/api/v1/client/game/{}/version/{}", enc(game), enc(version)))
    }

    pub fn manifest(&self, version: &str, previous: Option<&str>) -> Result<ManifestDetails, String> {
        let mut path = format!("/api/v1/client/game/manifest?version={}", enc(version));
        if let Some(previous) = previous.filter(|p| !p.is_empty()) {
            path.push_str(&format!("&previous={}", enc(previous)));
        }
        self.json(&path)
    }

    fn depots(&self) -> Result<Vec<String>, String> {
        let depots: Vec<Depot> = self.json("/api/v1/client/depots")?;
        Ok(depots.into_iter().map(|d| if d.endpoint.ends_with('/') { d.endpoint } else { format!("{}/", d.endpoint) }).collect())
    }

    /// The depot endpoints that say they have this version.
    pub fn depots_for(&self, game: &str, version: &str) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        for endpoint in self.depots()? {
            let Ok(mut r) = self.agent.get(&format!("{endpoint}manifest.json")).call() else { continue };
            let Ok(text) = r.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string() else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
            let has = v
                .get("content")
                .and_then(|c| c.get(game))
                .and_then(Value::as_array)
                .is_some_and(|versions| versions.iter().any(|x| x.get("versionId").and_then(Value::as_str) == Some(version)));
            if has {
                out.push(endpoint);
            }
        }
        if out.is_empty() {
            return Err(tr!("No Drop depot has this game version").into());
        }
        Ok(out)
    }

    /// Downloads one encrypted chunk. No total timeout: chunks can be large.
    pub fn chunk(&self, depot: &str, game: &str, version: &str, chunk: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
        let url = format!("{}content/{}/{}/{}", depot, enc(game), enc(version), enc(chunk));
        let auth = self.auth.header()?;
        let agent: ureq::Agent =
            ureq::Agent::config_builder().timeout_connect(Some(Duration::from_secs(10))).user_agent("Heronited").build().into();
        agent.get(&url).header("Authorization", auth).call().map_err(error)
    }
}

/// Starts a code sign-in. The user enters the returned code on the server.
pub fn initiate(base: &str, name: &str) -> Result<String, String> {
    let base = normalize_url(base)?;
    let agent = Client::unsigned(&base)?;
    let mut r = agent
        .post(&format!("{base}/api/v1/client/auth/initiate"))
        .header("content-type", "application/json")
        .send(initiate_body(name).to_string().as_bytes())
        .map_err(error)?;
    let text = r.body_mut().with_config().limit(1024 * 1024).read_to_string().map_err(|e| e.to_string())?;
    let code = text.trim().trim_matches('"').to_string();
    if code.len() < 4 {
        return Err(tr!("Drop did not return a sign-in code").into());
    }
    Ok(code)
}

/// Waits for the user to approve `code`, then finishes the handshake.
pub fn complete_code(base: &str, code: &str) -> Result<Auth, String> {
    let base = normalize_url(base)?;
    let ws_base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else {
        format!("ws://{}", base.trim_start_matches("http://"))
    };
    let url = format!("{ws_base}/api/v1/client/auth/code/ws");
    let mut request = tungstenite::ClientRequestBuilder::new(tungstenite::http::Uri::from_str(&url).map_err(|e| e.to_string())?)
        .with_header("Authorization", code)
        .into_client_request()
        .map_err(|e| e.to_string())?;
    request.headers_mut().insert("User-Agent", "Heronited".parse().map_err(|e| format!("{e}"))?);
    let (mut socket, _) = tungstenite::connect(request).map_err(|e| e.to_string())?;
    let token = loop {
        let msg = socket.read().map_err(|e| e.to_string())?;
        if let tungstenite::Message::Text(text) = msg {
            let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            if v.get("type").and_then(Value::as_str) == Some("token") {
                let value = v.get("value").and_then(Value::as_str).unwrap_or_default();
                if !value.is_empty() {
                    break value.to_string();
                }
            }
            if v.get("type").and_then(Value::as_str) == Some("error") {
                return Err(v.get("value").and_then(Value::as_str).unwrap_or("Drop sign-in failed").into());
            }
        }
    };
    let (client_id, token) = token.split_once('/').ok_or_else(|| tr!("Drop returned a broken sign-in token").to_string())?;
    let agent = Client::unsigned(&base)?;
    let body = serde_json::json!({"clientId": client_id, "token": token}).to_string();
    let mut r = agent
        .post(&format!("{base}/api/v1/client/auth/handshake"))
        .header("content-type", "application/json")
        .send(body.as_bytes())
        .map_err(error)?;
    let text = r.body_mut().with_config().limit(1024 * 1024).read_to_string().map_err(|e| e.to_string())?;
    let response: HandshakeResponse = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(response.into())
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn error(e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(401) | ureq::Error::StatusCode(403) => tr!("Drop rejected this client sign-in").into(),
        ureq::Error::StatusCode(404) => tr!("That Drop server, game or file was not found").into(),
        ureq::Error::StatusCode(c) => format!("Drop HTTP {c}"),
        e => e.to_string(),
    }
}

/// `server` as a plain base URL: https assumed, query and user info refused.
pub fn normalize_url(server: &str) -> Result<String, String> {
    let s = server.trim();
    if s.is_empty() {
        return Err(tr!("Enter the server address").into());
    }
    let with_scheme = if s.contains("://") { s.into() } else { format!("https://{s}") };
    let uri = ureq::http::Uri::from_str(&with_scheme).map_err(|_| bad())?;
    let scheme = uri.scheme_str().unwrap_or_default();
    let auth = uri.authority().ok_or_else(bad)?;
    let host = auth.host().trim_matches(['[', ']']);
    let local = matches!(host, "localhost" | "127.0.0.1" | "::1");
    if auth.as_str().contains('@') || uri.query().is_some() {
        return Err(bad());
    }
    match scheme {
        "https" => {}
        "http" if local => {}
        "http" => return Err(tr!("The Drop server has to use https. Plain http is only for this PC.").into()),
        _ => return Err(bad()),
    }
    let path = uri.path().trim_end_matches('/');
    Ok(format!("{scheme}://{auth}{path}"))
}

fn bad() -> String {
    tr!("That is not a server address. Use a name like games.example.com.").into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_server_addresses() {
        assert_eq!(normalize_url("games.example.com/").unwrap(), "https://games.example.com");
        assert_eq!(normalize_url("http://localhost:8080").unwrap(), "http://localhost:8080");
        assert_eq!(normalize_url("https://games.example.com/drop/").unwrap(), "https://games.example.com/drop");
        assert!(normalize_url("http://games.example.com").is_err());
        assert!(normalize_url("https://me@games.example.com").is_err());
    }
}
