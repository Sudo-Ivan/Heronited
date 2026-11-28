//! S3 against a server that recomputes SigV4 itself: a bug in how the crate
//! uses rusty-s3 shows up as SignatureDoesNotMatch.

mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use heronite_share::config::{seal_secret, Kind, Target, S3};
use heronite_share::progress::{Continue, Progress};
use heronite_share::seal::{use_seal, PlainSeal};
use heronite_share::{s3, ShareContext, TlsOptions};

use common::{s3_server, s3_server_strict};

/// The multipart thresholds are globals; every test here holds one lock so
/// they do not race each other.
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn setup() {
    use_seal(Arc::new(PlainSeal));
}

fn target_for(endpoint: &str, public_url: &str) -> Target {
    target_with(endpoint, public_url, "testsecret")
}

fn target_with(endpoint: &str, public_url: &str, secret: &str) -> Target {
    Target {
        id: "t".into(),
        name: "test".into(),
        enabled: true,
        kind: Kind::S3(S3 {
            endpoint: endpoint.into(),
            region: "us-east-1".into(),
            bucket: "shots".into(),
            access_key: "testkey".into(),
            sealed_secret_key: seal_secret(secret).unwrap(),
            ..Default::default()
        }),
        public_url: public_url.into(),
        folder: "games".into(),
        tls: TlsOptions::default(),
    }
}

fn file_of(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("shot.png");
    std::fs::write(&file, bytes).unwrap();
    (dir, file)
}

fn s3_of(t: &Target) -> &S3 {
    match &t.kind {
        Kind::S3(s) => s,
        _ => unreachable!(),
    }
}

fn noop() -> impl FnMut(Progress) -> Continue {
    |_| Continue::Go
}

#[test]
fn single_put_stores_the_exact_bytes() {
    setup();
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let server = s3_server(&["shots"]);
    let t = target_for(&server.url(), "https://cdn.example/{name}");
    let bytes: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let (_dir, file) = file_of(&bytes);
    let mut progress = noop();
    let shared = s3::upload(&t, s3_of(&t), &file, "games/a/shot.png", bytes.len() as u64, &mut progress).unwrap();
    assert_eq!(shared.url, "https://cdn.example/games/a/shot.png");
    let state = server.state.lock().unwrap();
    let stored = state.objects.get(&("shots".into(), "games/a/shot.png".into())).expect("object stored");
    assert_eq!(common::sha256_hex(stored), common::sha256_hex(&bytes));
}

#[test]
fn multipart_above_the_lowered_threshold() {
    setup();
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let server = s3_server(&["shots"]);
    s3::MULTIPART_THRESHOLD.store(4096, Ordering::Relaxed);
    s3::PART_SIZE.store(1024, Ordering::Relaxed);
    let t = target_for(&server.url(), "https://cdn.example/{name}");
    // 10240 bytes = exactly 10 parts of 1024
    let bytes: Vec<u8> = (0..10240u32).map(|i| (i % 253) as u8).collect();
    let (_dir, file) = file_of(&bytes);
    let mut progress = noop();
    s3::upload(&t, s3_of(&t), &file, "games/big.mp4", bytes.len() as u64, &mut progress).unwrap();
    let state = server.state.lock().unwrap();
    let stored = state.objects.get(&("shots".into(), "games/big.mp4".into())).expect("object stored");
    assert_eq!(common::sha256_hex(stored), common::sha256_hex(&bytes));
    // ten parts, etags chained in order
    let completed = state.completed.iter().find(|(k, _)| k == "games/big.mp4").expect("completed");
    assert_eq!(completed.1.len(), 10, "part count");
    assert!(state.aborted.is_empty());
    s3::MULTIPART_THRESHOLD.store(64 * 1024 * 1024, Ordering::Relaxed);
    s3::PART_SIZE.store(16 * 1024 * 1024, Ordering::Relaxed);
}

#[test]
fn cancel_mid_multipart_aborts() {
    setup();
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let server = s3_server(&["shots"]);
    s3::MULTIPART_THRESHOLD.store(4096, Ordering::Relaxed);
    s3::PART_SIZE.store(1024, Ordering::Relaxed);
    let t = target_for(&server.url(), "https://cdn.example/{name}");
    let bytes = vec![9u8; 10240];
    let (_dir, file) = file_of(&bytes);
    let mut calls = 0;
    let mut progress = move |_: Progress| {
        calls += 1;
        if calls > 2 {
            Continue::Cancel
        } else {
            Continue::Go
        }
    };
    let err = s3::upload(&t, s3_of(&t), &file, "games/big.mp4", bytes.len() as u64, &mut progress).unwrap_err();
    assert!(matches!(err, heronite_share::ShareError::Cancelled), "{err}");
    let state = server.state.lock().unwrap();
    assert!(!state.aborted.is_empty(), "abort was called");
    assert!(!state.objects.contains_key(&("shots".into(), "games/big.mp4".into())), "no final object");
    assert!(state.completed.is_empty(), "never completed");
    s3::MULTIPART_THRESHOLD.store(64 * 1024 * 1024, Ordering::Relaxed);
    s3::PART_SIZE.store(16 * 1024 * 1024, Ordering::Relaxed);
}

#[test]
fn wrong_secret_is_auth_failed() {
    setup();
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let server = s3_server(&["shots"]);
    let t = target_with(&server.url(), "https://cdn.example/{name}", "wrongsecret");
    let (_dir, file) = file_of(b"bytes");
    let mut progress = noop();
    let err = s3::upload(&t, s3_of(&t), &file, "games/shot.png", 5, &mut progress).unwrap_err();
    assert!(matches!(err, heronite_share::ShareError::Forbidden(_) | heronite_share::ShareError::AuthFailed(_)), "{err}");
}

#[test]
fn missing_bucket_is_not_found() {
    setup();
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let server = s3_server(&["other-bucket"]);
    let t = target_for(&server.url(), "https://cdn.example/{name}");
    let (_dir, file) = file_of(b"bytes");
    let mut progress = noop();
    let err = s3::upload(&t, s3_of(&t), &file, "games/shot.png", 5, &mut progress).unwrap_err();
    assert!(matches!(err, heronite_share::ShareError::NotFound(_)), "{err}");
}

#[test]
fn a_500_is_retried_then_succeeds() {
    setup();
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let server = s3_server(&["shots"]);
    server.state.lock().unwrap().faults.flaky.store(2, Ordering::Relaxed);
    let t = target_for(&server.url(), "https://cdn.example/{name}");
    let (_dir, file) = file_of(b"bytes");
    let mut progress = noop();
    s3::upload(&t, s3_of(&t), &file, "games/shot.png", 5, &mut progress).unwrap();
    assert!(server.state.lock().unwrap().objects.contains_key(&("shots".into(), "games/shot.png".into())));
}

#[test]
fn presigned_link_downloads_the_object() {
    setup();
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let server = s3_server(&["shots"]);
    let t = target_for(&server.url(), ""); // no public_url: presign
    let (_dir, file) = file_of(b"the bytes");
    let mut progress = noop();
    let shared = s3::upload(&t, s3_of(&t), &file, "games/shot.png", 9, &mut progress).unwrap();
    assert!(shared.url.contains("X-Amz-Signature="), "{}", shared.url);
    assert!(shared.url.contains("X-Amz-Expires=604800"), "default 7 days: {}", shared.url);
    // fetch the link back from the test server: the presigned GET verifies
    let body = ureq::get(&shared.url).call().unwrap().body_mut().read_to_vec().unwrap();
    assert_eq!(body, b"the bytes");
}

#[test]
fn public_url_template_link_mode() {
    setup();
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let server = s3_server(&["shots"]);
    let t = target_for(&server.url(), "https://cdn.example/prefix/{name}");
    let (_dir, file) = file_of(b"x");
    let mut progress = noop();
    let shared = s3::upload(&t, s3_of(&t), &file, "games/shot.png", 1, &mut progress).unwrap();
    assert_eq!(shared.url, "https://cdn.example/prefix/games/shot.png");
}

#[test]
fn acl_public_read_is_sent() {
    setup();
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let server = s3_server(&["shots"]);
    let mut t = target_for(&server.url(), "https://cdn.example/{name}");
    if let Kind::S3(s3) = &mut t.kind {
        s3.acl_public_read = true;
    }
    let (_dir, file) = file_of(b"x");
    let mut progress = noop();
    s3::upload(&t, s3_of(&t), &file, "games/shot.png", 1, &mut progress).unwrap();
    assert_eq!(server.state.lock().unwrap().acls.get(&("shots".into(), "games/shot.png".into())).map(|s| s.as_str()), Some("public-read"));
}

#[test]
fn check_probe_put_head_delete() {
    setup();
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let server = s3_server(&["shots"]);
    let t = target_for(&server.url(), "https://cdn.example/{name}");
    let report = s3::check(&t, s3_of(&t)).unwrap();
    assert!(report.steps.len() >= 3, "{:?}", report.steps);
    // probe cleaned up after itself
    assert!(!server.state.lock().unwrap().objects.contains_key(&("shots".into(), "games/heronite-probe.txt".into())));
}

#[test]
fn end_to_end_share_and_delete() {
    setup();
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let server = s3_server(&["shots"]);
    let t = target_for(&server.url(), "https://cdn.example/{name}");
    // local_library in a tempdir
    let dir = tempfile::tempdir().unwrap();
    let paths = heronite_data::DataPaths::at(dir.path());
    let mut targets = heronite_share::Targets::default();
    targets.upsert(t.clone());
    targets.save(&paths).unwrap();
    let (_fdir, file) = file_of(b"end to end");
    let mut progress = noop();
    let record = heronite_share::share(&paths, "t", &file, &ShareContext::default(), &mut progress).unwrap();
    assert!(record.url.contains("games/"));
    assert_eq!(heronite_share::history(&paths).len(), 1);
    assert!(server.state.lock().unwrap().objects.values().any(|b| b == b"end to end"));
    heronite_share::delete(&paths, &record.id).unwrap();
    assert!(heronite_share::history(&paths).is_empty());
    assert!(!server.state.lock().unwrap().objects.values().any(|b| b == b"end to end"));
}

#[test]
fn non_strict_server_still_serves() {
    // sanity of the harness itself: the signature check can be switched off
    setup();
    let server = s3_server_strict(&["shots"], false);
    let t = target_with(&server.url(), "https://cdn.example/{name}", "whatever");
    let (_dir, file) = file_of(b"x");
    let mut progress = noop();
    s3::upload(&t, s3_of(&t), &file, "games/shot.png", 1, &mut progress).unwrap();
}
