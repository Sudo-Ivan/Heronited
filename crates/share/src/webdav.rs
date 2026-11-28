//! WebDAV (incl. Nextcloud/ownCloud): PUT streaming, MKCOL for missing
//! collections, PROPFIND/DELETE, and OCS public share links.

use std::io::Read;
use std::path::Path;

use heronite_i18n::tr;

use crate::config::{unseal, Target, WebDav};
use crate::error::{status_error, Result, ShareError};
use crate::history::Record;
use crate::http_util::{agent, expect_ok, retried, send_error, snippet};
use crate::progress::{Continue, CountingReader, Progress};
use crate::Shared;

/// MKCOL, PROPFIND and friends have no ureq shortcut: build the request.
fn custom(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
    auth: &[(String, String)],
    extra: &[(String, String)],
) -> Result<ureq::http::Response<ureq::Body>> {
    let mut builder = ureq::http::Request::builder().method(method).uri(url);
    for (n, v) in auth.iter().chain(extra.iter()) {
        builder = builder.header(n, v);
    }
    let request = builder.body(()).map_err(|e| ShareError::network(e.to_string()))?;
    agent.run(request).map_err(send_error)
}

fn auth_headers(w: &WebDav) -> Result<Vec<(String, String)>> {
    if !w.sealed_token.is_empty() {
        return Ok(vec![("Authorization".into(), format!("Bearer {}", unseal(&w.sealed_token)?))]);
    }
    use base64::Engine;
    let password = unseal(&w.sealed_password)?;
    let userpass = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", w.user, password));
    Ok(vec![("Authorization".into(), format!("Basic {userpass}"))])
}

/// The collection URL `remote` lives at, joined with single slashes, and
/// every segment percent-encoded (slashes of `remote` kept as structure).
pub(crate) fn file_url(base: &str, remote: &str) -> String {
    let base = base.trim_end_matches('/');
    let mut out = String::with_capacity(base.len() + remote.len() + 2);
    out.push_str(base);
    for segment in remote.split('/') {
        if segment.is_empty() {
            continue;
        }
        out.push('/');
        // Form encoding uses '+' for spaces; in paths it must be %20. A
        // literal plus is %2B already, so the swap is safe.
        out.push_str(&url::form_urlencoded::byte_serialize(segment.as_bytes()).collect::<String>().replace('+', "%20"));
    }
    out
}

/// The server root and the user's files root for the OCS API, derived from a
/// `/remote.php/dav/files/<user>/` or `/remote.php/webdav/` base URL.
pub fn nextcloud_root(w: &WebDav) -> Result<(url::Url, String)> {
    let parsed = url::Url::parse(&w.url).map_err(|_| ShareError::config(tr!("The WebDAV URL is not valid")))?;
    let path = parsed.path();
    let files = format!("/remote.php/dav/files/{}/", w.user);
    if let Some(at) = path.find(&files) {
        let root = format!(
            "{}://{}{}",
            parsed.scheme(),
            parsed.host_str().unwrap_or_default(),
            parsed.port().map(|p| format!(":{p}")).unwrap_or_default()
        );
        let prefix = &path[..at];
        let root = format!("{root}{prefix}");
        let root = url::Url::parse(&root).map_err(|_| ShareError::config("bad root"))?;
        return Ok((root, files.to_string()));
    }
    if path.contains("/remote.php/webdav") {
        return Err(ShareError::config(tr!("Nextcloud sharing needs the user's files URL: {scheme}://host/remote.php/dav/files/{user}/ (the webdav endpoint does not say which user)", scheme = parsed.scheme(), user = w.user)));
    }
    Err(ShareError::config(tr!(
        "Nextcloud sharing needs a URL like {scheme}://host/remote.php/dav/files/<user>/",
        scheme = parsed.scheme()
    )))
}

/// Creates the missing collections of `remote`'s parent path level by level.
fn ensure_collections(agent: &ureq::Agent, w: &WebDav, remote: &str, auth: &[(String, String)]) -> Result<()> {
    let mut path = String::new();
    let parts: Vec<&str> = remote.split('/').collect();
    for segment in &parts[..parts.len().saturating_sub(1)] {
        if segment.is_empty() {
            continue;
        }
        if !path.is_empty() {
            path.push('/');
        }
        path.push_str(segment);
        let url = file_url(&w.url, &path);
        let mut response = custom(agent, "MKCOL", &url, auth, &[])?;
        // 201 created, 405 already there, 301/302 not followed.
        let status = response.status().as_u16();
        if ![201, 200, 204, 405].contains(&status) && !(300..400).contains(&status) {
            let body = snippet(&mut response);
            return Err(status_error(status, &body));
        }
    }
    Ok(())
}

fn put_file(
    agent: &ureq::Agent,
    w: &WebDav,
    target: &Target,
    file: &Path,
    remote: &str,
    size: u64,
    progress: &mut dyn FnMut(Progress) -> Continue,
) -> Result<u16> {
    let auth = auth_headers(w)?;
    let url = file_url(&w.url, remote);
    let f = std::fs::File::open(file).map_err(ShareError::io)?;
    let mut reader = CountingReader::new(f, size, &mut *progress);
    let mut builder = ureq::http::Request::builder().method("PUT").uri(&url);
    builder = builder
        .header("Content-Type", crate::mime::mime_of(remote))
        .header("Content-Length", size.to_string())
        .header("If-None-Match", "*");
    for (n, v) in &auth {
        builder = builder.header(n, v);
    }
    let request = builder.body(ureq::SendBody::from_reader(&mut reader)).map_err(|e| ShareError::network(e.to_string()))?;
    let mut response = agent.run(request).map_err(|e| {
        if reader.failed {
            return ShareError::Cancelled;
        }
        let err = send_error(e);
        if matches!(&err, ShareError::Network(t) if t.contains("cancelled")) {
            ShareError::Cancelled
        } else {
            err
        }
    })?;
    expect_ok_status(&mut response, target)
}

fn expect_ok_status(response: &mut ureq::http::Response<ureq::Body>, _target: &Target) -> Result<u16> {
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(status);
    }
    let body = snippet(response);
    Err(status_error(status, &body))
}

pub fn upload(
    target: &Target,
    w: &WebDav,
    file: &Path,
    remote: &str,
    size: u64,
    progress: &mut dyn FnMut(Progress) -> Continue,
) -> Result<Shared> {
    let ag = agent(&target.tls)?;
    let auth = auth_headers(w)?;
    ensure_collections(&ag, w, remote, &auth)?;
    // PUT with one retry after MKCOL if the server answers 409/404.
    let mut attempt_error: Option<ShareError> = None;
    for attempt in 0..3 {
        match put_file(&ag, w, target, file, remote, size, progress) {
            Ok(_) => {
                attempt_error = None;
                break;
            }
            Err(ShareError::Cancelled) => {
                cleanup_delete(&ag, w, remote, &auth);
                return Err(ShareError::Cancelled);
            }
            Err(e @ (ShareError::NotFound(_) | ShareError::Server { .. }))
                if attempt == 0 && matches!(&e, ShareError::NotFound(_) | ShareError::Server { status: 409, .. }) =>
            {
                ensure_collections(&ag, w, remote, &auth)?;
                attempt_error = Some(e);
            }
            Err(e) if e.retryable() && attempt < 2 => {
                attempt_error = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            Err(e) => {
                if matches!(e, ShareError::Network(_)) {
                    cleanup_delete(&ag, w, remote, &auth);
                }
                return Err(e);
            }
        }
    }
    if let Some(e) = attempt_error {
        if matches!(e, ShareError::Network(_) | ShareError::Server { status: 500..=599, .. }) {
            return Err(e);
        }
    }

    let url = if w.nextcloud_share { ocs_share(&ag, w, remote, &auth)? } else { target.link_of(remote) };
    Ok(Shared { url, remote: remote.to_string(), delete: Some(remote.to_string()) })
}

/// POST the OCS share API and read `ocs.data.url` from the JSON answer.
fn ocs_share(agent: &ureq::Agent, w: &WebDav, remote: &str, auth: &[(String, String)]) -> Result<String> {
    let (root, files_prefix) = nextcloud_root(w)?;
    // The path relative to the user's files, with the target folder included.
    let base_path = url::Url::parse(&w.url).map_err(|_| ShareError::config("bad URL"))?.path().to_string();
    let base_rel = base_path.trim_start_matches(&files_prefix).trim_matches('/');
    let full = if base_rel.is_empty() { remote.to_string() } else { format!("{base_rel}/{remote}") };
    let ocs_url = format!("{}/ocs/v2.php/apps/files_sharing/api/v1/shares", root.as_str().trim_end_matches('/'));
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("path", &format!("/{full}"))
        .append_pair("shareType", "3")
        .append_pair("permissions", "1")
        .finish();
    let mut response = retried(|| {
        let mut req = agent
            .post(&ocs_url)
            .header("OCS-APIRequest", "true")
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded");
        for (n, v) in auth {
            req = req.header(n, v);
        }
        let mut r = req.send(form.as_bytes()).map_err(send_error)?;
        if (200..300).contains(&r.status().as_u16()) {
            Ok(r)
        } else {
            Err(status_error(r.status().as_u16(), &snippet(&mut r)))
        }
    })?;
    let mut body = Vec::new();
    let _ = response.body_mut().as_reader().take(64 * 1024).read_to_end(&mut body);
    let json: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| ShareError::Server { status: 0, body: tr!("the share answer is not JSON").into() })?;
    json.pointer("/ocs/data/url")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| ShareError::Server { status: 0, body: tr!("the share answer has no link").into() })
}

/// Cleanup after a cancelled or broken upload. The server often finishes
/// noticing the broken pipe a moment after the client has moved on, so the
/// partial object can appear *after* a first delete: retry a missing file a
/// few times before giving up. Best effort, errors are swallowed by callers.
fn cleanup_delete(agent: &ureq::Agent, w: &WebDav, remote: &str, auth: &[(String, String)]) {
    // A cancelled upload can still be flushing server-side: the DELETE lands before
    // the partial file even exists, so verify absence and delete again if it reappears.
    for _ in 0..12 {
        let _ = delete_remote(agent, w, remote, auth);
        std::thread::sleep(std::time::Duration::from_millis(200));
        if !remote_exists(agent, w, remote, auth) {
            std::thread::sleep(std::time::Duration::from_millis(250));
            if !remote_exists(agent, w, remote, auth) {
                return;
            }
        }
    }
}

fn remote_exists(agent: &ureq::Agent, w: &WebDav, remote: &str, auth: &[(String, String)]) -> bool {
    let url = file_url(&w.url, remote);
    let mut req = agent.head(&url);
    for (n, v) in auth {
        req = req.header(n, v);
    }
    match req.call() {
        Ok(response) => response.status().is_success(),
        Err(ureq::Error::StatusCode(404)) => false,
        Err(_) => true,
    }
}

fn delete_remote(agent: &ureq::Agent, w: &WebDav, remote: &str, auth: &[(String, String)]) -> Result<()> {
    let url = file_url(&w.url, remote);
    let mut req = agent.delete(&url);
    for (n, v) in auth {
        req = req.header(n, v);
    }
    let mut response = req.call().map_err(send_error)?;
    expect_ok(&mut response, |s| (200..300).contains(&s) || s == 404)
}

pub fn delete(target: &Target, w: &WebDav, record: &Record) -> Result<()> {
    let ag = agent(&target.tls)?;
    let auth = auth_headers(w)?;
    let remote = record.delete.clone().unwrap_or_else(|| record.remote.clone());
    retried(|| delete_remote(&ag, w, &remote, &auth))
}

pub fn check(target: &Target, w: &WebDav) -> Result<crate::CheckReport> {
    let ag = agent(&target.tls)?;
    let auth = auth_headers(w)?;
    let mut steps = Vec::new();
    // PROPFIND depth 0 on the base collection.
    let mut response = custom(&ag, "PROPFIND", w.url.trim_end_matches('/'), &auth, &[("Depth".into(), "0".into())])?;
    expect_ok(&mut response, |s| s == 207 || (200..300).contains(&s))?;
    steps.push(tr!("Login and collection exist").into());
    // Probe PUT, GET, DELETE in the target folder.
    let remote = crate::join_remote(&target.folder, "heronite-probe.txt");
    ensure_collections(&ag, w, &remote, &auth)?;
    let probe = std::env::temp_dir().join(format!("heronite-wd-probe-{}", std::process::id()));
    std::fs::write(&probe, b"probe").map_err(ShareError::io)?;
    let mut noop = |_| Continue::Go;
    put_file(&ag, w, target, &probe, &remote, 5, &mut noop)?;
    let _ = std::fs::remove_file(&probe);
    steps.push(tr!("Upload works").into());
    let url = file_url(&w.url, &remote);
    let mut req = ag.get(&url);
    for (n, v) in &auth {
        req = req.header(n, v);
    }
    let mut response = req.call().map_err(send_error)?;
    expect_ok(&mut response, |s| (200..300).contains(&s))?;
    steps.push(tr!("Reading back works").into());
    delete_remote(&ag, w, &remote, &auth)?;
    steps.push(tr!("Delete works").into());
    Ok(crate::CheckReport { steps })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_join_and_encode() {
        assert_eq!(file_url("https://h/dav/files/me/", "a/b c.png"), "https://h/dav/files/me/a/b%20c.png");
        assert_eq!(file_url("https://h/dav/files/me", "x.png"), "https://h/dav/files/me/x.png");
        assert_eq!(file_url("https://h/dav/files/me/", "/x.png"), "https://h/dav/files/me/x.png");
        assert_eq!(file_url("https://h/dav/files/me/", "a#b?.png"), "https://h/dav/files/me/a%23b%3F.png");
    }

    #[test]
    fn nextcloud_roots_derive() {
        crate::seal::use_seal(std::sync::Arc::new(crate::seal::PlainSeal));
        let w = WebDav { url: "https://cloud.example/remote.php/dav/files/ivan/shots/".into(), user: "ivan".into(), ..Default::default() };
        let (root, prefix) = nextcloud_root(&w).unwrap();
        assert_eq!(root.as_str(), "https://cloud.example/");
        assert_eq!(prefix, "/remote.php/dav/files/ivan/");
        // A Nextcloud in a subdirectory keeps its prefix.
        let w = WebDav { url: "https://example.com/nc/remote.php/dav/files/ivan/".into(), user: "ivan".into(), ..Default::default() };
        let (root, _) = nextcloud_root(&w).unwrap();
        assert_eq!(root.as_str(), "https://example.com/nc");
        // Plain webdav endpoint: cannot know the user.
        let w = WebDav { url: "https://h/remote.php/webdav/".into(), user: "ivan".into(), ..Default::default() };
        assert!(nextcloud_root(&w).is_err());
        // Not a Nextcloud URL at all.
        let w = WebDav { url: "https://h/dav/".into(), user: "ivan".into(), ..Default::default() };
        assert!(nextcloud_root(&w).is_err());
    }
}
