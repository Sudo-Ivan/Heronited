//! Fault injection: unreachable hosts, stalls, connections dying mid-body,
//! and the junk cleanup every backend owes after them.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use heronite_share::config::{seal_secret, Extract, Http, Kind, Target, WebDav};
use heronite_share::http_util::set_timeouts;
use heronite_share::progress::{Continue, Progress};
use heronite_share::seal::{use_seal, PlainSeal};
use heronite_share::{http, webdav, ShareContext, ShareError, TlsOptions};

fn setup() {
    use_seal(Arc::new(PlainSeal));
}

fn noop() -> impl FnMut(Progress) -> Continue {
    |_| Continue::Go
}

static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn guard() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
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

fn http_target(url: &str) -> Target {
    base_target(Kind::Http(Http { url: format!("{url}/up"), method: "POST".into(), extract: Extract::Body, ..Default::default() }))
}

fn webdav_target(url: &str) -> Target {
    base_target(Kind::WebDav(WebDav {
        url: format!("{url}/dav/"),
        user: "ivan".into(),
        sealed_password: seal_secret("hunter2").unwrap(),
        ..Default::default()
    }))
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// What a connection does.
pub enum Act {
    /// Read a bit, then drop the connection without any response.
    CloseEarly,
    /// Read the whole request and answer with this status and body.
    Answer(u16, &'static str),
}

pub struct RawServer {
    pub port: u16,
    pub requests: Arc<Mutex<Vec<(String, String)>>>,
}

/// Answers connection number i with acts[i] (the last one repeating).
fn raw_server(acts: Vec<Act>) -> RawServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let reqs = requests.clone();
    let count = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let i = count.fetch_add(1, Ordering::SeqCst);
            let act = if i < acts.len() { i } else { acts.len() - 1 };
            let close = matches!(acts[act], Act::CloseEarly);
            let (status, body) = match &acts[act] {
                Act::Answer(s, b) => (*s, *b),
                _ => (200, ""),
            };
            let reqs = reqs.clone();
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                let mut length = 0usize;
                // headers first
                loop {
                    match stream.read(&mut byte) {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {
                            head.push(byte[0]);
                            if head.ends_with(b"\r\n\r\n") || head.len() > 64 * 1024 {
                                break;
                            }
                        }
                    }
                }
                let head_text = String::from_utf8_lossy(&head).to_string();
                let mut lines = head_text.split("\r\n");
                let request_line = lines.next().unwrap_or("").to_string();
                for line in lines {
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut parts = request_line.split(' ');
                let method = parts.next().unwrap_or("").to_string();
                let url = parts.next().unwrap_or("").to_string();
                if close {
                    // a sip of the body, then die mid-transfer
                    let mut buf = [0u8; 128];
                    let _ = stream.read(&mut buf);
                    return;
                }
                let mut remaining = length;
                let mut buf = [0u8; 8192];
                while remaining > 0 {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => remaining = remaining.saturating_sub(n),
                    }
                }
                reqs.lock().unwrap().push((method, url));
                let response = format!("HTTP/1.1 {status} x\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            });
        }
    });
    RawServer { port, requests }
}

fn url_of(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

#[test]
fn unreachable_port_fails_fast() {
    setup();
    let _guard = guard();
    let port = free_port(); // nothing listens here
    let t = http_target(&url_of(port));
    let (_d, file) = file_of(b"x");
    let start = Instant::now();
    let h = match &t.kind {
        Kind::Http(h) => h,
        _ => unreachable!(),
    };
    let err = http::upload(&t, h, &file, "shot.png", 1, &ShareContext::default(), &mut noop()).unwrap_err();
    assert!(matches!(err, ShareError::Network(_)), "{err}");
    assert!(start.elapsed() < Duration::from_secs(3), "took {:?}", start.elapsed());
}

#[test]
fn stalling_server_trips_the_timeout() {
    setup();
    let _guard = guard();
    set_timeouts(Duration::from_secs(1), Duration::from_secs(1));
    // accepts and never says a word
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            std::mem::forget(stream); // hold it open forever
        }
    });
    let t = http_target(&url_of(port));
    let (_d, file) = file_of(b"x");
    let start = Instant::now();
    let h = match &t.kind {
        Kind::Http(h) => h,
        _ => unreachable!(),
    };
    let err = http::upload(&t, h, &file, "shot.png", 1, &ShareContext::default(), &mut noop()).unwrap_err();
    set_timeouts(Duration::from_secs(15), Duration::from_secs(60));
    assert!(matches!(err, ShareError::Network(_)), "{err}");
    assert!(start.elapsed() < Duration::from_secs(20), "took {:?}", start.elapsed());
}

#[test]
fn mid_body_close_retries_then_succeeds() {
    setup();
    let _guard = guard();
    let server = raw_server(vec![Act::CloseEarly, Act::Answer(200, "https://cdn.example/back.png")]);
    let t = http_target(&url_of(server.port));
    let bytes = vec![5u8; 600 * 1024];
    let (_d, file) = file_of(&bytes);
    let h = match &t.kind {
        Kind::Http(h) => h,
        _ => unreachable!(),
    };
    let shared = http::upload(&t, h, &file, "shot.png", bytes.len() as u64, &ShareContext::default(), &mut noop()).unwrap();
    assert_eq!(shared.url, "https://cdn.example/back.png");
    assert_eq!(server.requests.lock().unwrap().len(), 1, "only the retry is a full request");
}

#[test]
fn webdav_cancel_cleans_the_partial() {
    setup();
    let _guard = guard();
    let server = raw_server(vec![Act::Answer(201, ""), Act::Answer(204, "")]);
    let t = webdav_target(&url_of(server.port));
    let bytes = vec![7u8; 2 * 1024 * 1024];
    let (_d, file) = file_of(&bytes);
    let w = match &t.kind {
        Kind::WebDav(w) => w,
        _ => unreachable!(),
    };
    let mut calls = 0;
    let mut progress = move |_: Progress| {
        calls += 1;
        if calls > 1 {
            Continue::Cancel
        } else {
            Continue::Go
        }
    };
    let err = webdav::upload(&t, w, &file, "shot.png", bytes.len() as u64, &mut progress).unwrap_err();
    assert!(matches!(err, ShareError::Cancelled), "{err}");
    // the cleanup DELETE went out for the partial upload
    std::thread::sleep(Duration::from_millis(200));
    let reqs = server.requests.lock().unwrap();
    assert!(reqs.iter().any(|(m, u)| m == "DELETE" && u.contains("shot.png")), "{reqs:?}");
}

#[test]
fn webdav_network_failure_cleans_the_partial() {
    setup();
    let _guard = guard();
    // every PUT connection dies mid-body; the DELETE is answered
    let server = raw_server(vec![Act::CloseEarly, Act::CloseEarly, Act::CloseEarly, Act::Answer(204, "")]);
    let t = webdav_target(&url_of(server.port));
    let bytes = vec![7u8; 600 * 1024];
    let (_d, file) = file_of(&bytes);
    let w = match &t.kind {
        Kind::WebDav(w) => w,
        _ => unreachable!(),
    };
    let err = webdav::upload(&t, w, &file, "shot.png", bytes.len() as u64, &mut noop()).unwrap_err();
    assert!(matches!(err, ShareError::Network(_)), "{err}");
    std::thread::sleep(Duration::from_millis(200));
    let reqs = server.requests.lock().unwrap();
    assert!(reqs.iter().any(|(m, u)| m == "DELETE" && u.contains("shot.png")), "{reqs:?}");
}
