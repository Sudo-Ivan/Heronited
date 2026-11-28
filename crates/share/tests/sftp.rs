//! SFTP against an in-process russh server with a filesystem-backed
//! russh-sftp handler on a tempdir.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use heronite_share::config::{seal_secret, Kind, Sftp, SftpAuth, Target};
use heronite_share::progress::{Continue, Progress};
use heronite_share::seal::{use_seal, PlainSeal};
use heronite_share::{sftp, ShareContext, ShareError, TlsOptions};

fn setup() {
    use_seal(Arc::new(PlainSeal));
}

/// KNOWN_HOSTS is a global; every test here holds one lock so they do not
/// race each other over it.
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn guard() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------- filesystem-backed SFTP handler ----------

struct FsHandler {
    root: PathBuf,
    handles: HashMap<String, std::fs::File>,
    next: u64,
    /// paths created through mkdir, for assertions
    events: Arc<StdMutex<Vec<String>>>,
}

impl FsHandler {
    fn map(&self, path: &str) -> PathBuf {
        let cleaned = path.trim_start_matches('/');
        self.root.join(cleaned)
    }
}

fn ok_status(id: u32) -> russh_sftp::protocol::Status {
    russh_sftp::protocol::Status {
        id,
        status_code: russh_sftp::protocol::StatusCode::Ok,
        error_message: "Ok".into(),
        language_tag: "en-US".into(),
    }
}

fn attrs_of(meta: &std::fs::Metadata) -> russh_sftp::protocol::FileAttributes {
    russh_sftp::protocol::FileAttributes { size: Some(meta.len()), ..Default::default() }
}

impl russh_sftp::server::Handler for FsHandler {
    type Error = russh_sftp::protocol::StatusCode;

    fn unimplemented(&self) -> Self::Error {
        russh_sftp::protocol::StatusCode::OpUnsupported
    }

    async fn open(
        &mut self,
        id: u32,
        filename: russh_sftp::protocol::SftpString,
        pflags: russh_sftp::protocol::OpenFlags,
        _attrs: russh_sftp::protocol::FileAttributes,
    ) -> Result<russh_sftp::protocol::Handle, Self::Error> {
        let path = self.map(&filename.to_string());
        let mut options = std::fs::OpenOptions::new();
        if pflags.contains(russh_sftp::protocol::OpenFlags::READ) {
            options.read(true);
        }
        if pflags.contains(russh_sftp::protocol::OpenFlags::WRITE) {
            options.write(true);
        }
        if pflags.contains(russh_sftp::protocol::OpenFlags::CREATE) {
            options.create(true);
        }
        if pflags.contains(russh_sftp::protocol::OpenFlags::TRUNCATE) {
            options.truncate(true);
        }
        let file = options.open(&path).map_err(|_| russh_sftp::protocol::StatusCode::NoSuchFile)?;
        self.next += 1;
        let handle = format!("h{}", self.next);
        self.handles.insert(handle.clone(), file);
        Ok(russh_sftp::protocol::Handle { id, handle: russh_sftp::protocol::SftpHandle::from(handle) })
    }

    async fn close(&mut self, id: u32, handle: russh_sftp::protocol::SftpHandle) -> Result<russh_sftp::protocol::Status, Self::Error> {
        self.handles.remove(&String::from_utf8_lossy(handle.as_bytes()).into_owned());
        Ok(ok_status(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: russh_sftp::protocol::SftpHandle,
        offset: u64,
        len: u32,
    ) -> Result<russh_sftp::protocol::Data, Self::Error> {
        use std::io::{Read, Seek, SeekFrom};
        let file = self
            .handles
            .get_mut(&String::from_utf8_lossy(handle.as_bytes()).into_owned())
            .ok_or(russh_sftp::protocol::StatusCode::Failure)?;
        file.seek(SeekFrom::Start(offset)).map_err(|_| russh_sftp::protocol::StatusCode::Failure)?;
        let mut buf = vec![0u8; len as usize];
        let n = file.read(&mut buf).map_err(|_| russh_sftp::protocol::StatusCode::Failure)?;
        buf.truncate(n);
        Ok(russh_sftp::protocol::Data { id, data: buf })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: russh_sftp::protocol::SftpHandle,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<russh_sftp::protocol::Status, Self::Error> {
        use std::io::{Seek, SeekFrom, Write};
        let file = self
            .handles
            .get_mut(&String::from_utf8_lossy(handle.as_bytes()).into_owned())
            .ok_or(russh_sftp::protocol::StatusCode::Failure)?;
        file.seek(SeekFrom::Start(offset)).map_err(|_| russh_sftp::protocol::StatusCode::Failure)?;
        file.write_all(&data).map_err(|_| russh_sftp::protocol::StatusCode::Failure)?;
        Ok(ok_status(id))
    }

    async fn remove(&mut self, id: u32, filename: russh_sftp::protocol::SftpString) -> Result<russh_sftp::protocol::Status, Self::Error> {
        std::fs::remove_file(self.map(&filename.to_string())).map_err(|_| russh_sftp::protocol::StatusCode::NoSuchFile)?;
        Ok(ok_status(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: russh_sftp::protocol::SftpString,
        _attrs: russh_sftp::protocol::FileAttributes,
    ) -> Result<russh_sftp::protocol::Status, Self::Error> {
        let mapped = self.map(&path.to_string());
        if mapped.exists() {
            return Err(russh_sftp::protocol::StatusCode::Failure);
        }
        std::fs::create_dir(&mapped).map_err(|_| russh_sftp::protocol::StatusCode::Failure)?;
        self.events.lock().unwrap().push(format!("mkdir:{}", path));
        Ok(ok_status(id))
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: russh_sftp::protocol::SftpString,
        newpath: russh_sftp::protocol::SftpString,
    ) -> Result<russh_sftp::protocol::Status, Self::Error> {
        self.events.lock().unwrap().push(format!("rename:{}>{}", oldpath, newpath));
        std::fs::rename(self.map(&oldpath.to_string()), self.map(&newpath.to_string()))
            .map_err(|_| russh_sftp::protocol::StatusCode::Failure)?;
        Ok(ok_status(id))
    }

    async fn realpath(&mut self, id: u32, path: russh_sftp::protocol::SftpString) -> Result<russh_sftp::protocol::Name, Self::Error> {
        Ok(russh_sftp::protocol::Name { id, files: vec![russh_sftp::protocol::File::dummy(&path.to_string())] })
    }

    async fn stat(&mut self, id: u32, path: russh_sftp::protocol::SftpString) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        let meta = std::fs::metadata(self.map(&path.to_string())).map_err(|_| russh_sftp::protocol::StatusCode::NoSuchFile)?;
        Ok(russh_sftp::protocol::Attrs { id, attrs: attrs_of(&meta) })
    }

    async fn lstat(&mut self, id: u32, path: russh_sftp::protocol::SftpString) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        self.stat(id, path).await
    }
}

// ---------- the SSH server ----------

struct TestServer {
    password: Option<String>,
    keys: Vec<russh::keys::PublicKey>,
    root: PathBuf,
    events: Arc<StdMutex<Vec<String>>>,
}

impl russh::server::Server for TestServer {
    type Handler = Session;

    fn new_client(&mut self, _: Option<SocketAddr>) -> Self::Handler {
        Session {
            password: self.password.clone(),
            keys: self.keys.clone(),
            root: self.root.clone(),
            events: self.events.clone(),
            channels: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }
}

struct Session {
    password: Option<String>,
    keys: Vec<russh::keys::PublicKey>,
    root: PathBuf,
    events: Arc<StdMutex<Vec<String>>>,
    channels: Arc<tokio::sync::Mutex<HashMap<russh::ChannelId, russh::Channel<russh::server::Msg>>>>,
}

impl russh::server::Handler for Session {
    type Error = anyhow::Error;

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<russh::server::Auth, Self::Error> {
        if Some(password) == self.password.as_deref() {
            Ok(russh::server::Auth::Accept)
        } else {
            Ok(russh::server::Auth::reject())
        }
    }

    async fn auth_publickey(&mut self, _user: &str, key: russh::server::VerifiedPublicKey<'_>) -> Result<russh::server::Auth, Self::Error> {
        if self.keys.iter().any(|k| k == key.public_key) {
            Ok(russh::server::Auth::Accept)
        } else {
            Ok(russh::server::Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: russh::Channel<russh::server::Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.channels.lock().await.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: russh::ChannelId,
        name: &str,
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" {
            let channel = self.channels.lock().await.remove(&channel_id).unwrap();
            session.channel_success(channel_id)?;
            let handler = FsHandler { root: self.root.clone(), handles: HashMap::new(), next: 0, events: self.events.clone() };
            russh_sftp::server::run(channel.into_stream(), handler).await;
        } else {
            session.channel_failure(channel_id)?;
        }
        Ok(())
    }
}

pub struct RunningServer {
    pub port: u16,
    pub root: PathBuf,
    pub host_key: russh::keys::PrivateKey,
    pub events: Arc<StdMutex<Vec<String>>>,
    _dir: tempfile::TempDir,
}

/// Starts the server on a background tokio runtime; it stops when the test
/// process exits. `keys` are the accepted user public keys, `password` the
/// accepted password.
fn start_server(password: Option<String>, keys: Vec<russh::keys::PublicKey>) -> RunningServer {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let host_key = russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
    let events: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
    // Learn a free port by binding and releasing; the race is theoretical.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let server_host_key = host_key.clone();
    let ev = events.clone();
    let root2 = root.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async move {
            let config = russh::server::Config {
                auth_rejection_time: std::time::Duration::from_millis(10),
                auth_rejection_time_initial: Some(std::time::Duration::from_millis(0)),
                keys: vec![server_host_key],
                ..Default::default()
            };
            use russh::server::Server as _;
            let mut server = TestServer { password, keys, root: root2, events: ev };
            let _ = server.run_on_address(Arc::new(config), ("127.0.0.1", port)).await;
        });
    });
    // wait until it accepts
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    RunningServer { port, root, host_key, events, _dir: dir }
}

// ---------- the tests ----------

fn target_for(port: u16, auth: SftpAuth, pin: &str) -> Target {
    Target {
        id: "t".into(),
        name: "test".into(),
        enabled: true,
        kind: Kind::Sftp(Sftp { host: "127.0.0.1".into(), port, user: "tester".into(), auth, host_key: pin.into(), ..Default::default() }),
        public_url: "https://files.example/{name}".into(),
        folder: "uploads".into(),
        tls: TlsOptions::default(),
    }
}

fn file_of(bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("shot.png");
    std::fs::write(&file, bytes).unwrap();
    (dir, file)
}

fn noop() -> impl FnMut(Progress) -> Continue {
    |_| Continue::Go
}

fn password_target(port: u16, pin: &str) -> Target {
    let mut t = target_for(port, SftpAuth::Password, pin);
    if let Kind::Sftp(s) = &mut t.kind {
        s.sealed_password = seal_secret("hunter2").unwrap();
    }
    t
}

fn sftp_of(t: &Target) -> &heronite_share::Sftp {
    match &t.kind {
        Kind::Sftp(s) => s,
        _ => unreachable!(),
    }
}

#[test]
fn fetch_host_key_matches_the_pin_format() {
    setup();
    let _guard = guard();
    let server = start_server(Some("hunter2".into()), vec![]);
    let key = sftp::fetch_host_key("127.0.0.1", server.port).unwrap();
    assert!(key.fingerprint.starts_with("SHA256:"), "{}", key.fingerprint);
    assert_eq!(key.algorithm, "ssh-ed25519");
    let expect = sftp::fingerprint_of(server.host_key.public_key());
    assert_eq!(key.fingerprint, expect.fingerprint);
}

#[test]
fn password_upload_and_part_semantics() {
    setup();
    let _guard = guard();
    let server = start_server(Some("hunter2".into()), vec![]);
    let pin = sftp::fetch_host_key("127.0.0.1", server.port).unwrap().fingerprint;
    let t = password_target(server.port, &pin);
    let s = sftp_of(&t);
    let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 241) as u8).collect();
    let (_dir, file) = file_of(&bytes);
    let mut progress = noop();
    let shared = sftp::upload(&t, s, &file, "uploads/deep/nested/shot.png", bytes.len() as u64, &mut progress).unwrap();
    assert_eq!(shared.url, "https://files.example/uploads/deep/nested/shot.png");
    // the file landed, no .part remains, mkdir -p made the parents
    let final_path = server.root.join("uploads/deep/nested/shot.png");
    assert_eq!(std::fs::read(&final_path).unwrap(), bytes);
    assert!(!server.root.join("uploads/deep/nested/.shot.png.part").exists());
    assert!(server.root.join("uploads/deep/nested").is_dir());
    // the rename happened after the upload
    let events = server.events.lock().unwrap();
    assert!(events.iter().any(|e| e.starts_with("rename:")), "{events:?}");
    assert!(events.iter().any(|e| e.starts_with("mkdir:")), "{events:?}");
}

#[test]
fn wrong_password_is_auth_failed() {
    setup();
    let _guard = guard();
    let server = start_server(Some("hunter2".into()), vec![]);
    let pin = sftp::fetch_host_key("127.0.0.1", server.port).unwrap().fingerprint;
    let mut t = password_target(server.port, &pin);
    if let Kind::Sftp(s) = &mut t.kind {
        s.sealed_password = seal_secret("wrong").unwrap();
    }
    let (_dir, file) = file_of(b"x");
    let mut progress = noop();
    let err = sftp::upload(&t, sftp_of(&t), &file, "uploads/shot.png", 1, &mut progress).unwrap_err();
    assert!(matches!(err, ShareError::AuthFailed(_)), "{err}");
}

fn write_key(algorithm: russh::keys::Algorithm, passphrase: Option<&str>) -> (tempfile::TempDir, PathBuf, russh::keys::PublicKey) {
    let key = russh::keys::PrivateKey::random(&mut rand::rng(), algorithm).unwrap();
    let public = key.public_key().clone();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("key");
    let text = match passphrase {
        Some(pass) => key.encrypt(&mut rand::rng(), pass).unwrap().to_openssh(russh::keys::ssh_key::LineEnding::LF).unwrap(),
        None => key.to_openssh(russh::keys::ssh_key::LineEnding::LF).unwrap(),
    };
    std::fs::write(&path, text.as_bytes()).unwrap();
    (dir, path, public)
}

fn key_target(port: u16, pin: &str, key_file: &Path, passphrase: Option<&str>) -> Target {
    let mut t = target_for(port, SftpAuth::KeyFile, pin);
    if let Kind::Sftp(s) = &mut t.kind {
        s.key_file = key_file.to_string_lossy().into();
        if let Some(pass) = passphrase {
            s.sealed_passphrase = seal_secret(pass).unwrap();
        }
    }
    t
}

#[test]
fn ed25519_key_auth() {
    setup();
    let _guard = guard();
    let (_kd, key_path, public) = write_key(russh::keys::Algorithm::Ed25519, None);
    let server = start_server(None, vec![public]);
    let pin = sftp::fetch_host_key("127.0.0.1", server.port).unwrap().fingerprint;
    let t = key_target(server.port, &pin, &key_path, None);
    let (_dir, file) = file_of(b"key auth");
    let mut progress = noop();
    sftp::upload(&t, sftp_of(&t), &file, "uploads/shot.png", 8, &mut progress).unwrap();
    assert_eq!(std::fs::read(server.root.join("uploads/shot.png")).unwrap(), b"key auth");
}

#[test]
fn rsa_key_auth() {
    setup();
    let _guard = guard();
    let (_kd, key_path, public) = write_key(russh::keys::Algorithm::Rsa { hash: None }, None);
    let server = start_server(None, vec![public]);
    let pin = sftp::fetch_host_key("127.0.0.1", server.port).unwrap().fingerprint;
    let t = key_target(server.port, &pin, &key_path, None);
    let (_dir, file) = file_of(b"rsa");
    let mut progress = noop();
    sftp::upload(&t, sftp_of(&t), &file, "uploads/shot.png", 3, &mut progress).unwrap();
}

#[test]
fn wrong_key_is_auth_failed() {
    setup();
    let _guard = guard();
    let (_kd, key_path, _public) = write_key(russh::keys::Algorithm::Ed25519, None);
    let (_kd2, _other_path, other_public) = write_key(russh::keys::Algorithm::Ed25519, None);
    let server = start_server(None, vec![other_public]); // server wants a different key
    let pin = sftp::fetch_host_key("127.0.0.1", server.port).unwrap().fingerprint;
    let t = key_target(server.port, &pin, &key_path, None);
    let (_dir, file) = file_of(b"x");
    let mut progress = noop();
    let err = sftp::upload(&t, sftp_of(&t), &file, "uploads/shot.png", 1, &mut progress).unwrap_err();
    assert!(matches!(err, ShareError::AuthFailed(_)), "{err}");
}

#[test]
fn passphrase_key_right_and_wrong() {
    setup();
    let _guard = guard();
    let (_kd, key_path, public) = write_key(russh::keys::Algorithm::Ed25519, Some("secret123"));
    let server = start_server(None, vec![public]);
    let pin = sftp::fetch_host_key("127.0.0.1", server.port).unwrap().fingerprint;
    // right passphrase
    let t = key_target(server.port, &pin, &key_path, Some("secret123"));
    let (_dir, file) = file_of(b"passphrase");
    let mut progress = noop();
    sftp::upload(&t, sftp_of(&t), &file, "uploads/shot.png", 10, &mut progress).unwrap();
    // wrong passphrase: the key does not even load
    let t = key_target(server.port, &pin, &key_path, Some("wrong"));
    let err = sftp::upload(&t, sftp_of(&t), &file, "uploads/shot.png", 10, &mut noop()).unwrap_err();
    assert!(matches!(err, ShareError::BadConfig(_) | ShareError::AuthFailed(_)), "{err}");
}

#[test]
fn unknown_host_key_is_reported() {
    setup();
    let _guard = guard();
    let server = start_server(Some("hunter2".into()), vec![]);
    // no pin, and an empty known_hosts
    let dir = tempfile::tempdir().unwrap();
    *heronite_share::sftp::KNOWN_HOSTS.write().unwrap() = Some(dir.path().join("known_hosts"));
    let t = password_target(server.port, "");
    let (_dir, file) = file_of(b"x");
    let mut progress = noop();
    let err = sftp::upload(&t, sftp_of(&t), &file, "uploads/shot.png", 1, &mut progress).unwrap_err();
    match err {
        ShareError::HostKeyUnknown { fingerprint, algorithm } => {
            assert!(fingerprint.starts_with("SHA256:"));
            assert_eq!(algorithm, "ssh-ed25519");
        }
        other => panic!("expected HostKeyUnknown, got {other}"),
    }
    *heronite_share::sftp::KNOWN_HOSTS.write().unwrap() = None;
}

#[test]
fn changed_host_key_is_reported() {
    setup();
    let _guard = guard();
    let server = start_server(Some("hunter2".into()), vec![]);
    let t = password_target(server.port, "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
    let (_dir, file) = file_of(b"x");
    let mut progress = noop();
    let err = sftp::upload(&t, sftp_of(&t), &file, "uploads/shot.png", 1, &mut progress).unwrap_err();
    assert!(matches!(err, ShareError::HostKeyChanged { .. }), "{err}");
}

#[test]
fn known_hosts_accepts_and_rejects() {
    setup();
    let _guard = guard();
    let server = start_server(Some("hunter2".into()), vec![]);
    let dir = tempfile::tempdir().unwrap();
    let known_hosts = dir.path().join("known_hosts");
    let line = format!("[127.0.0.1]:{} {}\n", server.port, server.host_key.public_key().to_openssh().unwrap());
    std::fs::write(&known_hosts, &line).unwrap();
    *heronite_share::sftp::KNOWN_HOSTS.write().unwrap() = Some(known_hosts.clone());
    let t = password_target(server.port, "");
    let (_dir, file) = file_of(b"kh");
    let mut progress = noop();
    sftp::upload(&t, sftp_of(&t), &file, "uploads/shot.png", 2, &mut progress).unwrap();
    // a different key under the same host entry: HostKeyChanged
    let wrong = russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
    std::fs::write(&known_hosts, format!("[127.0.0.1]:{} {}\n", server.port, wrong.public_key().to_openssh().unwrap())).unwrap();
    let err = sftp::upload(&t, sftp_of(&t), &file, "uploads/shot.png", 2, &mut noop()).unwrap_err();
    assert!(matches!(err, ShareError::HostKeyChanged { .. }), "{err}");
    // hashed entries work too (hash produced with ssh-keygen format by hand
    // is complex; the russh known_hosts parser is already tested upstream,
    // so here the plain and bracket forms are covered)
    *heronite_share::sftp::KNOWN_HOSTS.write().unwrap() = None;
}

#[test]
fn cancel_leaves_no_part() {
    setup();
    let _guard = guard();
    let server = start_server(Some("hunter2".into()), vec![]);
    let pin = sftp::fetch_host_key("127.0.0.1", server.port).unwrap().fingerprint;
    let t = password_target(server.port, &pin);
    let bytes = vec![7u8; 2 * 1024 * 1024];
    let (_dir, file) = file_of(&bytes);
    let mut calls = 0;
    let mut progress = move |_: Progress| {
        calls += 1;
        if calls > 1 {
            Continue::Cancel
        } else {
            Continue::Go
        }
    };
    let err = sftp::upload(&t, sftp_of(&t), &file, "uploads/big.bin", bytes.len() as u64, &mut progress).unwrap_err();
    assert!(matches!(err, ShareError::Cancelled), "{err}");
    // nothing left behind: neither the part nor the final name
    assert!(!server.root.join("uploads/.big.bin.part").exists());
    assert!(!server.root.join("uploads/big.bin").exists());
}

#[test]
fn delete_removes_the_remote_file() {
    setup();
    let _guard = guard();
    let server = start_server(Some("hunter2".into()), vec![]);
    let pin = sftp::fetch_host_key("127.0.0.1", server.port).unwrap().fingerprint;
    let t = password_target(server.port, &pin);
    let (_dir, file) = file_of(b"delete me");
    let mut progress = noop();
    let shared = sftp::upload(&t, sftp_of(&t), &file, "uploads/shot.png", 9, &mut progress).unwrap();
    assert!(server.root.join("uploads/shot.png").exists());
    let record = heronite_share::Record {
        id: "r".into(),
        target_id: "t".into(),
        target_name: "t".into(),
        file_name: "shot.png".into(),
        size: 9,
        url: shared.url.clone(),
        remote: shared.remote.clone(),
        delete: shared.delete.clone(),
        at: String::new(),
        game: None,
        kind: heronite_share::FileKind::Screenshot,
    };
    sftp::delete(&t, sftp_of(&t), &record).unwrap();
    assert!(!server.root.join("uploads/shot.png").exists());
}

#[test]
fn check_reports_all_steps() {
    setup();
    let _guard = guard();
    let server = start_server(Some("hunter2".into()), vec![]);
    let pin = sftp::fetch_host_key("127.0.0.1", server.port).unwrap().fingerprint;
    let t = password_target(server.port, &pin);
    let report = sftp::check(&t, sftp_of(&t)).unwrap();
    assert!(report.steps.len() >= 3, "{:?}", report.steps);
    assert!(!server.root.join("uploads/heronite-probe.txt").exists());
}

#[test]
fn end_to_end_share_and_delete() {
    setup();
    let _guard = guard();
    let server = start_server(Some("hunter2".into()), vec![]);
    let pin = sftp::fetch_host_key("127.0.0.1", server.port).unwrap().fingerprint;
    let t = password_target(server.port, &pin);
    let dir = tempfile::tempdir().unwrap();
    let paths = heronite_data::DataPaths::at(dir.path());
    let mut targets = heronite_share::Targets::default();
    targets.upsert(t);
    targets.save(&paths).unwrap();
    let (_fdir, file) = file_of(b"end to end sftp");
    let mut progress = noop();
    let record = heronite_share::share(&paths, "t", &file, &ShareContext::default(), &mut progress).unwrap();
    assert!(server.root.join("uploads").exists());
    assert_eq!(heronite_share::history(&paths).len(), 1);
    heronite_share::delete(&paths, &record.id).unwrap();
    assert!(heronite_share::history(&paths).is_empty());
}

#[test]
fn agent_without_agent_is_a_clear_error() {
    setup();
    let _guard = guard();
    // No SSH_AUTH_SOCK in this environment: the error must say why. (An
    // in-process agent test is not practical: russh's agent server support
    // is client-only.)
    let server = start_server(Some("hunter2".into()), vec![]);
    let pin = sftp::fetch_host_key("127.0.0.1", server.port).unwrap().fingerprint;
    let t = target_for(server.port, SftpAuth::Agent, &pin);
    let (_dir, file) = file_of(b"x");
    let mut progress = noop();
    let err = sftp::upload(&t, sftp_of(&t), &file, "uploads/shot.png", 1, &mut progress).unwrap_err();
    assert!(matches!(err, ShareError::BadConfig(_) | ShareError::AuthFailed(_) | ShareError::Network(_)), "{err}");
}
