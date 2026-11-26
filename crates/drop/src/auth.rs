//! Drop client certificates and the short-lived ES384 tokens the API expects.

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Auth {
    pub private: String,
    pub certificate: String,
    pub id: String,
}

impl Auth {
    /// `JWT <client id> <ES384 token>`, as Drop's client API expects.
    pub fn header(&self) -> Result<String, String> {
        #[derive(Serialize)]
        struct Claims {
            exp: usize,
            nbf: usize,
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs() as usize;
        let key = jsonwebtoken::EncodingKey::from_ec_pem(self.private.as_bytes()).map_err(|e| e.to_string())?;
        let token =
            jsonwebtoken::encode(&jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES384), &Claims { nbf: now, exp: now + 10 }, &key)
                .map_err(|e| e.to_string())?;
        Ok(format!("JWT {} {token}", self.id))
    }

    pub fn seal(&self) -> Result<String, String> {
        let plain = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        let sealed = heronite_platform::secret::protect(&plain).map_err(|e| e.to_string())?;
        Ok(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, sealed))
    }

    pub fn unseal(text: &str) -> Result<Self, String> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD.decode(text).map_err(|e| e.to_string())?;
        let plain = heronite_platform::secret::unprotect(&bytes).map_err(|e| e.to_string())?;
        serde_json::from_slice(&plain).map_err(|e| e.to_string())
    }
}

pub fn initiate_body(name: &str) -> serde_json::Value {
    json!({
        "name": name,
        "platform": std::env::consts::OS,
        "capabilities": {},
        "mode": "code",
    })
}

#[derive(Deserialize)]
pub struct HandshakeResponse {
    pub private: String,
    pub certificate: String,
    pub id: String,
}

impl From<HandshakeResponse> for Auth {
    fn from(value: HandshakeResponse) -> Self {
        Self { private: value.private, certificate: value.certificate, id: value.id }
    }
}
