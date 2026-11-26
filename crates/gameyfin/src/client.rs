//! Blocking Gameyfin `/v1` API client. The key goes in `X-API-Key`, never in
//! a URL or log. `http` is accepted only for loopback servers.

use std::str::FromStr;
use std::time::Duration;

use serde::Deserialize;

use heronite_i18n::tr;

#[derive(Debug, Clone)]
pub struct Client {
    base: String,
    key: String,
    agent: ureq::Agent,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Game {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub developers: Vec<String>,
    #[serde(default)]
    pub publishers: Vec<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub cover: Option<Image>,
    #[serde(default)]
    pub header: Option<Image>,
    #[serde(default)]
    #[serde(rename = "fileSize")]
    pub file_size: Option<u64>,
    #[serde(default)]
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<String>,
}

impl Game {
    /// A PC game. Empty platform data is kept because older servers may not
    /// have classified their libraries yet.
    pub fn is_pc(&self) -> bool {
        self.platforms.is_empty() || self.platforms.iter().any(|p| p == "PC_MICROSOFT_WINDOWS" || p == "LINUX")
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Image {
    pub id: String,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Provider {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub priority: i32,
}

#[derive(Debug, Deserialize)]
struct Page<T> {
    items: Vec<T>,
    #[serde(default)]
    #[allow(dead_code)]
    page: u32,
    #[serde(default)]
    #[allow(dead_code)]
    size: u32,
    #[serde(default, rename = "totalElements")]
    total: u64,
}

impl Client {
    pub fn new(url: &str, key: &str) -> Result<Self, String> {
        let agent = ureq::Agent::config_builder().timeout_connect(Some(Duration::from_secs(10))).user_agent("Heronited").build().into();
        Ok(Self { base: normalize_url(url)?, key: key.into(), agent })
    }

    pub fn get_public(&self, path: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
        let url = format!("{}{}", self.base, path);
        self.agent.get(&url).header("X-API-Key", &self.key).call().map_err(|e| error(e))
    }

    fn json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T, String> {
        let text = self.get_public(path)?.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string().map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }

    /// The server's own version and API versions. Also validates the key.
    pub fn instance(&self) -> Result<serde_json::Value, String> {
        self.json("/v1/instance")
    }

    pub fn providers(&self) -> Result<Vec<Provider>, String> {
        let mut v: Vec<Provider> = self.json("/v1/download-providers")?;
        v.sort_by(|a, b| b.priority.cmp(&a.priority));
        Ok(v)
    }

    /// Every game, following pages.
    pub fn games(&self) -> Result<Vec<Game>, String> {
        let mut out = Vec::new();
        let size = 100;
        for page in 0..1000 {
            let p: Page<Game> = self.json(&format!("/v1/games?page={page}&size={size}&sort=id,asc"))?;
            let got = p.items.len() as u64;
            out.extend(p.items);
            if out.len() as u64 >= p.total || got == 0 || got < size {
                break;
            }
        }
        Ok(out)
    }

    /// One game.
    pub fn game(&self, id: &str) -> Result<Game, String> {
        self.json(&format!("/v1/games/{}", enc(id)))
    }

    /// The best download provider (Gameyfin returns priority order).
    pub fn provider(&self) -> Result<Provider, String> {
        self.providers()?.into_iter().next().ok_or_else(|| tr!("The Gameyfin server has no download provider").into())
    }

    /// Download response for a game. Binary downloads are streamed by the caller.
    /// Unlike metadata requests it has no total timeout: games can be large.
    pub fn download(&self, game: &str, provider: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
        let url = format!("{}/v1/games/{}/download?provider={}", self.base, enc(game), enc(provider));
        let agent: ureq::Agent =
            ureq::Agent::config_builder().timeout_connect(Some(Duration::from_secs(10))).user_agent("Heronited").build().into();
        agent.get(&url).header("X-API-Key", &self.key).call().map_err(error)
    }

    pub fn image_url(&self, game: &str, image: &str) -> String {
        format!("{}/v1/games/{}/images/{}", self.base, enc(game), enc(image))
    }
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn error(e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(401) => tr!("Gameyfin rejected the API key").into(),
        ureq::Error::StatusCode(403) => tr!("The Gameyfin API key cannot read games or downloads").into(),
        ureq::Error::StatusCode(404) => tr!("That Gameyfin server or game was not found").into(),
        ureq::Error::StatusCode(c) => format!("Gameyfin HTTP {c}"),
        e => e.to_string(),
    }
}

/// `server` as a plain origin: https assumed, trailing slash removed, query,
/// user info and fragments refused. Plain http only for loopback.
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
        "http" => return Err(tr!("The Gameyfin server has to use https. Plain http is only for this PC.").into()),
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
    use std::net::TcpListener;

    fn serve(mut answer: impl FnMut(&str) -> String + Send + 'static) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            while let Ok((mut s, _)) = l.accept() {
                let mut buf = [0; 8192];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = answer(&req);
                write!(
                    s,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });
        base
    }

    #[test]
    fn normalizes_server_addresses() {
        assert_eq!(normalize_url("games.example.com/").unwrap(), "https://games.example.com");
        assert_eq!(normalize_url("http://localhost:8080").unwrap(), "http://localhost:8080");
        assert!(normalize_url("http://games.example.com").is_err());
        assert!(normalize_url("https://me@games.example.com").is_err());
        assert_eq!(normalize_url("https://games.example.com/gameyfin/").unwrap(), "https://games.example.com/gameyfin");
    }

    #[test]
    fn reads_paged_games_with_the_api_key_header() {
        let base = serve(|req| {
            assert!(req.contains("X-API-Key: secret") || req.contains("x-api-key: secret"));
            if req.contains("page=0") {
                r#"{"items":[{"id":"1","title":"One","platforms":[],"developers":[],"publishers":[],"genres":[],"externalIds":[],"features":[],"images":[],"keywords":[],"mediaOrder":[],"perspectives":[],"themes":[],"videoUrls":[],"cover":null,"header":null,"comment":null,"createdAt":null,"criticRating":null,"libraryId":"l","release":null,"summary":null,"updatedAt":null,"userRating":null,"fileSize":12}],"page":0,"size":100,"totalElements":1}"#.into()
            } else {
                r#"{"items":[],"page":1,"size":100,"totalElements":1}"#.into()
            }
        });
        let c = Client::new(&base, "secret").unwrap();
        let games = c.games().unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].title.as_deref(), Some("One"));
        assert_eq!(games[0].file_size, Some(12));
    }
}
