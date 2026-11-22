//! Making a snapshot from files on this PC and sending it.
//!
//! Every byte is hashed twice: once to name its chunk in the manifest and
//! once more as it is read for the upload, and a file that changed in
//! between stops the backup instead of sending a state that never existed.
//! Nothing is sent that would not fit: the whole snapshot is checked against
//! the server's limits
//! before the first byte leaves, and what the server does not hold yet is
//! checked against the space left before the first chunk goes.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use base64::Engine;
use heronite_i18n::tr;

use crate::client::{ChunkRef, Client, FileEntry, Limits, Manifest, Me};
use crate::error::{Result, SavesError};
use crate::hash::sha256_hex;

/// The chunk size this client cuts files into (clamped to the server's).
const CHUNK_TARGET: u64 = 1 << 20;

/// Longest a path of the snapshot may be, in bytes, and a name inside it.
const MAX_PATH_BYTES: usize = 1024;
const MAX_NAME_BYTES: usize = 255;
const MAX_DEPTH: usize = 64;

/// A file to send: `path` is where it lives in the snapshot (`/` between
/// names, each name already [`escape_name`]d), `full` where it is on this PC.
#[derive(Debug, Clone)]
pub struct LocalFile {
    pub path: String,
    pub full: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// Its path is too long or too deep for the server.
    PathTooLong,
    /// Another file whose name differs only in case came first.
    CaseCollision,
    /// It is bigger than the server accepts for one file.
    TooLarge,
    /// It could not be read (in use, no permission, gone).
    Unreadable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub path: String,
    pub reason: SkipReason,
}

/// A manifest and where to read each chunk from.
#[derive(Debug)]
pub struct Prepared {
    pub manifest: Manifest,
    /// For each file of the manifest (same order), where it is.
    sources: Vec<PathBuf>,
    pub total_bytes: u64,
    pub skipped: Vec<Skipped>,
}

impl Prepared {
    pub fn files(&self) -> usize {
        self.manifest.files.len()
    }

    /// Where each file of the manifest is on this PC (same order).
    pub fn sources(&self) -> &[PathBuf] {
        &self.sources
    }
}

/// A name of a file made safe for any system: the characters Windows forbids,
/// control characters, `%`, a trailing dot or space and a reserved device
/// name are written as `%XX`, so a file with such a name on Linux is still
/// saved and comes back under its own name. [`unescape_name`] reverses it.
pub fn escape_name(name: &str) -> String {
    let reserved = {
        let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
        matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (base.len() == 4
                && (base.starts_with("COM") || base.starts_with("LPT"))
                && base.as_bytes()[3].is_ascii_digit()
                && base.as_bytes()[3] != b'0')
    };
    if name == "." || name == ".." {
        return name.replace('.', "%2E");
    }
    let last = name.chars().count().saturating_sub(1);
    let mut out = String::with_capacity(name.len());
    for (i, c) in name.chars().enumerate() {
        let bad = c.is_control()
            || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\' | '/' | '%')
            || (i == last && (c == '.' || c == ' '))
            || (reserved && i == 0);
        if bad {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The name [`escape_name`] made `escaped` from.
pub fn unescape_name(escaped: &str) -> String {
    let bytes = escaped.as_bytes();
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether `path` of a snapshot is something to write under a folder: no
/// empty names, no `.` or `..`, no separators other than `/`.
pub fn is_safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path.split('/').all(|n| !n.is_empty() && n != "." && n != ".." && !n.contains('\\') && !n.contains('\0') && !n.contains(':'))
}

fn path_fits(path: &str) -> bool {
    path.len() <= MAX_PATH_BYTES && path.split('/').count() <= MAX_DEPTH && path.split('/').all(|n| n.len() <= MAX_NAME_BYTES)
}

fn chunk_size(limits: &Limits) -> u64 {
    CHUNK_TARGET.clamp(limits.min_chunk_bytes.max(1), limits.max_chunk_bytes.max(limits.min_chunk_bytes).max(1))
}

fn mtime_ns(md: &fs::Metadata) -> i64 {
    md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

/// Hashes `files` into a manifest. Files that cannot go (name too long,
/// unreadable, too big, case twins) are listed in `skipped`; the rest is
/// what the snapshot holds. Fails when the snapshot as a whole is over a
/// limit, so the caller never sends part of what it was asked to.
pub fn prepare(files: Vec<LocalFile>, limits: &Limits, device: &str, label: &str, parent: Option<&str>) -> Result<Prepared> {
    let mut files = files;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files.dedup_by(|a, b| a.path == b.path);
    let cap = limits.max_snapshot_bytes.max(1);
    let size = chunk_size(limits);
    let (mut entries, mut sources, mut skipped) = (Vec::new(), Vec::new(), Vec::new());
    let mut seen_fold: HashSet<String> = HashSet::new();
    let (mut total, mut chunk_count) = (0u64, 0u64);
    for f in files {
        let skip = |reason| Skipped { path: f.path.clone(), reason };
        if !path_fits(&f.path) {
            skipped.push(skip(SkipReason::PathTooLong));
            continue;
        }
        if !seen_fold.insert(f.path.to_lowercase()) {
            skipped.push(skip(SkipReason::CaseCollision));
            continue;
        }
        let md = match fs::symlink_metadata(&f.full) {
            Ok(md) if md.is_file() => md,
            Ok(_) => continue,
            Err(e) => {
                skipped.push(skip(SkipReason::Unreadable(e.to_string())));
                continue;
            }
        };
        if md.len() > limits.max_file_bytes {
            skipped.push(skip(SkipReason::TooLarge));
            continue;
        }
        let entry = match hash_file(&f, &md, limits, size) {
            Ok(e) => e,
            Err(e) => {
                skipped.push(skip(SkipReason::Unreadable(e.to_string())));
                continue;
            }
        };
        total += entry.size as u64;
        chunk_count += entry.chunks.as_ref().map_or(0, |c| c.len() as u64);
        if total > cap {
            return Err(SavesError::local(tr!(
                "These saves are {size} MB, more than the {limit} MB one account may keep.",
                size = total / 1_000_000,
                limit = cap / 1_000_000
            )));
        }
        if entries.len() as u64 >= limits.max_files || chunk_count > limits.max_chunks_per_snapshot {
            return Err(SavesError::local(tr!("These saves have more files than the saves server accepts.")));
        }
        entries.push(entry);
        sources.push(f.full);
    }
    let manifest = Manifest {
        version: 1,
        enc: None,
        parent: parent.map(str::to_string),
        device: device.to_string(),
        label: label.to_string(),
        dirs: Vec::new(),
        files: entries,
    };
    Ok(Prepared { manifest, sources, total_bytes: total, skipped })
}

fn hash_file(f: &LocalFile, md: &fs::Metadata, limits: &Limits, chunk: u64) -> std::io::Result<FileEntry> {
    let mode = unix_mode(md);
    let mut file = fs::File::open(&f.full)?;
    let mut entry = FileEntry { path: f.path.clone(), size: 0, mode, mtime_ns: mtime_ns(md), chunks: None, inline: None };
    if md.len() <= limits.inline_max_bytes {
        let mut data = Vec::with_capacity(md.len() as usize);
        (&mut file).take(limits.inline_max_bytes + 1).read_to_end(&mut data)?;
        if data.len() as u64 > limits.inline_max_bytes {
            return Err(std::io::Error::other("the file grew while it was read"));
        }
        entry.size = data.len() as i64;
        entry.inline = Some(base64::engine::general_purpose::STANDARD.encode(&data));
        return Ok(entry);
    }
    let mut chunks = Vec::new();
    let mut size = 0u64;
    let mut buf = vec![0u8; chunk as usize];
    loop {
        let n = read_full(&mut file, &mut buf)?;
        if n == 0 {
            break;
        }
        chunks.push(ChunkRef { h: sha256_hex(&buf[..n]), n: n as u32 });
        size += n as u64;
        if n < buf.len() {
            break;
        }
    }
    if size > limits.max_file_bytes {
        return Err(std::io::Error::other("the file grew while it was read"));
    }
    entry.size = size as i64;
    entry.chunks = Some(chunks);
    Ok(entry)
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

#[cfg(unix)]
fn unix_mode(md: &fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    i64::from(md.mode() & 0o777)
}

#[cfg(not(unix))]
fn unix_mode(_: &fs::Metadata) -> i64 {
    0o644
}

/// Whether the bytes in `local` are the bytes `entry` describes. Files are
/// read in the chunk sizes of the backup, so nothing big is held in memory.
pub fn file_matches(local: &Path, entry: &FileEntry) -> bool {
    let Ok(md) = fs::metadata(local) else { return false };
    if !md.is_file() || md.len() != entry.size.max(0) as u64 {
        return false;
    }
    let Ok(mut f) = fs::File::open(local) else { return false };
    if let Some(inline) = &entry.inline {
        use base64::Engine;
        let Ok(want) = base64::engine::general_purpose::STANDARD.decode(inline) else { return false };
        let mut got = Vec::with_capacity(want.len());
        return f.read_to_end(&mut got).is_ok() && got == want;
    }
    for c in entry.chunks.iter().flatten() {
        let mut buf = vec![0u8; c.n as usize];
        if f.read_exact(&mut buf).is_err() || sha256_hex(&buf) != c.h {
            return false;
        }
    }
    let mut rest = [0u8; 1];
    matches!(f.read(&mut rest), Ok(0))
}

/// Whether two manifests hold the same bytes under the same paths (modes,
/// times, labels and parents do not count).
pub fn same_content(a: &Manifest, b: &Manifest) -> bool {
    a.files.len() == b.files.len()
        && a.files.iter().zip(&b.files).all(|(x, y)| x.path == y.path && x.size == y.size && x.chunks == y.chunks && x.inline == y.inline)
}

#[derive(Debug, Clone, Default)]
pub struct PushOutcome {
    /// The new head; `None` when the server already held these exact bytes.
    pub snapshot: Option<String>,
    pub files: usize,
    pub bytes: u64,
    /// Bytes the server did not hold yet.
    pub uploaded: u64,
    pub skipped: Vec<Skipped>,
}

/// Sends a prepared snapshot of `app`/`slot`: a snapshot identical to the head
/// is not sent, a head moved by another device is built upon (the old head
/// stays in the history), and what does not fit is refused before any chunk
/// is uploaded.
pub fn push(client: &Client, me: &Me, app: &str, slot: &str, mut prepared: Prepared, progress: &dyn Fn(u64, u64)) -> Result<PushOutcome> {
    let limit = match me.capacity() {
        0 => u64::MAX,
        c => c,
    };
    let mut outcome = PushOutcome {
        files: prepared.files(),
        bytes: prepared.total_bytes,
        skipped: std::mem::take(&mut prepared.skipped),
        ..Default::default()
    };
    let mut last_conflict = None;
    for _ in 0..3 {
        let head = client.slot(app, slot)?.and_then(|s| s.head).filter(|h| !h.is_empty());
        if let Some(h) = &head {
            if let Ok(old) = client.manifest(app, slot, h) {
                if same_content(&old, &prepared.manifest) {
                    return Ok(outcome);
                }
            }
        }
        prepared.manifest.parent.clone_from(&head);
        let begun = match client.begin(app, slot, head.as_deref(), &prepared.manifest) {
            Ok(b) => b,
            Err(e) if e.is_conflict() => {
                last_conflict = Some(e);
                continue;
            }
            Err(e) => return Err(e),
        };
        let missing: HashSet<&str> = begun.missing.iter().map(String::as_str).collect();
        let mut sizes: HashMap<&str, u64> = HashMap::new();
        for f in &prepared.manifest.files {
            for c in f.chunks.iter().flatten() {
                if missing.contains(c.h.as_str()) {
                    sizes.insert(&c.h, u64::from(c.n));
                }
            }
        }
        // The server counts a small file by the length of its base64 text.
        let inline: u64 = prepared.manifest.files.iter().filter_map(|f| f.inline.as_ref()).map(|i| i.len() as u64).sum();
        let upload: u64 = sizes.values().sum();
        outcome.uploaded = upload + inline;
        let held = me.held();
        if held + upload + inline > limit {
            let _ = client.delete_snapshot(app, slot, &begun.id);
            return Err(SavesError::local(tr!(
                "Not enough space on the saves server: {used} MB of {limit} MB are used and this backup needs {need} MB more.",
                used = held / 1_000_000,
                limit = limit / 1_000_000,
                need = (upload + inline) / 1_000_000
            )));
        }
        let mut sent: HashSet<&str> = HashSet::new();
        let mut done = 0u64;
        progress(0, upload);
        for (file, source) in prepared.manifest.files.iter().zip(&prepared.sources) {
            let Some(chunks) = &file.chunks else { continue };
            if !chunks.iter().any(|c| missing.contains(c.h.as_str()) && !sent.contains(c.h.as_str())) {
                continue;
            }
            let mut f = fs::File::open(source)?;
            let mut offset = 0u64;
            for c in chunks {
                if missing.contains(c.h.as_str()) && sent.insert(&c.h) {
                    f.seek(SeekFrom::Start(offset))?;
                    let mut buf = vec![0u8; c.n as usize];
                    f.read_exact(&mut buf).map_err(|_| changed(&file.path))?;
                    if sha256_hex(&buf) != c.h {
                        let _ = client.delete_snapshot(app, slot, &begun.id);
                        return Err(changed(&file.path));
                    }
                    client.put_chunk(app, slot, &begun.id, &c.h, &buf)?;
                    done += u64::from(c.n);
                    progress(done, upload);
                }
                offset += u64::from(c.n);
            }
        }
        match client.commit(app, slot, &begun.id, head.as_deref()) {
            Ok(id) => {
                outcome.snapshot = Some(id);
                return Ok(outcome);
            }
            Err(e) if e.is_conflict() => {
                last_conflict = Some(e);
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_conflict.unwrap_or_else(|| SavesError::local(tr!("Another device saved first."))))
}

pub(crate) fn changed(path: &str) -> SavesError {
    SavesError::local(tr!("{file} changed while it was being backed up. Nothing was saved, try again.", file = unescape_path(path)))
}

/// A snapshot path with each name unescaped, to show.
pub fn unescape_path(path: &str) -> String {
    path.split('/').map(unescape_name).collect::<Vec<_>>().join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Me::default().effective_limits()
    }

    #[test]
    fn names_round_trip_and_become_portable() {
        for name in
            ["save.dat", "a:b.sav", "100%.txt", "dir.", "trail ", "aux", "COM1.dat", "q?*", "über", "tab\tname", "emoji😀", "..", "."]
        {
            let e = escape_name(name);
            assert!(!e.contains([':', '?', '*', '<', '>', '|', '"', '\\', '/']), "{e}");
            assert!(!e.ends_with(['.', ' ']), "{e}");
            assert_eq!(unescape_name(&e), name, "{name}");
        }
        assert_eq!(escape_name("save.dat"), "save.dat");
        assert_eq!(escape_name("aux"), "%61ux");
        assert_eq!(escape_name("a:b"), "a%3Ab");
    }

    #[test]
    fn safe_paths() {
        assert!(is_safe_path("home/.config/game/save.dat"));
        for bad in ["", "/etc/passwd", "a//b", "a/../b", "./a", "a/./b", "a\\b", "C:/x", "a/b/", "a\0b"] {
            assert!(!is_safe_path(bad), "{bad}");
        }
    }

    fn write(dir: &std::path::Path, name: &str, bytes: &[u8]) -> LocalFile {
        let full = dir.join(name);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, bytes).unwrap();
        LocalFile { path: name.to_string(), full }
    }

    #[test]
    fn small_files_go_inline_and_big_ones_in_hashed_chunks() {
        let d = tempfile::tempdir().unwrap();
        let big: Vec<u8> = (0..(3 << 20) + 17).map(|i| (i % 251) as u8).collect();
        let files = vec![write(d.path(), "b/small.dat", b"hello"), write(d.path(), "a/big.bin", &big)];
        let p = prepare(files, &limits(), "pc", "x", None).unwrap();
        assert_eq!(p.manifest.files[0].path, "a/big.bin");
        assert_eq!(p.manifest.files[1].path, "b/small.dat");
        let chunks = p.manifest.files[0].chunks.as_ref().unwrap();
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks.iter().map(|c| u64::from(c.n)).sum::<u64>(), big.len() as u64);
        assert_eq!(chunks[0].h, sha256_hex(&big[..1 << 20]));
        assert_eq!(p.manifest.files[1].inline.as_deref(), Some("aGVsbG8="));
        assert_eq!(p.total_bytes, big.len() as u64 + 5);
    }

    #[test]
    fn skips_what_cannot_go_and_keeps_the_rest() {
        let d = tempfile::tempdir().unwrap();
        let files = vec![
            write(d.path(), "Save.dat", b"1"),
            write(d.path(), "save.dat", b"2"),
            LocalFile { path: "gone.dat".into(), full: d.path().join("gone.dat") },
            LocalFile { path: format!("{}/x", "n".repeat(300)), full: d.path().join("Save.dat") },
        ];
        let p = prepare(files, &limits(), "pc", "", None).unwrap();
        assert_eq!(p.files(), 1);
        let reasons: Vec<_> = p.skipped.iter().map(|s| s.reason.clone()).collect();
        assert!(reasons.contains(&SkipReason::CaseCollision));
        assert!(reasons.iter().any(|r| matches!(r, SkipReason::Unreadable(_))));
        assert!(reasons.contains(&SkipReason::PathTooLong));
    }

    #[test]
    fn refuses_a_snapshot_over_the_limits() {
        let d = tempfile::tempdir().unwrap();
        let files = vec![write(d.path(), "a.bin", &vec![1u8; 200_000])];
        let small = Limits { max_snapshot_bytes: 100_000, ..limits() };
        assert!(prepare(files, &small, "pc", "", None).is_err());
        let files = vec![write(d.path(), "a.bin", &vec![1u8; 200_000]), write(d.path(), "b.bin", &vec![2u8; 200_000])];
        let few = Limits { max_files: 1, ..limits() };
        assert!(prepare(files, &few, "pc", "", None).is_err());
        let files = vec![write(d.path(), "a.bin", &vec![1u8; 200_000])];
        let huge_file = Limits { max_file_bytes: 1000, ..limits() };
        let p = prepare(files, &huge_file, "pc", "", None).unwrap();
        assert_eq!(p.skipped[0].reason, SkipReason::TooLarge);
        assert_eq!(p.files(), 0);
    }

    #[test]
    fn same_content_ignores_times_and_modes() {
        let d = tempfile::tempdir().unwrap();
        let a = prepare(vec![write(d.path(), "s.dat", b"one")], &limits(), "pc", "l1", None).unwrap().manifest;
        let mut b = a.clone();
        b.files[0].mtime_ns += 5;
        b.files[0].mode = 0o600;
        b.label = "l2".into();
        assert!(same_content(&a, &b));
        let c = prepare(vec![write(d.path(), "s.dat", b"two")], &limits(), "pc", "", None).unwrap().manifest;
        assert!(!same_content(&a, &c));
    }
}
