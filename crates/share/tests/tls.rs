//! Self-signed TLS for the HTTP-ish backends: refused by default, accepted
//! with the CA file or the insecure toggle.

use std::path::PathBuf;
use std::sync::Arc;

use heronite_share::config::{seal_secret, Extract, Http, Kind, Target, WebDav, S3};
use heronite_share::progress::{Continue, Progress};
use heronite_share::seal::{use_seal, PlainSeal};
use heronite_share::{http, s3, webdav, ShareContext, ShareError, Shared, TlsOptions};

mod common;

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

fn base_target(kind: Kind) -> Target {
    Target {
        id: "t".into(),
        name: "t".into(),
        enabled: true,
        kind,
        public_url: "https://files.example/{name}".into(),
        folder: String::new(),
        tls: TlsOptions::default(),
    }
}

fn responder(_m: &str, _u: &str, _h: &[(String, String)], _b: &[u8]) -> (u16, Vec<(String, String)>, Vec<u8>) {
    (200, vec![("ETag".into(), "\"abc\"".into())], b"https://cdn.example/link.png".to_vec())
}

fn server() -> (common::TlsServer, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = common::write_cert(dir.path());
    (common::serve_tls(&cert, &key, responder), dir)
}

/// One backend, three answers: Tls error plain, then success with the CA
/// file and with the insecure toggle.
fn check_backend(make: impl Fn(&str) -> Target, upload: impl Fn(&Target, &std::path::Path, u64) -> heronite_share::error::Result<Shared>) {
    let (server, certs) = server();
    let (_d, file) = file_of(b"tls probe");

    let plain = make(&server.url);
    let err = upload(&plain, &file, 9).unwrap_err();
    assert!(matches!(err, ShareError::Tls(_)), "default should be Tls, got {err}");

    let mut ca = make(&server.url);
    ca.tls.ca_file = Some(certs.path().join("cert.pem").to_string_lossy().into());
    upload(&ca, &file, 9).unwrap();

    let mut insecure = make(&server.url);
    insecure.tls.insecure = true;
    upload(&insecure, &file, 9).unwrap();
}

#[test]
fn s3_tls_trust() {
    setup();
    check_backend(
        |url| {
            base_target(Kind::S3(S3 {
                endpoint: url.into(),
                region: "us-east-1".into(),
                bucket: "shots".into(),
                access_key: "AKID".into(),
                sealed_secret_key: seal_secret("secret").unwrap(),
                ..Default::default()
            }))
        },
        |t, file, size| {
            let s3c = match &t.kind {
                Kind::S3(s) => s,
                _ => unreachable!(),
            };
            s3::upload(t, s3c, file, "shot.png", size, &mut noop())
        },
    );
}

#[test]
fn webdav_tls_trust() {
    setup();
    check_backend(
        |url| {
            base_target(Kind::WebDav(WebDav {
                url: format!("{url}/dav/"),
                user: "ivan".into(),
                sealed_password: seal_secret("hunter2").unwrap(),
                ..Default::default()
            }))
        },
        |t, file, size| {
            let w = match &t.kind {
                Kind::WebDav(w) => w,
                _ => unreachable!(),
            };
            webdav::upload(t, w, file, "shot.png", size, &mut noop())
        },
    );
}

#[test]
fn http_tls_trust() {
    setup();
    check_backend(
        |url| {
            base_target(Kind::Http(Http { url: format!("{url}/up"), method: "POST".into(), extract: Extract::Body, ..Default::default() }))
        },
        |t, file, size| {
            let h = match &t.kind {
                Kind::Http(h) => h,
                _ => unreachable!(),
            };
            http::upload(t, h, file, "shot.png", size, &ShareContext::default(), &mut noop())
        },
    );
}
