//! The account on the saves server: the secure key kept on this PC and the
//! signed-in session built from it.
//!
//! The key is the account. It is never replaced without being asked to, and
//! the one it replaces is kept next to it, so a wrong import can be undone.
//! It is sealed for this user when the system can ([`heronite_platform::secret`]:
//! DPAPI, or the desktop keyring). A system with neither keeps it in a file only this
//! user can read, since a key that cannot be stored would lose the account.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine;
use heronite_data::{settings, DataPaths};
use heronite_i18n::tr;
use heronite_saves::{login, normalize_server, Client, Me, SavesError, SecureKey};
use serde::{Deserialize, Serialize};

const KEY_FILE: &str = "saves-key.json";
const PREVIOUS_FILE: &str = "saves-key.previous.json";

#[derive(Serialize, Deserialize)]
struct KeyFile {
    v: u32,
    /// The seed sealed for this user, base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sealed: Option<String>,
    /// The seed as text, only where nothing can seal it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plain: Option<String>,
}

fn key_path(paths: &DataPaths) -> PathBuf {
    paths.root.join(KEY_FILE)
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    // A name of its own per write: two writers never share a half-written tmp.
    static TMP_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = TMP_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp-{}-{n}", std::process::id()));
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    #[cfg(not(unix))]
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

fn encode(key: &SecureKey) -> Vec<u8> {
    let file = match heronite_platform::secret::protect(key.seed()) {
        Ok(sealed) => KeyFile { v: 1, sealed: Some(base64::engine::general_purpose::STANDARD.encode(sealed)), plain: None },
        Err(e) => {
            log::warn!("the secure key cannot be sealed on this system ({e}); keeping it in a private file");
            KeyFile { v: 1, sealed: None, plain: Some(key.export()) }
        }
    };
    serde_json::to_vec_pretty(&file).unwrap_or_default()
}

fn decode(bytes: &[u8]) -> Result<SecureKey, String> {
    let unreadable = || tr!("The secure key file is damaged. It was left as it is.").to_string();
    let file: KeyFile = serde_json::from_slice(bytes).map_err(|_| unreadable())?;
    if let Some(sealed) = file.sealed {
        let raw = base64::engine::general_purpose::STANDARD.decode(sealed).map_err(|_| unreadable())?;
        let seed = heronite_platform::secret::unprotect(&raw)
            .map_err(|_| tr!("The secure key cannot be opened right now (is the keyring locked?). It was left as it is.").to_string())?;
        let seed = <[u8; 32]>::try_from(seed.as_slice()).map_err(|_| unreadable())?;
        return Ok(SecureKey::from_seed(seed));
    }
    file.plain.ok_or_else(unreadable).and_then(|p| SecureKey::parse(&p).map_err(|_| unreadable()))
}

/// The key of this PC; `None` when none was made yet. A key that exists but
/// cannot be read is an error, never a reason to make another one.
pub fn load_key(paths: &DataPaths) -> Result<Option<SecureKey>, String> {
    match fs::read(key_path(paths)) {
        Ok(bytes) => decode(&bytes).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Making the key is a check and a write that must not interleave: two
/// callers at once would each make a key, and the one that lost the write
/// would sign in to a different, empty account.
fn key_lock() -> &'static Mutex<()> {
    static L: Mutex<()> = Mutex::new(());
    &L
}

/// The key, made on the first call.
pub fn ensure_key(paths: &DataPaths) -> Result<SecureKey, String> {
    let _guard = key_lock().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(key) = load_key(paths)? {
        return Ok(key);
    }
    let key = SecureKey::generate().map_err(|e| e.to_string())?;
    write_private(&key_path(paths), &encode(&key)).map_err(|e| e.to_string())?;
    Ok(key)
}

/// Makes `key` this PC's key, keeping the one it replaces as `saves-key.previous.json`.
pub fn replace_key(paths: &DataPaths, key: &SecureKey) -> Result<(), String> {
    let _guard = key_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = key_path(paths);
    if path.is_file() {
        fs::copy(&path, paths.root.join(PREVIOUS_FILE)).map_err(|e| e.to_string())?;
    }
    write_private(&path, &encode(key)).map_err(|e| e.to_string())?;
    forget_session();
    Ok(())
}

/// A new key for a new account (the old one stays as the previous one).
pub fn regenerate_key(paths: &DataPaths) -> Result<SecureKey, String> {
    let key = SecureKey::generate().map_err(|e| e.to_string())?;
    replace_key(paths, &key)?;
    Ok(key)
}

/// The server in use: the one in the settings, else the default. A server in
/// the settings that is not an acceptable address is an error, never a quiet
/// switch to the default: the saves would go to a server nobody chose.
pub fn server_of(paths: &DataPaths) -> Result<String, SavesError> {
    normalize_server(&settings::load(paths).map(|s| s.saves.server).unwrap_or_default())
}

/// A signed-in client and what the server says about the account right now.
pub struct Connection {
    pub client: Arc<Client>,
    pub me: Me,
    pub server: String,
}

struct Cached {
    server: String,
    public: String,
    client: Arc<Client>,
}

fn session() -> &'static Mutex<Option<Cached>> {
    static S: Mutex<Option<Cached>> = Mutex::new(None);
    &S
}

/// Signs in again at the next use (the key or the server changed).
pub fn forget_session() {
    *session().lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn device_name() -> String {
    let host = if cfg!(windows) {
        std::env::var("COMPUTERNAME").ok()
    } else {
        fs::read_to_string("/proc/sys/kernel/hostname").ok().or_else(|| fs::read_to_string("/etc/hostname").ok())
    };
    let host = host.map(|h| h.trim().to_string()).filter(|h| !h.is_empty()).unwrap_or_else(|| "pc".into());
    let os = if cfg!(windows) { "Windows" } else { "Linux" };
    let name = format!("Heronited {host} ({os})");
    name.chars().take(64).collect()
}

fn client_for(server: &str, key: &SecureKey) -> Result<Arc<Client>, SavesError> {
    let mut cache = session().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = cache.as_ref().filter(|c| c.server == server && c.public == key.public()) {
        return Ok(c.client.clone());
    }
    let signed = login(server, key, &device_name())?;
    let client = Arc::new(Client::new(server, &signed.token)?);
    *cache = Some(Cached { server: server.to_string(), public: key.public(), client: client.clone() });
    Ok(client)
}

fn local(e: String) -> SavesError {
    SavesError::local(e)
}

/// Signs in (the first time, or after the server dropped the session) and
/// reads the account's limits and usage.
pub fn connect(paths: &DataPaths) -> Result<Connection, SavesError> {
    let key = ensure_key(paths).map_err(local)?;
    let server = server_of(paths)?;
    for attempt in 0..2 {
        let client = client_for(&server, &key)?;
        match client.me() {
            Ok(me) => {
                if me.require_encryption {
                    return Err(SavesError::Api {
                        status: 400,
                        code: "encryption_required".into(),
                        title: String::new(),
                        detail: String::new(),
                        retry_after: None,
                        head: None,
                    });
                }
                return Ok(Connection { client, me, server });
            }
            Err(e) if e.is_unauthorized() && attempt == 0 => forget_session(),
            Err(e) => return Err(e),
        }
    }
    Err(SavesError::local(tr!("The saves server did not accept this secure key.")))
}

/// Runs `f` with a connection, signing in again once when the server says the
/// session ended.
pub fn with_connection<T>(paths: &DataPaths, f: impl Fn(&Connection) -> Result<T, SavesError>) -> Result<T, SavesError> {
    let conn = connect(paths)?;
    match f(&conn) {
        Err(e) if e.is_unauthorized() => {
            forget_session();
            f(&connect(paths)?)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use heronite_saves::DEFAULT_SERVER;

    #[test]
    fn a_key_is_made_once_and_replacing_keeps_the_old_one() {
        let d = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(d.path());
        assert!(load_key(&paths).unwrap().is_none());
        let first = ensure_key(&paths).unwrap();
        assert_eq!(ensure_key(&paths).unwrap(), first, "asking again never makes another key");
        let second = SecureKey::from_seed([5; 32]);
        replace_key(&paths, &second).unwrap();
        assert_eq!(load_key(&paths).unwrap().unwrap(), second);
        let previous = decode(&fs::read(d.path().join(PREVIOUS_FILE)).unwrap()).unwrap();
        assert_eq!(previous, first);
    }

    #[test]
    fn a_damaged_key_file_is_an_error_and_is_not_replaced() {
        let d = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(d.path());
        fs::write(key_path(&paths), "not json").unwrap();
        assert!(load_key(&paths).is_err());
        assert!(ensure_key(&paths).is_err());
        assert_eq!(fs::read_to_string(key_path(&paths)).unwrap(), "not json");
    }

    #[cfg(unix)]
    #[test]
    fn the_key_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(d.path());
        ensure_key(&paths).unwrap();
        assert_eq!(fs::metadata(key_path(&paths)).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn the_server_defaults_and_is_checked() {
        let d = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(d.path());
        assert_eq!(server_of(&paths).unwrap(), DEFAULT_SERVER);
        fs::create_dir_all(paths.local_library()).unwrap();
        fs::write(paths.collection_settings(), r#"{"saves":{"server":"saves.example.com"}}"#).unwrap();
        assert_eq!(server_of(&paths).unwrap(), "https://saves.example.com");
        fs::write(paths.collection_settings(), r#"{"saves":{"server":"http://evil.example.com"}}"#).unwrap();
        assert!(server_of(&paths).is_err(), "a plain-http server is never used, nor swapped for the default");
    }
}
