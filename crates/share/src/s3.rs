//! S3-compatible targets: SigV4 signed with rusty-s3, sent with ureq. Single
//! PUT up to a threshold, multipart above (parts retried, aborted on failure
//! or cancel). Links are the public template or a presigned GET.

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use heronite_i18n::tr;
use rusty_s3::actions::{AbortMultipartUpload, CompleteMultipartUpload, CreateMultipartUpload, S3Action, UploadPart};
use rusty_s3::{Bucket, Credentials, UrlStyle};

use crate::config::{unseal, Target, S3};
use crate::error::{status_error, Result, ShareError};
use crate::history::Record;
use crate::http_util::{agent, send_error, snippet};
use crate::progress::{Continue, CountingReader, Progress};
use crate::Shared;

/// Test-overridable sizes: single PUT under the threshold, parts of this size.
pub static MULTIPART_THRESHOLD: AtomicU64 = AtomicU64::new(64 * 1024 * 1024);
pub static PART_SIZE: AtomicU64 = AtomicU64::new(16 * 1024 * 1024);

fn bucket_of(s3: &S3) -> Result<Bucket> {
    let endpoint = if s3.endpoint.trim().is_empty() {
        if s3.region.trim().is_empty() {
            return Err(ShareError::config(tr!("Enter the region for AWS")));
        }
        format!("https://s3.{}.amazonaws.com", s3.region.trim())
    } else {
        s3.endpoint.trim().trim_end_matches('/').to_string()
    };
    let endpoint_url = url::Url::parse(&endpoint).map_err(|_| ShareError::config(tr!("The endpoint is not a valid URL")))?;
    let path_style = match s3.path_style {
        Some(p) => p,
        None => !s3.endpoint.trim().is_empty(),
    };
    let style = if path_style { UrlStyle::Path } else { UrlStyle::VirtualHost };
    let region = if s3.region.trim().is_empty() { "us-east-1".to_string() } else { s3.region.trim().to_string() };
    Bucket::new(endpoint_url, style, s3.bucket.trim().to_string(), region).map_err(|e| ShareError::config(format!("{e:?}")))
}

fn credentials_of(s3: &S3) -> Result<Credentials> {
    let secret = unseal(&s3.sealed_secret_key)?;
    if s3.sealed_session_token.is_empty() {
        Ok(Credentials::new(s3.access_key.trim(), secret))
    } else {
        Ok(Credentials::new_with_token(s3.access_key.trim(), secret, unseal(&s3.sealed_session_token)?))
    }
}

const SIGN_SECS: u64 = 300;

/// Sends a signed request. The body is empty or a reader; failures map the
/// status with the first bytes of the answer (never the signed URL).
fn send_signed(
    agent: &ureq::Agent,
    method: &str,
    signed: url::Url,
    extra_headers: &[(String, String)],
    body: Option<(u64, &mut dyn Read)>,
) -> Result<ureq::http::Response<ureq::Body>> {
    let mut builder = ureq::http::Request::builder().method(method).uri(signed.as_str());
    for (n, v) in extra_headers {
        builder = builder.header(n, v);
    }
    if let Some((len, _)) = &body {
        builder = builder.header("Content-Length", len.to_string());
    }
    let result = match body {
        Some((_, reader)) => {
            let request = builder.body(ureq::SendBody::from_reader(reader)).map_err(|e| ShareError::network(e.to_string()))?;
            agent.run(request)
        }
        None => {
            let request = builder.body(()).map_err(|e| ShareError::network(e.to_string()))?;
            agent.run(request)
        }
    };
    let mut response = result.map_err(send_error)?;
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(response);
    }
    let body_bytes = snippet(&mut response);
    Err(status_error(status, &body_bytes))
}

fn retried_send(
    agent: &ureq::Agent,
    method: &str,
    sign: &dyn Fn() -> url::Url,
    extra_headers: &[(String, String)],
    body: &mut dyn FnMut() -> Option<Vec<u8>>,
) -> Result<ureq::http::Response<ureq::Body>> {
    let mut last = None;
    for attempt in 0..3 {
        let bytes = body();
        match bytes {
            Some(b) => {
                let len = b.len() as u64;
                let mut cursor = std::io::Cursor::new(b);
                match send_signed(agent, method, sign(), extra_headers, Some((len, &mut cursor))) {
                    Ok(r) => return Ok(r),
                    Err(e) if e.retryable() && attempt < 2 => {
                        last = Some(e);
                        std::thread::sleep(Duration::from_millis(150 * (attempt + 1) as u64));
                    }
                    Err(e) => return Err(e),
                }
            }
            None => match send_signed(agent, method, sign(), extra_headers, None) {
                Ok(r) => return Ok(r),
                Err(e) if e.retryable() && attempt < 2 => {
                    last = Some(e);
                    std::thread::sleep(Duration::from_millis(150 * (attempt + 1) as u64));
                }
                Err(e) => return Err(e),
            },
        }
    }
    Err(last.unwrap())
}

pub fn upload(
    target: &Target,
    s3: &S3,
    file: &Path,
    remote: &str,
    size: u64,
    progress: &mut dyn FnMut(Progress) -> Continue,
) -> Result<Shared> {
    let ag = agent(&target.tls)?;
    let bucket = bucket_of(s3)?;
    let credentials = credentials_of(s3)?;
    let threshold = MULTIPART_THRESHOLD.load(Ordering::Relaxed);
    let mime = crate::mime::mime_of(remote);
    let mut acl_headers = Vec::new();
    if s3.acl_public_read {
        acl_headers.push(("x-amz-acl".to_string(), "public-read".to_string()));
    }
    acl_headers.push(("Content-Type".to_string(), mime.to_string()));

    if size <= threshold {
        single_put(&ag, &bucket, &credentials, file, remote, size, mime, &acl_headers, progress)?;
    } else {
        multipart(&ag, &bucket, &credentials, file, remote, size, mime, &acl_headers, progress)?;
    }

    let url =
        if !target.public_url.is_empty() { target.link_of(remote) } else { presign_get(&bucket, &credentials, remote, s3.presign_secs)? };
    Ok(Shared { url, remote: remote.to_string(), delete: Some(remote.to_string()) })
}

/// A presigned GET link, valid `presign_secs` (default 7 days, capped).
pub fn presign_get(bucket: &Bucket, credentials: &Credentials, remote: &str, presign_secs: Option<u32>) -> Result<String> {
    let secs = presign_secs.unwrap_or(7 * 24 * 3600).clamp(1, 604800) as u64;
    let action = rusty_s3::actions::GetObject::new(bucket, Some(credentials), remote);
    Ok(action.sign(Duration::from_secs(secs)).as_str().to_string())
}

fn sign_put<'a>(
    bucket: &'a Bucket,
    credentials: &'a Credentials,
    remote: &'a str,
    mime: &str,
    acl: &[(String, String)],
) -> (url::Url, Vec<(String, String)>) {
    let mut action = rusty_s3::actions::PutObject::new(bucket, Some(credentials), remote);
    let mut extra = Vec::new();
    for (n, v) in acl {
        if n == "x-amz-acl" {
            action.headers_mut().insert(std::borrow::Cow::Owned(n.clone()), std::borrow::Cow::Owned(v.clone()));
            extra.push((n.clone(), v.clone()));
        }
    }
    let _ = mime;
    (action.sign(Duration::from_secs(SIGN_SECS)), extra)
}

fn single_put(
    ag: &ureq::Agent,
    bucket: &Bucket,
    credentials: &Credentials,
    file: &Path,
    remote: &str,
    size: u64,
    mime: &str,
    acl: &[(String, String)],
    progress: &mut dyn FnMut(Progress) -> Continue,
) -> Result<()> {
    let mut last = None;
    for attempt in 0..3 {
        let (signed, extra) = sign_put(bucket, credentials, remote, mime, acl);
        let mut headers = vec![("Content-Type".to_string(), mime.to_string())];
        headers.extend(extra.clone());
        let f = std::fs::File::open(file).map_err(ShareError::io)?;
        let mut reader = CountingReader::new(f, size, &mut *progress);
        match send_signed(ag, "PUT", signed, &headers, Some((size, &mut reader))) {
            Ok(_) => return Ok(()),
            Err(e) if matches!(&e, ShareError::Network(t) if t.contains("cancelled") || t.contains("Interrupted")) || reader.failed => {
                return Err(ShareError::Cancelled)
            }
            Err(e) if e.retryable() && attempt < 2 => {
                last = Some(e);
                std::thread::sleep(Duration::from_millis(150));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap())
}

fn multipart(
    ag: &ureq::Agent,
    bucket: &Bucket,
    credentials: &Credentials,
    file: &Path,
    remote: &str,
    size: u64,
    mime: &str,
    acl: &[(String, String)],
    progress: &mut dyn FnMut(Progress) -> Continue,
) -> Result<()> {
    // CreateMultipartUpload.
    let mut create = CreateMultipartUpload::new(bucket, Some(credentials), remote);
    let mut headers = vec![("Content-Type".to_string(), mime.to_string())];
    for (n, v) in acl {
        if n == "x-amz-acl" {
            create.headers_mut().insert(std::borrow::Cow::Owned(n.clone()), std::borrow::Cow::Owned(v.clone()));
            headers.push((n.clone(), v.clone()));
        }
    }
    let sign = || create.sign(Duration::from_secs(SIGN_SECS));
    let headers_c = headers.clone();
    let mut response = retried_send(ag, "POST", &sign, &headers_c, &mut || None::<Vec<u8>>)?;
    let mut body = Vec::new();
    let _ = response.body_mut().as_reader().take(64 * 1024).read_to_end(&mut body);
    let created = CreateMultipartUpload::parse_response(&String::from_utf8_lossy(&body))
        .map_err(|_| ShareError::Server { status: 0, body: tr!("the multipart answer is not XML").into() })?;
    let upload_id = created.upload_id().to_string();

    let part_size = PART_SIZE.load(Ordering::Relaxed).max(1024);
    let outcome = upload_parts(ag, bucket, credentials, file, remote, size, part_size, &upload_id, progress);
    let etags = match outcome {
        Ok(e) => e,
        Err(e) => {
            abort_multipart(ag, bucket, credentials, remote, &upload_id);
            return Err(e);
        }
    };

    // CompleteMultipartUpload.
    let complete = CompleteMultipartUpload::new(bucket, Some(credentials), remote, &upload_id, etags.iter().map(|s| s.as_str()));
    let signed = complete.sign(Duration::from_secs(SIGN_SECS));
    let xml = complete.body().into_bytes();
    let sign = || signed.clone();
    match retried_send(ag, "POST", &sign, &[], &mut || Some(xml.clone())) {
        Ok(_) => Ok(()),
        Err(e) => {
            abort_multipart(ag, bucket, credentials, remote, &upload_id);
            Err(e)
        }
    }
}

fn upload_parts(
    ag: &ureq::Agent,
    bucket: &Bucket,
    credentials: &Credentials,
    file: &Path,
    remote: &str,
    size: u64,
    part_size: u64,
    upload_id: &str,
    progress: &mut dyn FnMut(Progress) -> Continue,
) -> Result<Vec<String>> {
    let mut etags = Vec::new();
    let mut offset = 0u64;
    let mut part_number = 1u16;
    let mut sent_before = 0u64;
    while offset < size {
        let len = part_size.min(size - offset);
        let mut attempt = 0;
        let etag = loop {
            let upload_part = UploadPart::new(bucket, Some(credentials), remote, part_number, upload_id);
            let signed = upload_part.sign(Duration::from_secs(SIGN_SECS));
            let f = std::fs::File::open(file).map_err(ShareError::io)?;
            let part_reader = OffsetReader::new(f, offset, len);
            let mut counting = CountingReader::with_base(part_reader, size, sent_before, &mut *progress);
            let cancelled = counting.failed;
            let _ = cancelled;
            match send_signed(ag, "PUT", signed, &[], Some((len, &mut counting))) {
                Ok(response) => {
                    let etag =
                        response.headers().get("ETag").and_then(|v| v.to_str().ok()).unwrap_or_default().trim_matches('"').to_string();
                    break etag;
                }
                Err(e)
                    if counting.failed || matches!(&e, ShareError::Network(t) if t.contains("cancelled") || t.contains("Interrupted")) =>
                {
                    return Err(ShareError::Cancelled)
                }
                Err(e) if e.retryable() && attempt < 2 => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(150));
                }
                Err(e) => return Err(e),
            }
        };
        etags.push(etag);
        sent_before += len;
        offset += len;
        part_number += 1;
        // ureq reads exactly Content-Length, so small parts never report
        // through the counting reader: report per finished part instead.
        if progress(Progress { sent: sent_before, total: size }).cancelled() {
            return Err(ShareError::Cancelled);
        }
    }
    if progress(Progress { sent: size, total: size }).cancelled() {
        return Err(ShareError::Cancelled);
    }
    Ok(etags)
}

fn abort_multipart(ag: &ureq::Agent, bucket: &Bucket, credentials: &Credentials, remote: &str, upload_id: &str) {
    let abort = AbortMultipartUpload::new(bucket, Some(credentials), remote, upload_id);
    let signed = abort.sign(Duration::from_secs(SIGN_SECS));
    if let Err(e) = send_signed(ag, "DELETE", signed, &[], None) {
        log::warn!("share: multipart abort failed: {e}");
    }
}

/// Reads exactly `len` bytes of `file` starting at `offset`.
struct OffsetReader {
    inner: std::fs::File,
    left: u64,
}

impl OffsetReader {
    fn new(mut inner: std::fs::File, offset: u64, len: u64) -> Self {
        use std::io::{Seek, SeekFrom};
        let _ = inner.seek(SeekFrom::Start(offset));
        OffsetReader { inner, left: len }
    }
}

impl Read for OffsetReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        let take = buf.len().min(self.left as usize);
        let n = self.inner.read(&mut buf[..take])?;
        self.left -= n as u64;
        Ok(n)
    }
}

pub fn delete(target: &Target, s3: &S3, record: &Record) -> Result<()> {
    let ag = agent(&target.tls)?;
    let bucket = bucket_of(s3)?;
    let credentials = credentials_of(s3)?;
    let remote = record.delete.clone().unwrap_or_else(|| record.remote.clone());
    let action = rusty_s3::actions::DeleteObject::new(&bucket, Some(&credentials), &remote);
    let sign = || action.sign(Duration::from_secs(SIGN_SECS));
    // 204 and 404 both mean gone.
    match retried_send(&ag, "DELETE", &sign, &[], &mut || None::<Vec<u8>>) {
        Ok(_) => Ok(()),
        Err(ShareError::NotFound(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

pub fn check(target: &Target, s3: &S3) -> Result<crate::CheckReport> {
    let ag = agent(&target.tls)?;
    let bucket = bucket_of(s3)?;
    let credentials = credentials_of(s3)?;
    let remote = crate::join_remote(&target.folder, "heronite-probe.txt");
    let mut steps = Vec::new();
    // PUT probe.
    let mut headers = vec![("Content-Type".to_string(), "text/plain".to_string())];
    let (signed, extra) = sign_put(&bucket, &credentials, &remote, "text/plain", &[]);
    headers.extend(extra);
    let probe: Vec<u8> = b"probe".to_vec();
    let probe_len = probe.len() as u64;
    let sign = || signed.clone();
    let _ = probe_len;
    retried_send(&ag, "PUT", &sign, &headers, &mut || Some(probe.clone()))?;
    steps.push(tr!("Upload works").into());
    // HEAD probe.
    let head = rusty_s3::actions::HeadObject::new(&bucket, Some(&credentials), &remote);
    let sign = || head.sign(Duration::from_secs(SIGN_SECS));
    retried_send(&ag, "HEAD", &sign, &[], &mut || None::<Vec<u8>>)?;
    steps.push(tr!("Reading back works").into());
    // DELETE probe.
    let action = rusty_s3::actions::DeleteObject::new(&bucket, Some(&credentials), &remote);
    let sign = || action.sign(Duration::from_secs(SIGN_SECS));
    retried_send(&ag, "DELETE", &sign, &[], &mut || None::<Vec<u8>>)?;
    steps.push(tr!("Delete works").into());
    Ok(crate::CheckReport { steps })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s3(endpoint: &str, path_style: Option<bool>) -> S3 {
        crate::seal::use_seal(std::sync::Arc::new(crate::seal::PlainSeal));
        S3 {
            endpoint: endpoint.into(),
            region: "eu-central-1".into(),
            bucket: "shots".into(),
            access_key: "AKIAEXAMPLE".into(),
            sealed_secret_key: crate::config::seal_secret("secret").unwrap(),
            path_style,
            ..Default::default()
        }
    }

    #[test]
    fn path_style_urls() {
        let cfg = s3("https://minio.lan:9000", Some(true));
        let bucket = bucket_of(&cfg).unwrap();
        assert_eq!(bucket.object_url("a/b.png").unwrap().as_str(), "https://minio.lan:9000/shots/a/b.png");
    }

    #[test]
    fn virtual_host_urls() {
        let cfg = s3("https://s3.example.com", Some(false));
        let bucket = bucket_of(&cfg).unwrap();
        assert_eq!(bucket.object_url("a/b.png").unwrap().as_str(), "https://shots.s3.example.com/a/b.png");
    }

    #[test]
    fn aws_default_endpoint() {
        let cfg = s3("", Some(false));
        let bucket = bucket_of(&cfg).unwrap();
        let url = bucket.object_url("x.png").unwrap();
        assert!(url.as_str().starts_with("https://shots.s3.eu-central-1.amazonaws.com/"), "{url}");
    }

    #[test]
    fn custom_endpoint_defaults_to_path_style() {
        let cfg = s3("https://minio.lan:9000", None);
        let bucket = bucket_of(&cfg).unwrap();
        assert_eq!(bucket.object_url("x.png").unwrap().as_str(), "https://minio.lan:9000/shots/x.png");
    }

    #[test]
    fn prefix_in_keys() {
        let cfg = s3("https://minio.lan:9000", Some(true));
        let bucket = bucket_of(&cfg).unwrap();
        assert_eq!(bucket.object_url("games/x.png").unwrap().as_str(), "https://minio.lan:9000/shots/games/x.png");
    }

    #[test]
    fn presigned_links_carry_expiry_and_cap() {
        crate::seal::use_seal(std::sync::Arc::new(crate::seal::PlainSeal));
        let cfg = s3("https://minio.lan:9000", Some(true));
        let bucket = bucket_of(&cfg).unwrap();
        let credentials = credentials_of(&cfg).unwrap();
        let link = presign_get(&bucket, &credentials, "x.png", Some(3600)).unwrap();
        assert!(link.contains("X-Amz-Expires=3600"), "{link}");
        assert!(link.contains("X-Amz-Signature="), "{link}");
        assert!(link.contains("x.png"), "{link}");
        let link = presign_get(&bucket, &credentials, "x.png", Some(u32::MAX)).unwrap();
        assert!(link.contains("X-Amz-Expires=604800"), "{link}");
        let link = presign_get(&bucket, &credentials, "x.png", None).unwrap();
        assert!(link.contains("X-Amz-Expires=604800"), "{link}");
    }
}
