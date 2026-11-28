//! Custom HTTP uploaders, ShareX style: POST or PUT, multipart or raw,
//! headers (some secret), the link extracted from the answer.

use std::io::Read;
use std::path::Path;

use heronite_i18n::tr;

use crate::config::{Extract, Http, HttpBody, Target};
use crate::error::{Result, ShareError};
use crate::history::Record;
use crate::http_util::{agent, expect_ok, retried, send_error};
use crate::progress::{ChainReader, Continue, CountingReader, Progress};
use crate::{ShareContext, Shared};

fn boundary_of(remote: &str) -> String {
    // Deterministic enough for tests, unique enough for servers.
    let mut h: u64 = 5381;
    for b in remote.bytes().chain(b"heronite-share".iter().copied()) {
        h = h.wrapping_mul(33).wrapping_add(b as u64);
    }
    format!("heronite{h:016x}")
}

fn fill(template: &str, file_name: &str, ctx: &ShareContext) -> String {
    template
        .replace("{name}", file_name)
        .replace("{game}", ctx.game.as_deref().unwrap_or(""))
        .replace("{ext}", crate::naming::extension_of(file_name).as_str())
}

/// The multipart body: prefix + file + suffix, exact total length.
fn multipart_parts(http: &Http, file_name: &str, ctx: &ShareContext, boundary: &str) -> (Vec<u8>, Vec<u8>) {
    let mime = crate::mime::mime_of(file_name);
    let mut prefix = Vec::new();
    let fields = match &http.body {
        HttpBody::Multipart { fields, .. } => fields.clone(),
        HttpBody::Raw => Vec::new(),
    };
    for (name, value) in &fields {
        prefix.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes());
        prefix.extend_from_slice(fill(value, file_name, ctx).as_bytes());
        prefix.extend_from_slice(b"\r\n");
    }
    let field = match &http.body {
        HttpBody::Multipart { field, .. } => field.clone(),
        HttpBody::Raw => "file".into(),
    };
    prefix.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{file_name}\"\r\nContent-Type: {mime}\r\n\r\n"
        )
        .as_bytes(),
    );
    let suffix = format!("\r\n--{boundary}--\r\n").into_bytes();
    (prefix, suffix)
}

pub fn extract_link(http: &Http, body: &[u8]) -> Result<String> {
    let text = String::from_utf8_lossy(body);
    let value = match &http.extract {
        Extract::Body => text.trim().to_string(),
        Extract::JsonPointer { pointer } => {
            let json: serde_json::Value =
                serde_json::from_slice(body).map_err(|_| ShareError::Server { status: 0, body: tr!("the answer is not JSON").into() })?;
            json.pointer(pointer)
                .and_then(|v| v.as_str())
                .ok_or_else(|| ShareError::Server { status: 0, body: tr!("the answer has no string at {pointer}", pointer = pointer) })?
                .to_string()
        }
        Extract::Regex { pattern } => {
            let re = regex::Regex::new(pattern)
                .map_err(|e| ShareError::config(tr!("The extraction regex does not compile: {error}", error = e.to_string())))?;
            re.captures(&text)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
                .ok_or_else(|| ShareError::Server { status: 0, body: tr!("the extraction regex matched nothing").into() })?
        }
    };
    if value.is_empty() {
        return Err(ShareError::Server { status: 0, body: tr!("the answer gave an empty link").into() });
    }
    if http.link_template.is_empty() {
        Ok(value)
    } else {
        Ok(http.link_template.replace("{value}", &value))
    }
}

fn headers_of(http: &Http) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for h in &http.headers {
        out.push((h.name.clone(), h.value()?));
    }
    Ok(out)
}

pub fn upload(
    target: &Target,
    http: &Http,
    file: &Path,
    remote: &str,
    size: u64,
    ctx: &ShareContext,
    progress: &mut dyn FnMut(Progress) -> Continue,
) -> Result<Shared> {
    let _ = target;
    let agent = agent(&target_tls(target))?;
    let headers = headers_of(http)?;
    let method = http.method.to_ascii_uppercase();
    let file_name = remote.rsplit('/').next().unwrap_or(remote);
    let mime = crate::mime::mime_of(file_name);
    let boundary = boundary_of(remote);
    let (content_type, total_len) = match &http.body {
        HttpBody::Multipart { .. } => {
            let (prefix, suffix) = multipart_parts(http, file_name, ctx, &boundary);
            (format!("multipart/form-data; boundary={boundary}"), size + prefix.len() as u64 + suffix.len() as u64)
        }
        HttpBody::Raw => (mime.to_string(), size),
    };
    let mut last_err = None;
    let mut body_bytes: Vec<u8> = Vec::new();
    for attempt in 0..3 {
        let f = std::fs::File::open(file).map_err(ShareError::io)?;
        let chain = match &http.body {
            HttpBody::Multipart { .. } => {
                let (prefix, suffix) = multipart_parts(http, file_name, ctx, &boundary);
                ChainReader::new(prefix, f, suffix)
            }
            HttpBody::Raw => ChainReader::new(Vec::new(), f, Vec::new()),
        };
        let mut reader = CountingReader::new(chain, total_len, &mut *progress);
        let mut builder = ureq::http::Request::builder().method(method.as_str()).uri(&http.url);
        for (name, value) in &headers {
            builder = builder.header(name, value);
        }
        builder = builder.header("Content-Type", &content_type).header("Content-Length", total_len.to_string());
        let request = builder.body(ureq::SendBody::from_reader(&mut reader)).map_err(|e| ShareError::network(e.to_string()))?;
        match agent.run(request) {
            Ok(mut response) => {
                let status = response.status().as_u16();
                let mut buf = Vec::new();
                let _ = response.body_mut().as_reader().take(64 * 1024).read_to_end(&mut buf);
                if (200..300).contains(&status) {
                    body_bytes = buf;
                    last_err = None;
                    break;
                }
                let err = crate::error::status_error(status, &buf);
                if err.retryable() && attempt < 2 {
                    last_err = Some(err);
                    std::thread::sleep(std::time::Duration::from_millis(150 * (attempt + 1) as u64));
                    continue;
                }
                return Err(err);
            }
            Err(e) => {
                if reader.failed {
                    return Err(ShareError::Cancelled);
                }
                let err = send_error(e);
                if err.retryable() && attempt < 2 {
                    last_err = Some(err);
                    std::thread::sleep(std::time::Duration::from_millis(150 * (attempt + 1) as u64));
                    continue;
                }
                if matches!(&err, ShareError::Network(t) if t.contains("cancelled")) {
                    return Err(ShareError::Cancelled);
                }
                return Err(err);
            }
        }
    }
    if let Some(e) = last_err {
        return Err(e);
    }
    let url = extract_link(http, &body_bytes)?;
    // The delete hint is the raw extracted value: the template needs it.
    let delete = if http.delete_url.is_empty() { None } else { Some(extract_link_value(http, &body_bytes)?) };
    Ok(Shared { url, remote: remote.to_string(), delete })
}

fn extract_link_value(http: &Http, body: &[u8]) -> Result<String> {
    let text = String::from_utf8_lossy(body);
    Ok(match &http.extract {
        Extract::Body => text.trim().to_string(),
        Extract::JsonPointer { pointer: p } => serde_json::from_slice::<serde_json::Value>(body)
            .ok()
            .and_then(|j| j.pointer(p).and_then(|v| v.as_str().map(str::to_string)))
            .unwrap_or_default(),
        Extract::Regex { pattern } => regex::Regex::new(pattern)
            .ok()
            .and_then(|re| re.captures(&text))
            .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
            .unwrap_or_default(),
    })
}

fn target_tls(target: &Target) -> crate::TlsOptions {
    target.tls.clone()
}

pub fn check(target: &Target, http: &Http) -> Result<crate::CheckReport> {
    let mut steps = Vec::new();
    // A tiny probe file through the same path the real upload takes.
    let dir = std::env::temp_dir().join(format!("heronite-share-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(ShareError::io)?;
    let probe = dir.join("probe.txt");
    std::fs::write(&probe, b"heronite share probe").map_err(ShareError::io)?;
    let mut noop = |_| Continue::Go;
    let outcome = upload(target, http, &probe, "probe.txt", 20, &ShareContext::default(), &mut noop);
    let _ = std::fs::remove_dir_all(&dir);
    let shared = outcome?;
    steps.push(tr!("Upload works").into());
    if !http.delete_url.is_empty() {
        let record = Record {
            id: "probe".into(),
            target_id: target.id.clone(),
            target_name: target.label(),
            file_name: "probe.txt".into(),
            size: 20,
            url: shared.url.clone(),
            remote: shared.remote.clone(),
            delete: shared.delete.clone(),
            at: String::new(),
            game: None,
            kind: crate::FileKind::Other,
        };
        delete(target, http, &record)?;
        steps.push(tr!("Delete works").into());
    }
    steps.push(tr!("The link reads: {url}", url = &shared.url));
    Ok(crate::CheckReport { steps })
}

pub fn delete(_target: &Target, http: &Http, record: &Record) -> Result<()> {
    if http.delete_url.is_empty() {
        return Ok(());
    }
    let Some(value) = &record.delete else { return Ok(()) };
    let agent = agent(&_target.tls)?;
    let url = http.delete_url.replace("{value}", value);
    let method = http.delete_method.to_ascii_uppercase();
    let headers = headers_of(http)?;
    retried(|| {
        let mut builder = ureq::http::Request::builder().method(method.as_str()).uri(&url);
        for (name, v) in &headers {
            builder = builder.header(name, v);
        }
        let request = builder.body(()).map_err(|e| ShareError::network(e.to_string()))?;
        let mut response = agent.run(request).map_err(send_error)?;
        expect_ok(&mut response, |s| (200..300).contains(&s) || s == 404)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HttpHeader;

    fn ctx() -> ShareContext {
        ShareContext { game: Some("Test Game".into()) }
    }

    fn multipart_http() -> Http {
        Http {
            url: "https://up.example/x".into(),
            method: "POST".into(),
            headers: vec![],
            body: HttpBody::Multipart {
                field: "file".into(),
                fields: vec![("key".into(), "{name}".into()), ("game".into(), "{game}".into())],
            },
            extract: Extract::Body,
            link_template: String::new(),
            delete_url: String::new(),
            delete_method: "DELETE".into(),
        }
    }

    #[test]
    fn multipart_body_is_byte_exact() {
        let http = multipart_http();
        let boundary = boundary_of("probe.txt");
        let (prefix, suffix) = multipart_parts(&http, "probe.txt", &ctx(), &boundary);
        let file_bytes = b"hello file";
        let chain = ChainReader::new(prefix.clone(), std::io::Cursor::new(file_bytes.to_vec()), suffix.clone());
        let mut all = Vec::new();
        let mut chain = chain;
        chain.read_to_end(&mut all).unwrap();
        let text = String::from_utf8(all).unwrap();
        let expected = format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"key\"\r\n\r\nprobe.txt\r\n--{b}\r\nContent-Disposition: form-data; name=\"game\"\r\n\r\nTest Game\r\n--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"probe.txt\"\r\nContent-Type: application/octet-stream\r\n\r\nhello file\r\n--{b}--\r\n",
            b = boundary
        );
        assert_eq!(text, expected);
    }

    #[test]
    fn extraction_modes() {
        let mut http = multipart_http();
        assert_eq!(extract_link(&http, b"  https://x/y \n").unwrap(), "https://x/y");
        http.extract = Extract::JsonPointer { pointer: "/data/url".into() };
        assert_eq!(extract_link(&http, br#"{"data":{"url":"https://x/z"}}"#).unwrap(), "https://x/z");
        assert!(extract_link(&http, br#"{"data":{}}"#).is_err());
        assert!(extract_link(&http, b"not json").is_err());
        http.extract = Extract::Regex { pattern: r#"src="([^"]+)""#.into() };
        assert_eq!(extract_link(&http, br#"<img src="https://x/i.png">"#).unwrap(), "https://x/i.png");
        assert!(extract_link(&http, b"nothing").is_err());
        http.extract = Extract::JsonPointer { pointer: "/id".into() };
        http.link_template = "https://img.example/{value}.png".into();
        assert_eq!(extract_link(&http, br#"{"id":"abc"}"#).unwrap(), "https://img.example/abc.png");
    }

    #[test]
    fn extraction_failures_say_why() {
        let mut http = multipart_http();
        http.extract = Extract::JsonPointer { pointer: "/missing".into() };
        let err = extract_link(&http, br#"{"a":1}"#).unwrap_err();
        assert!(err.to_string().contains("/missing"), "{err}");
    }

    #[test]
    fn secret_headers_unseal() {
        crate::seal::use_seal(std::sync::Arc::new(crate::seal::PlainSeal));
        let sealed = crate::config::seal_secret("s3cr3t").unwrap();
        let h = HttpHeader { name: "Authorization".into(), value: String::new(), secret: true, sealed_value: sealed };
        assert_eq!(h.value().unwrap(), "s3cr3t");
    }

    #[test]
    fn unseal_roundtrip() {
        crate::seal::use_seal(std::sync::Arc::new(crate::seal::PlainSeal));
        let sealed = crate::config::seal_secret("  spaced ").unwrap();
        assert_eq!(crate::config::unseal(&sealed).unwrap(), "spaced");
    }
}
