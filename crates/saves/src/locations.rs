//! Where a game's saves are on this PC, and where they go back to.
//!
//! A manifest path starts with a placeholder (`<winAppData>/Game/Profile*.sav`,
//! `<base>/saves`). The snapshot keeps the placeholder as the first name of
//! each path (`winAppData/Game/Profile1.sav`) and never a folder of this PC,
//! so a backup made under Wine on Linux restores into the real AppData of a
//! Windows PC and the other way round, into whichever prefix, Steam library or
//! user the game has on the PC that restores.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::manifest::{Game, Rule};
use crate::snapshot::{escape_name, is_safe_path, unescape_name, LocalFile, USER_ID};

/// Most files one game may list, and entries a walk looks at, so a rule that
/// names a huge folder (the whole install, the home folder) cannot run away.
pub const MAX_FILES: usize = 50_000;
const MAX_WALK: usize = 200_000;
/// How deep below its anchor a walk goes; the snapshot refuses deeper paths
/// anyway, and a link loop would otherwise recurse until the walk limit.
const MAX_DEPTH: usize = 64;

/// The folders of a Windows user, on a Windows PC.
#[derive(Debug, Clone, Default)]
pub struct WinDirs {
    pub profile: Option<PathBuf>,
    pub roaming: Option<PathBuf>,
    pub local: Option<PathBuf>,
    pub documents: Option<PathBuf>,
    pub public: Option<PathBuf>,
    pub program_data: Option<PathBuf>,
}

/// What the game and this PC give a rule to stand on.
#[derive(Debug, Clone, Default)]
pub struct Target {
    /// This is Windows: the game runs as it is, with no prefix.
    pub windows_host: bool,
    /// `steam`, `gog`, `epic` ... when known.
    pub store: Option<String>,
    pub install_dir: Option<PathBuf>,
    /// Folders of the store holding the game and its users' data.
    pub roots: Vec<PathBuf>,
    /// Wine prefix (the folder holding `drive_c`).
    pub prefix: Option<PathBuf>,
    pub wine_user: Option<String>,
    pub home: Option<PathBuf>,
    pub xdg_data: Option<PathBuf>,
    pub xdg_config: Option<PathBuf>,
    /// The user folders on Windows.
    pub win: WinDirs,
    /// The name of the user on this PC.
    pub login: String,
    /// The folder the user named for this game's saves, when the rules find
    /// none (`custom/` paths of a snapshot).
    pub custom: Option<PathBuf>,
}

impl Target {
    /// Whether Windows paths mean something here.
    pub fn windows_ok(&self) -> bool {
        self.windows_host || self.prefix.is_some()
    }

    fn os_ok(&self, os: &str) -> bool {
        match os {
            "windows" => self.windows_ok(),
            "linux" => !self.windows_host,
            _ => false,
        }
    }

    fn user(&self) -> &str {
        self.wine_user.as_deref().unwrap_or("steamuser")
    }

    fn windows_dirs(&self) -> WinDirs {
        if self.windows_host {
            return self.win.clone();
        }
        let Some(prefix) = &self.prefix else { return WinDirs::default() };
        let profile = prefix.join("drive_c/users").join(self.user());
        let documents =
            ["Documents", "My Documents"].iter().map(|d| profile.join(d)).find(|p| p.is_dir()).unwrap_or_else(|| profile.join("Documents"));
        WinDirs {
            roaming: Some(profile.join("AppData/Roaming")),
            local: Some(profile.join("AppData/Local")),
            documents: Some(documents),
            public: Some(prefix.join("drive_c/users/Public")),
            program_data: Some(prefix.join("drive_c/ProgramData")),
            profile: Some(profile),
        }
    }

    /// The folders a snapshot's first name stands for here.
    pub fn anchors(&self, key: &str) -> Vec<PathBuf> {
        let win = || self.windows_dirs();
        let one = |p: Option<PathBuf>| p.into_iter().collect::<Vec<_>>();
        match key {
            "base" => one(self.install_dir.clone()),
            "root" => self.roots.clone(),
            "home" if !self.windows_host => one(self.home.clone()),
            "xdgData" if !self.windows_host => one(self.xdg_data.clone()),
            "xdgConfig" if !self.windows_host => one(self.xdg_config.clone()),
            "winHome" => one(win().profile),
            "winAppData" => one(win().roaming),
            "winLocalAppData" => one(win().local),
            "winDocuments" => one(win().documents),
            "winPublic" => one(win().public),
            "winProgramData" => one(win().program_data),
            "winPrefix" if !self.windows_host => one(self.prefix.clone()),
            "custom" => one(self.custom.clone()),
            _ => Vec::new(),
        }
    }
}

/// The first name of a snapshot path for a placeholder of the manifest, as
/// seen from `os`.
fn key_for(placeholder: &str, os: &str) -> Option<&'static str> {
    Some(match (placeholder, os) {
        ("base" | "game", _) => "base",
        ("root", _) => "root",
        ("home", "windows") => "winHome",
        ("home", _) => "home",
        ("xdgData", _) => "xdgData",
        ("xdgConfig", _) => "xdgConfig",
        ("winAppData", _) => "winAppData",
        ("winLocalAppData", _) => "winLocalAppData",
        ("winDocuments", _) => "winDocuments",
        ("winPublic", _) => "winPublic",
        ("winProgramData", _) => "winProgramData",
        _ => return None,
    })
}

/// A save file found on this PC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Path in the snapshot.
    pub path: String,
    pub full: PathBuf,
}

impl From<Found> for LocalFile {
    fn from(f: Found) -> Self {
        LocalFile { path: f.path, full: f.full }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seg<'a> {
    Literal(&'a str),
    Glob(&'a str),
    UserId,
}

/// `<placeholder>rest` split into the placeholder name and the segments of
/// `rest`; `None` when the path does not start with a placeholder.
fn split_template(template: &str) -> Option<(&str, Vec<&str>)> {
    let t = template.strip_prefix('<')?;
    let (name, rest) = t.split_once('>')?;
    Some((name, rest.split('/').filter(|s| !s.is_empty()).collect()))
}

/// Literal names with `<osUserName>` filled in and the user-id placeholder
/// recognised; `None` for a segment holding a placeholder this cannot fill.
fn segment<'a>(raw: &'a str, user: &'a str) -> Option<(Seg<'a>, Option<String>)> {
    if raw == "<storeUserId>" {
        return Some((Seg::UserId, None));
    }
    if raw.contains('<') {
        let filled = raw.replace("<osUserName>", user);
        return (!filled.contains(['<', '>'])).then_some((Seg::Literal(""), Some(filled)));
    }
    Some((if raw.contains(['*', '?']) { Seg::Glob(raw) } else { Seg::Literal(raw) }, None))
}

fn child_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.symlink_metadata().is_ok() {
        return Some(direct);
    }
    let lower = name.to_lowercase();
    fs::read_dir(dir).ok()?.flatten().find(|e| e.file_name().to_string_lossy().to_lowercase() == lower).map(|e| e.path())
}

fn real_name(dir: &Path, name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    fs::read_dir(dir).ok()?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).find(|n| n.to_lowercase() == lower)
}

fn mtime(p: &Path) -> SystemTime {
    fs::metadata(p).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH)
}

struct Walk {
    seen: usize,
    out: Vec<(PathBuf, Vec<String>, Option<String>)>,
    /// Canonical folders already walked: a link back to one would walk it
    /// again, deeper each time.
    dirs: HashSet<PathBuf>,
}

/// Entries of `dir` sorted by name, links last so a real folder is walked
/// before a link to it (the real path is the one the snapshot keeps).
fn entries(dir: &Path) -> Vec<fs::DirEntry> {
    let mut entries: Vec<_> = fs::read_dir(dir).into_iter().flatten().flatten().collect();
    entries.sort_by_key(|e| (e.file_type().map(|t| t.is_symlink()).unwrap_or(false), e.file_name()));
    entries
}

/// Expands `segs` below `dir`: each entry is the file, its names relative to
/// the anchor and the account id the path went through.
fn expand(dir: &Path, rel: &[String], segs: &[(Seg, Option<String>)], ci: bool, uid: Option<String>, walk: &mut Walk) {
    if walk.seen > MAX_WALK || walk.out.len() >= MAX_FILES {
        return;
    }
    let Some(((seg, filled), rest)) = segs.split_first() else {
        collect(dir, rel, uid, walk);
        return;
    };
    let next = |path: PathBuf, name: String, uid: Option<String>, walk: &mut Walk| {
        let mut r = rel.to_vec();
        r.push(name);
        expand(&path, &r, rest, ci, uid, walk);
    };
    let literal = filled.clone().or_else(|| if let Seg::Literal(n) = seg { Some(n.to_string()) } else { None });
    if let Some(name) = literal {
        if let Some(p) = if ci { child_ci(dir, &name) } else { Some(dir.join(&name)).filter(|p| p.symlink_metadata().is_ok()) } {
            // A case-insensitive folder answers to any spelling: keep the real one.
            let real = if ci || cfg!(windows) { real_name(dir, &name) } else { None };
            let shown = real.or_else(|| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or(name);
            next(p, escape_name(&shown), uid, walk);
        }
        return;
    }
    match seg {
        Seg::Glob(pattern) => {
            let opts = glob::MatchOptions { case_sensitive: !ci, require_literal_separator: true, require_literal_leading_dot: false };
            let Ok(pat) = glob::Pattern::new(pattern) else { return };
            let names = entries(dir);
            for e in names {
                walk.seen += 1;
                let name = e.file_name().to_string_lossy().into_owned();
                if pat.matches_with(&name, opts) {
                    next(e.path(), escape_name(&name), uid.clone(), walk);
                }
            }
        }
        Seg::UserId => {
            for e in entries(dir) {
                walk.seen += 1;
                let name = e.file_name().to_string_lossy().into_owned();
                if e.path().is_dir() && name.bytes().all(|b| b.is_ascii_digit()) && name != "0" {
                    // The token stays bare: it is what `place` looks for.
                    next(e.path(), USER_ID.to_string(), Some(name), walk);
                }
            }
        }
        Seg::Literal(_) => {}
    }
}

/// A file, or every file below a folder. Links are followed (a save behind
/// one is still a save); a link that loops or is broken finds nothing, and a
/// folder is walked once however many links reach it.
fn collect(path: &Path, rel: &[String], uid: Option<String>, walk: &mut Walk) {
    walk.seen += 1;
    let Ok(md) = fs::metadata(path) else { return };
    if md.is_file() {
        walk.out.push((path.to_path_buf(), rel.to_vec(), uid));
    } else if md.is_dir() {
        if rel.len() >= MAX_DEPTH {
            return;
        }
        let canon = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if !walk.dirs.insert(canon) {
            return;
        }
        for e in entries(path) {
            if walk.seen > MAX_WALK || walk.out.len() >= MAX_FILES {
                return;
            }
            let mut r = rel.to_vec();
            r.push(escape_name(&e.file_name().to_string_lossy()));
            collect(&e.path(), &r, uid.clone(), walk);
        }
    }
}

/// The first name of the paths [`find_prefix`] gives.
pub const PREFIX_KEY: &str = "winPrefix";

/// Folders of a prefix that only hold caches and temporary files.
const PREFIX_SKIP: [&str; 8] = ["temp", "tmp", "cache", "gpucache", "shadercache", "dawncache", "crashpad", "crashdumps"];

fn walk_prefix(dir: &Path, rel: &mut Vec<String>, out: &mut Vec<Found>, seen: &mut usize, dirs: &mut HashSet<PathBuf>) {
    for e in entries(dir) {
        *seen += 1;
        if *seen > MAX_WALK || out.len() >= MAX_FILES || rel.len() >= MAX_DEPTH {
            return;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        // Followed, not inspected: a file behind a link is still a file, and
        // a folder is walked once however many links reach it.
        let Ok(md) = fs::metadata(e.path()) else { continue };
        rel.push(escape_name(&name));
        if md.is_file() {
            out.push(Found { path: format!("{PREFIX_KEY}/{}", rel.join("/")), full: e.path() });
        } else if md.is_dir() && !PREFIX_SKIP.contains(&name.to_lowercase().as_str()) {
            let canon = fs::canonicalize(e.path()).unwrap_or_else(|_| e.path());
            if dirs.insert(canon) {
                walk_prefix(&e.path(), rel, out, seen, dirs);
            }
        }
        rel.pop();
    }
}

/// The data of the game's Wine prefix worth keeping: its registry files and
/// the user folder (no Windows system files, no caches), under
/// [`PREFIX_KEY`]. Empty without a prefix, and on Windows.
pub fn find_prefix(target: &Target) -> Vec<Found> {
    let Some(prefix) = target.prefix.as_ref().filter(|_| !target.windows_host) else { return Vec::new() };
    let mut out = Vec::new();
    for reg in ["system.reg", "user.reg", "userdef.reg"] {
        if prefix.join(reg).is_file() {
            out.push(Found { path: format!("{PREFIX_KEY}/{reg}"), full: prefix.join(reg) });
        }
    }
    let users = prefix.join("drive_c/users");
    let user = users.join(target.user());
    let mut seen = 0;
    let mut dirs = HashSet::new();
    dirs.insert(fs::canonicalize(&user).unwrap_or_else(|_| user.clone()));
    walk_prefix(&user, &mut vec!["drive_c".into(), "users".into(), escape_name(target.user())], &mut out, &mut seen, &mut dirs);
    out
}

/// The first name of the paths [`find_custom`] gives.
pub const CUSTOM_KEY: &str = "custom";

/// The files below the folder the user named for this game
/// ([`Target::custom`]), as `custom/<path>`: files only, and a link is
/// followed only while it stays inside the folder. Empty when it is not set
/// or cannot be read.
pub fn find_custom(target: &Target) -> Vec<Found> {
    let Some(folder) = target.custom.as_ref() else { return Vec::new() };
    let Ok(root) = fs::canonicalize(folder) else { return Vec::new() };
    if !root.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut seen = 0;
    let mut dirs = HashSet::new();
    dirs.insert(root.clone());
    walk_custom(&root, &root, &mut Vec::new(), &mut out, &mut seen, &mut dirs);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// [`walk_prefix`] for [`find_custom`]: every entry is resolved first and a
/// link that leaves the folder finds nothing.
fn walk_custom(root: &Path, dir: &Path, rel: &mut Vec<String>, out: &mut Vec<Found>, seen: &mut usize, dirs: &mut HashSet<PathBuf>) {
    for e in entries(dir) {
        *seen += 1;
        if *seen > MAX_WALK || out.len() >= MAX_FILES || rel.len() >= MAX_DEPTH {
            return;
        }
        let Ok(canon) = fs::canonicalize(e.path()) else { continue };
        if !canon.starts_with(root) {
            continue;
        }
        let Ok(md) = fs::metadata(&canon) else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        rel.push(escape_name(&name));
        if md.is_file() {
            out.push(Found { path: format!("{CUSTOM_KEY}/{}", rel.join("/")), full: canon });
        } else if md.is_dir() && dirs.insert(canon.clone()) {
            walk_custom(root, &canon, rel, out, seen, dirs);
        }
        rel.pop();
    }
}

/// The save files of `game` on this PC, as the snapshot names them. Files
/// of several account folders keep the newest account only.
pub fn find(game: &Game, target: &Target) -> Vec<Found> {
    let mut by_path: HashMap<String, (Found, Option<String>)> = HashMap::new();
    for rule in &game.files {
        for os in rule.oses().into_iter().filter(|os| target.os_ok(os) && rule.applies(os, target.store.as_deref())) {
            find_rule(rule, os, target, &mut by_path);
        }
    }
    let mut newest: HashMap<&str, SystemTime> = HashMap::new();
    for (f, uid) in by_path.values() {
        if let Some(u) = uid {
            let t = newest.entry(u).or_insert(SystemTime::UNIX_EPOCH);
            *t = (*t).max(mtime(&f.full));
        }
    }
    let best = newest.iter().max_by_key(|(_, t)| **t).map(|(u, _)| u.to_string());
    let mut out: Vec<Found> = by_path.into_values().filter(|(_, uid)| uid.is_none() || *uid == best).map(|(f, _)| f).collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.truncate(MAX_FILES);
    out
}

fn find_rule(rule: &Rule, os: &str, target: &Target, out: &mut HashMap<String, (Found, Option<String>)>) {
    let Some((placeholder, raw)) = split_template(&rule.path) else { return };
    let Some(key) = key_for(placeholder, os) else { return };
    let user = if os == "windows" { target.user().to_string() } else { target.login.clone() };
    let Some(segs) = raw.iter().map(|s| segment(s, &user)).collect::<Option<Vec<_>>>() else { return };
    let ci = os == "windows" && !target.windows_host;
    for anchor in target.anchors(key) {
        let mut walk = Walk { seen: 0, out: Vec::new(), dirs: HashSet::new() };
        expand(&anchor, &[], &segs, ci, None, &mut walk);
        for (full, names, uid) in walk.out {
            // The names are final already: real ones escaped at the push, the
            // account token bare — a real file of the token's name cannot be
            // mistaken for it.
            let path = std::iter::once(key.to_string()).chain(names).collect::<Vec<_>>().join("/");
            // Two accounts name the same snapshot path: both are kept apart until the newest is picked.
            out.entry(format!("{}\0{path}", uid.as_deref().unwrap_or(""))).or_insert((Found { path, full }, uid));
        }
    }
}

/// A name of a snapshot path, unescaped, or `None` when it is not a plain
/// file name (a separator, `.` or `..` that the escaping hid).
pub fn plain_name(escaped: &str) -> Option<String> {
    let name = unescape_name(escaped);
    (!name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0']) && !(cfg!(windows) && name.contains(':')))
        .then_some(name)
}

/// Why a path of a snapshot cannot be put back on this PC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unplaced {
    /// The path is not one this client would ever have made.
    Unsafe,
    /// The folder it belongs in does not exist on this PC (no prefix yet, the
    /// game is not installed, no Windows side).
    NoFolder(String),
}

/// The file on this PC a path of a snapshot goes to.
pub fn place(path: &str, target: &Target) -> Result<PathBuf, Unplaced> {
    if !is_safe_path(path) && !path.contains(USER_ID) {
        return Err(Unplaced::Unsafe);
    }
    let mut names = path.split('/');
    let key = names.next().ok_or(Unplaced::Unsafe)?;
    let rest: Vec<&str> = names.collect();
    if rest.is_empty() || rest.iter().any(|n| n.is_empty() || *n == "." || *n == "..") {
        return Err(Unplaced::Unsafe);
    }
    let anchors = target.anchors(key);
    if anchors.is_empty() {
        return Err(Unplaced::NoFolder(key.to_string()));
    }
    // The token is matched on the escaped name only: an escaped real file of
    // the token's name unescapes to the same text and must stay a file.
    let mut converted: Vec<String> = Vec::with_capacity(rest.len());
    let mut tokens: Vec<bool> = Vec::with_capacity(rest.len());
    for n in &rest {
        if *n == USER_ID {
            converted.push(USER_ID.to_string());
            tokens.push(true);
        } else {
            converted.push(plain_name(n).ok_or(Unplaced::Unsafe)?);
            tokens.push(false);
        }
    }
    // The anchor that already holds the first folder of the path, else the first.
    let anchor = anchors
        .iter()
        .find(|a| {
            converted
                .first()
                .zip(tokens.first())
                .is_some_and(|(f, tok)| !tok && child_ci(a, f).is_some() || *tok && a.join("userdata").is_dir())
        })
        .unwrap_or(&anchors[0]);
    // A prefix backed up under another Wine user goes into this PC's user folder.
    if key == PREFIX_KEY && converted.len() > 2 && converted[0] == "drive_c" && converted[1] == "users" && converted[2] != "Public" {
        converted[2] = target.user().to_string();
    }
    let mut full = anchor.clone();
    for (i, name) in converted.iter().enumerate() {
        if tokens[i] {
            let Some(id) = newest_account(&full) else { return Err(Unplaced::NoFolder(full.display().to_string())) };
            full.push(id);
        } else if i + 1 < converted.len() && target.windows_ok() && !target.windows_host {
            full = child_ci(&full, name).unwrap_or_else(|| full.join(name));
        } else {
            full.push(name);
        }
    }
    if !full.starts_with(anchor) {
        return Err(Unplaced::Unsafe);
    }
    if link_escapes(&full, anchor) {
        return Err(Unplaced::Unsafe);
    }
    Ok(full)
}

/// Whether writing `full` can leave `anchor` through a link on the way: the
/// deepest folder of `full` that exists, resolved, must sit inside the
/// anchor resolved. Folders not there yet are made as real folders, and the
/// file itself is not checked: a rename replaces a link of its name.
fn link_escapes(full: &Path, anchor: &Path) -> bool {
    let Ok(real_anchor) = fs::canonicalize(anchor) else { return false };
    for dir in full.ancestors().skip(1) {
        if !dir.starts_with(anchor) {
            break;
        }
        if fs::symlink_metadata(dir).is_ok() {
            return fs::canonicalize(dir).is_ok_and(|real| !real.starts_with(&real_anchor));
        }
    }
    false
}

/// The newest change of a file in `path` or below it (a few levels, so a big
/// folder does not cost much). Folders' own times are left out: they only say
/// when an entry was added.
fn tree_mtime(path: &Path, depth: u32) -> SystemTime {
    let Ok(md) = fs::symlink_metadata(path) else { return SystemTime::UNIX_EPOCH };
    if md.is_file() {
        return md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    }
    if depth == 0 || !md.is_dir() {
        return SystemTime::UNIX_EPOCH;
    }
    fs::read_dir(path).into_iter().flatten().flatten().map(|e| tree_mtime(&e.path(), depth - 1)).max().unwrap_or(SystemTime::UNIX_EPOCH)
}

/// The numeric folder of `dir` changed last (the account that plays here).
fn newest_account(dir: &Path) -> Option<String> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.is_empty() && n != "0" && n.bytes().all(|b| b.is_ascii_digit()))
        .max_by_key(|n| tree_mtime(&dir.join(n), 5))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{parse, When};

    fn game(yaml: &str) -> Game {
        parse(yaml.as_bytes()).unwrap().remove(0)
    }

    fn put(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn linux_target(d: &Path) -> Target {
        Target {
            home: Some(d.join("home")),
            xdg_data: Some(d.join("home/.local/share")),
            xdg_config: Some(d.join("home/.config")),
            login: "me".into(),
            ..Default::default()
        }
    }

    #[test]
    fn native_linux_saves_are_named_by_placeholder() {
        let d = tempfile::tempdir().unwrap();
        put(&d.path().join("home/.local/share/Game/slot1.sav"), "1");
        put(&d.path().join("home/.local/share/Game/deep/slot2.sav"), "2");
        put(&d.path().join("install/data/a.cfg"), "c");
        let g = game(
            r#"
G:
  files:
    "<xdgData>/Game":
      tags: [save]
      when: [{os: linux}]
    "<base>/data/*.cfg":
      tags: [config]
    "<winAppData>/Game":
      when: [{os: windows}]
"#,
        );
        let mut t = linux_target(d.path());
        t.install_dir = Some(d.path().join("install"));
        let found = find(&g, &t);
        let paths: Vec<_> = found.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["base/data/a.cfg", "xdgData/Game/deep/slot2.sav", "xdgData/Game/slot1.sav"]);
    }

    #[test]
    fn windows_rules_reach_into_the_prefix_and_restore_into_another() {
        let d = tempfile::tempdir().unwrap();
        let p1 = d.path().join("pfx1");
        put(&p1.join("drive_c/users/steamuser/AppData/Roaming/Studio/Game/Profile1.sav"), "p1");
        put(&p1.join("drive_c/users/steamuser/AppData/Roaming/Studio/Game/Other.txt"), "x");
        let g = game(
            r#"
G:
  files:
    "<winAppData>/studio/game/Profile*.sav":
      tags: [save]
      when: [{os: windows}]
"#,
        );
        let mut t = linux_target(d.path());
        t.prefix = Some(p1.clone());
        t.wine_user = Some("steamuser".into());
        let found = find(&g, &t);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, "winAppData/Studio/Game/Profile1.sav");
        assert_eq!(fs::read_to_string(&found[0].full).unwrap(), "p1");

        let p2 = d.path().join("pfx2");
        let mut t2 = linux_target(d.path());
        t2.prefix = Some(p2.clone());
        t2.wine_user = Some("other".into());
        assert_eq!(place(&found[0].path, &t2).unwrap(), p2.join("drive_c/users/other/AppData/Roaming/Studio/Game/Profile1.sav"));
        assert!(matches!(place("winAppData/x", &linux_target(d.path())), Err(Unplaced::NoFolder(_))));
    }

    #[test]
    fn windows_host_uses_its_known_folders() {
        let d = tempfile::tempdir().unwrap();
        let roaming = d.path().join("AppData/Roaming");
        put(&roaming.join("Game/save.dat"), "w");
        put(&d.path().join("Documents/My Games/G/save.dat"), "d");
        let g = game(
            r#"
G:
  files:
    "<winAppData>/Game": {}
    "<home>/Documents/My Games/G": {when: [{os: windows}]}
    "<xdgData>/G": {when: [{os: linux}]}
"#,
        );
        let t = Target {
            windows_host: true,
            win: WinDirs { profile: Some(d.path().to_path_buf()), roaming: Some(roaming), ..Default::default() },
            ..Default::default()
        };
        let paths: Vec<_> = find(&g, &t).into_iter().map(|f| f.path).collect();
        assert_eq!(paths, ["winAppData/Game/save.dat", "winHome/Documents/My Games/G/save.dat"]);
    }

    #[test]
    fn steam_account_folder_is_a_token_and_restores_to_the_newest_account() {
        let d = tempfile::tempdir().unwrap();
        let steam = d.path().join("steam");
        put(&steam.join("userdata/1001/42/remote/save.bin"), "old");
        put(&steam.join("userdata/2002/42/remote/save.bin"), "new");
        // File systems stamp times coarsely: set them, so "newest" is not a race.
        for (acct, secs) in [("userdata/1001", 1_000), ("userdata/2002", 2_000)] {
            let f = fs::File::options().write(true).open(steam.join(acct).join("42/remote/save.bin")).unwrap();
            f.set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)).unwrap();
        }
        let g = game("G:\n  files:\n    \"<root>/userdata/<storeUserId>/42/remote\": {}\n");
        let mut t = linux_target(d.path());
        t.roots = vec![steam.clone()];
        let found = find(&g, &t);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "root/userdata/~storeUserId/42/remote/save.bin");
        assert_eq!(fs::read_to_string(&found[0].full).unwrap(), "new");
        assert_eq!(place(&found[0].path, &t).unwrap(), steam.join("userdata/2002/42/remote/save.bin"));
    }

    #[test]
    fn restore_never_leaves_its_folder() {
        let d = tempfile::tempdir().unwrap();
        let mut t = linux_target(d.path());
        t.install_dir = Some(d.path().join("install"));
        for bad in
            ["base/../../etc/passwd", "base/%2E%2E/x", "base/a%2F..%2Fb", "/etc/passwd", "base//x", "base", "base/a%5Cb", "base/x%00y"]
        {
            assert!(place(bad, &t).is_err(), "{bad}");
        }
        #[cfg(unix)]
        assert_eq!(place("base/sub/a%3Ab.sav", &t).unwrap(), d.path().join("install/sub/a:b.sav"));
        #[cfg(windows)]
        assert!(place("base/sub/a%3Ab.sav", &t).is_err(), "a colon is never part of a Windows name");
    }

    #[test]
    fn rules_for_the_other_os_or_store_are_left_out() {
        let d = tempfile::tempdir().unwrap();
        put(&d.path().join("home/.local/share/G/a"), "a");
        let g = Game {
            title: "G".into(),
            files: vec![Rule {
                path: "<xdgData>/G".into(),
                tags: String::new(),
                when: vec![When { os: Some("linux".into()), store: Some("gog".into()) }],
            }],
            ..Default::default()
        };
        let mut t = linux_target(d.path());
        t.store = Some("steam".into());
        assert!(find(&g, &t).is_empty());
        t.store = Some("gog".into());
        assert_eq!(find(&g, &t).len(), 1);
        t.store = None;
        assert_eq!(find(&g, &t).len(), 1);
    }

    #[test]
    fn the_prefix_keeps_registry_and_user_data_and_restores_under_another_user() {
        let d = tempfile::tempdir().unwrap();
        let p1 = d.path().join("pfx1");
        put(&p1.join("user.reg"), "reg");
        put(&p1.join("drive_c/windows/system32/big.dll"), "no");
        put(&p1.join("drive_c/users/steamuser/Documents/My Games/G/a.cfg"), "cfg");
        put(&p1.join("drive_c/users/steamuser/AppData/Local/Temp/junk.tmp"), "no");
        put(&p1.join("drive_c/users/steamuser/AppData/Local/G/ShaderCache/x"), "no");
        let mut t = linux_target(d.path());
        t.prefix = Some(p1);
        t.wine_user = Some("steamuser".into());
        let paths: Vec<_> = find_prefix(&t).into_iter().map(|f| f.path).collect();
        assert_eq!(paths, ["winPrefix/user.reg", "winPrefix/drive_c/users/steamuser/Documents/My Games/G/a.cfg"]);
        let mut other = linux_target(d.path());
        other.prefix = Some(d.path().join("pfx2"));
        other.wine_user = Some("me".into());
        assert_eq!(place(&paths[1], &other).unwrap(), d.path().join("pfx2/drive_c/users/me/Documents/My Games/G/a.cfg"));
        assert!(find_prefix(&linux_target(d.path())).is_empty(), "no prefix, nothing");
    }

    #[cfg(unix)]
    #[test]
    fn odd_names_are_escaped_and_come_back() {
        let d = tempfile::tempdir().unwrap();
        put(&d.path().join("install/slot: 1.sav"), "1");
        let g = game("G:\n  files:\n    \"<base>\": {}\n");
        let mut t = linux_target(d.path());
        t.install_dir = Some(d.path().join("install"));
        let found = find(&g, &t);
        assert_eq!(found[0].path, "base/slot%3A 1.sav");
        assert_eq!(place(&found[0].path, &t).unwrap(), d.path().join("install/slot: 1.sav"));
    }

    #[test]
    fn the_account_token_and_a_real_folder_of_its_name_stay_apart() {
        let d = tempfile::tempdir().unwrap();
        put(&d.path().join("root/userdata/~storeUserId/config.sav"), "mine");
        put(&d.path().join("root/userdata/123456/config.sav"), "theirs");
        let g = game(
            r#"
G:
  files:
    "<root>/userdata/<storeUserId>/config.sav": {}
    "<root>/userdata/~storeUserId/config.sav": {}
"#,
        );
        let t = Target { roots: vec![d.path().join("root")], ..Default::default() };
        let mut paths: Vec<_> = find(&g, &t).into_iter().map(|f| f.path).collect();
        paths.sort();
        assert_eq!(paths, ["root/userdata/%7EstoreUserId/config.sav", "root/userdata/~storeUserId/config.sav"]);
        // Each goes back where it came from: the token to the account, the
        // escaped name to the real folder of that name.
        assert_eq!(place("root/userdata/~storeUserId/config.sav", &t).unwrap(), d.path().join("root/userdata/123456/config.sav"));
        assert_eq!(place("root/userdata/%7EstoreUserId/config.sav", &t).unwrap(), d.path().join("root/userdata/~storeUserId/config.sav"));
    }

    #[cfg(unix)]
    #[test]
    fn the_named_folder_is_found_and_restored_and_links_cannot_leave_it() {
        let d = tempfile::tempdir().unwrap();
        let saves = d.path().join("my saves");
        put(&saves.join("slot1.sav"), "1");
        put(&saves.join("deep/slot: 2.sav"), "2");
        put(&saves.join("inner/real.sav"), "r");
        // Links that leave the folder find nothing; links inside are followed
        // (a file under its link's name, a folder once under its real name).
        let outside = d.path().join("outside");
        put(&outside.join("secret.sav"), "x");
        std::os::unix::fs::symlink(&outside, saves.join("escape")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.sav"), saves.join("linked.sav")).unwrap();
        std::os::unix::fs::symlink(saves.join("slot1.sav"), saves.join("linked-inside.sav")).unwrap();
        std::os::unix::fs::symlink(saves.join("inner"), saves.join("alias")).unwrap();
        std::os::unix::fs::symlink(&saves, saves.join("loop")).unwrap();
        let mut t = linux_target(d.path());
        t.custom = Some(saves.clone());
        let found = find_custom(&t);
        let paths: Vec<_> = found.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            ["custom/deep/slot%3A 2.sav", "custom/inner/real.sav", "custom/linked-inside.sav", "custom/slot1.sav"],
            "{paths:?}"
        );
        // Every path goes back into the folder, escaped names included.
        for f in &found {
            let placed = place(&f.path, &t).unwrap();
            assert_eq!(fs::canonicalize(&placed).unwrap(), f.full, "{}", f.path);
        }
        assert_eq!(place("custom/slot1.sav", &t).unwrap(), saves.join("slot1.sav"));
        assert_eq!(place("custom/deep/slot%3A 2.sav", &t).unwrap(), saves.join("deep/slot: 2.sav"));
        for bad in ["custom/../x", "custom/%2E%2E/x", "custom/a%2F..%2Fb", "custom//x", "custom"] {
            assert!(place(bad, &t).is_err(), "{bad}");
        }
        // No folder named: nothing found, and its paths have no place.
        let mut none = linux_target(d.path());
        none.custom = None;
        assert!(find_custom(&none).is_empty());
        assert!(matches!(place("custom/slot1.sav", &none), Err(Unplaced::NoFolder(_))));
        none.custom = Some(d.path().join("missing"));
        assert!(find_custom(&none).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_inside_the_folder_cannot_carry_a_restore_out_of_it() {
        let d = tempfile::tempdir().unwrap();
        let saves = d.path().join("saves");
        put(&saves.join("real.sav"), "r");
        let outside = d.path().join("outside");
        put(&outside.join("victim.sav"), "v");
        std::os::unix::fs::symlink(&outside, saves.join("link")).unwrap();
        let mut t = linux_target(d.path());
        t.custom = Some(saves.clone());
        assert!(place("custom/link/victim.sav", &t).is_err());
        assert!(place("custom/link/deep/victim.sav", &t).is_err());
        // The link's own name is replaced by the file, not written through.
        assert_eq!(place("custom/link", &t).unwrap(), saves.join("link"));
        assert_eq!(place("custom/real.sav", &t).unwrap(), saves.join("real.sav"));
        // A folder not there yet cannot hold a link out.
        assert_eq!(place("custom/new/deep.sav", &t).unwrap(), saves.join("new/deep.sav"));
    }

    #[cfg(unix)]
    #[test]
    fn saves_behind_links_are_found_and_a_loop_ends() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real");
        put(&real.join("deep/s1.sav"), "1");
        // The save folder itself is a link, a save file is a link, and a
        // folder inside links back to the top.
        std::os::unix::fs::symlink(&real, d.path().join("saves")).unwrap();
        std::os::unix::fs::symlink(real.join("deep/s1.sav"), real.join("linked.sav")).unwrap();
        std::os::unix::fs::symlink(d.path(), real.join("loop")).unwrap();
        let g = game("G:\n  files:\n    \"<xdgData>/saves\": {}\n");
        let mut t = linux_target(d.path());
        t.xdg_data = Some(d.path().to_path_buf());
        let found = find(&g, &t);
        let paths: Vec<_> = found.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"xdgData/saves/deep/s1.sav"), "{paths:?}");
        assert!(paths.contains(&"xdgData/saves/linked.sav"), "{paths:?}");
        assert!(paths.iter().all(|p| !p.contains("loop")), "a link loop does not recurse: {paths:?}");
    }
}
