//! Sharing screenshots and clips to the user's own server: S3-compatible,
//! SFTP, WebDAV (incl. Nextcloud), FTP/FTPS and custom HTTP uploaders.
//!
//! Blocking API for worker threads. `share` streams the file, reports
//! progress and honours cancellation; the history remembers what went where
//! so uploads can be deleted later.

pub mod config;
pub mod error;
pub mod ftp;
pub mod history;
pub mod http;
pub mod http_util;
pub mod mime;
pub mod naming;
pub mod progress;
pub mod s3;
pub mod seal;
pub mod sftp;
pub mod tls;
pub mod webdav;

use std::path::Path;

use heronite_data::DataPaths;

pub use config::{Extract, Ftp, FtpTls, Http, HttpBody, HttpHeader, Kind, Sftp, SftpAuth, Target, Targets, WebDav, S3};
pub use error::{Result, ShareError};
pub use history::{FileKind, History, Record};
pub use naming::{NameMode, Naming};
pub use progress::{Continue, Progress};
pub use tls::TlsOptions;

/// What the upload is about; fills the `{game}` template placeholder.
#[derive(Debug, Clone, Default)]
pub struct ShareContext {
    pub game: Option<String>,
}

/// What an upload produced: the link, the remote name, and what the backend
/// needs to delete it.
#[derive(Debug, Clone)]
pub struct Shared {
    pub url: String,
    pub remote: String,
    pub delete: Option<String>,
}

/// What "test connection" did.
#[derive(Debug, Clone)]
pub struct CheckReport {
    pub steps: Vec<String>,
}

/// Uploads `file` to the target, streams it (never in memory), reports
/// progress. On success the history gains a record; on failure or cancel the
/// server is cleaned up best-effort.
pub fn share(
    paths: &DataPaths,
    target_id: &str,
    file: &Path,
    ctx: &ShareContext,
    progress: &mut dyn FnMut(Progress) -> Continue,
) -> Result<Record> {
    let targets = Targets::load(paths);
    let target =
        targets.get(target_id).ok_or_else(|| ShareError::config(heronite_i18n::tr!("That sharing target does not exist anymore")))?;
    if !target.enabled {
        return Err(ShareError::config(heronite_i18n::tr!("That sharing target is disabled")));
    }
    target.validate()?;
    let meta = std::fs::metadata(file).map_err(ShareError::io)?;
    if !meta.is_file() {
        return Err(ShareError::Io(heronite_i18n::tr!("{file} is not a file", file = file.display().to_string())));
    }
    let file_name = file.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
    let remote_name = target.naming_or(&targets.naming).name_for(&file_name, ctx)?;
    let remote = join_remote(&target.folder, &remote_name);

    let shared = upload(target, file, &remote, meta.len(), ctx, progress)?;

    let record = Record {
        id: format!("{:x}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()),
        target_id: target.id.clone(),
        target_name: target.label(),
        file_name,
        size: meta.len(),
        url: shared.url.clone(),
        remote: shared.remote.clone(),
        delete: shared.delete.clone(),
        at: chrono::Utc::now().to_rfc3339(),
        game: ctx.game.clone(),
        kind: FileKind::of(&remote_name),
    };
    let mut history = History::load(paths);
    history.push(record.clone());
    if let Err(e) = history.save(paths) {
        log::warn!("share: history did not save: {e}");
    }
    Ok(record)
}

fn upload(
    target: &Target,
    file: &Path,
    remote: &str,
    size: u64,
    ctx: &ShareContext,
    progress: &mut dyn FnMut(Progress) -> Continue,
) -> Result<Shared> {
    match &target.kind {
        Kind::S3(s3) => s3::upload(target, s3, file, remote, size, progress),
        Kind::Sftp(sftp) => sftp::upload(target, sftp, file, remote, size, progress),
        Kind::WebDav(w) => webdav::upload(target, w, file, remote, size, progress),
        Kind::Ftp(f) => ftp::upload(target, f, file, remote, size, progress),
        Kind::Http(h) => http::upload(target, h, file, remote, size, ctx, progress),
    }
}

/// "Test connection": authenticate, write a small probe, read it back,
/// delete it. The steps say how far it got when something fails.
pub fn check(target: &Target) -> Result<CheckReport> {
    target.validate()?;
    match &target.kind {
        Kind::S3(s3) => s3::check(target, s3),
        Kind::Sftp(sftp) => sftp::check(target, sftp),
        Kind::WebDav(w) => webdav::check(target, w),
        Kind::Ftp(f) => ftp::check(target, f),
        Kind::Http(h) => http::check(target, h),
    }
}

/// Removes the upload from the server best-effort and drops its record.
pub fn delete(paths: &DataPaths, record_id: &str) -> Result<()> {
    let mut history = History::load(paths);
    let Some(record) = history.get(record_id).cloned() else {
        return Ok(());
    };
    let targets = Targets::load(paths);
    if let Some(target) = targets.get(&record.target_id) {
        let outcome = match &target.kind {
            Kind::S3(s3) => s3::delete(target, s3, &record),
            Kind::Sftp(sftp) => sftp::delete(target, sftp, &record),
            Kind::WebDav(w) => webdav::delete(target, w, &record),
            Kind::Ftp(f) => ftp::delete(target, f, &record),
            Kind::Http(h) => http::delete(target, h, &record),
        };
        if let Err(e) = outcome {
            // Best effort: a gone server must not trap the record forever.
            log::warn!("share: remote delete failed for {}: {e}", record.remote);
        }
    }
    history.remove(record_id);
    if let Err(e) = history.save(paths) {
        return Err(ShareError::io(e));
    }
    Ok(())
}

/// The newest shares first, capped at 200.
pub fn history(paths: &DataPaths) -> Vec<Record> {
    History::load(paths).records
}

/// One path: the folder prefix and the file name, single slashes, never
/// leading or trailing ones.
pub(crate) fn join_remote(folder: &str, name: &str) -> String {
    let folder = folder.trim_matches('/');
    if folder.is_empty() {
        name.to_string()
    } else {
        format!("{folder}/{name}")
    }
}

impl Target {
    pub(crate) fn naming_or<'a>(&'a self, naming: &'a Naming) -> &'a Naming {
        naming
    }
}
