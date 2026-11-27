//! GameHub server profiles (`local_library/gamehub.json`). There is no sign-in
//! yet: a profile is only a name, an address and whether it is used.

use anyhow::Result;
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
    #[serde(default)]
    pub enabled: bool,
    /// RFC 3339.
    #[serde(default)]
    pub last_sync: String,
}

impl Server {
    pub fn label(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().into();
        }
        url::Url::parse(&self.url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_else(|| self.url.clone())
    }
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

    /// Adds or replaces a profile.
    pub fn upsert(&mut self, id: String, name: String, url: String, enabled: bool) -> Result<(), String> {
        let server = Server {
            id: id.clone(),
            name,
            url: crate::client::normalize_url(&url)?,
            enabled,
            last_sync: self.get(&id).map(|s| s.last_sync.clone()).unwrap_or_default(),
        };
        match self.servers.iter_mut().find(|s| s.id == id) {
            Some(s) => *s = server,
            None => self.servers.push(server),
        }
        self.servers.sort_by_key(|a| a.label());
        Ok(())
    }

    pub fn remove(&mut self, id: &str) {
        self.servers.retain(|s| s.id != id);
    }

    /// A new profile id that does not collide (`gamehub`, `gamehub-2`...).
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
    paths.local_library().join("gamehub.json")
}
