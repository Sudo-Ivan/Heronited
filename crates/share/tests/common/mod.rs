//! Shared harness for the in-process protocol tests: a tiny_http server that
//! records requests, plus a SigV4-verifying S3 implementation.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

pub type Request = (String, String, Vec<(String, String)>, Vec<u8>);

/// A recording tiny_http server on 127.0.0.1. The responder answers from the
/// method, the raw url (query included), the headers and the body.
pub struct Server {
    pub url: String,
    pub port: u16,
    pub requests: Arc<Mutex<Vec<Request>>>,
}

pub fn serve(
    responder: impl Fn(&str, &str, &[(String, String)], &[u8]) -> (u16, Vec<(String, String)>, Vec<u8>) + Send + Sync + 'static,
) -> Server {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    let requests: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
    let reqs = requests.clone();
    std::thread::spawn(move || loop {
        let mut request = match server.recv() {
            Ok(r) => r,
            Err(_) => break,
        };
        let mut body = Vec::new();
        let _ = request.as_reader().take(256 * 1024 * 1024).read_to_end(&mut body);
        let headers: Vec<(String, String)> =
            request.headers().iter().map(|h| (h.field.as_str().to_string(), h.value.as_str().to_string())).collect();
        let method = request.method().to_string();
        let url = request.url().to_string();
        reqs.lock().unwrap().push((method.clone(), url.clone(), headers.clone(), body.clone()));
        let (status, out_headers, answer) = responder(&method, &url, &headers, &body);
        let mut response = tiny_http::Response::from_data(answer).with_status_code(status);
        for (n, v) in out_headers {
            response.add_header(tiny_http::Header::from_bytes(n.as_bytes(), v.as_bytes()).unwrap());
        }
        let _ = request.respond(response);
    });
    Server { url: format!("http://127.0.0.1:{port}"), port, requests }
}

/// Simple responder adapter for servers without custom headers.
pub fn serve_simple(responder: impl Fn(&str, &str, &[(String, String)], &[u8]) -> (u16, String) + Send + Sync + 'static) -> Server {
    serve(move |m, u, h, b| {
        let (s, body) = responder(m, u, h, b);
        (s, vec![], body.into_bytes())
    })
}

// ---------- SigV4 ----------

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).unwrap();
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// AWS percent-encoding: everything except A-Za-z0-9-._~ is %XX uppercase.
fn aws_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut parts = p.splitn(2, '=');
            let k = urlencoding_decode(parts.next().unwrap_or_default());
            let v = urlencoding_decode(parts.next().unwrap_or_default());
            (k, v)
        })
        .collect()
}

fn urlencoding_decode(s: &str) -> String {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Verifies a presigned-query SigV4 request completely independently of
/// rusty-s3. Returns Ok(()) or a description of what is wrong.
pub fn verify_presigned(method: &str, raw_url: &str, headers: &[(String, String)], secret: &str) -> Result<(), String> {
    let (path, query) = raw_url.split_once('?').ok_or("no query")?;
    let params = parse_query(query);
    let get = |name: &str| params.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    let algorithm = get("X-Amz-Algorithm").ok_or("no algorithm")?;
    if algorithm != "AWS4-HMAC-SHA256" {
        return Err(format!("bad algorithm {algorithm}"));
    }
    let credential = get("X-Amz-Credential").ok_or("no credential")?;
    let date = get("X-Amz-Date").ok_or("no date")?;
    let signed_headers = get("X-Amz-SignedHeaders").ok_or("no signed headers")?;
    let signature = get("X-Amz-Signature").ok_or("no signature")?;
    // scope is credential without the access key
    let scope = credential.splitn(2, '/').nth(1).ok_or("bad credential")?.to_string();

    // Canonical query: every parameter but the signature, sorted, aws-encoded.
    let mut pairs: Vec<(String, String)> = params.into_iter().filter(|(k, _)| k != "X-Amz-Signature").collect();
    pairs.sort();
    let canonical_query = pairs.iter().map(|(k, v)| format!("{}={}", aws_encode(k), aws_encode(v))).collect::<Vec<_>>().join("&");

    // Canonical headers for exactly the signed headers.
    let mut canonical_headers = String::new();
    for name in signed_headers.split(';') {
        let value = headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim().to_string())
            .ok_or_else(|| format!("signed header {name} missing"))?;
        canonical_headers.push_str(&format!("{}:{}\n", name.to_lowercase(), value.split_whitespace().collect::<Vec<_>>().join(" ")));
    }
    // Canonical URI: aws-encode each segment but keep slashes.
    let canonical_uri: String = path.split('/').map(aws_encode).collect::<Vec<_>>().join("/");
    let canonical = format!("{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\nUNSIGNED-PAYLOAD");
    let string_to_sign = format!("AWS4-HMAC-SHA256\n{date}\n{scope}\n{}", sha256_hex(canonical.as_bytes()));

    let parts: Vec<&str> = scope.split('/').collect();
    if parts.len() != 4 {
        return Err(format!("bad scope {scope}"));
    }
    let mut key = hmac(format!("AWS4{secret}").as_bytes(), parts[0]);
    key = hmac(&key, parts[1]);
    key = hmac(&key, parts[2]);
    key = hmac(&key, "aws4_request");
    let expect = hex::encode(hmac(&key, &string_to_sign));
    if expect != signature {
        return Err(format!("signature mismatch (canonical request hash {})", sha256_hex(canonical.as_bytes())));
    }
    Ok(())
}

// ---------- the S3 server ----------

pub struct Faults {
    /// Number of 500s to answer before succeeding (any operation).
    pub flaky: AtomicUsize,
}

pub struct S3State {
    /// (bucket, key) -> bytes
    pub objects: HashMap<(String, String), Vec<u8>>,
    pub acls: HashMap<(String, String), String>,
    /// upload id -> (bucket, key, parts so far)
    pub uploads: HashMap<String, (String, String, HashMap<u16, (String, Vec<u8>)>)>,
    pub aborted: Vec<String>,
    pub completed: Vec<(String, Vec<String>)>,
    pub next_upload: usize,
    pub buckets: Vec<String>,
    pub faults: Faults,
}

pub struct S3Server {
    pub server: Server,
    pub state: Arc<Mutex<S3State>>,
    pub secret: String,
    pub key: String,
    /// When true the signature check is bypassed (for non-crypto tests).
    pub strict: bool,
}

impl S3Server {
    pub fn url(&self) -> String {
        self.server.url.clone()
    }
}

fn xml_error(code: &str, message: &str) -> Vec<u8> {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{code}</Code><Message>{message}</Message></Error>").into_bytes()
}

pub fn s3_server(buckets: &[&str]) -> Arc<S3Server> {
    s3_server_strict(buckets, true)
}

pub fn s3_server_strict(buckets: &[&str], strict: bool) -> Arc<S3Server> {
    let state = Arc::new(Mutex::new(S3State {
        objects: HashMap::new(),
        acls: HashMap::new(),
        uploads: HashMap::new(),
        aborted: Vec::new(),
        completed: Vec::new(),
        next_upload: 0,
        buckets: buckets.iter().map(|s| s.to_string()).collect(),
        faults: Faults { flaky: AtomicUsize::new(0) },
    }));
    let st = state.clone();
    let secret = "testsecret".to_string();
    let sec = secret.clone();
    let server = serve(move |method, raw_url, headers, body| {
        // fault injection: 500 then success
        {
            let state = st.lock().unwrap();
            if state.faults.flaky.load(Ordering::Relaxed) > 0 {
                state.faults.flaky.fetch_sub(1, Ordering::Relaxed);
                return (500, vec![], xml_error("InternalError", "injected"));
            }
        }
        if strict {
            if let Err(e) = verify_presigned(method, raw_url, headers, &sec) {
                return (403, vec![], xml_error("SignatureDoesNotMatch", &e));
            }
        }
        let (path, query) = raw_url.split_once('?').unwrap_or((raw_url, ""));
        let params: HashMap<String, String> = parse_query_map(query).into_iter().collect();
        // bucket and key: path-style first, virtual-host by the Host header
        let host = headers.iter().find(|(n, _)| n.eq_ignore_ascii_case("host")).map(|(_, v)| v.clone()).unwrap_or_default();
        let (bucket, key) = split_bucket_key(path, &host, &st.lock().unwrap().buckets);
        let mut state = st.lock().unwrap();
        if !state.buckets.contains(&bucket) {
            return (404, vec![], xml_error("NoSuchBucket", "the bucket does not exist"));
        }
        let upload_id = params.get("uploadId").cloned();
        match (method, params.contains_key("uploads"), upload_id, params.get("partNumber").cloned()) {
            // CreateMultipartUpload
            ("POST", true, _, _) => {
                state.next_upload += 1;
                let id = format!("upload-{}", state.next_upload);
                state.uploads.insert(id.clone(), (bucket.clone(), key.clone(), HashMap::new()));
                let xml = format!("<?xml version=\"1.0\"?><InitiateMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>{bucket}</Bucket><Key>{key}</Key><UploadId>{id}</UploadId></InitiateMultipartUploadResult>");
                (200, vec![], xml.into_bytes())
            }
            // UploadPart
            ("PUT", false, Some(id), Some(part)) if params.contains_key("partNumber") => {
                let number: u16 = part.parse().unwrap_or(0);
                let Some((_, _, parts)) = state.uploads.get_mut(&id) else {
                    return (404, vec![], xml_error("NoSuchUpload", "no such upload"));
                };
                let etag = format!("{:032x}", crc32_simple(body) as u128 + number as u128);
                parts.insert(number, (etag.clone(), body.to_vec()));
                (200, vec![("ETag".into(), format!("\"{etag}\""))], vec![])
            }
            // CompleteMultipartUpload
            ("POST", false, Some(id), _) => {
                let Some((b, k, parts)) = state.uploads.remove(&id) else {
                    return (404, vec![], xml_error("NoSuchUpload", "no such upload"));
                };
                // parse the XML body for part order and etags
                let text = String::from_utf8_lossy(body).to_string();
                let mut requested: Vec<(u16, String)> = Vec::new();
                for piece in text.split("<Part>").skip(1) {
                    let num = between(piece, "<PartNumber>", "</PartNumber>").and_then(|s| s.parse::<u16>().ok()).unwrap_or(0);
                    let etag = between(piece, "<ETag>", "</ETag>").unwrap_or_default().trim_matches('"').to_string();
                    requested.push((num, etag));
                }
                // parts must be ascending
                let ascending = requested.windows(2).all(|w| w[0].0 < w[1].0);
                if !ascending || requested.is_empty() {
                    return (400, vec![], xml_error("MalformedXML", "parts not ascending"));
                }
                let mut all = Vec::new();
                let mut etags = Vec::new();
                for (num, etag) in &requested {
                    match parts.get(num) {
                        Some((stored_etag, bytes)) if stored_etag == etag => {
                            all.extend_from_slice(bytes);
                            etags.push(etag.clone());
                        }
                        _ => return (400, vec![], xml_error("InvalidPart", &format!("part {num} etag mismatch"))),
                    }
                }
                state.objects.insert((b.clone(), k.clone()), all);
                state.completed.push((k.clone(), etags));
                let final_etag = format!("\"{:032x}-{}\"", crc32_simple(&state.objects[&(b.clone(), k.clone())]) as u128, requested.len());
                let xml = format!("<?xml version=\"1.0\"?><CompleteMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>{b}</Bucket><Key>{k}</Key><ETag>{final_etag}</ETag></CompleteMultipartUploadResult>");
                (200, vec![], xml.into_bytes())
            }
            // AbortMultipartUpload
            ("DELETE", false, Some(id), _) if params.contains_key("uploadId") => {
                state.uploads.remove(&id);
                state.aborted.push(id);
                (204, vec![], vec![])
            }
            // PutObject
            ("PUT", false, None, _) => {
                if let Some((_, v)) = headers.iter().find(|(n, _)| n.eq_ignore_ascii_case("x-amz-acl")) {
                    state.acls.insert((bucket.clone(), key.clone()), v.clone());
                }
                state.objects.insert((bucket, key), body.to_vec());
                (200, vec![("ETag".into(), "\"etag\"".into())], vec![])
            }
            ("HEAD", false, None, _) => {
                if state.objects.contains_key(&(bucket, key)) {
                    (200, vec![], vec![])
                } else {
                    (404, vec![], vec![])
                }
            }
            ("GET", false, None, _) => match state.objects.get(&(bucket, key)) {
                Some(bytes) => (200, vec![], bytes.clone()),
                None => (404, vec![], xml_error("NoSuchKey", "no such key")),
            },
            ("DELETE", false, None, _) => {
                state.objects.remove(&(bucket, key));
                (204, vec![], vec![])
            }
            _ => (400, vec![], xml_error("NotImplemented", &format!("{method} {raw_url}"))),
        }
    });
    Arc::new(S3Server { server, state, secret, key: "testkey".into(), strict })
}

fn crc32_simple(data: &[u8]) -> u64 {
    let mut h: u64 = 5381;
    for b in data {
        h = h.wrapping_mul(33).wrapping_add(*b as u64);
    }
    h
}

fn between(s: &str, open: &str, close: &str) -> Option<String> {
    let a = s.find(open)? + open.len();
    let b = s[a..].find(close)? + a;
    Some(s[a..b].to_string())
}

pub fn parse_query_map(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut parts = p.splitn(2, '=');
            let k = urlencoding_decode(parts.next().unwrap_or_default());
            let v = urlencoding_decode(parts.next().unwrap_or_default());
            (k, v)
        })
        .collect()
}

/// Path-style `/bucket/key`; virtual-host when the host starts with a bucket
/// name followed by a dot.
fn split_bucket_key(path: &str, host: &str, buckets: &[String]) -> (String, String) {
    let trimmed = path.trim_start_matches('/');
    for bucket in buckets {
        if host.starts_with(&format!("{bucket}.")) {
            return (bucket.clone(), trimmed.to_string());
        }
    }
    match trimmed.split_once('/') {
        Some((b, k)) => (b.to_string(), k.to_string()),
        None => (trimmed.to_string(), String::new()),
    }
}
