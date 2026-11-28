//! Sealing secrets for this user, behind a seam tests can replace: the real
//! implementation uses the system keyring, which does not exist on test
//! machines.

use std::sync::{Arc, RwLock};

use base64::Engine;

use crate::error::{Result, ShareError};

pub trait Seal: Send + Sync {
    fn protect(&self, plain: &str) -> Result<String>;
    fn unprotect(&self, sealed: &str) -> Result<String>;
}

struct KeyringSeal;

impl Seal for KeyringSeal {
    fn protect(&self, plain: &str) -> Result<String> {
        let bytes = heronite_platform::secret::protect(plain.as_bytes()).map_err(|e| {
            ShareError::config(heronite_i18n::tr!("This system cannot seal secrets (no keyring): {error}", error = e.to_string()))
        })?;
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    fn unprotect(&self, sealed: &str) -> Result<String> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(sealed)
            .map_err(|_| ShareError::config(heronite_i18n::tr!("The stored secret does not decode; enter it again")))?;
        let plain = heronite_platform::secret::unprotect(&bytes).map_err(|e| {
            ShareError::config(heronite_i18n::tr!("The stored secret does not open here; enter it again ({error})", error = e.to_string()))
        })?;
        String::from_utf8(plain).map_err(|_| ShareError::config("bad secret encoding"))
    }
}

static OVERRIDE: RwLock<Option<Arc<dyn Seal>>> = RwLock::new(None);

/// Replaces the keyring sealing. For tests of this crate and its callers.
pub fn use_seal(seal: Arc<dyn Seal>) {
    if let Ok(mut o) = OVERRIDE.write() {
        *o = Some(seal);
    }
}

pub fn seal() -> Arc<dyn Seal> {
    if let Ok(o) = OVERRIDE.read() {
        if let Some(s) = o.as_ref() {
            return s.clone();
        }
    }
    Arc::new(KeyringSeal)
}

/// A sealing that keeps nothing secret, for tests only: base64 of the plain
/// text with a marker, so mistakes stay visible.
pub struct PlainSeal;

impl Seal for PlainSeal {
    fn protect(&self, plain: &str) -> Result<String> {
        Ok(format!("plain:{}", base64::engine::general_purpose::STANDARD.encode(plain)))
    }

    fn unprotect(&self, sealed: &str) -> Result<String> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(sealed.strip_prefix("plain:").unwrap_or(sealed))
            .map_err(|_| ShareError::config("bad test secret"))?;
        String::from_utf8(bytes).map_err(|_| ShareError::config("bad test secret"))
    }
}
