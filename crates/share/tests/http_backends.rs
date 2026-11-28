//! In-process protocol tests for the HTTP-ish backends: real tiny_http
//! servers on 127.0.0.1 that check what the crate sends.

use std::io::Read;
use std::sync::{Arc, Mutex};

use heronite_share::config::{seal_secret, Extract, Http, HttpBody, HttpHeader, Kind, Target, WebDav};
use heronite_share::progress::{Continue, Progress};
use heronite_share::seal::{use_seal, PlainSeal};
use heronite_share::{http, webdav, ShareContext, TlsOptions};

fn setup() {
    use_seal(Arc::new(PlainSeal));
}

fn target(kind: Kind, public_url: &str) -> Target {
    Target {
        id: "t".into(),
        name: "test".into(),
        enabled: true,
        kind,
        public_url: public_url.into(),
        folder: String::new(),
        tls: TlsOptions::default(),
    }
}

fn http_target(url: &str, extract: Extract) -> Target {
    target(
        Kind::Http(Http {
            url: url.into(),
            method: "POST".into(),
            headers: vec![],
            body: HttpBody::Multipart { field: "file".into(), fields: vec![("game".into(), "{game}".into())] },
            extract,
            link_template: String::new(),
            delete_url: String::new(),
            delete_method: "DELETE".into(),
        }),
        "",
    )
}

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<(String, String, Vec<(String, String)>, Vec<u8>)>>>,
}

fn serve(responder: impl Fn(&str, &str, &[(String, String)], &[u8]) -> (u16, String) + Send + Sync + 'static) -> Server {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    let requests: Arc<Mutex<Vec<(String, String, Vec<(String, String)>, Vec<u8>)>>> = Arc::new(Mutex::new(Vec::new()));
    let reqs = requests.clone();
    let resp = Arc::new(responder);
    std::thread::spawn(move || loop {
        let mut request = match server.recv() {
            Ok(r) => r,
            Err(_) => break,
        };
        let mut body = Vec::new();
        let _ = request.as_reader().take(64 * 1024 * 1024).read_to_end(&mut body);
        let headers: Vec<(String, String)> =
            request.headers().iter().map(|h| (h.field.as_str().to_string(), h.value.as_str().to_string())).collect();
        let method = request.method().to_string();
        let url = request.url().to_string();
        reqs.lock().unwrap().push((method.clone(), url.clone(), headers.clone(), body.clone()));
        let (status, answer) = resp(&method, &url, &headers, &body);
        let response = tiny_http::Response::from_string(answer).with_status_code(status);
        let _ = request.respond(response);
    });
    Server { url: format!("http://127.0.0.1:{port}"), requests }
}

fn probe_file() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("shot.png");
    std::fs::write(&file, b"fake-png-bytes-for-the-test").unwrap();
    (dir, file)
}

fn noop() -> impl FnMut(Progress) -> Continue {
    |_| Continue::Go
}

#[test]
fn http_multipart_upload_and_extraction() {
    setup();
    let server = serve(|_, _, _, _| (200, r#"{"data":{"url":"https://img.example/abc.png"}}"#.into()));
    let mut t = http_target(&format!("{}/up", server.url), Extract::JsonPointer { pointer: "/data/url".into() });
    if let Kind::Http(h) = &mut t.kind {
        h.headers.push(HttpHeader {
            name: "X-Token".into(),
            value: String::new(),
            secret: true,
            sealed_value: seal_secret("s3cr3t").unwrap(),
        });
    }
    let (_dir, file) = probe_file();
    let mut progress = noop();
    let ctx = ShareContext { game: Some("My Game".into()) };
    let shared = http::upload(
        &t,
        match &t.kind {
            Kind::Http(h) => h,
            _ => unreachable!(),
        },
        &file,
        "folder/shot.png",
        27,
        &ctx,
        &mut progress,
    )
    .unwrap();
    assert_eq!(shared.url, "https://img.example/abc.png");
    let reqs = server.requests.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    let (method, _, headers, body) = &reqs[0];
    assert_eq!(method, "POST");
    // The secret header arrived, multipart shape is right.
    assert!(headers.iter().any(|(n, v)| n.eq_ignore_ascii_case("X-Token") && v == "s3cr3t"), "{headers:?}");
    let text = String::from_utf8_lossy(body);
    assert!(text.contains("name=\"game\"\r\n\r\nMy Game\r\n"), "{text}");
    assert!(text.contains("filename=\"shot.png\""), "{text}");
    assert!(text.contains("fake-png-bytes-for-the-test"), "{text}");
    assert!(text.contains("Content-Type: image/png"), "{text}");
    // Exact content length.
    let len: usize = headers.iter().find(|(n, _)| n.eq_ignore_ascii_case("Content-Length")).map(|(_, v)| v.parse().unwrap()).unwrap();
    assert_eq!(len, body.len());
}

#[test]
fn http_raw_put_and_body_link() {
    setup();
    let server = serve(|_, _, _, _| (200, "  https://cdn.example/x.png \n".into()));
    let mut t = http_target(&format!("{}/up", server.url), Extract::Body);
    if let Kind::Http(h) = &mut t.kind {
        h.method = "PUT".into();
        h.body = HttpBody::Raw;
    }
    let (_dir, file) = probe_file();
    let mut progress = noop();
    let shared = http::upload(
        &t,
        match &t.kind {
            Kind::Http(h) => h,
            _ => unreachable!(),
        },
        &file,
        "shot.png",
        27,
        &ShareContext::default(),
        &mut progress,
    )
    .unwrap();
    assert_eq!(shared.url, "https://cdn.example/x.png");
    let reqs = server.requests.lock().unwrap();
    assert_eq!(reqs[0].3, b"fake-png-bytes-for-the-test");
}

#[test]
fn http_delete_uses_the_extracted_value() {
    setup();
    let server = serve(|m, _, _, _| if m == "DELETE" { (200, String::new()) } else { (200, r#"{"id":"xyz"}"#.into()) });
    let mut t = http_target(&format!("{}/up", server.url), Extract::JsonPointer { pointer: "/id".into() });
    if let Kind::Http(h) = &mut t.kind {
        h.delete_url = format!("{}/del/{{value}}", server.url);
    }
    let (_dir, file) = probe_file();
    let mut progress = noop();
    let shared = http::upload(
        &t,
        match &t.kind {
            Kind::Http(h) => h,
            _ => unreachable!(),
        },
        &file,
        "shot.png",
        27,
        &ShareContext::default(),
        &mut progress,
    )
    .unwrap();
    assert_eq!(shared.delete.as_deref(), Some("xyz"));
    let record = heronite_share::Record {
        id: "r".into(),
        target_id: "t".into(),
        target_name: "t".into(),
        file_name: "shot.png".into(),
        size: 27,
        url: shared.url.clone(),
        remote: shared.remote.clone(),
        delete: shared.delete.clone(),
        at: String::new(),
        game: None,
        kind: heronite_share::FileKind::Screenshot,
    };
    http::delete(
        &t,
        match &t.kind {
            Kind::Http(h) => h,
            _ => unreachable!(),
        },
        &record,
    )
    .unwrap();
    let reqs = server.requests.lock().unwrap();
    assert!(reqs.iter().any(|(m, u, _, _)| m == "DELETE" && u == "/del/xyz"), "{reqs:?}");
}

#[test]
fn http_errors_map_by_status() {
    setup();
    for (status, expect) in [(401u16, "login"), (403, "allow"), (413, "fit"), (500, "answered")] {
        let server = serve(move |_, _, _, _| (status, "no".into()));
        let t = http_target(&format!("{}/up", server.url), Extract::Body);
        let (_dir, file) = probe_file();
        let mut progress = noop();
        let err = http::upload(
            &t,
            match &t.kind {
                Kind::Http(h) => h,
                _ => unreachable!(),
            },
            &file,
            "shot.png",
            27,
            &ShareContext::default(),
            &mut progress,
        )
        .unwrap_err();
        assert!(err.to_string().contains(expect), "{status}: {err}");
    }
}

#[test]
fn http_retries_a_500_then_succeeds() {
    setup();
    let count = Arc::new(Mutex::new(0));
    let c = count.clone();
    let server = serve(move |_, _, _, _| {
        let mut n = c.lock().unwrap();
        *n += 1;
        if *n == 1 {
            (500, "boom".into())
        } else {
            (200, "https://ok".into())
        }
    });
    let t = http_target(&format!("{}/up", server.url), Extract::Body);
    let (_dir, file) = probe_file();
    let mut progress = noop();
    let shared = http::upload(
        &t,
        match &t.kind {
            Kind::Http(h) => h,
            _ => unreachable!(),
        },
        &file,
        "shot.png",
        27,
        &ShareContext::default(),
        &mut progress,
    )
    .unwrap();
    assert_eq!(shared.url, "https://ok");
    assert_eq!(*count.lock().unwrap(), 2);
}

#[test]
fn webdav_put_mkcol_get_delete() {
    setup();
    let server = serve(|m, u, _, _| match (m, u) {
        ("MKCOL", _) => (201, String::new()),
        ("PUT", _) => (201, String::new()),
        ("GET", _) => (200, "fake-png-bytes-for-the-test".into()),
        ("DELETE", _) => (204, String::new()),
        ("PROPFIND", _) => (207, String::new()),
        _ => (405, String::new()),
    });
    let w = WebDav {
        url: format!("{}/dav/", server.url),
        user: "me".into(),
        sealed_password: seal_secret("pw").unwrap(),
        ..Default::default()
    };
    let mut t = target(Kind::WebDav(w), "https://files.example/{name}");
    t.folder = "shots/deep".into();
    let Kind::WebDav(w) = &t.kind else { unreachable!() };
    let (_dir, file) = probe_file();
    let mut progress = noop();
    // remote names arrive with the folder prefix already joined, as share() does
    let shared = webdav::upload(&t, w, &file, "shots/deep/a b/shot.png", 27, &mut progress).unwrap();
    assert_eq!(shared.url, "https://files.example/shots/deep/a b/shot.png");
    let reqs = server.requests.lock().unwrap();
    // Collections created level by level, file PUT with encoded space.
    assert!(reqs.iter().any(|(m, u, _, _)| m == "MKCOL" && u.contains("shots")));
    let put = reqs.iter().find(|(m, _, _, _)| m == "PUT").unwrap();
    assert!(put.1.contains("a%20b/shot.png"), "{}", put.1);
    assert_eq!(put.3, b"fake-png-bytes-for-the-test");
    assert!(put.2.iter().any(|(n, v)| n.eq_ignore_ascii_case("Authorization") && v.starts_with("Basic ")));
    assert!(put.2.iter().any(|(n, v)| n.eq_ignore_ascii_case("If-None-Match") && v == "*"));
    // check() passes against the same server.
    drop(reqs);
    let report = webdav::check(&t, w).unwrap();
    assert!(!report.steps.is_empty());
}

#[test]
fn webdav_auth_failure_is_typed() {
    setup();
    let server = serve(|m, _, _, _| if m == "MKCOL" { (201, String::new()) } else { (401, "no".into()) });
    let w = WebDav {
        url: format!("{}/dav/", server.url),
        user: "me".into(),
        sealed_password: seal_secret("pw").unwrap(),
        ..Default::default()
    };
    let t = target(Kind::WebDav(w), "https://files.example/{name}");
    let Kind::WebDav(w) = &t.kind else { unreachable!() };
    let (_dir, file) = probe_file();
    let mut progress = noop();
    let err = webdav::upload(&t, w, &file, "shot.png", 27, &mut progress).unwrap_err();
    assert!(matches!(err, heronite_share::ShareError::AuthFailed(_)), "{err}");
}

#[test]
fn nextcloud_share_link_from_ocs() {
    setup();
    let server = serve(|m, u, headers, body| {
        if u.contains("/ocs/v2.php/apps/files_sharing/api/v1/shares") {
            assert!(m == "POST");
            assert!(headers.iter().any(|(n, v)| n.eq_ignore_ascii_case("OCS-APIRequest") && v == "true"));
            let text = String::from_utf8_lossy(body);
            assert!(text.contains("shareType=3"), "{text}");
            assert!(text.contains("path="), "{text}");
            return (200, r#"{"ocs":{"data":{"url":"https://cloud.example/s/abc123"}}}"#.into());
        }
        match m {
            "PUT" | "MKCOL" => (201, String::new()),
            _ => (405, String::new()),
        }
    });
    let w = WebDav {
        url: format!("{}/remote.php/dav/files/me/shots/", server.url),
        user: "me".into(),
        sealed_password: seal_secret("pw").unwrap(),
        nextcloud_share: true,
        ..Default::default()
    };
    let t = target(Kind::WebDav(w), "");
    let Kind::WebDav(w) = &t.kind else { unreachable!() };
    let (_dir, file) = probe_file();
    let mut progress = noop();
    let shared = webdav::upload(&t, w, &file, "shot.png", 27, &mut progress).unwrap();
    assert_eq!(shared.url, "https://cloud.example/s/abc123");
}

fn save_targets(paths: &heronite_data::DataPaths, t: Target) {
    let mut targets = heronite_share::Targets::default();
    targets.upsert(t);
    targets.save(paths).unwrap();
}

#[test]
fn webdav_end_to_end_share_delete_history() {
    setup();
    let server = serve(|m, _, _, _| match m {
        "MKCOL" | "PUT" => (201, String::new()),
        "DELETE" => (204, String::new()),
        "PROPFIND" => (207, String::new()),
        _ => (405, String::new()),
    });
    let mut t = target(
        Kind::WebDav(WebDav {
            url: format!("{}/dav/", server.url),
            user: "me".into(),
            sealed_password: seal_secret("pw").unwrap(),
            ..Default::default()
        }),
        "https://files.example/{name}",
    );
    t.folder = "shots".into();
    let dir = tempfile::tempdir().unwrap();
    let paths = heronite_data::DataPaths::at(dir.path());
    save_targets(&paths, t);
    let (_fdir, file) = probe_file();
    let record = heronite_share::share(&paths, "t", &file, &ShareContext::default(), &mut noop()).unwrap();
    assert!(record.url.starts_with("https://files.example/shots/"));
    let history = heronite_share::history(&paths);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].target_id, "t");
    assert_eq!(history[0].remote, record.remote);
    assert_eq!(history[0].size, 27);
    assert!(matches!(history[0].kind, heronite_share::FileKind::Screenshot));
    heronite_share::delete(&paths, &record.id).unwrap();
    assert!(heronite_share::history(&paths).is_empty());
    let reqs = server.requests.lock().unwrap();
    assert!(reqs.iter().any(|(m, u, _, _)| m == "DELETE" && u.contains(&record.remote)), "{reqs:?}");
}

#[test]
fn failing_remote_delete_keeps_the_record() {
    setup();
    let server = serve(|m, _, _, _| match m {
        "PUT" | "MKCOL" => (201, String::new()),
        "DELETE" => (500, "boom".into()),
        _ => (405, String::new()),
    });
    let t = target(
        Kind::WebDav(WebDav {
            url: format!("{}/dav/", server.url),
            user: "me".into(),
            sealed_password: seal_secret("pw").unwrap(),
            ..Default::default()
        }),
        "https://files.example/{name}",
    );
    let dir = tempfile::tempdir().unwrap();
    let paths = heronite_data::DataPaths::at(dir.path());
    save_targets(&paths, t);
    let (_fdir, file) = probe_file();
    let record = heronite_share::share(&paths, "t", &file, &ShareContext::default(), &mut noop()).unwrap();
    let err = heronite_share::delete(&paths, &record.id).unwrap_err();
    assert!(!format!("{err}").is_empty());
    assert_eq!(heronite_share::history(&paths).len(), 1, "the record stays when the remote delete fails");
}

#[test]
fn nextcloud_end_to_end_uses_the_ocs_link() {
    setup();
    let server = serve(|m, u, _, _| {
        if u.contains("/ocs/v2.php/") {
            return (200, r#"{"ocs":{"data":{"url":"https://cloud.example/s/xyzzy"}}}"#.into());
        }
        match m {
            "PUT" | "MKCOL" => (201, String::new()),
            "DELETE" => (204, String::new()),
            _ => (405, String::new()),
        }
    });
    let t = target(
        Kind::WebDav(WebDav {
            url: format!("{}/remote.php/dav/files/me/", server.url),
            user: "me".into(),
            sealed_password: seal_secret("pw").unwrap(),
            nextcloud_share: true,
            ..Default::default()
        }),
        "",
    );
    let dir = tempfile::tempdir().unwrap();
    let paths = heronite_data::DataPaths::at(dir.path());
    save_targets(&paths, t);
    let (_fdir, file) = probe_file();
    let record = heronite_share::share(&paths, "t", &file, &ShareContext::default(), &mut noop()).unwrap();
    assert_eq!(record.url, "https://cloud.example/s/xyzzy");
    heronite_share::delete(&paths, &record.id).unwrap();
    assert!(heronite_share::history(&paths).is_empty());
}

#[test]
fn http_end_to_end_with_and_without_delete_url() {
    setup();
    let server = serve(|m, u, _, _| {
        if m == "DELETE" {
            return (200, "ok".into());
        }
        if u.contains("/delete") {
            return (200, "ok".into());
        }
        (200, r#"{"id":"img42"}"#.into())
    });
    let mut t = http_target(&format!("{}/up", server.url), Extract::JsonPointer { pointer: "/id".into() });
    if let Kind::Http(h) = &mut t.kind {
        h.link_template = "https://img.example/{value}.png".into();
        h.delete_url = format!("{}/delete/{{value}}", server.url);
    }
    let dir = tempfile::tempdir().unwrap();
    let paths = heronite_data::DataPaths::at(dir.path());
    save_targets(&paths, t);
    let (_fdir, file) = probe_file();
    let record = heronite_share::share(&paths, "t", &file, &ShareContext::default(), &mut noop()).unwrap();
    assert_eq!(record.url, "https://img.example/img42.png");
    heronite_share::delete(&paths, &record.id).unwrap();
    assert!(heronite_share::history(&paths).is_empty());
    let reqs = server.requests.lock().unwrap();
    assert!(reqs.iter().any(|(m, u, _, _)| m == "DELETE" && u.contains("img42")), "{reqs:?}");
    drop(reqs);

    // without a delete url the record alone goes away
    let t2 = http_target(&format!("{}/up", server.url), Extract::JsonPointer { pointer: "/id".into() });
    let dir2 = tempfile::tempdir().unwrap();
    let paths2 = heronite_data::DataPaths::at(dir2.path());
    save_targets(&paths2, t2);
    let record2 = heronite_share::share(&paths2, "t", &file, &ShareContext::default(), &mut noop()).unwrap();
    heronite_share::delete(&paths2, &record2.id).unwrap();
    assert!(heronite_share::history(&paths2).is_empty());
}

#[test]
fn history_caps_at_200_newest_first() {
    setup();
    let dir = tempfile::tempdir().unwrap();
    let paths = heronite_data::DataPaths::at(dir.path());
    let mut history = heronite_share::history::History::load(&paths);
    for i in 0..230 {
        history.push(heronite_share::Record {
            id: format!("r{i}"),
            target_id: "t".into(),
            target_name: "t".into(),
            file_name: "f.png".into(),
            size: 1,
            url: String::new(),
            remote: String::new(),
            delete: None,
            at: String::new(),
            game: None,
            kind: heronite_share::FileKind::Other,
        });
    }
    history.save(&paths).unwrap();
    let loaded = heronite_share::history(&paths);
    assert_eq!(loaded.len(), 200);
    assert_eq!(loaded[0].id, "r229", "newest first");
    assert_eq!(loaded[199].id, "r30");
}
