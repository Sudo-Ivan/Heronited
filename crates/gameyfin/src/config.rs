//! Server profiles (`local_library/gameyfin.json`). API keys are sealed for
//! this user where the system can; the sealed bytes are stored, never the key.

use anyhow::Result;
use base64::Engine;
use serde::{Deserialize, Serialize};

use heronite_data::{json_store, DataPaths};

use crate::SOURCE;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Servers {
    #[serde(default)]
    pub servers: Vec<Server>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    /// Stable id chosen when the profile is added.
    pub id: String,
    /// Display name ("Mine", "Alex's server"). Empty shows the host.
    #[serde(default)]
    pub name: String,
    pub url: String,
    /// API key sealed for this user, base64.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sealed_key: String,
    #[serde(default)]
    pub enabled: bool,
    /// RFC 3339.
    #[serde(default)]
    pub last_sync: String,
}

impl Server {
    pub fn key(&self) -> Result<String, String> {
        let bytes = base64::engine::general_purpose::STANDARD.decode(&self.sealed_key).map_err(|_| "bad sealed key")?;
        let plain = heronite_platform::secret::unprotect(&bytes).map_err(|e| e.to_string())?;
        String::from_utf8(plain).map_err(|_| "bad key encoding".into())
    }

    pub fn label(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().into();
        }
        url::Url::parse(&self.url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_else(|| self.url.clone())
    }
}

pub fn seal_key(key: &str) -> Result<String, String> {
    let sealed = heronite_platform::secret::protect(key.as_bytes()).map_err(|e| e.to_string())?;
    Ok(base64::engine::general_purpose::STANDARD.encode(sealed))
}

impl Servers {
    pub fn load(paths: &DataPaths) -> Self {
        json_store::read(&file(paths)).unwrap_or_default()
    }

    pub fn save(&self, paths: &DataPaths) -> Result<()> {
        json_store::write(&file(paths), self)
    }

    pub fn get(&self, id: &str) -> Option<&Server> {
        self.servers.iter().find(|s| s.id == id)
    }

    /// Adds or replaces a profile. `key: None` keeps the existing key.
    pub fn upsert(&mut self, id: String, name: String, url: String, key: Option<&str>, enabled: bool) -> Result<(), String> {
        let sealed_key = match key {
            Some(k) if !k.trim().is_empty() => seal_key(k.trim())?,
            _ => self.get(&id).map(|s| s.sealed_key.clone()).unwrap_or_default(),
        };
        if sealed_key.is_empty() {
            return Err(heronite_i18n::tr!("Enter the API key").into());
        }
        let server = Server {
            id: id.clone(),
            name,
            url: client::normalize_url(&url)?,
            sealed_key,
            enabled,
            last_sync: self.get(&id).map(|s| s.last_sync.clone()).unwrap_or_default(),
        };
        match self.servers.iter_mut().find(|s| s.id == id) {
            Some(s) => *s = server,
            None => self.servers.push(server),
        }
        self.servers.sort_by(|a, b| a.label().cmp(&b.label()));
        Ok(())
    }

    pub fn remove(&mut self, id: &str) {
        self.servers.retain(|s| s.id != id);
    }

    /// A new profile id that does not collide (`server`, `server-2`...).
    pub fn new_id(&self) -> String {
        for n in 1..1000 {
            let id = if n == 1 { SOURCE.into() } else { format!("{SOURCE}-{n}") };
            if self.get(&id).is_none() {
                return id;
            }
        }
        format!("{SOURCE}-{}", chrono::Utc::now().timestamp())
    }
}

pub fn file(paths: &DataPaths) -> std::path::PathBuf {
    paths.local_library().join("gameyfin.json")
}

use crate::client;
