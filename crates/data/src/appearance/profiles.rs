//! Looks saved under a name: the built-in ones, the user's own in
//! `appearance.json` (`profiles`), and a file format to share one.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::*;

/// A built-in look. `id` is stable; the app translates the names.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preset {
    pub id: &'static str,
    pub look: Appearance,
}

/// A built-in look on a custom palette: solid panels, no background art,
/// the rest is today's default.
const fn themed(look: Appearance, light: bool, page: Rgb, panel: Rgb, lift: Rgb, text: Rgb, muted: Rgb, accent: Rgb) -> Appearance {
    Appearance {
        theme: Theme::Custom,
        custom: Palette { light, page, panel, lift, text, muted, accent },
        interface_mode: InterfaceMode::Solid,
        interface_opacity: 100.,
        background: BackgroundSource::Solid,
        ..look
    }
}

const fn hex(r: u8, g: u8, b: u8) -> Rgb {
    (r, g, b)
}

/// The built-in looks; the first is today's.
pub fn presets() -> [Preset; 11] {
    let d = Appearance::DEFAULT;
    let solid = Appearance { interface_mode: InterfaceMode::Solid, interface_opacity: 100., background: BackgroundSource::Solid, ..d };
    let round = Appearance { cover_corners: Corners::Round, ..solid };
    [
        Preset { id: "default", look: d },
        Preset {
            id: "cinema",
            look: Appearance {
                background: BackgroundSource::Screenshot,
                background_slideshow: true,
                cover_hover: CoverHover::Glow,
                cover_label: CoverLabel::Nothing,
                cover_corners: Corners::Round,
                accent: Accent::Game,
                ..d
            },
        },
        Preset {
            id: "minimal",
            look: Appearance {
                interface_mode: InterfaceMode::Solid,
                solid_tint: SolidTint::Graphite,
                background: BackgroundSource::Solid,
                cover_badges: false,
                cover_hover: CoverHover::Nothing,
                cover_corners: Corners::Square,
                accent: Accent::Mint,
                density: Density::Compact,
                motion: Motion::Reduced,
                ..d
            },
        },
        Preset {
            id: "contrast",
            look: Appearance {
                high_contrast: true,
                accent: Accent::Amber,
                interface_opacity: 96.,
                collection_opacity: 70.,
                cover_label: CoverLabel::Title,
                cover_badges: true,
                motion: Motion::Reduced,
                ..d
            },
        },
        // Near-black with a teal accent, the Quad4 look.
        Preset {
            id: "void",
            look: themed(
                Appearance { cover_hover: CoverHover::Glow, ..round },
                false,
                hex(0x05, 0x06, 0x0A),
                hex(0x0C, 0x0F, 0x16),
                hex(0x18, 0x1D, 0x29),
                hex(0xE4, 0xE8, 0xF0),
                hex(0x87, 0x91, 0xA6),
                hex(0x3F, 0xB5, 0xD2),
            ),
        },
        // Pure black page, so OLED pixels switch off.
        Preset {
            id: "oled",
            look: themed(
                Appearance { motion: Motion::Reduced, cover_hover: CoverHover::Nothing, ..round },
                false,
                hex(0x00, 0x00, 0x00),
                hex(0x05, 0x05, 0x05),
                hex(0x12, 0x12, 0x12),
                hex(0xF2, 0xF2, 0xF2),
                hex(0x9A, 0x9A, 0x9A),
                hex(0xA5, 0xDC, 0xEB),
            ),
        },
        Preset {
            id: "nord",
            look: themed(
                solid,
                false,
                hex(0x2E, 0x34, 0x40),
                hex(0x3B, 0x42, 0x52),
                hex(0x43, 0x4C, 0x5E),
                hex(0xEC, 0xEF, 0xF4),
                hex(0xA7, 0xB0, 0xC0),
                hex(0x88, 0xC0, 0xD0),
            ),
        },
        Preset {
            id: "dracula",
            look: themed(
                solid,
                false,
                hex(0x1E, 0x1F, 0x29),
                hex(0x28, 0x2A, 0x36),
                hex(0x34, 0x37, 0x46),
                hex(0xF8, 0xF8, 0xF2),
                hex(0xA9, 0xB1, 0xD6),
                hex(0xBD, 0x93, 0xF9),
            ),
        },
        Preset {
            id: "mocha",
            look: themed(
                solid,
                false,
                hex(0x11, 0x11, 0x1B),
                hex(0x1E, 0x1E, 0x2E),
                hex(0x31, 0x32, 0x44),
                hex(0xCD, 0xD6, 0xF4),
                hex(0xA6, 0xAD, 0xC8),
                hex(0xCB, 0xA6, 0xF7),
            ),
        },
        Preset {
            id: "gruvbox",
            look: themed(
                solid,
                false,
                hex(0x1D, 0x20, 0x21),
                hex(0x28, 0x28, 0x28),
                hex(0x3C, 0x38, 0x36),
                hex(0xEB, 0xDB, 0xB2),
                hex(0xA8, 0x99, 0x84),
                hex(0xFA, 0xBD, 0x2F),
            ),
        },
        Preset {
            id: "latte",
            look: themed(
                solid,
                true,
                hex(0xDC, 0xE0, 0xE8),
                hex(0xE6, 0xE9, 0xEF),
                hex(0xEF, 0xF1, 0xF5),
                hex(0x4C, 0x4F, 0x69),
                hex(0x5C, 0x5F, 0x77),
                hex(0x1E, 0x66, 0xF5),
            ),
        },
    ]
}

/// A look the user saved.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub name: String,
    pub look: Appearance,
}

/// Marker key of an exported look (its value is the format version).
const EXPORT_KEY: &str = "heroniteAppearance";

/// The user's profiles, in the order they were saved. Entries without a
/// name are skipped.
pub fn load_profiles(paths: &AppPaths) -> Vec<Profile> {
    let Ok(doc) = json_store::read_value(&paths.appearance()) else { return Vec::new() };
    doc.get("profiles")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|v| {
                    let name = v.get("name").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())?;
                    Some(Profile { name: name.to_owned(), look: Appearance::from_value(v) })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Replaces the stored profiles; the rest of the file is kept.
pub fn save_profiles(paths: &AppPaths, profiles: &[Profile]) -> Result<()> {
    let list: Vec<Value> = profiles
        .iter()
        .map(|p| {
            let mut v = p.look.to_value();
            v["name"] = json!(p.name);
            v
        })
        .collect();
    json_store::update_value(&paths.appearance(), |doc| {
        if !doc.is_object() {
            *doc = Value::Object(Default::default());
        }
        doc["profiles"] = Value::Array(list);
        Ok(())
    })
}

/// Writes `look` to `path` as a file another install can import.
pub fn export_look(path: &Path, name: &str, look: &Appearance) -> Result<()> {
    let mut v = look.to_value();
    v[EXPORT_KEY] = json!(1);
    v["name"] = json!(name);
    json_store::write(path, &v)
}

/// Reads a file written by [`export_look`]: the look and its name (the file
/// name when it has none).
pub fn import_look(path: &Path) -> Result<(String, Appearance)> {
    let v = json_store::read_value(path).with_context(|| format!("reading {}", path.display()))?;
    if v.get(EXPORT_KEY).is_none() {
        bail!("{} is not an exported look", path.display());
    }
    let name = v
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default();
    Ok((name, Appearance::from_value(&v)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &Path) -> AppPaths {
        AppPaths { config: dir.join("cfg"), cache: dir.join("cache") }
    }

    #[test]
    fn first_preset_is_todays_look() {
        assert_eq!(presets()[0].look, Appearance::DEFAULT);
        let ids: Vec<_> = presets().iter().map(|p| p.id).collect();
        assert_eq!(ids, ["default", "cinema", "minimal", "contrast", "void", "oled", "nord", "dracula", "mocha", "gruvbox", "latte"]);
    }

    #[test]
    fn preset_ids_are_unique() {
        let ids: Vec<_> = presets().iter().map(|p| p.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(ids.len(), sorted.len());
    }

    #[test]
    fn presets_round_trip_through_json() {
        for p in presets() {
            assert_eq!(Appearance::from_value(&p.look.to_value()), p.look, "preset {}", p.id);
        }
    }

    #[test]
    fn custom_presets_read_on_their_backgrounds() {
        // Text, muted text and accents stay readable: at least 3:1 as picked
        // (the custom theme editor only repairs below 4.5:1, so nothing here
        // may need a repair either).
        for p in presets() {
            if p.look.theme != Theme::Custom {
                continue;
            }
            for role in [Role::Text, Role::Muted, Role::Accent] {
                let worst = p.look.custom.contrast_of(role).unwrap();
                assert!(worst >= 3., "preset {} {:?}: {worst}", p.id, role);
                let fixed = Palette { ..p.look.custom };
                let mut repaired = fixed;
                repaired.set(role, fixed.readable(role));
                assert!(repaired.contrast_of(role).unwrap() >= 4.5, "preset {} {:?} after readable()", p.id, role);
            }
        }
    }

    #[test]
    fn profiles_live_next_to_the_settings() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        assert!(load_profiles(&p).is_empty());
        let look = Appearance { accent: Accent::Rose, density: Density::Compact, ..Appearance::DEFAULT };
        look.save(&p).unwrap();
        let saved = vec![Profile { name: "Night".into(), look: presets()[1].look }];
        save_profiles(&p, &saved).unwrap();
        assert_eq!(load_profiles(&p), saved);
        // Saving the settings keeps the profiles, and the other way round.
        look.save(&p).unwrap();
        assert_eq!(load_profiles(&p), saved);
        assert_eq!(Appearance::load(&p), look);
    }

    #[test]
    fn export_and_import() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Cozy.json");
        let look = Appearance { accent: Accent::Lavender, cover_hover: CoverHover::Lift, ..Appearance::DEFAULT };
        export_look(&file, "", &look).unwrap();
        let (name, back) = import_look(&file).unwrap();
        assert_eq!((name.as_str(), back), ("Cozy", look));
        std::fs::write(&file, "{\"accent\":\"rose\"}").unwrap();
        assert!(import_look(&file).is_err());
    }
}
