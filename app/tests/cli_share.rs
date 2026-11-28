//! The `--share` command line path against a real local HTTP server. The
//! child process gets its own data folder and never opens the running
//! instance's socket.

use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<(String, String, Vec<u8>)>>>,
}

fn serve() -> Server {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    let requests: Arc<Mutex<Vec<(String, String, Vec<u8>)>>> = Arc::new(Mutex::new(Vec::new()));
    let reqs = requests.clone();
    std::thread::spawn(move || loop {
        let mut request = match server.recv() {
            Ok(r) => r,
            Err(_) => break,
        };
        let mut body = Vec::new();
        let _ = request.as_reader().take(1024 * 1024).read_to_end(&mut body);
        reqs.lock().unwrap().push((request.method().to_string(), request.url().to_string(), body));
        let _ = request.respond(tiny_http::Response::from_string("http://test.local/shared.png").with_status_code(200));
    });
    Server { url: format!("http://127.0.0.1:{port}"), requests }
}

fn write_fixture(data: &tempfile::TempDir, upload_url: &str) {
    let local = data.path().join("local_library");
    std::fs::create_dir_all(&local).unwrap();
    let fixture = serde_json::json!({
        "targets": [{
            "id": "test",
            "name": "Test server",
            "enabled": true,
            "type": "http",
            "url": upload_url,
            "method": "POST",
            "body": { "kind": "multipart", "field": "file", "fields": [] },
            "extract": { "kind": "body" }
        }],
        "default": "test",
        "naming": { "mode": { "kind": "original" } }
    });
    std::fs::write(local.join("share.json"), serde_json::to_vec_pretty(&fixture).unwrap()).unwrap();
}

fn run_share(data: &tempfile::TempDir, file: &std::path::Path) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_heronite-rs"))
        .arg("--share")
        .arg(file)
        .env("HERONITE_DATA_DIR", data.path())
        .env("HERONITE_NO_SINGLE_INSTANCE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "--share timed out\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn png_file(dir: &tempfile::TempDir) -> std::path::PathBuf {
    let file = dir.path().join("shot.png");
    std::fs::write(&file, b"fake-png-bytes-for-the-cli-test").unwrap();
    file
}

#[test]
fn share_uploads_to_the_http_target_and_writes_history() {
    let server = serve();
    let data = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    write_fixture(&data, &format!("{}/upload", server.url));
    let file = png_file(&files);

    let output = run_share(&data, &file);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines, ["http://test.local/shared.png"]);

    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "POST");
    assert_eq!(requests[0].1, "/upload");
    assert!(
        requests[0].2.windows(b"fake-png-bytes-for-the-cli-test".len()).any(|w| w == b"fake-png-bytes-for-the-cli-test"),
        "multipart body did not contain the file bytes"
    );
    drop(requests);

    let history: serde_json::Value =
        serde_json::from_slice(&std::fs::read(data.path().join("local_library/share_history.json")).unwrap()).unwrap();
    let records = history["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["url"], "http://test.local/shared.png");
    assert_eq!(records[0]["fileName"], "shot.png");
}

#[test]
fn share_reports_an_unreachable_target_and_fails() {
    let data = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    write_fixture(&data, "http://127.0.0.1:9/upload");
    let file = png_file(&files);

    let output = run_share(&data, &file);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("heronite-rs:"), "stderr had no error text: {stderr}");
    assert!(stderr.contains("shot.png"), "stderr did not name the file: {stderr}");
}
