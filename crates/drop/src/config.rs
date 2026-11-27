//! Drop server profiles (`local_library/drop.json`). Client private keys and
//! certificates stay sealed for this user.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use heronite_data::{json_store, DataPaths};

use crate::{Auth, SOURCE};

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
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub url: String,
    /// Sealed `Auth` JSON.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sealed_auth: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub last_sync: String,
}

impl Server {
    pub fn auth(&self) -> Result<Auth, String> {
        if self.sealed_auth.is_empty() {
            return Err(heronite_i18n::tr!("This Drop server is not signed in").into());
        }
        Auth::unseal(&self.sealed_auth)
    }

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

    /// Adds or updates a profile. `auth: None` keeps the existing sign-in.
    pub fn upsert(&mut self, id: String, name: String, url: String, auth: Option<&Auth>, enabled: bool) -> Result<(), String> {
        let sealed_auth = match auth {
            Some(auth) => auth.seal()?,
            None => self.get(&id).map(|s| s.sealed_auth.clone()).unwrap_or_default(),
        };
        let server = Server {
            id: id.clone(),
            name,
            url: crate::client::normalize_url(&url)?,
            sealed_auth,
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

    pub fn set_auth(&mut self, id: &str, auth: &Auth) -> Result<(), String> {
        let s =
            self.servers.iter_mut().find(|s| s.id == id).ok_or_else(|| heronite_i18n::tr!("The Drop server was not found").to_string())?;
        s.sealed_auth = auth.seal()?;
        Ok(())
    }

    pub fn signed_in(&self, id: &str) -> bool {
        self.get(id).is_some_and(|s| !s.sealed_auth.is_empty())
    }

    pub fn remove(&mut self, id: &str) {
        self.servers.retain(|s| s.id != id);
    }

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
    paths.local_library().join("drop.json")
}
