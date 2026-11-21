//! Data only this user can read back. Windows: DPAPI (`CryptProtectData`,
//! bound to the user's logon credentials). Linux: AES-256-GCM with a key
//! the user's keyring holds (Secret Service — GNOME Keyring or KWallet).
//! Systems with no such store fail: protecting errors, so nothing secret
//! is written in the clear by callers that fail closed.

use std::io;

/// Purpose text mixed into the protection, so a blob made for something
/// else does not open here.
#[cfg(windows)]
const ENTROPY: &[u8] = b"Heronited secret v1";

#[cfg(windows)]
mod imp {
    use super::ENTROPY;
    use std::io;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
    }

    fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: DPAPI filled `out` with a LocalAlloc'd buffer of cbData bytes.
        let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
        // SAFETY: freed once, with the allocator DPAPI used.
        unsafe { LocalFree(out.pbData as _) };
        bytes
    }

    pub fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
        let (input, entropy) = (blob(data), blob(ENTROPY));
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: the blobs point at live slices for the call; `out` is written by it.
        let ok = unsafe {
            CryptProtectData(&input, std::ptr::null(), &entropy, std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(take(out))
    }

    pub fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
        let (input, entropy) = (blob(data), blob(ENTROPY));
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: as in `protect`.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(take(out))
    }
}

/// The blob `protect` writes: a tag, the nonce, and the sealed bytes. Linux
/// keeps the key in the user's keyring (Secret Service — GNOME Keyring or
/// KWallet); there is no blob without it, so protecting fails closed like
/// on every system without such a store.
#[cfg(target_os = "linux")]
pub const TAG: &[u8] = b"HRN2";

#[cfg(target_os = "linux")]
mod imp {
    use super::TAG;
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Nonce};
    use std::io;

    const SERVICE: &str = "io.github.sudoivan.Heronited";
    const KEY_NAME: &str = "secret-key-v1";

    fn unsupported(e: impl std::fmt::Display) -> io::Error {
        io::Error::new(io::ErrorKind::Unsupported, format!("no keyring on this system: {e}"))
    }

    /// The key sealed blobs are made with: the user's keyring's copy, or a
    /// fresh one stored there. `Unsupported` when this system has none. A
    /// fresh key is made only when the entry is empty: any other error
    /// (locked, unreachable) fails closed, so a transient failure can never
    /// rotate the key out from under the blobs it sealed.
    fn key() -> io::Result<[u8; 32]> {
        let entry = keyring::Entry::new(SERVICE, KEY_NAME).map_err(unsupported)?;
        match entry.get_secret() {
            Ok(raw) => <[u8; 32]>::try_from(raw.as_slice())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the keyring's key is not 32 bytes")),
            Err(keyring::Error::NoEntry) => {
                let mut raw = [0u8; 32];
                getrandom::fill(&mut raw).map_err(|e| io::Error::other(format!("no random numbers: {e}")))?;
                entry.set_secret(&raw).map_err(unsupported)?;
                Ok(raw)
            }
            Err(e) => Err(unsupported(e)),
        }
    }

    /// `data` sealed with `key`: `TAG` + nonce + AES-256-GCM. Split out so
    /// tests need no keyring.
    fn protect_with(key: &[u8; 32], data: &[u8]) -> io::Result<Vec<u8>> {
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).map_err(|e| io::Error::other(format!("no random numbers: {e}")))?;
        let ct = Aes256Gcm::new(key.into())
            .encrypt(&Nonce::try_from(&nonce[..]).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad nonce: {e}")))?, data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("sealing failed: {e}")))?;
        let mut out = Vec::with_capacity(TAG.len() + nonce.len() + ct.len());
        out.extend_from_slice(TAG);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// What [`protect_with`] sealed, when `key` sealed it.
    fn unprotect_with(key: &[u8; 32], data: &[u8]) -> io::Result<Vec<u8>> {
        let rest = data.strip_prefix(TAG).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not a Heronited sealed blob"))?;
        let (nonce, ct) =
            rest.split_at_checked(12).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "a Heronited sealed blob is too short"))?;
        Aes256Gcm::new(key.into())
            .decrypt(&Nonce::try_from(nonce).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad nonce: {e}")))?, ct)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("the sealed data cannot be opened: {e}")))
    }

    pub fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
        protect_with(&key()?, data)
    }

    pub fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
        unprotect_with(&key()?, data)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn round_trips_and_refuses_what_it_did_not_seal() {
            let key = [7u8; 32];
            let sealed = protect_with(&key, b"refresh token").unwrap();
            assert!(!sealed.windows(7).any(|w| w == b"refresh"), "not in the clear");
            assert_eq!(unprotect_with(&key, &sealed).unwrap(), b"refresh token");
            // A wrong key, a truncated blob and a flipped bit all fail.
            assert!(unprotect_with(&[9u8; 32], &sealed).is_err());
            assert!(unprotect_with(&key, &sealed[..sealed.len() - 4]).is_err());
            let mut broken = sealed.clone();
            let last = broken.len() - 1;
            broken[last] ^= 0xFF;
            assert!(unprotect_with(&key, &broken).is_err());
            assert!(unprotect_with(&key, b"plain json").is_err());
        }
    }
}

#[cfg(all(not(windows), not(target_os = "linux")))]
mod imp {
    use std::io;

    pub fn protect(_: &[u8]) -> io::Result<Vec<u8>> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "no protected storage on this system"))
    }

    pub fn unprotect(_: &[u8]) -> io::Result<Vec<u8>> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "no protected storage on this system"))
    }
}

/// `data` sealed for this user.
pub fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    imp::protect(data)
}

/// What [`protect`] sealed, when this user sealed it on this system.
pub fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    imp::unprotect(data)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_refuses_what_it_did_not_seal() {
        let sealed = protect(b"refresh token").unwrap();
        assert!(!sealed.windows(7).any(|w| w == b"refresh"), "not in the clear");
        assert_eq!(unprotect(&sealed).unwrap(), b"refresh token");
        assert!(unprotect(b"not a blob").is_err());
        let mut broken = sealed.clone();
        let last = broken.len() - 1;
        broken[last] ^= 0xFF;
        assert!(unprotect(&broken).is_err());
    }
}
