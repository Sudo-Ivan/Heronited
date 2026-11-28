//! What was shared where: `local_library/share_history.json`, newest first,
//! pruned to 200 records.

use serde::{Deserialize, Serialize};

use heronite_data::{json_store, DataPaths};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    Screenshot,
    Replay,
    Other,
}

impl FileKind {
    pub fn of(name: &str) -> Self {
        match crate::mime::mime_of(name) {
            m if m.starts_with("image/") => FileKind::Screenshot,
            m if m.starts_with("video/") => FileKind::Replay,
            _ => FileKind::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub id: String,
    pub target_id: String,
    pub target_name: String,
    pub file_name: String,
    pub size: u64,
    pub url: String,
    pub remote: String,
    /// What the backend needs to delete the upload, if it can.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete: Option<String>,
    /// RFC 3339.
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<String>,
    pub kind: FileKind,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct History {
    #[serde(default)]
    pub records: Vec<Record>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

pub const CAP: usize = 200;

pub fn file(paths: &DataPaths) -> std::path::PathBuf {
    paths.local_library().join("share_history.json")
}

impl History {
    pub fn load(paths: &DataPaths) -> Self {
        json_store::read(&file(paths)).unwrap_or_default()
    }

    pub fn save(&self, paths: &DataPaths) -> anyhow::Result<()> {
        json_store::write(&file(paths), self)
    }

    /// Newest first, capped: the oldest fall off.
    pub fn push(&mut self, record: Record) {
        self.records.insert(0, record);
        self.records.truncate(CAP);
    }

    pub fn remove(&mut self, id: &str) -> Option<Record> {
        let at = self.records.iter().position(|r| r.id == id)?;
        Some(self.records.remove(at))
    }

    pub fn get(&self, id: &str) -> Option<&Record> {
        self.records.iter().find(|r| r.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str) -> Record {
        Record {
            id: id.into(),
            target_id: "t".into(),
            target_name: "T".into(),
            file_name: "f.png".into(),
            size: 1,
            url: "u".into(),
            remote: "r".into(),
            delete: None,
            at: "2025-01-01T00:00:00Z".into(),
            game: None,
            kind: FileKind::Screenshot,
        }
    }

    #[test]
    fn newest_first_and_pruned_at_the_cap() {
        let mut h = History::default();
        for n in 0..250 {
            h.push(record(&format!("r{n}")));
        }
        assert_eq!(h.records.len(), 200);
        assert_eq!(h.records[0].id, "r249");
        assert_eq!(h.records[199].id, "r50");
    }

    #[test]
    fn kinds_by_extension() {
        assert_eq!(FileKind::of("a.png"), FileKind::Screenshot);
        assert_eq!(FileKind::of("a.mp4"), FileKind::Replay);
        assert_eq!(FileKind::of("a.zip"), FileKind::Other);
    }

    #[test]
    fn removing() {
        let mut h = History::default();
        h.push(record("a"));
        h.push(record("b"));
        assert!(h.remove("a").is_some());
        assert!(h.remove("x").is_none());
        assert_eq!(h.records.len(), 1);
    }
}
