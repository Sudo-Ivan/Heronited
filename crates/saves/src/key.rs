//! The secure key: an Ed25519 key pair made from a 32-byte seed. The server
//! learns the public key only. The seed is the account: the same seed on
//! another PC reaches the same saves, and nobody can recover it for you.

use anyhow::{anyhow, bail, Result};
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

/// The text a challenge is signed with in front of it.
pub const CHALLENGE_PREFIX: &str = "saves-auth-v1:";

#[derive(Clone, PartialEq, Eq)]
pub struct SecureKey {
    seed: [u8; 32],
}

impl std::fmt::Debug for SecureKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecureKey(<hidden>)")
    }
}

impl SecureKey {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self { seed }
    }

    pub fn generate() -> Result<Self> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| anyhow!("no random numbers: {e}"))?;
        Ok(Self { seed })
    }

    pub fn seed(&self) -> &[u8; 32] {
        &self.seed
    }

    fn signing(&self) -> SigningKey {
        SigningKey::from_bytes(&self.seed)
    }

    /// The public key, base64url without padding (what the server stores).
    pub fn public(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.signing().verifying_key().as_bytes())
    }

    /// A short id of the public key to show next to the account.
    pub fn fingerprint(&self) -> String {
        let hash = Sha256::digest(self.signing().verifying_key().as_bytes());
        URL_SAFE_NO_PAD.encode(&hash[..8])
    }

    /// Signs `challenge` the way `/v1/auth/verify` checks it.
    pub fn sign_challenge(&self, challenge: &str) -> String {
        let message = format!("{CHALLENGE_PREFIX}{challenge}");
        URL_SAFE_NO_PAD.encode(self.signing().sign(message.as_bytes()).to_bytes())
    }

    /// The seed as text to keep: base64url, 43 characters.
    pub fn export(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.seed)
    }

    /// A seed typed or pasted back: base64url or base64 (with or without
    /// padding), or 64 hex digits. Anything that is not exactly 32 bytes is
    /// refused, so a mistyped key never becomes a different account.
    pub fn parse(text: &str) -> Result<Self> {
        let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        if t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()) {
            let mut seed = [0u8; 32];
            for (i, byte) in seed.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&t[i * 2..i * 2 + 2], 16)?;
            }
            return Ok(Self { seed });
        }
        for engine in [&URL_SAFE_NO_PAD, &URL_SAFE, &STANDARD_NO_PAD, &STANDARD] {
            if let Ok(bytes) = engine.decode(t.as_bytes()) {
                if let Ok(seed) = <[u8; 32]>::try_from(bytes.as_slice()) {
                    return Ok(Self { seed });
                }
            }
        }
        bail!("not a secure key: it has to be the 32 bytes the key was made of")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_and_parse_round_trip() {
        let key = SecureKey::generate().unwrap();
        let text = key.export();
        assert_eq!(text.len(), 43);
        assert_eq!(SecureKey::parse(&text).unwrap(), key);
        assert_eq!(SecureKey::parse(&format!("  {}\n", text)).unwrap(), key);
        let hex: String = key.seed().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(SecureKey::parse(&hex).unwrap(), key);
        assert_eq!(SecureKey::parse(&STANDARD.encode(key.seed())).unwrap(), key);
    }

    #[test]
    fn refuses_what_is_not_a_seed() {
        assert!(SecureKey::parse("").is_err());
        assert!(SecureKey::parse("abc").is_err());
        assert!(SecureKey::parse(&URL_SAFE_NO_PAD.encode([1u8; 31])).is_err());
        assert!(SecureKey::parse(&URL_SAFE_NO_PAD.encode([1u8; 33])).is_err());
    }

    #[test]
    fn same_seed_same_public_key_and_valid_signature() {
        use ed25519_dalek::{Signature, Verifier};
        let a = SecureKey::from_seed([7; 32]);
        let b = SecureKey::from_seed([7; 32]);
        assert_eq!(a.public(), b.public());
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_ne!(a.public(), SecureKey::from_seed([8; 32]).public());
        let sig = URL_SAFE_NO_PAD.decode(a.sign_challenge("nonce")).unwrap();
        let public = a.signing().verifying_key();
        let sig = Signature::from_bytes(&<[u8; 64]>::try_from(sig.as_slice()).unwrap());
        assert!(public.verify(b"saves-auth-v1:nonce", &sig).is_ok());
        assert!(public.verify(b"saves-auth-v1:other", &sig).is_err());
    }

    #[test]
    fn debug_never_shows_the_seed() {
        assert!(!format!("{:?}", SecureKey::from_seed([9; 32])).contains('9'));
    }
}
