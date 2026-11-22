//! The list of games and where they keep their saves: the community
//! maintained Ludusavi manifest (MIT licensed), downloaded on demand and
//! never bundled. Downloading checks the size and keeps the old copy when
//! anything goes wrong. The 17 MB YAML is read once into a compact JSON
//! index that later runs load.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Result, SavesError};

pub const MANIFEST_URL: &str = "https://raw.githubusercontent.com/mtkennerly/ludusavi-manifest/master/data/manifest.yaml";

/// The YAML is about 17 MB; a body past this is not it.
const MAX_DOWNLOAD: u64 = 120 << 20;
/// Smallest YAML that can be the manifest (an error page is smaller).
const MIN_DOWNLOAD: usize = 1 << 20;

const YAML_FILE: &str = "manifest.yaml";
const ETAG_FILE: &str = "manifest.etag";
const INDEX_FILE: &str = "manifest-index.json";
const INDEX_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct When {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rule {
    /// With `<placeholders>`, as the manifest writes it.
    pub path: String,
    /// The tags `save` and `config` of the manifest, as `s` and `c`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tags: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when: Vec<When>,
}

impl Rule {
    /// Whether the rule is for `os` (`windows`, `linux`, `mac`) on `store`
    /// (`None` when the store is not known, which matches any).
    pub fn applies(&self, os: &str, store: Option<&str>) -> bool {
        self.when.is_empty()
            || self.when.iter().any(|w| {
                w.os.as_deref().is_none_or(|o| o == os) && (store.is_none() || w.store.as_deref().is_none_or(|s| Some(s) == store))
            })
    }

    /// Whether the rule can name a file for either OS the game could be
    /// running as (Windows, or Linux).
    pub fn oses(&self) -> Vec<&'static str> {
        ["windows", "linux"].into_iter().filter(|os| self.applies(os, None)).collect()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Game {
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steam: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gog: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<Rule>,
    /// This entry only points at another one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    etag: String,
    games: Vec<Game>,
}

/// The games, with lookups by store id and by name.
#[derive(Debug, Default)]
pub struct Index {
    pub games: Vec<Game>,
    pub etag: String,
    by_steam: HashMap<String, usize>,
    by_gog: HashMap<String, usize>,
    by_name: HashMap<String, usize>,
}

/// A title as the lookups compare it: letters and digits, lowercase.
pub fn normalize(title: &str) -> String {
    title.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

impl Index {
    fn build(games: Vec<Game>, etag: String) -> Self {
        let mut ix = Index { etag, ..Default::default() };
        for (i, g) in games.iter().enumerate() {
            for id in &g.steam {
                ix.by_steam.entry(id.clone()).or_insert(i);
            }
            for id in &g.gog {
                ix.by_gog.entry(id.clone()).or_insert(i);
            }
            ix.by_name.entry(normalize(&g.title)).or_insert(i);
        }
        ix.games = games;
        ix
    }

    /// The game a store id or a name stands for: the Steam id, then the GOG
    /// id, then each name in order. An alias leads to the game it names.
    pub fn find(&self, steam: Option<&str>, gog: Option<&str>, names: &[String]) -> Option<&Game> {
        let hit = steam
            .and_then(|id| self.by_steam.get(id))
            .or_else(|| gog.and_then(|id| self.by_gog.get(id)))
            .or_else(|| names.iter().find_map(|n| self.by_name.get(&normalize(n))))?;
        let mut game = &self.games[*hit];
        for _ in 0..4 {
            match game.alias.as_deref().and_then(|a| self.by_name.get(&normalize(a))) {
                Some(&i) => game = &self.games[i],
                None => break,
            }
        }
        (!game.files.is_empty()).then_some(game)
    }

    pub fn len(&self) -> usize {
        self.games.len()
    }

    pub fn is_empty(&self) -> bool {
        self.games.is_empty()
    }
}

#[derive(Deserialize, Default)]
struct RawId {
    id: Option<serde_json::Value>,
}

#[derive(Deserialize, Default)]
struct RawExtra {
    #[serde(default, rename = "steamExtra")]
    steam_extra: Vec<serde_json::Value>,
    #[serde(default, rename = "gogExtra")]
    gog_extra: Vec<serde_json::Value>,
}

#[derive(Deserialize, Default)]
struct RawWhen {
    os: Option<String>,
    store: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawFile {
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    when: Vec<RawWhen>,
}

#[derive(Deserialize, Default)]
struct RawGame {
    #[serde(default)]
    files: BTreeMap<String, RawFile>,
    #[serde(default)]
    steam: Option<RawId>,
    #[serde(default)]
    gog: Option<RawId>,
    #[serde(default)]
    id: Option<RawExtra>,
    #[serde(default)]
    alias: Option<String>,
}

fn scalar(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Reads a manifest. Games without file rules (registry only, or no known
/// saves) are left out, since there is nothing to find for them.
pub fn parse(yaml: impl Read) -> Result<Vec<Game>> {
    // The default budget stops at 250,000 nodes; the manifest has about two million.
    let mut budget = serde_saphyr::Budget::default();
    budget.max_events = 40_000_000;
    budget.max_nodes = 10_000_000;
    budget.max_total_scalar_bytes = MAX_DOWNLOAD as usize;
    budget.max_reader_input_bytes = Some(MAX_DOWNLOAD as usize);
    let mut options = serde_saphyr::Options::default();
    options.budget = Some(budget);
    let raw: BTreeMap<String, RawGame> =
        serde_saphyr::from_reader_with_options(yaml, options).map_err(|e| SavesError::local(format!("the game list is not valid: {e}")))?;
    let mut games = Vec::new();
    for (title, g) in raw {
        if g.files.is_empty() && g.alias.is_none() {
            continue;
        }
        let mut steam: Vec<String> = g.steam.and_then(|s| s.id).and_then(|v| scalar(&v)).into_iter().collect();
        let mut gog: Vec<String> = g.gog.and_then(|s| s.id).and_then(|v| scalar(&v)).into_iter().collect();
        if let Some(extra) = g.id {
            steam.extend(extra.steam_extra.iter().filter_map(scalar));
            gog.extend(extra.gog_extra.iter().filter_map(scalar));
        }
        let files = g
            .files
            .into_iter()
            .map(|(path, f)| Rule {
                path,
                tags: [("save", 's'), ("config", 'c')].iter().filter(|(t, _)| f.tags.iter().any(|x| x == t)).map(|(_, c)| *c).collect(),
                when: f.when.into_iter().map(|w| When { os: w.os, store: w.store }).collect(),
            })
            .collect();
        games.push(Game { title, steam, gog, files, alias: g.alias });
    }
    Ok(games)
}

/// Where the list lives, under the app's data.
pub fn dir(data: &Path) -> PathBuf {
    data.join("saves-manifest")
}

fn load_index(folder: &Path) -> Option<Index> {
    let stored: Stored = serde_json::from_slice(&fs::read(folder.join(INDEX_FILE)).ok()?).ok()?;
    (stored.version == INDEX_VERSION).then(|| Index::build(stored.games, stored.etag))
}

fn memory() -> &'static Mutex<Option<(PathBuf, std::time::SystemTime, Arc<Index>)>> {
    static M: OnceLock<Mutex<Option<(PathBuf, std::time::SystemTime, Arc<Index>)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(None))
}

/// The game list, `None` until it was downloaded once. Kept in memory while
/// the file does not change.
pub fn load(folder: &Path) -> Option<Arc<Index>> {
    let stamp = fs::metadata(folder.join(INDEX_FILE)).and_then(|m| m.modified()).ok()?;
    let mut cache = memory().lock().unwrap_or_else(|e| e.into_inner());
    if let Some((p, t, ix)) = cache.as_ref() {
        if p == folder && *t == stamp {
            return Some(ix.clone());
        }
    }
    let ix = Arc::new(load_index(folder)?);
    *cache = Some((folder.to_path_buf(), stamp, ix.clone()));
    Some(ix)
}

/// How old the list is (`None` when there is none).
pub fn age(folder: &Path) -> Option<Duration> {
    fs::metadata(folder.join(INDEX_FILE)).and_then(|m| m.modified()).ok()?.elapsed().ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Updated {
    Downloaded(usize),
    UpToDate,
}

/// Downloads the list when it changed (ETag), checks it reads as a manifest
/// and replaces the old one only then.
pub fn update(folder: &Path, url: &str) -> Result<Updated> {
    fs::create_dir_all(folder)?;
    let etag = fs::read_to_string(folder.join(ETAG_FILE)).ok().filter(|_| folder.join(INDEX_FILE).is_file());
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .http_status_as_error(false)
        .user_agent(concat!("Heronited/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut req = agent.get(url);
    if let Some(e) = &etag {
        req = req.header("If-None-Match", e.trim());
    }
    let mut resp = req.call().map_err(|e| SavesError::Network(e.to_string()))?;
    match resp.status().as_u16() {
        304 => {
            touch(&folder.join(INDEX_FILE));
            return Ok(Updated::UpToDate);
        }
        200 => {}
        s => return Err(SavesError::local(heronite_i18n::tr!("The game list server answered {status}.", status = s))),
    }
    let new_etag = resp.headers().get("etag").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    let body = resp.body_mut().with_config().limit(MAX_DOWNLOAD).read_to_vec().map_err(|e| SavesError::Network(e.to_string()))?;
    if body.len() < MIN_DOWNLOAD {
        return Err(SavesError::local(heronite_i18n::tr!("The game list download is incomplete.")));
    }
    let games = parse(body.as_slice())?;
    if games.len() < 1000 {
        return Err(SavesError::local(heronite_i18n::tr!("The game list download is incomplete.")));
    }
    let count = games.len();
    let stored = serde_json::to_vec(&Stored { version: INDEX_VERSION, etag: new_etag.clone(), games })
        .map_err(|e| SavesError::local(e.to_string()))?;
    write_atomic(&folder.join(YAML_FILE), &body)?;
    write_atomic(&folder.join(ETAG_FILE), new_etag.as_bytes())?;
    write_atomic(&folder.join(INDEX_FILE), &stored)?;
    Ok(Updated::Downloaded(count))
}

fn touch(path: &Path) {
    if let Ok(f) = fs::OpenOptions::new().append(true).open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// Keeps `games` as the game list of `folder` (the same as a download does).
pub fn store(folder: &Path, games: Vec<Game>, etag: &str) -> Result<()> {
    fs::create_dir_all(folder)?;
    let stored = serde_json::to_vec(&Stored { version: INDEX_VERSION, etag: etag.to_string(), games })
        .map_err(|e| SavesError::local(e.to_string()))?;
    write_atomic(&folder.join(INDEX_FILE), &stored)
}

/// Builds an index from games (tests, and callers with their own list).
pub fn index_of(games: Vec<Game>) -> Index {
    Index::build(games, String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    const YAML: &str = r#"
"Hades":
  files:
    "<winAppData>/Hades/Profile*.sav":
      tags: [save]
      when:
        - os: windows
    "<home>/.local/share/Hades":
      tags: [save, config]
      when:
        - os: linux
          store: steam
  steam:
    id: 1145360
  id:
    steamExtra: [1145361]
    gogExtra: [1234]
"Hades II":
  alias: "Hades"
"Registry Only":
  registry:
    HKEY_CURRENT_USER/SOFTWARE/X:
      tags: [save]
"Cloud Game":
  gog:
    id: 99
  files:
    "<base>/save.dat": {}
"#;

    #[test]
    fn reads_the_parts_that_matter() {
        let games = parse(YAML.as_bytes()).unwrap();
        let titles: Vec<_> = games.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["Cloud Game", "Hades", "Hades II"]);
        let hades = games.iter().find(|g| g.title == "Hades").unwrap();
        assert_eq!(hades.steam, ["1145360", "1145361"]);
        assert_eq!(hades.gog, ["1234"]);
        assert_eq!(hades.files.len(), 2);
        let home = hades.files.iter().find(|r| r.path.starts_with("<home>")).unwrap();
        assert_eq!(home.tags, "sc");
        assert_eq!(home.when, [When { os: Some("linux".into()), store: Some("steam".into()) }]);
    }

    #[test]
    fn finds_by_id_then_by_name_and_follows_aliases() {
        let ix = index_of(parse(YAML.as_bytes()).unwrap());
        assert_eq!(ix.find(Some("1145361"), None, &[]).unwrap().title, "Hades");
        assert_eq!(ix.find(None, Some("99"), &[]).unwrap().title, "Cloud Game");
        assert_eq!(ix.find(Some("0"), None, &["hades!".to_string()]).unwrap().title, "Hades");
        assert_eq!(ix.find(None, None, &["Hades II".to_string()]).unwrap().title, "Hades");
        assert!(ix.find(None, None, &["Registry Only".to_string()]).is_none());
        assert!(ix.find(None, None, &["Nothing".to_string()]).is_none());
        assert_eq!(normalize("Tony Hawk's Pro Skater™ 1+2"), "tonyhawksproskater12");
    }

    #[test]
    fn rules_know_their_os_and_store() {
        let ix = index_of(parse(YAML.as_bytes()).unwrap());
        let hades = ix.find(Some("1145360"), None, &[]).unwrap();
        let rule = |p: &str| hades.files.iter().find(|r| r.path.starts_with(p)).unwrap();
        let (win, lin) = (rule("<winAppData>"), rule("<home>"));
        assert!(win.applies("windows", Some("steam")));
        assert!(!win.applies("linux", Some("steam")));
        assert_eq!(win.oses(), ["windows"]);
        assert!(lin.applies("linux", Some("steam")));
        assert!(!lin.applies("linux", Some("gog")));
        assert!(lin.applies("linux", None));
        let cloud = ix.find(None, Some("99"), &[]).unwrap();
        assert_eq!(cloud.files[0].oses(), ["windows", "linux"]);
    }

    #[test]
    fn index_survives_a_round_trip_and_is_cached() {
        let d = tempfile::tempdir().unwrap();
        let games = parse(YAML.as_bytes()).unwrap();
        let stored = serde_json::to_vec(&Stored { version: INDEX_VERSION, etag: "e".into(), games }).unwrap();
        write_atomic(&d.path().join(INDEX_FILE), &stored).unwrap();
        let ix = load(d.path()).unwrap();
        assert_eq!(ix.etag, "e");
        assert!(ix.find(Some("1145360"), None, &[]).is_some());
        assert!(Arc::ptr_eq(&ix, &load(d.path()).unwrap()));
        assert!(age(d.path()).is_some());
        assert!(load(&d.path().join("none")).is_none());
    }
}
