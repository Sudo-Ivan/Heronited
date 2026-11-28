//! The remote name of an uploaded file: random, original or templated, always
//! safe as one path segment on any server.

use heronite_i18n::tr;
use serde::{Deserialize, Serialize};

use crate::error::{Result, ShareError};
use crate::ShareContext;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Naming {
    #[serde(default)]
    pub mode: NameMode,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "template")]
pub enum NameMode {
    /// 12 lowercase base32 characters plus the original extension.
    #[default]
    Random,
    /// The file's own name, sanitised.
    Original,
    /// `{game} {date} {time} {name} {stem} {ext} {rand}`, spaces allowed.
    Template(String),
}

/// The longest remote name: enough for a title and a timestamp.
const MAX_BYTES: usize = 120;

/// One path segment, safe everywhere: no `..`, no leading or nested slashes,
/// no control characters, no empty pieces. Characters outside a conservative
/// set become `-`.
pub fn sanitize_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = false;
    for c in raw.chars() {
        let ok = c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' ' | '(' | ')');
        if ok {
            out.push(c);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let out = out.trim_matches(|c| c == '-' || c == '.' || c == ' ').to_string();

    if out.is_empty() {
        "file".into()
    } else {
        out
    }
}

/// The extension of `name`, lowercase without the dot; empty when there is none.
pub fn extension_of(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    base.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()).map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// 12 lowercase base32 characters of randomness (`[a-z2-7]`).
pub fn random_stem() -> String {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        // No entropy source is not a reason to fail the upload: mix in time.
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
        bytes = nanos.to_le_bytes();
    }
    let n = u64::from_le_bytes(bytes);
    const ALPHA: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity(12);
    for i in 0..12 {
        let shift = (i * 5) % 60;
        out.push(ALPHA[((n >> shift) & 31) as usize] as char);
    }
    out
}

/// Truncates to `MAX_BYTES` bytes on a char boundary, the extension kept.
fn truncate(name: &str) -> String {
    if name.len() <= MAX_BYTES {
        return name.to_string();
    }
    let ext = extension_of(name);
    let suffix = if ext.is_empty() { String::new() } else { format!(".{ext}") };
    let budget = MAX_BYTES - suffix.len();
    let mut end = budget.min(name.len());
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    let stem = name[..end].trim_end_matches(['.', ' ', '-']);
    format!("{stem}{suffix}")
}

impl Naming {
    /// The remote file name for `file_name` (the original, with directories
    /// stripped) in `ctx`. Errors only for a template that names nothing.
    pub fn name_for(&self, file_name: &str, ctx: &ShareContext) -> Result<String> {
        let base_name = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
        let ext = extension_of(base_name);
        let stem = base_name.strip_suffix(&format!(".{ext}")).filter(|_| !ext.is_empty()).unwrap_or(base_name);
        let name = match &self.mode {
            NameMode::Random => {
                let dot = if ext.is_empty() { String::new() } else { format!(".{ext}") };
                format!("{}{}", random_stem(), dot)
            }
            NameMode::Original => sanitize_segment(base_name),
            NameMode::Template(t) => {
                let now = chrono::Local::now();
                let mut out = t.clone();
                let pairs = [
                    ("{game}", ctx.game.as_deref().unwrap_or("game").to_string()),
                    ("{date}", now.format("%Y-%m-%d").to_string()),
                    ("{time}", now.format("%H-%M-%S").to_string()),
                    ("{name}", base_name.to_string()),
                    ("{stem}", stem.to_string()),
                    ("{ext}", ext.clone()),
                    ("{rand}", random_stem()),
                ];
                for (k, v) in pairs {
                    out = out.replace(k, &v);
                }
                // Every segment sanitised on its own: a template may shape
                // subfolders with `/`, but never traversal or empty pieces.
                let segments: Vec<String> =
                    out.split('/').map(sanitize_segment).filter(|s| !s.is_empty() && s != "." && s != "..").collect();
                if segments.is_empty() {
                    return Err(ShareError::config(tr!("The file name template names nothing")));
                }
                segments.join("/")
            }
        };
        // Only the last segment is the file's name; folder parts stay short.
        let (dirs, file) = match name.rsplit_once('/') {
            Some((d, f)) => (format!("{d}/"), f.to_string()),
            None => (String::new(), name),
        };
        let file = truncate(&file);
        if file.is_empty() {
            return Err(ShareError::config(tr!("The file name template names nothing")));
        }
        Ok(format!("{dirs}{file}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ShareContext {
        ShareContext { game: Some("Hollow Knight: Silksong".into()) }
    }

    fn none_ctx() -> ShareContext {
        ShareContext { game: None }
    }

    #[test]
    fn random_names_are_well_formed_and_unique() {
        let n = Naming::default();
        let one = n.name_for("shot.png", &ctx()).unwrap();
        let two = n.name_for("shot.png", &ctx()).unwrap();
        assert_ne!(one, two);
        assert!(one.ends_with(".png"));
        let stem = one.strip_suffix(".png").unwrap();
        assert_eq!(stem.len(), 12);
        assert!(stem.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        assert!(stem.chars().all(|c| "abcdefghijklmnopqrstuvwxyz234567".contains(c)));
    }

    #[test]
    fn original_names_are_sanitised() {
        let n = Naming { mode: NameMode::Original };
        assert_eq!(n.name_for("my shot (final).png", &ctx()).unwrap(), "my shot (final).png");
        assert_eq!(n.name_for("../../etc/passwd", &ctx()).unwrap(), "passwd");
        assert_eq!(n.name_for("töst☃.png", &ctx()).unwrap(), "t-st-.png");
        assert_eq!(n.name_for("...", &ctx()).unwrap(), "file");
    }

    #[test]
    fn templates_fill_every_placeholder() {
        let n = Naming { mode: NameMode::Template("{game}-{stem}-{rand}.{ext}".into()) };
        let name = n.name_for("clip one.mkv", &ctx()).unwrap();
        assert!(name.starts_with("Hollow Knight- Silksong-clip one-"), "{name}");
        assert!(name.ends_with(".mkv"));
        let n = Naming { mode: NameMode::Template("{name}".into()) };
        assert_eq!(n.name_for("a b.png", &ctx()).unwrap(), "a b.png");
    }

    #[test]
    fn templates_cannot_traverse() {
        for t in ["../{stem}.{ext}", "{stem}/../../x.{ext}", "/{stem}.{ext}", "a//b/{stem}.{ext}"] {
            let n = Naming { mode: NameMode::Template(t.into()) };
            let name = n.name_for("shot.png", &none_ctx()).unwrap();
            assert!(!name.contains(".."), "{t} -> {name}");
            assert!(!name.starts_with('/'), "{t} -> {name}");
            assert!(!name.split('/').any(|s| s.is_empty()), "{t} -> {name}");
        }
    }

    #[test]
    fn gameless_templates_still_name() {
        let n = Naming { mode: NameMode::Template("{game}-{date}.{ext}".into()) };
        let name = n.name_for("shot.png", &none_ctx()).unwrap();
        assert!(name.starts_with("game-"), "{name}");
        assert!(name.ends_with(".png"));
    }

    #[test]
    fn long_names_are_truncated_keeping_the_extension() {
        let n = Naming { mode: NameMode::Original };
        let long = format!("{}.png", "a".repeat(300));
        let name = n.name_for(&long, &ctx()).unwrap();
        assert!(name.len() <= 120, "{}", name.len());
        assert!(name.ends_with(".png"));
        let multi = format!("{}.tarballs", "ü".repeat(200));
        let name = n.name_for(&multi, &ctx()).unwrap();
        assert!(name.len() <= 120, "{}", name.len());
    }

    #[test]
    fn extensions() {
        assert_eq!(extension_of("a.png"), "png");
        assert_eq!(extension_of("A.PNG"), "png");
        assert_eq!(extension_of("noext"), "");
        assert_eq!(extension_of(".hidden"), "");
        assert_eq!(extension_of("dir/file.webp"), "webp");
    }
}
