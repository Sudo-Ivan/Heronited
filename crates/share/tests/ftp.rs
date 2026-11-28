//! FTP and FTPS against in-process servers: libunftp for plain and explicit
//! TLS, plus a small hand-written server for implicit FTPS and rename
//! refusal, which libunftp cannot be told to do.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;

use heronite_share::config::{seal_secret, Ftp, FtpTls, Kind, Target};
use heronite_share::progress::{Continue, Progress};
use heronite_share::seal::{use_seal, PlainSeal};
use heronite_share::{ftp, ShareContext, ShareError, TlsOptions};

fn setup() {
    use_seal(Arc::new(PlainSeal));
}

fn noop() -> impl FnMut(Progress) -> Continue {
    |_| Continue::Go
}

fn file_of(bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shot.png");
    std::fs::write(&path, bytes).unwrap();
    (dir, path)
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn target(port: u16, user: &str, password: &str, tls: FtpTls) -> Target {
    Target {
        id: "t".into(),
        name: "t".into(),
        enabled: true,
        kind: Kind::Ftp(Ftp {
            host: "127.0.0.1".into(),
            port,
            user: user.into(),
            sealed_password: if password.is_empty() { String::new() } else { seal_secret(password).unwrap() },
            tls,
        }),
        public_url: "https://files.example/{name}".into(),
        folder: "uploads".into(),
        tls: TlsOptions::default(),
    }
}

fn ftp_of(t: &Target) -> &Ftp {
    match &t.kind {
        Kind::Ftp(f) => f,
        _ => unreachable!(),
    }
}

// ---------- libunftp server ----------

#[derive(Debug)]
struct UserAuth {
    user: String,
    password: String,
}

#[async_trait::async_trait]
impl unftp_core::auth::Authenticator for UserAuth {
    async fn authenticate(
        &self,
        username: &str,
        creds: &unftp_core::auth::Credentials,
    ) -> Result<unftp_core::auth::Principal, unftp_core::auth::AuthenticationError> {
        if username == self.user && creds.password.as_deref() == Some(self.password.as_str()) {
            Ok(unftp_core::auth::Principal { username: username.to_string() })
        } else {
            Err(unftp_core::auth::AuthenticationError::BadPassword)
        }
    }
}

struct Unftp {
    port: u16,
    root: PathBuf,
    _temp: tempfile::TempDir,
}

fn start_unftp(auth: Arc<dyn unftp_core::auth::Authenticator + Send + Sync>, certs: Option<(PathBuf, PathBuf)>) -> Unftp {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let port = free_port();
    let root_clone = root.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async move {
            let mut builder =
                libunftp::ServerBuilder::with_authenticator(Box::new(move || unftp_sbe_fs::Filesystem::new(&root_clone).unwrap()), auth)
                    .passive_host(libunftp::options::PassiveHost::Ip(Ipv4Addr::LOCALHOST));
            if let Some((cert, key)) = certs {
                builder = builder.ftps(cert, key);
            }
            let server = builder.build().unwrap();
            server.listen(format!("127.0.0.1:{port}")).await.unwrap();
        });
    });
    // wait until the control port answers
    for _ in 0..100 {
        if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    Unftp { port, root, _temp: temp }
}

fn user_server() -> Unftp {
    start_unftp(Arc::new(UserAuth { user: "ivan".into(), password: "hunter2".into() }), None)
}

fn make_cert(dir: &std::path::Path) -> (PathBuf, PathBuf) {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string(), "localhost".to_string()]).unwrap();
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    std::fs::write(&cert_path, cert.pem()).unwrap();
    std::fs::write(&key_path, signing_key.serialize_pem()).unwrap();
    (cert_path, key_path)
}

// ---------- hand-written mini FTP server (implicit TLS, rename refusal) ----------

/// Speaks just enough FTP for the upload path: USER/PASS/PBSZ/PROT/TYPE,
/// EPSV, MKD, CWD, STOR, RNFR/RNTO, DELE, QUIT over a single connection.
fn mini_server(root: PathBuf, tls: Option<Arc<rustls::ServerConfig>>, refuse_rename: bool, port: u16) {
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let root = root.clone();
            let tls = tls.clone();
            std::thread::spawn(move || {
                let tls_conn = tls.clone();
                let stream: Box<dyn ReadWrite> = match &tls_conn {
                    Some(config) => match rustls::ServerConnection::new(config.clone()) {
                        Ok(mut conn) => {
                            let mut stream = stream;
                            // Drive the handshake eagerly; rustls does not do
                            // it as a side effect of the first write.
                            let mut rounds = 0;
                            while conn.is_handshaking() && rounds < 100 {
                                match conn.complete_io(&mut stream) {
                                    Ok(_) => rounds += 1,
                                    Err(e) => {
                                        eprintln!("mini: handshake failed: {e}");
                                        return;
                                    }
                                }
                            }
                            if conn.is_handshaking() {
                                eprintln!("mini: handshake did not finish");
                                return;
                            }
                            Box::new(rustls::StreamOwned::new(conn, stream))
                        }
                        Err(e) => {
                            eprintln!("mini: no connection: {e}");
                            return;
                        }
                    },
                    None => Box::new(stream),
                };
                serve_ftp(stream, root, refuse_rename, tls_conn);
            });
        }
    });
}

trait ReadWrite: Read + Write + Send {}
impl<T: Read + Write + Send> ReadWrite for T {}

fn serve_ftp(stream: Box<dyn ReadWrite>, root: PathBuf, refuse_rename: bool, tls: Option<Arc<rustls::ServerConfig>>) {
    let stream = stream;
    let mut reader = BufReader::new(stream);
    let mut cwd = PathBuf::new();
    let mut data_listener: Option<TcpListener> = None;
    let mut pending_rename: Option<PathBuf> = None;
    write_line(reader.get_mut(), "220 mini ftp");
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let line = line.trim().to_string();
        let (verb, arg) = match line.split_once(' ') {
            Some((v, a)) => (v.to_uppercase(), a.to_string()),
            None => (line.to_uppercase(), String::new()),
        };
        let w = reader.get_mut();
        match verb.as_str() {
            "USER" | "PASS" => write_line(w, "230 logged in"),
            "PBSZ" => write_line(w, "200 ok"),
            "PROT" => write_line(w, "200 ok"),
            "TYPE" => write_line(w, "200 binary"),
            "SYST" | "FEAT" | "OPTS" | "NOOP" | "SIZE" | "MDTM" | "REST" | "ALLO" | "HELP" | "STAT" | "MODE" | "STRU" => {
                write_line(w, "200 ok")
            }
            "PWD" | "XPWD" => write_line(w, "257 \"/\""),
            "EPSV" => {
                let l = TcpListener::bind("127.0.0.1:0").unwrap();
                let port = l.local_addr().unwrap().port();
                data_listener = Some(l);
                write_line(w, &format!("229 (|||{port}|)"));
            }
            "PASV" => {
                let l = TcpListener::bind("127.0.0.1:0").unwrap();
                let port = l.local_addr().unwrap().port();
                data_listener = Some(l);
                write_line(w, &format!("227 (127,0,0,1,{},{})", port / 256, port % 256));
            }
            "MKD" => {
                let _ = std::fs::create_dir_all(root.join(cwd.join(&arg)));
                write_line(w, "257 made");
            }
            "CWD" => {
                if root.join(cwd.join(&arg)).is_dir() {
                    cwd = cwd.join(&arg);
                    write_line(w, "250 ok");
                } else {
                    write_line(w, "550 no such directory");
                }
            }
            "STOR" => {
                write_line(w, "150 go");
                let target = root.join(cwd.join(&arg));
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let accepted = data_listener.take().and_then(|l| l.accept().ok()).map(|(s, _)| s);
                let accepted: Option<Box<dyn ReadWrite>> = accepted.map(|data| match &tls {
                    Some(config) => match rustls::ServerConnection::new(config.clone()) {
                        Ok(conn) => Box::new(rustls::StreamOwned::new(conn, data)) as Box<dyn ReadWrite>,
                        Err(_) => Box::new(data) as Box<dyn ReadWrite>,
                    },
                    None => Box::new(data) as Box<dyn ReadWrite>,
                });
                match accepted {
                    Some(mut data) => {
                        let mut file = std::fs::File::create(&target).unwrap();
                        let mut buf = [0u8; 8192];
                        loop {
                            match data.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => {
                                    file.write_all(&buf[..n]).unwrap();
                                }
                                Err(_) => break,
                            }
                        }
                        write_line(w, "226 done");
                    }
                    None => write_line(w, "425 no data connection"),
                }
            }
            "RNFR" => {
                if refuse_rename {
                    write_line(w, "550 rename not allowed");
                } else {
                    pending_rename = Some(cwd.join(&arg));
                    write_line(w, "350 go ahead");
                }
            }
            "RNTO" => {
                if let Some(from) = pending_rename.take() {
                    let _ = std::fs::rename(root.join(&from), root.join(cwd.join(&arg)));
                    write_line(w, "250 renamed");
                } else {
                    write_line(w, "503 no RNFR");
                }
            }
            "DELE" => {
                let _ = std::fs::remove_file(root.join(cwd.join(&arg)));
                write_line(w, "250 deleted");
            }
            "QUIT" => {
                write_line(w, "221 bye");
                return;
            }
            _ => write_line(w, "502 not implemented"),
        }
    }
}

fn write_line(w: &mut dyn Write, line: &str) {
    let _ = w.write_all(format!("{line}\r\n").as_bytes());
    let _ = w.flush();
}

fn start_mini(tls_cert: Option<(&PathBuf, &PathBuf)>, refuse_rename: bool) -> (u16, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let port = free_port();
    let tls = tls_cert.map(|(cert, key)| {
        let certs: Vec<_> =
            rustls_pemfile::certs(&mut std::io::BufReader::new(std::fs::File::open(cert).unwrap())).collect::<Result<_, _>>().unwrap();
        let key = rustls_pemfile::private_key(&mut std::io::BufReader::new(std::fs::File::open(key).unwrap())).unwrap().unwrap();
        Arc::new(
            rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .unwrap(),
        )
    });
    mini_server(root, tls, refuse_rename, port);
    for _ in 0..100 {
        if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    (port, temp)
}

// ---------- plaintext tests ----------

#[test]
fn upload_lands_after_mkdir_and_rename() {
    setup();
    let server = user_server();
    let t = target(server.port, "ivan", "hunter2", FtpTls::None);
    let bytes = b"png bytes here".to_vec();
    let (_dir, file) = file_of(&bytes);
    let mut progress = noop();
    let shared = ftp::upload(&t, ftp_of(&t), &file, "uploads/deep/shot.png", bytes.len() as u64, &mut progress).unwrap();
    assert_eq!(std::fs::read(server.root.join("uploads/deep/shot.png")).unwrap(), bytes);
    assert!(!server.root.join("uploads/deep/.shot.png.part").exists());
    assert_eq!(shared.url, "https://files.example/uploads/deep/shot.png");
    assert_eq!(shared.delete.as_deref(), Some("uploads/deep/shot.png"));
}

#[test]
fn wrong_password_is_auth_failed() {
    setup();
    let server = user_server();
    let t = target(server.port, "ivan", "wrong", FtpTls::None);
    let (_dir, file) = file_of(b"x");
    let err = ftp::upload(&t, ftp_of(&t), &file, "uploads/shot.png", 1, &mut noop()).unwrap_err();
    assert!(matches!(err, ShareError::AuthFailed(_)), "{err}");
}

#[test]
fn anonymous_login_works() {
    setup();
    let server = start_unftp(Arc::new(libunftp::auth::AnonymousAuthenticator), None);
    let t = target(server.port, "", "", FtpTls::None);
    let bytes = b"anon".to_vec();
    let (_dir, file) = file_of(&bytes);
    ftp::upload(&t, ftp_of(&t), &file, "uploads/shot.png", bytes.len() as u64, &mut noop()).unwrap();
    assert!(server.root.join("uploads/shot.png").exists());
}

#[test]
fn cancel_leaves_no_part() {
    setup();
    let server = user_server();
    let t = target(server.port, "ivan", "hunter2", FtpTls::None);
    let bytes = vec![9u8; 2 * 1024 * 1024];
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
    let err = ftp::upload(&t, ftp_of(&t), &file, "uploads/big.bin", bytes.len() as u64, &mut progress).unwrap_err();
    assert!(matches!(err, ShareError::Cancelled), "{err}");
    assert!(!server.root.join("uploads/.big.bin.part").exists());
    assert!(!server.root.join("uploads/big.bin").exists());
}

#[test]
fn delete_removes_the_remote_file() {
    setup();
    let server = user_server();
    let t = target(server.port, "ivan", "hunter2", FtpTls::None);
    let (_dir, file) = file_of(b"delete me");
    let shared = ftp::upload(&t, ftp_of(&t), &file, "uploads/shot.png", 9, &mut noop()).unwrap();
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
    ftp::delete(&t, ftp_of(&t), &record).unwrap();
    assert!(!server.root.join("uploads/shot.png").exists());
}

#[test]
fn check_reports_all_steps() {
    setup();
    let server = user_server();
    let t = target(server.port, "ivan", "hunter2", FtpTls::None);
    let report = ftp::check(&t, ftp_of(&t)).unwrap();
    assert!(report.steps.len() >= 3, "{:?}", report.steps);
    assert!(!server.root.join("uploads/heronite-probe.txt").exists());
}

#[test]
fn end_to_end_share_and_delete() {
    setup();
    let server = user_server();
    let t = target(server.port, "ivan", "hunter2", FtpTls::None);
    let dir = tempfile::tempdir().unwrap();
    let paths = heronite_data::DataPaths::at(dir.path());
    let mut targets = heronite_share::Targets::default();
    targets.upsert(t);
    targets.save(&paths).unwrap();
    let (_fdir, file) = file_of(b"end to end ftp");
    let record = heronite_share::share(&paths, "t", &file, &ShareContext::default(), &mut noop()).unwrap();
    assert_eq!(heronite_share::history(&paths).len(), 1);
    assert!(server.root.join("uploads").exists());
    heronite_share::delete(&paths, &record.id).unwrap();
    assert!(heronite_share::history(&paths).is_empty());
}

// ---------- explicit FTPS (libunftp) ----------

fn ftps_server() -> (Unftp, tempfile::TempDir) {
    let certdir = tempfile::tempdir().unwrap();
    let (cert, key) = make_cert(certdir.path());
    let server = start_unftp(Arc::new(UserAuth { user: "ivan".into(), password: "hunter2".into() }), Some((cert, key)));
    (server, certdir)
}

#[test]
fn explicit_ftps_fails_without_trust() {
    setup();
    let (server, _certs) = ftps_server();
    let t = target(server.port, "ivan", "hunter2", FtpTls::Explicit);
    let (_dir, file) = file_of(b"x");
    let err = ftp::upload(&t, ftp_of(&t), &file, "uploads/shot.png", 1, &mut noop()).unwrap_err();
    assert!(matches!(err, ShareError::Tls(_)), "{err}");
}

#[test]
fn explicit_ftps_works_with_ca_file() {
    setup();
    let (server, certs) = ftps_server();
    let mut t = target(server.port, "ivan", "hunter2", FtpTls::Explicit);
    t.tls.ca_file = Some(certs.path().join("cert.pem").to_string_lossy().into());
    let bytes = b"tls upload".to_vec();
    let (_dir, file) = file_of(&bytes);
    ftp::upload(&t, ftp_of(&t), &file, "uploads/shot.png", bytes.len() as u64, &mut noop()).unwrap();
    assert_eq!(std::fs::read(server.root.join("uploads/shot.png")).unwrap(), bytes);
}

#[test]
fn explicit_ftps_works_insecure() {
    setup();
    let (server, _certs) = ftps_server();
    let mut t = target(server.port, "ivan", "hunter2", FtpTls::Explicit);
    t.tls.insecure = true;
    let (_dir, file) = file_of(b"insecure");
    ftp::upload(&t, ftp_of(&t), &file, "uploads/shot.png", 8, &mut noop()).unwrap();
    assert!(server.root.join("uploads/shot.png").exists());
}

// ---------- implicit FTPS and rename refusal (mini server) ----------

#[test]
fn implicit_ftps_fails_without_trust_and_works_with_ca() {
    setup();
    let certdir = tempfile::tempdir().unwrap();
    let (cert, key) = make_cert(certdir.path());
    let (port, temp) = start_mini(Some((&cert, &key)), false);
    let t = target(port, "ivan", "hunter2", FtpTls::Implicit);
    let (_dir, file) = file_of(b"x");
    let err = ftp::upload(&t, ftp_of(&t), &file, "uploads/shot.png", 1, &mut noop()).unwrap_err();
    assert!(matches!(err, ShareError::Tls(_)), "{err}");

    let mut trusted = target(port, "ivan", "hunter2", FtpTls::Implicit);
    trusted.tls.ca_file = Some(cert.to_string_lossy().into());
    let bytes = b"implicit tls".to_vec();
    let (_d2, file2) = file_of(&bytes);
    ftp::upload(&trusted, ftp_of(&trusted), &file2, "uploads/shot.png", bytes.len() as u64, &mut noop()).unwrap();
    assert_eq!(std::fs::read(temp.path().join("root/uploads/shot.png")).unwrap(), bytes);

    let mut insecure = target(port, "ivan", "hunter2", FtpTls::Implicit);
    insecure.tls.insecure = true;
    let (_d3, file3) = file_of(b"i2");
    ftp::upload(&insecure, ftp_of(&insecure), &file3, "uploads/other.png", 2, &mut noop()).unwrap();
    assert!(temp.path().join("root/uploads/other.png").exists());
}

#[test]
fn rename_refusal_falls_back_to_direct_stor() {
    setup();
    let (port, temp) = start_mini(None, true);
    let t = target(port, "ivan", "hunter2", FtpTls::None);
    let bytes = b"fallback stor".to_vec();
    let (_dir, file) = file_of(&bytes);
    ftp::upload(&t, ftp_of(&t), &file, "uploads/shot.png", bytes.len() as u64, &mut noop()).unwrap();
    assert_eq!(std::fs::read(temp.path().join("root/uploads/shot.png")).unwrap(), bytes);
    assert!(!temp.path().join("root/uploads/.shot.png.part").exists());
}
