//! Live tests against real servers, driven by scripts/share-live.sh. Every
//! test is `#[ignore]`d; the script runs them with the endpoints in the env.

use std::path::PathBuf;
use std::sync::Arc;

use heronite_share::config::{seal_secret, Ftp, FtpTls, Kind, Sftp, SftpAuth, Target, WebDav, S3};
use heronite_share::progress::{Continue, Progress};
use heronite_share::seal::{use_seal, PlainSeal};
use heronite_share::{ftp, sftp, webdav, TlsOptions};

fn setup() {
    use_seal(Arc::new(PlainSeal));
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
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
    // Filled, not sparse: the server really receives every byte.
    let chunk = vec![7u8; 1024 * 1024];
    let mut f = std::fs::File::create(&file).unwrap();
    use std::io::Write;
    for _ in 0..mebibytes {
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

#[test]
#[ignore]
fn webdav_against_rclone() {
    setup();
    let Some(base) = env("SHARE_LIVE_WEBDAV") else { return };
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
    let (_dir, file) = small_file();
    let mut progress = noop();
    let shared = webdav::upload(&t, w, &file, "live-test/shot.png", 15, &mut progress).unwrap();
    assert!(shared.url.contains("live-test/shot.png"));
}

#[test]
#[ignore]
fn ftp_against_rclone() {
    setup();
    let Some(addr) = env("SHARE_LIVE_FTP") else { return };
    let (host, port) = addr.split_once(':').unwrap();
    let f = Ftp {
        host: host.into(),
        port: port.parse().unwrap(),
        user: env("SHARE_LIVE_USER").unwrap(),
        sealed_password: seal_secret(&env("SHARE_LIVE_PASS").unwrap()).unwrap(),
        tls: FtpTls::None,
    };
    let t = target(Kind::Ftp(f), "https://files.example/{name}");
    let Kind::Ftp(f) = &t.kind else { unreachable!() };
    let report = ftp::check(&t, f).unwrap();
    assert!(report.steps.len() >= 3, "{:?}", report.steps);
    let (_dir, file) = small_file();
    let mut progress = noop();
    let shared = ftp::upload(&t, f, &file, "shot.png", 15, &mut progress).unwrap();
    assert!(shared.url.contains("shot.png"));
    let record = heronite_share::Record {
        id: "r".into(),
        target_id: "t".into(),
        target_name: "t".into(),
        file_name: "shot.png".into(),
        size: 15,
        url: shared.url.clone(),
        remote: shared.remote.clone(),
        delete: shared.delete.clone(),
        at: String::new(),
        game: None,
        kind: heronite_share::FileKind::Screenshot,
    };
    ftp::delete(&t, f, &record).unwrap();
}

#[test]
#[ignore]
fn sftp_password_against_rclone() {
    setup();
    let Some(addr) = env("SHARE_LIVE_SFTP_RCLONE") else { return };
    let (host, port) = addr.split_once(':').unwrap();
    let port: u16 = port.parse().unwrap();
    // Trust the key first, then upload pinned.
    let key = sftp::fetch_host_key(host, port).unwrap();
    assert!(key.fingerprint.starts_with("SHA256:"));
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
    let (_dir, file) = small_file();
    let mut progress = noop();
    let shared = sftp::upload(&t, s, &file, "shot.png", 15, &mut progress).unwrap();
    assert!(shared.url.contains("shot.png"));
    // A wrong pin must fail with HostKeyChanged.
    let mut bad = s.clone();
    bad.host_key = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into();
    let err = sftp::upload(&t, &bad, &file, "shot.png", 15, &mut noop()).unwrap_err();
    assert!(matches!(err, heronite_share::ShareError::HostKeyChanged { .. } | heronite_share::ShareError::Network(_)), "{err}");
}

#[test]
#[ignore]
fn sftp_key_against_openssh() {
    setup();
    let Some(addr) = env("SHARE_LIVE_SSHD") else { return };
    let (host, port) = addr.split_once(':').unwrap();
    let port: u16 = port.parse().unwrap();
    let key = sftp::fetch_host_key(host, port).unwrap();
    let user = std::env::var("USER").unwrap_or_else(|_| "dev".into());
    let s = Sftp {
        host: host.into(),
        port,
        user,
        auth: SftpAuth::KeyFile,
        key_file: env("SHARE_LIVE_SSH_KEY").unwrap(),
        host_key: key.fingerprint,
        ..Default::default()
    };
    let t = target(Kind::Sftp(s), "https://files.example/{name}");
    let Kind::Sftp(s) = &t.kind else { unreachable!() };
    let (_dir, file) = small_file();
    let mut progress = noop();
    let report = sftp::check(&t, s).unwrap();
    assert!(report.steps.len() >= 3, "{:?}", report.steps);
    let shared = sftp::upload(&t, s, &file, "shot.png", 15, &mut progress).unwrap();
    assert!(shared.url.contains("shot.png"));
}

#[test]
#[ignore]
fn s3_multipart_against_minio() {
    setup();
    let Some(base) = env("SHARE_LIVE_MINIO") else { return };
    // The bucket is made through the same signed API the upload uses.
    let s = S3 {
        endpoint: base.clone(),
        region: "us-east-1".into(),
        bucket: "live".into(),
        access_key: "liveaccess".into(),
        sealed_secret_key: seal_secret("livepass123").unwrap(),
        ..Default::default()
    };
    // Lower the multipart threshold so a small file exercises it for real.
    heronite_share::s3::MULTIPART_THRESHOLD.store(1024, std::sync::atomic::Ordering::Relaxed);
    heronite_share::s3::PART_SIZE.store(1024, std::sync::atomic::Ordering::Relaxed);
    let t = target(Kind::S3(s), "");
    let Kind::S3(s) = &t.kind else { unreachable!() };
    // MinIO needs the bucket to exist; make it with a signed CreateBucket.
    make_bucket(&base, s);
    let (_dir, file) = big_file(18);
    let mut progress = noop();
    let size = std::fs::metadata(&file).unwrap().len();
    let shared = heronite_share::s3::upload(&t, s, &file, "clip.mp4", size, &mut progress).unwrap();
    assert!(shared.url.contains("clip.mp4"), "{}", shared.url);
    assert!(shared.url.contains("X-Amz-Signature"), "public_url empty means a presigned link: {}", shared.url);
}

fn make_bucket(base: &str, s: &S3) {
    use rusty_s3::actions::S3Action;
    let endpoint = url::Url::parse(base).unwrap();
    let bucket = rusty_s3::Bucket::new(endpoint, rusty_s3::UrlStyle::Path, "live".to_string(), "us-east-1".to_string()).unwrap();
    let secret = heronite_share::config::unseal(&s.sealed_secret_key).unwrap();
    let credentials = rusty_s3::Credentials::new(s.access_key.trim(), secret);
    let action = rusty_s3::actions::CreateBucket::new(&bucket, &credentials);
    let signed = action.sign(std::time::Duration::from_secs(60));
    let _ = ureq::Agent::new_with_defaults().run(ureq::http::Request::builder().method("PUT").uri(signed.as_str()).body(()).unwrap());
}
