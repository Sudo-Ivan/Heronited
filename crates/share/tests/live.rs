//! Live tests against real servers, driven by scripts/share-live.sh.
//! Every test is `#[ignore]`d; the script starts the servers, exports the
//! endpoints and runs them one thread at a time.
//!
//! Environment (set by scripts/share-live.sh):
//!   SHARE_LIVE_WEBDAV        http://127.0.0.1:port of `rclone serve webdav`
//!   SHARE_LIVE_FTP           host:port of `rclone serve ftp` (plain)
//!   SHARE_LIVE_FTPS          host:port of `rclone serve ftp` (explicit TLS)
//!   SHARE_LIVE_FTPS_CA       path to the PEM certificate the FTPS server uses
//!   SHARE_LIVE_SFTP_RCLONE   host:port of `rclone serve sftp`
//!   SHARE_LIVE_SSHD          host:port of the real OpenSSH sshd (key auth)
//!   SHARE_LIVE_SSH_KEY       path to the user private key for the sshd
//!   SHARE_LIVE_SSH_ROOT      absolute directory the sshd user can write
//!   SHARE_LIVE_S3_RCLONE     http endpoint of `rclone serve s3`
//!   SHARE_LIVE_MINIO         http endpoint of MinIO in podman, when it runs
//!   SHARE_LIVE_USER / SHARE_LIVE_PASS    credentials for the rclone servers
//!   SHARE_LIVE_S3_KEY / SHARE_LIVE_S3_SECRET  rclone s3 access keys
//!
//! Every test prints exactly one line `LIVE-RESULT <name> PASS` or
//! `LIVE-RESULT <name> SKIP <reason>`; a panic is a FAIL the script reports.

use std::path::PathBuf;
use std::sync::Arc;

use heronite_share::config::{seal_secret, Ftp, FtpTls, Kind, Sftp, SftpAuth, Target, WebDav, S3};
use heronite_share::progress::{Continue, Progress};
use heronite_share::seal::{use_seal, PlainSeal};
use heronite_share::{ftp, sftp, webdav, ShareError, TlsOptions};
use sha2::{Digest, Sha256};

fn setup() {
    use_seal(Arc::new(PlainSeal));
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn pass(name: &str) {
    println!("LIVE-RESULT {name} PASS");
}

fn skip(name: &str, reason: &str) {
    println!("LIVE-RESULT {name} SKIP {reason}");
}

fn target(kind: Kind, public_url: &str) -> Target {
    Target {
        id: "t".into(),
        name: "live".into(),
        enabled: true,
        kind,
        public_url: public_url.into(),
        folder: "live-test".into(),
        tls: TlsOptions::default(),
    }
}

fn big_file(mebibytes: usize) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("clip.mp4");
    // filled, not sparse: the server really receives every byte
    let mut f = std::fs::File::create(&file).unwrap();
    use std::io::Write;
    for i in 0..mebibytes {
        let mut chunk = vec![0u8; 1024 * 1024];
        chunk[0] = i as u8;
        f.write_all(&chunk).unwrap();
    }
    (dir, file)
}

fn small_file() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("shot.png");
    std::fs::write(&file, b"live test bytes").unwrap();
    (dir, file)
}

fn noop() -> impl FnMut(Progress) -> Continue {
    |_| Continue::Go
}

fn cancelling() -> impl FnMut(Progress) -> Continue {
    let mut calls = 0;
    move |_: Progress| {
        calls += 1;
        if calls > 1 {
            Continue::Cancel
        } else {
            Continue::Go
        }
    }
}

fn sha256_of(path: &std::path::Path) -> String {
    hex::encode(Sha256::digest(std::fs::read(path).unwrap()))
}

fn record_of(shared: &heronite_share::Shared, size: u64) -> heronite_share::Record {
    heronite_share::Record {
        id: "r".into(),
        target_id: "t".into(),
        target_name: "t".into(),
        file_name: "shot.png".into(),
        size,
        url: shared.url.clone(),
        remote: shared.remote.clone(),
        delete: shared.delete.clone(),
        at: String::new(),
        game: None,
        kind: heronite_share::FileKind::Screenshot,
    }
}

/// Reads a file back over plain HTTP from the rclone webdav server (which
/// shares the same root as the rclone ftp/sftp servers), with basic auth.
fn webdav_get(base: &str, remote: &str) -> Option<Vec<u8>> {
    // rclone caches directory listings briefly (the script sets 500 ms), so
    // a fresh upload can 404 for the first moments: poll a little.
    for _ in 0..30 {
        if let Some(bytes) = webdav_get_once(base, remote) {
            return Some(bytes);
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    None
}

/// Absence is polled too: the same cache lag applies after a delete.
fn webdav_absent(base: &str, remote: &str) -> bool {
    for _ in 0..30 {
        if webdav_get_once(base, remote).is_none() {
            // confirmed absent for two cache rounds
            std::thread::sleep(std::time::Duration::from_millis(600));
            return webdav_get_once(base, remote).is_none();
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    false
}

fn webdav_get_once(base: &str, remote: &str) -> Option<Vec<u8>> {
    let url = format!("{}/{}", base.trim_end_matches('/'), remote);
    let user = env("SHARE_LIVE_USER").unwrap();
    let pass = env("SHARE_LIVE_PASS").unwrap();
    let auth = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, format!("{user}:{pass}"));
    let agent = ureq::Agent::new_with_defaults();
    let response = agent.get(&url).header("Authorization", &format!("Basic {auth}")).call().ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let mut body = Vec::new();
    use std::io::Read;
    response.into_body().as_reader().read_to_end(&mut body).ok()?;
    Some(body)
}

fn webdav_base() -> Option<String> {
    env("SHARE_LIVE_WEBDAV")
}

// ---------------- WebDAV against rclone ----------------

#[test]
#[ignore]
fn live_webdav_rclone() {
    setup();
    let name = "webdav-rclone";
    let Some(base) = env("SHARE_LIVE_WEBDAV") else {
        skip(name, "rclone webdav did not start");
        return;
    };
    let w = WebDav {
        url: format!("{base}/"),
        user: env("SHARE_LIVE_USER").unwrap(),
        sealed_password: seal_secret(&env("SHARE_LIVE_PASS").unwrap()).unwrap(),
        ..Default::default()
    };
    let t = target(Kind::WebDav(w), "https://files.example/{name}");
    let Kind::WebDav(w) = &t.kind else { unreachable!() };

    let report = webdav::check(&t, w).unwrap();
    assert!(report.steps.len() >= 3, "{:?}", report.steps);

    // small upload, read back over HTTP, sha256 equal
    let (_dir, file) = small_file();
    let shared = webdav::upload(&t, w, &file, "live-test/shot.png", 15, &mut noop()).unwrap();
    assert!(shared.url.contains("live-test/shot.png"));
    let back = webdav_get(&base, "live-test/shot.png").expect("the file reads back");
    assert_eq!(hex::encode(Sha256::digest(&back)), sha256_of(&file));

    // 20 MiB upload
    let (_d2, big) = big_file(20);
    let size = std::fs::metadata(&big).unwrap().len();
    webdav::upload(&t, w, &big, "live-test/clip.mp4", size, &mut noop()).unwrap();
    let back = webdav_get(&base, "live-test/clip.mp4").expect("the big file reads back");
    assert_eq!(hex::encode(Sha256::digest(&back)), sha256_of(&big));

    // nested folders
    webdav::upload(&t, w, &file, "live-test/deep/nested/dir/shot.png", 15, &mut noop()).unwrap();
    assert!(webdav_get(&base, "live-test/deep/nested/dir/shot.png").is_some());

    // cancel leaves no junk
    let err = webdav::upload(&t, w, &big, "live-test/cancelled.bin", size, &mut cancelling()).unwrap_err();
    assert!(matches!(err, ShareError::Cancelled), "{err}");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(webdav_absent(&base, "live-test/cancelled.bin"), "the partial must be gone");

    // delete
    let shared = webdav::upload(&t, w, &file, "live-test/gone.png", 15, &mut noop()).unwrap();
    let record = record_of(&shared, 15);
    webdav::delete(&t, w, &record).unwrap();
    assert!(webdav_absent(&base, "live-test/gone.png"));

    // wrong password
    let mut bad = w.clone();
    bad.sealed_password = seal_secret("wrong").unwrap();
    let err = webdav::upload(&t, &bad, &file, "live-test/nope.png", 15, &mut noop()).unwrap_err();
    assert!(matches!(err, ShareError::AuthFailed(_) | ShareError::Forbidden(_)), "{err}");
    pass(name);
}

// ---------------- FTP against rclone ----------------

fn ftp_target(addr: &str, tls: FtpTls) -> (Target, tempfile::TempDir) {
    let (host, port) = addr.split_once(':').unwrap();
    let f = Ftp {
        host: host.into(),
        port: port.parse().unwrap(),
        user: env("SHARE_LIVE_USER").unwrap(),
        sealed_password: seal_secret(&env("SHARE_LIVE_PASS").unwrap()).unwrap(),
        tls,
    };
    (target(Kind::Ftp(f), "https://files.example/{name}"), tempfile::tempdir().unwrap())
}

fn ftp_of(t: &Target) -> &Ftp {
    match &t.kind {
        Kind::Ftp(f) => f,
        _ => unreachable!(),
    }
}

#[test]
#[ignore]
fn live_ftp_rclone() {
    setup();
    let name = "ftp-rclone";
    let Some(addr) = env("SHARE_LIVE_FTP") else {
        skip(name, "rclone ftp did not start");
        return;
    };
    let base = webdav_base().expect("webdav readback needs the webdav server");
    let (t, _tmp) = ftp_target(&addr, FtpTls::None);
    let f = ftp_of(&t);

    let report = ftp::check(&t, f).unwrap();
    assert!(report.steps.len() >= 3, "{:?}", report.steps);

    let (_dir, file) = small_file();
    let shared = ftp::upload(&t, f, &file, "live-test/shot.png", 15, &mut noop()).unwrap();
    assert_eq!(shared.delete.as_deref(), Some("live-test/shot.png"));
    let back = webdav_get(&base, "live-test/shot.png").expect("reads back");
    assert_eq!(hex::encode(Sha256::digest(&back)), sha256_of(&file));

    let (_d2, big) = big_file(20);
    let size = std::fs::metadata(&big).unwrap().len();
    ftp::upload(&t, f, &big, "live-test/clip.mp4", size, &mut noop()).unwrap();
    let back = webdav_get(&base, "live-test/clip.mp4").expect("big reads back");
    assert_eq!(hex::encode(Sha256::digest(&back)), sha256_of(&big));

    // nested folders
    ftp::upload(&t, f, &file, "live-test/deep/nested/shot.png", 15, &mut noop()).unwrap();
    assert!(webdav_get(&base, "live-test/deep/nested/shot.png").is_some());

    // cancel: neither the part nor the final name stays
    let err = ftp::upload(&t, f, &big, "live-test/cancelled.bin", size, &mut cancelling()).unwrap_err();
    assert!(matches!(err, ShareError::Cancelled), "{err}");
    assert!(webdav_absent(&base, "live-test/cancelled.bin"));
    assert!(webdav_absent(&base, "live-test/.cancelled.bin.part"));

    // delete
    let shared = ftp::upload(&t, f, &file, "live-test/gone.png", 15, &mut noop()).unwrap();
    let record = record_of(&shared, 15);
    ftp::delete(&t, f, &record).unwrap();
    assert!(webdav_absent(&base, "live-test/gone.png"));

    // wrong password
    let mut bad = f.clone();
    bad.sealed_password = seal_secret("wrong").unwrap();
    let err = ftp::upload(&t, &bad, &file, "live-test/nope.png", 15, &mut noop()).unwrap_err();
    assert!(matches!(err, ShareError::AuthFailed(_)), "{err}");
    pass(name);
}

#[test]
#[ignore]
fn live_ftps_rclone() {
    setup();
    // rclone serve ftp --cert speaks implicit FTPS, not explicit AUTH TLS:
    // the banner only exists inside the TLS session. Explicit FTPS is covered
    // by the in-process libunftp tests instead.
    let name = "ftps-rclone";
    let Some(addr) = env("SHARE_LIVE_FTPS") else {
        skip(
            name,
            "rclone serve ftp TLS is implicit and leaves the data channel plaintext (goftp); FTPS runs against libunftp in-process instead",
        );
        return;
    };
    let (host, port) = addr.split_once(':').unwrap();
    let make = || Ftp {
        host: host.into(),
        port: port.parse().unwrap(),
        user: env("SHARE_LIVE_USER").unwrap(),
        sealed_password: seal_secret(&env("SHARE_LIVE_PASS").unwrap()).unwrap(),
        tls: FtpTls::Implicit,
    };

    // untrusted by default
    let (t, _tmp) = {
        let f = make();
        (target(Kind::Ftp(f), "https://files.example/{name}"), tempfile::tempdir().unwrap())
    };
    let (_dir, file) = small_file();
    let err = ftp::upload(&t, ftp_of(&t), &file, "live-test/x.png", 15, &mut noop()).unwrap_err();
    assert!(matches!(err, ShareError::Tls(_)), "{err}");

    // with the CA file
    let ca = env("SHARE_LIVE_FTPS_CA").unwrap();
    let mut t = t.clone();
    t.tls.ca_file = Some(ca);
    let shared = ftp::upload(&t, ftp_of(&t), &file, "live-test/secure.png", 15, &mut noop()).unwrap();
    assert!(shared.url.contains("secure.png"));

    // insecure
    let mut t = t.clone();
    t.tls.ca_file = None;
    t.tls.insecure = true;
    ftp::upload(&t, ftp_of(&t), &file, "live-test/secure2.png", 15, &mut noop()).unwrap();
    pass(name);
}

// ---------------- SFTP against rclone and real OpenSSH ----------------

#[test]
#[ignore]
fn live_sftp_rclone() {
    setup();
    let name = "sftp-rclone";
    let Some(addr) = env("SHARE_LIVE_SFTP_RCLONE") else {
        skip(name, "rclone sftp did not start");
        return;
    };
    let base = webdav_base().expect("webdav readback needs the webdav server");
    let (host, port) = addr.split_once(':').unwrap();
    let port: u16 = port.parse().unwrap();

    let key = sftp::fetch_host_key(host, port).unwrap();
    assert!(key.fingerprint.starts_with("SHA256:"), "{}", key.fingerprint);

    let s = Sftp {
        host: host.into(),
        port,
        user: env("SHARE_LIVE_USER").unwrap(),
        auth: SftpAuth::Password,
        sealed_password: seal_secret(&env("SHARE_LIVE_PASS").unwrap()).unwrap(),
        host_key: key.fingerprint.clone(),
        ..Default::default()
    };
    let t = target(Kind::Sftp(s), "https://files.example/{name}");
    let Kind::Sftp(s) = &t.kind else { unreachable!() };

    let report = sftp::check(&t, s).unwrap();
    assert!(report.steps.len() >= 3, "{:?}", report.steps);

    let (_dir, file) = small_file();
    let shared = sftp::upload(&t, s, &file, "live-test/shot.png", 15, &mut noop()).unwrap();
    assert_eq!(shared.delete.as_deref(), Some("live-test/shot.png"));
    let back = webdav_get(&base, "live-test/shot.png").expect("reads back");
    assert_eq!(hex::encode(Sha256::digest(&back)), sha256_of(&file));

    let (_d2, big) = big_file(20);
    let size = std::fs::metadata(&big).unwrap().len();
    sftp::upload(&t, s, &big, "live-test/clip.mp4", size, &mut noop()).unwrap();
    let back = webdav_get(&base, "live-test/clip.mp4").expect("big reads back");
    assert_eq!(hex::encode(Sha256::digest(&back)), sha256_of(&big));

    // nested folders
    sftp::upload(&t, s, &file, "live-test/deep/nested/shot.png", 15, &mut noop()).unwrap();
    assert!(webdav_get(&base, "live-test/deep/nested/shot.png").is_some());

    // cancel cleanup: the .part must go away
    let err = sftp::upload(&t, s, &big, "live-test/cancelled.bin", size, &mut cancelling()).unwrap_err();
    assert!(matches!(err, ShareError::Cancelled), "{err}");
    assert!(webdav_absent(&base, "live-test/cancelled.bin"));
    assert!(webdav_absent(&base, "live-test/.cancelled.bin.part"));

    // delete
    let shared = sftp::upload(&t, s, &file, "live-test/gone.png", 15, &mut noop()).unwrap();
    let record = record_of(&shared, 15);
    sftp::delete(&t, s, &record).unwrap();
    assert!(webdav_absent(&base, "live-test/gone.png"));

    // wrong pin is HostKeyChanged
    let mut wrong_pin = s.clone();
    wrong_pin.host_key = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into();
    let err = sftp::upload(&t, &wrong_pin, &file, "live-test/nope.png", 15, &mut noop()).unwrap_err();
    assert!(matches!(err, ShareError::HostKeyChanged { .. }), "{err}");

    // wrong password is AuthFailed
    let mut bad = s.clone();
    bad.sealed_password = seal_secret("wrong").unwrap();
    let err = sftp::upload(&t, &bad, &file, "live-test/nope.png", 15, &mut noop()).unwrap_err();
    assert!(matches!(err, ShareError::AuthFailed(_)), "{err}");
    pass(name);
}

#[test]
#[ignore]
fn live_sftp_openssh_key() {
    setup();
    let name = "sftp-openssh";
    let Some(addr) = env("SHARE_LIVE_SSHD") else {
        skip(name, "the OpenSSH sshd did not start");
        return;
    };
    let (host, port) = addr.split_once(':').unwrap();
    let port: u16 = port.parse().unwrap();
    let key = sftp::fetch_host_key(host, port).unwrap();
    let root = env("SHARE_LIVE_SSH_ROOT").unwrap();
    let user = std::env::var("USER").unwrap_or_else(|_| "dev".into());
    let s = Sftp {
        host: host.into(),
        port,
        user,
        auth: SftpAuth::KeyFile,
        key_file: env("SHARE_LIVE_SSH_KEY").unwrap(),
        host_key: key.fingerprint.clone(),
        ..Default::default()
    };
    let t = target(Kind::Sftp(s), "https://files.example/{name}");
    let Kind::Sftp(s) = &t.kind else { unreachable!() };

    // absolute remote paths: the sshd user writes straight into the live root
    let remote = format!("{root}/shot.png");
    let (_dir, file) = small_file();
    sftp::upload(&t, s, &file, &remote, 15, &mut noop()).unwrap();
    assert_eq!(sha256_of(std::path::Path::new(&remote)), sha256_of(&file));
    std::fs::remove_file(&remote).unwrap();

    // nested absolute folders
    let nested = format!("{root}/deep/nested/shot.png");
    sftp::upload(&t, s, &file, &nested, 15, &mut noop()).unwrap();
    assert_eq!(sha256_of(std::path::Path::new(&nested)), sha256_of(&file));

    // cancel cleanup
    let (_d2, big) = big_file(20);
    let size = std::fs::metadata(&big).unwrap().len();
    let cancel = format!("{root}/cancelled.bin");
    let err = sftp::upload(&t, s, &big, &cancel, size, &mut cancelling()).unwrap_err();
    assert!(matches!(err, ShareError::Cancelled), "{err}");
    assert!(!std::path::Path::new(&cancel).exists());
    assert!(!std::path::Path::new(&format!("{root}/.cancelled.bin.part")).exists());

    // wrong key: the server must reject before any upload happens
    let dir = tempfile::tempdir().unwrap();
    let wrong_key = dir.path().join("wrong");
    std::process::Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-f"]).arg(&wrong_key).output().unwrap();
    let mut bad = s.clone();
    bad.key_file = wrong_key.to_string_lossy().into();
    let err = sftp::upload(&t, &bad, &file, &remote, 15, &mut noop()).unwrap_err();
    assert!(matches!(err, ShareError::AuthFailed(_)), "{err}");
    pass(name);
}

// ---------------- S3 against rclone and MinIO ----------------

fn s3_target(endpoint: &str, access: &str, secret: &str, bucket: &str) -> Target {
    target(
        Kind::S3(S3 {
            endpoint: endpoint.into(),
            region: "us-east-1".into(),
            bucket: bucket.into(),
            access_key: access.into(),
            sealed_secret_key: seal_secret(secret).unwrap(),
            ..Default::default()
        }),
        "",
    )
}

fn s3_of(t: &Target) -> &S3 {
    match &t.kind {
        Kind::S3(s) => s,
        _ => unreachable!(),
    }
}

fn make_bucket(base: &str, s: &S3) {
    use rusty_s3::actions::S3Action;
    let endpoint = url::Url::parse(base).unwrap();
    let bucket = rusty_s3::Bucket::new(endpoint, rusty_s3::UrlStyle::Path, s.bucket.trim().to_string(), "us-east-1".to_string()).unwrap();
    let secret = heronite_share::config::unseal(&s.sealed_secret_key).unwrap();
    let credentials = rusty_s3::Credentials::new(s.access_key.trim(), secret);
    let action = rusty_s3::actions::CreateBucket::new(&bucket, &credentials);
    let signed = action.sign(std::time::Duration::from_secs(60));
    let _ = ureq::Agent::new_with_defaults().run(ureq::http::Request::builder().method("PUT").uri(signed.as_str()).body(()).unwrap());
}

#[test]
#[ignore]
fn live_s3_rclone() {
    setup();
    let name = "s3-rclone";
    let Some(base) = env("SHARE_LIVE_S3_RCLONE") else {
        skip(name, "rclone s3 did not start");
        return;
    };
    let access = env("SHARE_LIVE_S3_KEY").unwrap();
    let secret = env("SHARE_LIVE_S3_SECRET").unwrap();
    let t = s3_target(&base, &access, &secret, "bucket");
    let s = s3_of(&t);

    let report = heronite_share::s3::check(&t, s).unwrap();
    assert!(report.steps.len() >= 3, "{:?}", report.steps);

    let (_dir, file) = small_file();
    let shared = heronite_share::s3::upload(&t, s, &file, "live-test/shot.png", 15, &mut noop()).unwrap();
    // public_url is empty: a presigned link that must fetch the same bytes
    assert!(shared.url.contains("X-Amz-Signature"), "{}", shared.url);
    let fetched = ureq::Agent::new_with_defaults().get(&shared.url).call().unwrap();
    let mut body = Vec::new();
    use std::io::Read;
    let mut fetched = fetched;
    fetched.body_mut().as_reader().read_to_end(&mut body).unwrap();
    assert_eq!(hex::encode(Sha256::digest(&body)), sha256_of(&file));

    // multipart through the lowered threshold
    heronite_share::s3::MULTIPART_THRESHOLD.store(1024 * 1024, std::sync::atomic::Ordering::Relaxed);
    heronite_share::s3::PART_SIZE.store(5 * 1024 * 1024, std::sync::atomic::Ordering::Relaxed);
    let (_d2, big) = big_file(20);
    let size = std::fs::metadata(&big).unwrap().len();
    let shared = heronite_share::s3::upload(&t, s, &big, "live-test/clip.mp4", size, &mut noop()).unwrap();
    let fetched = ureq::Agent::new_with_defaults().get(&shared.url).call().unwrap();
    let mut body = Vec::new();
    let mut fetched = fetched;
    fetched.body_mut().as_reader().read_to_end(&mut body).unwrap();
    assert_eq!(hex::encode(Sha256::digest(&body)), sha256_of(&big));

    // cancel mid-multipart: the object must not complete
    let err = heronite_share::s3::upload(&t, s, &big, "live-test/cancelled.bin", size, &mut cancelling()).unwrap_err();
    assert!(matches!(err, ShareError::Cancelled), "{err}");

    // wrong secret
    let wrong = s3_target(&base, &access, "wrongsecret", "root");
    let err = heronite_share::s3::upload(&wrong, s3_of(&wrong), &file, "live-test/nope.png", 15, &mut noop()).unwrap_err();
    assert!(matches!(err, ShareError::AuthFailed(_) | ShareError::Forbidden(_)), "{err}");

    // delete
    let shared = heronite_share::s3::upload(&t, s, &file, "live-test/gone.png", 15, &mut noop()).unwrap();
    let record = record_of(&shared, 15);
    heronite_share::s3::delete(&t, s, &record).unwrap();
    pass(name);
}

#[test]
#[ignore]
fn live_s3_minio_multipart() {
    setup();
    let name = "s3-minio";
    let Some(base) = env("SHARE_LIVE_MINIO") else {
        skip(name, "MinIO did not run (no podman image)");
        return;
    };
    let t = s3_target(&base, "liveaccess", "livepass123", "live");
    let Kind::S3(s) = &t.kind else { unreachable!() };
    make_bucket(&base, s);

    // lowered threshold multipart, 18 MiB
    heronite_share::s3::MULTIPART_THRESHOLD.store(1024 * 1024, std::sync::atomic::Ordering::Relaxed);
    heronite_share::s3::PART_SIZE.store(5 * 1024 * 1024, std::sync::atomic::Ordering::Relaxed);
    let (_d2, big) = big_file(18);
    let size = std::fs::metadata(&big).unwrap().len();
    let shared = heronite_share::s3::upload(&t, s, &big, "live-test/clip.mp4", size, &mut noop()).unwrap();
    assert!(shared.url.contains("X-Amz-Signature"), "{}", shared.url);
    let fetched = ureq::Agent::new_with_defaults().get(&shared.url).call().unwrap();
    let mut body = Vec::new();
    use std::io::Read;
    let mut fetched = fetched;
    fetched.body_mut().as_reader().read_to_end(&mut body).unwrap();
    assert_eq!(hex::encode(Sha256::digest(&body)), sha256_of(&big));

    // one file above the real 64 MiB threshold with the default sizes
    heronite_share::s3::MULTIPART_THRESHOLD.store(64 * 1024 * 1024, std::sync::atomic::Ordering::Relaxed);
    heronite_share::s3::PART_SIZE.store(16 * 1024 * 1024, std::sync::atomic::Ordering::Relaxed);
    let (_d3, huge) = big_file(68);
    let size = std::fs::metadata(&huge).unwrap().len();
    let shared = heronite_share::s3::upload(&t, s, &huge, "live-test/huge.mp4", size, &mut noop()).unwrap();
    let fetched = ureq::Agent::new_with_defaults().get(&shared.url).call().unwrap();
    let mut body = Vec::new();
    let mut fetched = fetched;
    fetched.body_mut().as_reader().read_to_end(&mut body).unwrap();
    assert_eq!(hex::encode(Sha256::digest(&body)), sha256_of(&huge));
    pass(name);
}
