//! SFTP over russh + russh-sftp on a private tokio runtime per call, so no
//! async runtime leaks into the app. Uploads go to `.<name>.part` and rename
//! into place; host keys are pinned or matched against known_hosts.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use heronite_i18n::tr;
use russh::client::{Config, Handler};
use russh::keys::PublicKey;
use russh_sftp::client::SftpSession;

use crate::config::{unseal, Sftp, SftpAuth, Target};
use crate::error::{Result, ShareError};
use crate::history::Record;
use crate::progress::{Continue, Progress};
use crate::Shared;

/// A host key: algorithm name and the OpenSSH-style SHA256 fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    pub algorithm: String,
    pub fingerprint: String,
}

pub fn fingerprint_of(key: &PublicKey) -> HostKey {
    let fp = key.fingerprint(russh::keys::HashAlg::Sha256);
    HostKey { algorithm: key.algorithm().to_string(), fingerprint: fp.to_string() }
}

/// Connects, reads the server key, disconnects without authenticating: the
/// UI's "trust this key" step.
pub fn fetch_host_key(host: &str, port: u16) -> Result<HostKey> {
    let host = host.to_string();
    block_on(async move {
        let found = Arc::new(Mutex::new(None));
        let handler = KeySniffer { found: found.clone() };
        let config = Arc::new(config());
        let outcome = russh::client::connect(config, (host.as_str(), port), handler).await;
        let key = found.lock().ok().and_then(|g| g.clone());
        match key {
            Some(k) => Ok(fingerprint_of(&k)),
            None => Err(match outcome {
                Err(e) => ShareError::network(format!("{e:?}")),
                Ok(_) => ShareError::network("the server sent no key"),
            }),
        }
    })
}

struct KeySniffer {
    found: Arc<Mutex<Option<PublicKey>>>,
}

impl Handler for KeySniffer {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &russh::keys::PublicKeyOrCertificate) -> std::result::Result<bool, Self::Error> {
        if let Ok(mut f) = self.found.lock() {
            *f = Some(key.public_key());
        }
        Ok(false) // never authenticate: the fetch ends here
    }
}

/// Host key decision for the real client: pin, then known_hosts, else unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyVerdict {
    Accept,
    Unknown { fingerprint: String, algorithm: String },
    Changed { fingerprint: String, algorithm: String },
}

/// Where the known_hosts file is; tests point it elsewhere.
pub static KNOWN_HOSTS: std::sync::RwLock<Option<std::path::PathBuf>> = std::sync::RwLock::new(None);

fn known_hosts_path() -> std::path::PathBuf {
    if let Ok(guard) = KNOWN_HOSTS.read() {
        if let Some(p) = guard.as_ref() {
            return p.clone();
        }
    }
    dirs_home().join(".ssh").join("known_hosts")
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from("/"))
}

fn judge_key(key: &PublicKey, host: &str, port: u16, pin: &str) -> KeyVerdict {
    let found = fingerprint_of(key);
    if !pin.is_empty() {
        if pin.trim() == found.fingerprint {
            return KeyVerdict::Accept;
        }
        return KeyVerdict::Changed { fingerprint: found.fingerprint, algorithm: found.algorithm };
    }
    let path = known_hosts_path();
    if path.exists() {
        match russh::keys::check_known_hosts_path(host, port, key, &path) {
            Ok(true) => return KeyVerdict::Accept,
            Err(russh::keys::Error::KeyChanged { .. }) => {
                return KeyVerdict::Changed { fingerprint: found.fingerprint, algorithm: found.algorithm }
            }
            _ => {}
        }
    }
    KeyVerdict::Unknown { fingerprint: found.fingerprint, algorithm: found.algorithm }
}

struct Client {
    host: String,
    port: u16,
    pin: String,
    seen: Arc<Mutex<Option<PublicKey>>>,
}

impl Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &russh::keys::PublicKeyOrCertificate) -> std::result::Result<bool, Self::Error> {
        let public = key.public_key();
        if let Ok(mut seen) = self.seen.lock() {
            *seen = Some(public.clone());
        }
        match judge_key(&public, &self.host, self.port, &self.pin) {
            KeyVerdict::Accept => Ok(true),
            _ => Ok(false),
        }
    }
}

fn config() -> Config {
    Config { inactivity_timeout: Some(Duration::from_secs(60)), ..Default::default() }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
    runtime.block_on(future)
}

async fn connect_session(sftp: &Sftp) -> std::result::Result<russh::client::Handle<Client>, ShareError> {
    let seen = Arc::new(Mutex::new(None));
    let handler = Client { host: sftp.host.clone(), port: sftp.port, pin: sftp.host_key.clone(), seen: seen.clone() };
    let config = Arc::new(config());
    let connect_future = russh::client::connect(config, (sftp.host.as_str(), sftp.port), handler);
    let mut handle = match tokio::time::timeout(Duration::from_secs(15), connect_future).await {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => {
            // The key the server showed decides the failure kind; the handler
            // captured it during the handshake, no second connection needed.
            let captured = seen.lock().ok().and_then(|g| g.clone());
            if let Some(key) = captured {
                match judge_key(&key, &sftp.host, sftp.port, &sftp.host_key) {
                    KeyVerdict::Unknown { fingerprint, algorithm } => return Err(ShareError::HostKeyUnknown { fingerprint, algorithm }),
                    KeyVerdict::Changed { fingerprint, algorithm } => return Err(ShareError::HostKeyChanged { fingerprint, algorithm }),
                    KeyVerdict::Accept => {}
                }
            }
            return Err(ShareError::network(format!("{e:?}")));
        }
        Err(_) => return Err(ShareError::network(tr!("the server did not answer within 15 seconds"))),
    };
    // Authenticate.
    let authed = match sftp.auth {
        SftpAuth::Password => {
            let password = unseal(&sftp.sealed_password)?;
            handle.authenticate_password(&sftp.user, password).await
        }
        SftpAuth::KeyFile => {
            let passphrase = if sftp.sealed_passphrase.is_empty() { None } else { Some(unseal(&sftp.sealed_passphrase)?) };
            let key = match russh::keys::load_secret_key(&sftp.key_file, passphrase.as_deref()) {
                Ok(k) => k,
                Err(e) => return Err(ShareError::config(tr!("The key file does not read: {error}", error = e.to_string()).to_string())),
            };
            handle.authenticate_publickey(&sftp.user, russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), None)).await
        }
        SftpAuth::Agent => {
            #[cfg(not(unix))]
            return Err(ShareError::config(tr!("Agent authentication is not supported on this platform yet; use a key file").to_string()));
            #[cfg(unix)]
            {
                let Some(sock) = std::env::var_os("SSH_AUTH_SOCK") else {
                    return Err(ShareError::config(tr!("Agent authentication needs SSH_AUTH_SOCK; there is no agent here").to_string()));
                };
                let stream = match tokio::net::UnixStream::connect(sock).await {
                    Ok(s) => s,
                    Err(e) => return Err(ShareError::config(tr!("The agent does not answer: {error}", error = e.to_string()).to_string())),
                };
                let mut agent = russh::keys::agent::client::AgentClient::connect(stream).dynamic();
                let identities = agent
                    .request_identities()
                    .await
                    .map_err(|e| ShareError::config(tr!("The agent does not answer: {error}", error = e.to_string()).to_string()))?;
                let mut last = None;
                let mut outcome = None;
                for identity in identities {
                    let key = match &identity {
                        russh::keys::agent::AgentIdentity::PublicKey { key, .. } => key.clone(),
                        _ => continue,
                    };
                    match handle.authenticate_publickey_with(&sftp.user, key, None, &mut agent).await {
                        Ok(russh::client::AuthResult::Success) => {
                            outcome = Some(Ok(russh::client::AuthResult::Success));
                            break;
                        }
                        Ok(other) => outcome = Some(Ok(other)),
                        Err(e) => last = Some(e),
                    }
                }
                match outcome {
                    Some(o) => o,
                    None => match last {
                        Some(e) => return Err(ShareError::AuthFailed(format!("{e:?}"))),
                        None => return Err(ShareError::AuthFailed(tr!("the agent has no keys").to_string())),
                    },
                }
            }
        }
    };
    match authed {
        Ok(russh::client::AuthResult::Success) => Ok(handle),
        Ok(_) => Err(ShareError::AuthFailed(tr!("the server wants more than this login method").into())),
        Err(e) => Err(ShareError::AuthFailed(format!("{e:?}"))),
    }
}

async fn sftp_of(handle: &russh::client::Handle<Client>) -> std::result::Result<SftpSession, ShareError> {
    let channel = handle.channel_open_session().await.map_err(|e| ShareError::network(format!("{e:?}")))?;
    channel.request_subsystem(true, "sftp").await.map_err(|e| ShareError::network(format!("{e:?}")))?;
    let stream = channel.into_stream();
    let session = SftpSession::new(stream).await.map_err(|e| ShareError::network(e.to_string()))?;
    session.set_timeout(60);
    Ok(session)
}

/// mkdir -p: every parent of the remote file path, errors ignored where the
/// directory already exists.
async fn mkdirs(session: &SftpSession, folder: &str) -> std::result::Result<(), ShareError> {
    let mut path = String::new();
    for segment in folder.split('/').filter(|s| !s.is_empty()) {
        if !path.is_empty() {
            path.push('/');
        }
        path.push_str(segment);
        if session.create_dir(&path).await.is_err() {
            // Exists or a real problem: metadata decides.
            if session.metadata(&path).await.is_err() {
                return Err(ShareError::NotFound(tr!("the folder {path} cannot be created", path = &path)));
            }
        }
    }
    Ok(())
}

pub fn upload(
    target: &Target,
    sftp: &Sftp,
    file: &Path,
    remote: &str,
    size: u64,
    progress: &mut dyn FnMut(Progress) -> Continue,
) -> Result<Shared> {
    let result = block_on(async {
        let handle = connect_session(sftp).await?;
        let session = sftp_of(&handle).await?;
        // `remote` arrives with the target's folder prefix already joined;
        // the parents of the file path are what mkdir -p makes.
        let remote_path = remote.to_string();
        if let Some((parent, _)) = remote_path.rsplit_once('/') {
            mkdirs(&session, parent).await?;
        }
        // `.part` next to the final name: dir/.name.part
        let part_path = match remote_path.rsplit_once('/') {
            Some((dir, n)) => format!("{dir}/.{n}.part"),
            None => format!(".{remote_path}.part"),
        };
        let _ = session.remove_file(&part_path).await;
        let mut remote_file = session.create(&part_path).await.map_err(|e| ShareError::network(e.to_string()))?;
        // Stream in chunks so progress and cancel stay responsive.
        use tokio::io::AsyncWriteExt;
        let mut local = std::fs::File::open(file).map_err(ShareError::io)?;
        let mut buf = vec![0u8; 64 * 1024];
        let mut sent = 0u64;
        use std::io::Read;
        let write_outcome: std::result::Result<(), ShareError> = async {
            loop {
                let n = local.read(&mut buf).map_err(ShareError::io)?;
                if n == 0 {
                    break;
                }
                if let Err(e) = remote_file.write_all(&buf[..n]).await {
                    return Err(ShareError::network(e.to_string()));
                }
                sent += n as u64;
                if progress(Progress { sent, total: size }).cancelled() {
                    return Err(ShareError::Cancelled);
                }
            }
            Ok(())
        }
        .await;
        drop(remote_file);
        if let Err(e) = write_outcome {
            let _ = session.remove_file(&part_path).await;
            return Err(e);
        }
        if let Err(e) = session.rename(&part_path, &remote_path).await {
            let _ = session.remove_file(&part_path).await;
            return Err(ShareError::network(e.to_string()));
        }
        let _ = session.close().await;
        Ok(())
    });
    result?;
    Ok(Shared { url: target.link_of(remote), remote: remote.to_string(), delete: Some(remote.to_string()) })
}

pub fn delete(_target: &Target, sftp: &Sftp, record: &Record) -> Result<()> {
    block_on(async {
        let handle = connect_session(sftp).await?;
        let session = sftp_of(&handle).await?;
        // The delete hint is the full remote path, folder included.
        let path = record.delete.clone().unwrap_or_else(|| record.remote.clone());
        // Best effort: a missing file is fine.
        let _ = session.remove_file(&path).await;
        let _ = session.close().await;
        Ok(())
    })
}

pub fn check(target: &Target, sftp: &Sftp) -> Result<crate::CheckReport> {
    block_on(async {
        let handle = connect_session(sftp).await?;
        let session = sftp_of(&handle).await?;
        let mut steps = vec![tr!("Connected, host key accepted, logged in").into()];
        let probe = crate::join_remote(&target.folder, "heronite-probe.txt");
        if let Some((parent, _)) = probe.rsplit_once('/') {
            mkdirs(&session, parent).await?;
        }
        use tokio::io::AsyncWriteExt;
        let mut f = session.create(&probe).await.map_err(|e| ShareError::network(e.to_string()))?;
        f.write_all(b"probe").await.map_err(|e| ShareError::network(e.to_string()))?;
        drop(f);
        steps.push(tr!("Upload works").into());
        let meta = session.metadata(&probe).await.map_err(|e| ShareError::NotFound(e.to_string()))?;
        if meta.size != Some(5) {
            return Err(ShareError::Server { status: 0, body: tr!("the probe reads back wrong").into() });
        }
        steps.push(tr!("Reading back works").into());
        session.remove_file(&probe).await.map_err(|e| ShareError::network(e.to_string()))?;
        steps.push(tr!("Delete works").into());
        let _ = session.close().await;
        Ok(crate::CheckReport { steps })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_format_openssh_style() {
        // An ed25519 public key from OpenSSH's test corpus shape.
        let key = PublicKey::from_openssh("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ").unwrap();
        let fp = fingerprint_of(&key);
        assert_eq!(fp.algorithm, "ssh-ed25519");
        assert!(fp.fingerprint.starts_with("SHA256:"), "{}", fp.fingerprint);
        assert!(!fp.fingerprint.ends_with('='), "padding is stripped: {}", fp.fingerprint);
        assert_eq!(fp.fingerprint.len(), "SHA256:".len() + 43);
    }
}
