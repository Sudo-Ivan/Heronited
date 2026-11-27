//! Blocking GameHub `/api` client. GameHub has no login or API key yet.
//! `https` is assumed when the address has no scheme; an explicit `http://`
//! address is honored for local servers.

use std::str::FromStr;
use std::time::Duration;

use serde::Deserialize;

use heronite_i18n::tr;

#[derive(Debug, Clone)]
pub struct Client {
    base: String,
    agent: ureq::Agent,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Game {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub developers: Vec<String>,
    #[serde(default)]
    pub publishers: Vec<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub series: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub rating: Option<f32>,
    #[serde(default)]
    pub players: Option<String>,
    #[serde(default)]
    pub install_notes: Option<String>,
    #[serde(default)]
    pub wikipedia_url: Option<String>,
    #[serde(default)]
    pub pcgamingwiki_url: Option<String>,
    #[serde(default)]
    pub steam_appid: Option<u64>,
    #[serde(default)]
    pub gog_id: Option<String>,
    #[serde(default)]
    pub protondb_tier: Option<String>,
    #[serde(default)]
    pub os: Option<OsSupport>,
    #[serde(default)]
    pub files: Vec<GameFile>,
    #[serde(default)]
    pub versions: Vec<String>,
    #[serde(default)]
    pub total_size: u64,
    #[serde(default)]
    pub cover: Option<String>,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub added_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GameFile {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OsSupport {
    #[serde(default)]
    pub windows: bool,
    #[serde(default)]
    pub mac: bool,
    #[serde(default)]
    pub linux: Option<String>,
}

impl Game {
    /// A Windows or Linux PC game. Missing OS data is kept so a server with a
    /// small or unusual library does not have its games hidden.
    pub fn is_pc(&self) -> bool {
        self.os.as_ref().is_none_or(|os| os.windows || os.linux.is_some())
    }

    pub fn linux_native(&self) -> bool {
        self.os.as_ref().and_then(|os| os.linux.as_deref()) == Some("native")
    }

    pub fn windows_pc(&self) -> bool {
        self.os.as_ref().is_some_and(|os| os.windows || os.linux.as_deref() == Some("proton"))
    }

    /// The version GameHub's own web client downloads when several are present.
    pub fn preferred_version(&self) -> Option<&str> {
        self.versions.last().map(String::as_str)
    }

    /// Bytes of the version Heronited downloads, not of every version together.
    pub fn download_size(&self) -> u64 {
        let Some(version) = self.preferred_version().filter(|_| self.versions.len() > 1) else {
            return self.total_size;
        };
        let size: u64 = self.files.iter().filter(|f| f.version.as_deref() == Some(version) || f.version.is_none()).map(|f| f.size).sum();
        if size == 0 {
            self.total_size
        } else {
            size
        }
    }
}

#[derive(Debug, Deserialize)]
struct Page {
    total: usize,
    games: Vec<Game>,
}

impl Client {
    pub fn new(url: &str) -> Result<Self, String> {
        let agent = ureq::Agent::config_builder().timeout_connect(Some(Duration::from_secs(10))).user_agent("Heronited").build().into();
        Ok(Self { base: normalize_url(url)?, agent })
    }

    pub fn get_public(&self, path: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
        let url = format!("{}{}", self.base, path);
        self.agent.get(&url).call().map_err(error)
    }

    fn json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T, String> {
        let text = self.get_public(path)?.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string().map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }

    /// The server's library totals. Also validates that the address is a GameHub.
    pub fn stats(&self) -> Result<serde_json::Value, String> {
        self.json("/api/stats")
    }

    /// Every game with its detail record. The list endpoint is paged and only
    /// carries summaries, so details are fetched in small parallel batches.
    pub fn games(&self) -> Result<Vec<Game>, String> {
        let mut summaries = Vec::new();
        let limit = 500;
        for page in 0..1000 {
            let offset = page * limit;
            let p: Page = self.json(&format!("/api/games?limit={limit}&offset={offset}&sort=title&order=asc"))?;
            let got = p.games.len();
            summaries.extend(p.games);
            if summaries.len() >= p.total || got == 0 || got < limit {
                break;
            }
        }
        let mut out = Vec::with_capacity(summaries.len());
        for chunk in summaries.chunks(8) {
            let results = std::thread::scope(|scope| {
                let handles: Vec<_> = chunk.iter().map(|game| scope.spawn(|| self.game(&game.id))).collect();
                handles
                    .into_iter()
                    .map(|h| h.join().map_err(|_| "a GameHub request worker stopped".to_string()).and_then(|r| r))
                    .collect::<Vec<_>>()
            });
            for result in results {
                out.push(result?);
            }
        }
        Ok(out)
    }

    /// One game.
    pub fn game(&self, id: &str) -> Result<Game, String> {
        self.json(&format!("/api/games/{}", enc(id)))
    }

    /// The whole game as a stored zip. Binary downloads are streamed by the
    /// caller and have no total timeout: games can be large.
    pub fn download(&self, game: &str, version: Option<&str>) -> Result<ureq::http::Response<ureq::Body>, String> {
        let version = version.map(|v| format!("?version={}", enc(v))).unwrap_or_default();
        let url = format!("{}/api/games/{}/zip{}", self.base, enc(game), version);
        let agent: ureq::Agent =
            ureq::Agent::config_builder().timeout_connect(Some(Duration::from_secs(10))).user_agent("Heronited").build().into();
        agent.get(&url).call().map_err(error)
    }
}

pub(crate) fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Slash-separated media paths, with every segment percent-encoded.
pub fn enc_path(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn error(e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(404) => tr!("That GameHub server or game was not found").into(),
        ureq::Error::StatusCode(429) => tr!("The GameHub server is rate limiting requests").into(),
        ureq::Error::StatusCode(503) => tr!("The GameHub server has no free download slot").into(),
        ureq::Error::StatusCode(c) => format!("GameHub HTTP {c}"),
        e => e.to_string(),
    }
}

/// `server` as a plain base URL: https assumed, query, user info and
/// fragments refused. Unlike the authenticated server integrations, explicit
/// `http://` is kept because GameHub itself serves plain HTTP.
pub fn normalize_url(server: &str) -> Result<String, String> {
    let s = server.trim();
    if s.is_empty() {
        return Err(tr!("Enter the server address").into());
    }
    let with_scheme = if s.contains("://") { s.into() } else { format!("https://{s}") };
    let uri = ureq::http::Uri::from_str(&with_scheme).map_err(|_| bad())?;
    let scheme = uri.scheme_str().unwrap_or_default();
    let auth = uri.authority().ok_or_else(bad)?;
    if auth.as_str().contains('@') || uri.query().is_some() {
        return Err(bad());
    }
    match scheme {
        "https" | "http" => {}
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
    use std::io::{Read, Write};

    #[test]
    fn normalizes_server_addresses() {
        assert_eq!(normalize_url("games.example.com/").unwrap(), "https://games.example.com");
        assert_eq!(normalize_url("http://192.168.1.20:8484").unwrap(), "http://192.168.1.20:8484");
        assert_eq!(normalize_url("https://games.example.com/gamehub/").unwrap(), "https://games.example.com/gamehub");
        assert!(normalize_url("https://me@games.example.com").is_err());
    }

    #[test]
    fn media_paths_keep_slashes_and_encode_spaces() {
        assert_eq!(enc_path("screenshots/a b.png"), "screenshots/a%20b.png");
    }

    #[test]
    fn reads_the_paged_library_and_details() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0; 8192];
                let n = stream.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if req.starts_with("GET /api/games?") {
                    r#"{"total":1,"games":[{"id":"hollow-knight","title":"Hollow Knight","total_size":10,"os":{"windows":true}}]}"#
                } else if req.starts_with("GET /api/games/hollow-knight") {
                    r#"{"id":"hollow-knight","title":"Hollow Knight","description":"A game","versions":["1.0"],"files":[{"size":10,"version":"1.0"}],"total_size":10,"os":{"windows":true}}"#
                } else {
                    "{}"
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });
        let client = Client::new(&base).unwrap();
        let games = client.games().unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].description.as_deref(), Some("A game"));
        assert_eq!(games[0].download_size(), 10);
    }
}
