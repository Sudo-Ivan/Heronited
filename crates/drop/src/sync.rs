//! Copying a Drop server's games into the Collection.

use std::collections::HashSet;

use anyhow::Result;
use serde_json::{json, Value};

use heronite_data::json_store;
use heronite_data::models::{CollectionGameMetadata, GameInfo, LocalGameMeta, LocalMetaFile, MetadataFile};
use heronite_data::DataPaths;

use crate::client::VersionOption;
use crate::install::choose_version;
use crate::{app_name, Client, Game, Server, GAME_KEY, LAUNCH_KIND, SERVER_KEY, SOURCE};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    pub added: usize,
    pub updated: usize,
    pub missing: usize,
}

fn entry(paths: &DataPaths, server: &Server, client: &Client, game: &Game, version: Option<&VersionOption>) -> GameInfo {
    let app = app_name(&server.id, &game.id);
    let cover = image_file(paths, client, game, first(&game.m_cover_object_id, &game.m_icon_object_id), &app, "cover");
    let hero =
        image_file(paths, client, game, first(&game.m_banner_object_id, &game.m_cover_object_id), &app, "hero").or_else(|| cover.clone());
    let mut info = GameInfo {
        runner: "sideload".into(),
        app_name: app,
        title: game.m_name.clone(),
        art_cover: hero,
        art_square: cover,
        is_installed: false,
        is_linux_native: None,
        description: Some(game.m_description.clone())
            .filter(|d| !d.is_empty())
            .or_else(|| Some(game.m_short_description.clone()).filter(|d| !d.is_empty())),
        ..Default::default()
    };
    let platform = version.map(|v| v.platform.to_ascii_lowercase()).unwrap_or_else(|| "windows".into());
    info.install.platform = Some(platform.clone());
    info.install.install_size = version.map(|v| v.size.install_size.to_string()).filter(|s| s != "0");
    info.is_linux_native = Some(platform == "linux");
    info.rest.insert("canRunOffline".into(), Value::Bool(false));
    info
}

fn first<'a>(a: &'a str, b: &'a str) -> Option<&'a str> {
    if !a.is_empty() {
        Some(a)
    } else if !b.is_empty() {
        Some(b)
    } else {
        None
    }
}

fn image_file(paths: &DataPaths, client: &Client, _game: &Game, object: Option<&str>, app: &str, kind: &str) -> Option<String> {
    let object = object?;
    let dir = paths.images_cache_dir().join("drop/images");
    std::fs::create_dir_all(&dir).ok()?;
    let object_name: String = object.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    let base = dir.join(format!("{app}-{kind}-{object_name}"));
    for ext in ["jpg", "png", "webp", "gif"] {
        let existing = base.with_extension(ext);
        if existing.exists() {
            return Some(existing.to_string_lossy().into_owned());
        }
    }
    let (bytes, ctype) = client.object_content_type(object).ok()?;
    let ext = match ctype.as_str() {
        s if s.contains("png") => "png",
        s if s.contains("webp") => "webp",
        s if s.contains("gif") => "gif",
        _ => "jpg",
    };
    let path = base.with_extension(ext);
    std::fs::write(&path, bytes).ok()?;
    Some(path.to_string_lossy().into_owned())
}

fn meta(server: &Server, game: &Game) -> LocalGameMeta {
    let app = app_name(&server.id, &game.id);
    let mut meta = LocalGameMeta {
        app_name: app,
        runner: "sideload".into(),
        source: SOURCE.into(),
        launch_kind: LAUNCH_KIND.into(),
        store_game_id: Some(game.id.clone()),
        title: game.m_name.clone(),
        ..Default::default()
    };
    meta.extra.insert(SERVER_KEY.into(), json!(server.id));
    meta.extra.insert(GAME_KEY.into(), json!(game.id));
    meta
}

fn collection_meta(game: &Game) -> CollectionGameMetadata {
    CollectionGameMetadata {
        app_name: String::new(),
        runner: "sideload".into(),
        title: Some(game.m_name.clone()),
        description: Some(game.m_description.clone())
            .filter(|d| !d.is_empty())
            .or_else(|| Some(game.m_short_description.clone()).filter(|d| !d.is_empty())),
        ..Default::default()
    }
}

/// Brings `games` of `server` into the Collection. Entries of games removed
/// from the server stay; a game installed here stays installed.
pub fn sync(paths: &DataPaths, server: &Server, client: &Client, games: &[Game]) -> Result<Report> {
    let mut report = Report::default();
    let fresh: Vec<(GameInfo, &Game)> = games
        .iter()
        .map(|g| {
            let version = client.versions(&g.id, None).ok().and_then(|v| choose_version(&v));
            (entry(paths, server, client, g, version.as_ref()), g)
        })
        .collect();
    let wanted: HashSet<String> = fresh.iter().map(|(i, _)| i.app_name.clone()).collect();
    let before: LocalMetaFile = json_store::read(&paths.local_meta()).unwrap_or_default();
    let ours: HashSet<String> = before
        .games
        .values()
        .filter(|m| m.source == SOURCE && m.extra.get(SERVER_KEY) == Some(&json!(server.id)))
        .map(|m| m.app_name.clone())
        .collect();

    json_store::update::<LocalMetaFile, _>(&paths.local_meta(), |file| {
        for (info, game) in &fresh {
            let mut m = file.games.get(&info.app_name).cloned().unwrap_or_else(|| meta(server, game));
            m.title = info.title.clone();
            m.store_game_id = Some(game.id.clone());
            m.extra.insert(SERVER_KEY.into(), json!(server.id));
            m.extra.insert(GAME_KEY.into(), json!(game.id));
            file.games.insert(info.app_name.clone(), m);
        }
        Ok(())
    })?;

    json_store::update::<MetadataFile, _>(&paths.metadata(), |file| {
        for (info, game) in &fresh {
            let key = format!("sideload:{}", info.app_name);
            let mut m = file.games.get(&key).cloned().unwrap_or_else(|| collection_meta(game));
            m.app_name = info.app_name.clone();
            m.runner = "sideload".into();
            m.title = Some(game.m_name.clone());
            m.description = Some(game.m_description.clone())
                .filter(|d| !d.is_empty())
                .or_else(|| Some(game.m_short_description.clone()).filter(|d| !d.is_empty()));
            file.games.insert(key, m);
        }
        Ok(())
    })?;

    json_store::update_value_if_changed(&paths.sideload_library(), |doc| {
        if !doc.is_object() {
            *doc = json!({});
        }
        let list = doc.as_object_mut().expect("object").entry("games").or_insert_with(|| json!([]));
        if !list.is_array() {
            *list = json!([]);
        }
        let list = list.as_array_mut().expect("array");
        let mut seen = HashSet::new();
        for item in list.iter_mut().filter_map(Value::as_object_mut) {
            let Some(app) = item.get("app_name").and_then(Value::as_str).map(str::to_owned) else { continue };
            if let Some((info, _)) = fresh.iter().find(|(i, _)| i.app_name == app) {
                seen.insert(app);
                let installed = item.get("is_installed").and_then(Value::as_bool).unwrap_or(false);
                let before = item.clone();
                item.insert("title".into(), json!(info.title));
                for (key, value) in [
                    ("art_cover", &info.art_cover),
                    ("art_square", &info.art_square),
                    ("description", &info.description),
                    ("developer", &info.developer),
                ] {
                    if let Some(value) = value {
                        item.insert(key.into(), json!(value));
                    } else {
                        item.shift_remove(key);
                    }
                }
                if !installed {
                    item.insert("is_installed".into(), json!(false));
                    item.insert("install".into(), json!({"platform": info.install.platform, "install_size": info.install.install_size}));
                }
                if *item != before {
                    report.updated += 1;
                }
            } else if ours.contains(&app) && !wanted.contains(&app) {
                report.missing += 1;
            }
        }
        for (info, _) in fresh.iter().filter(|(i, _)| !seen.contains(&i.app_name)) {
            list.push(serde_json::to_value(info)?);
            report.added += 1;
        }
        Ok(())
    })?;
    Ok(report)
}

/// Removes the server's Collection entries that were never installed.
/// Installed games stay: their files are local and still playable.
pub fn remove_server_entries(paths: &DataPaths, server_id: &str) -> Result<usize> {
    let meta: LocalMetaFile = json_store::read(&paths.local_meta()).unwrap_or_default();
    let ours: HashSet<String> = meta
        .games
        .values()
        .filter(|m| m.source == SOURCE && m.extra.get(SERVER_KEY) == Some(&json!(server_id)))
        .map(|m| m.app_name.clone())
        .collect();
    if ours.is_empty() {
        return Ok(0);
    }
    let lib = json_store::read_value(&paths.sideload_library()).unwrap_or(Value::Null);
    let installed: HashSet<String> = lib["games"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter(|g| g.get("is_installed") == Some(&json!(true)))
        .filter_map(|g| g.get("app_name").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let remove: HashSet<String> = ours.into_iter().filter(|app| !installed.contains(app)).collect();
    if remove.is_empty() {
        return Ok(0);
    }
    json_store::update::<LocalMetaFile, _>(&paths.local_meta(), |file| {
        file.games.retain(|app, _| !remove.contains(app));
        Ok(())
    })?;
    json_store::update::<MetadataFile, _>(&paths.metadata(), |file| {
        file.games.retain(|key, _| !key.strip_prefix("sideload:").is_some_and(|app| remove.contains(app)));
        Ok(())
    })?;
    json_store::update_value_if_changed(&paths.sideload_library(), |doc| {
        if let Some(games) = doc["games"].as_array_mut() {
            games.retain(|g| g.get("app_name").and_then(Value::as_str).is_none_or(|app| !remove.contains(app)));
        }
        Ok(())
    })?;
    Ok(remove.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Auth, Server};

    fn server(id: &str) -> Server {
        Server {
            id: id.into(),
            name: id.into(),
            url: "http://localhost".into(),
            sealed_auth: Auth { private: String::new(), certificate: String::new(), id: String::new() }.seal().unwrap_or_default(),
            enabled: true,
            last_sync: String::new(),
        }
    }

    fn game(id: &str, title: &str) -> Game {
        Game { id: id.into(), game_type: "Game".into(), m_name: title.into(), ..Default::default() }
    }

    fn client() -> Client {
        Client::new("http://localhost", Auth { private: String::new(), certificate: String::new(), id: "c".into() }).unwrap()
    }

    fn library(paths: &DataPaths) -> Value {
        json_store::read_value(&paths.sideload_library()).unwrap()
    }

    #[test]
    fn namespaces_the_same_game_on_two_servers() {
        let data = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(data.path());
        let client = client();
        sync(&paths, &server("mine"), &client, &[game("7", "Shared")]).unwrap();
        sync(&paths, &server("friend"), &client, &[game("7", "Shared")]).unwrap();
        let lib = library(&paths);
        let names: Vec<_> = lib["games"].as_array().unwrap().iter().filter_map(|g| g["app_name"].as_str()).collect();
        assert!(names.contains(&"drop_mine_7"));
        assert!(names.contains(&"drop_friend_7"));
        let meta: LocalMetaFile = json_store::read(&paths.local_meta()).unwrap();
        assert_eq!(meta.games["drop_mine_7"].extra[SERVER_KEY], json!("mine"));
        assert_eq!(meta.games["drop_friend_7"].extra[SERVER_KEY], json!("friend"));
    }
    #[test]
    fn refresh_preserves_install_state_and_removal_keeps_installed_games() {
        let data = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(data.path());
        let client = client();
        sync(&paths, &server("mine"), &client, &[game("7", "Shared")]).unwrap();
        json_store::update_value(&paths.sideload_library(), |doc| {
            let game = doc["games"].as_array_mut().unwrap().iter_mut().find(|g| g["app_name"] == "drop_mine_7").unwrap();
            game["is_installed"] = json!(true);
            game["install"] = json!({"install_path": "/tmp/shared"});
            Ok(())
        })
        .unwrap();

        sync(&paths, &server("mine"), &client, &[game("7", "Shared")]).unwrap();
        let lib = library(&paths);
        let entry = lib["games"].as_array().unwrap().iter().find(|g| g["app_name"] == "drop_mine_7").unwrap();
        assert_eq!(entry["is_installed"], json!(true));
        assert_eq!(entry["install"]["install_path"], json!("/tmp/shared"));

        assert_eq!(remove_server_entries(&paths, "mine").unwrap(), 0);
        let lib = library(&paths);
        assert!(lib["games"].as_array().unwrap().iter().any(|g| g["app_name"] == "drop_mine_7"));

        json_store::update_value(&paths.sideload_library(), |doc| {
            let game = doc["games"].as_array_mut().unwrap().iter_mut().find(|g| g["app_name"] == "drop_mine_7").unwrap();
            game["is_installed"] = json!(false);
            Ok(())
        })
        .unwrap();
        assert_eq!(remove_server_entries(&paths, "mine").unwrap(), 1);
        let lib = library(&paths);
        assert!(!lib["games"].as_array().unwrap().iter().any(|g| g["app_name"] == "drop_mine_7"));
        let meta: LocalMetaFile = json_store::read(&paths.local_meta()).unwrap();
        assert!(!meta.games.contains_key("drop_mine_7"));
    }
    #[test]
    fn version_metadata_marks_linux_and_the_install_size() {
        let data = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(data.path());
        let option = VersionOption {
            game_id: "7".into(),
            version_id: "v1".into(),
            platform: "linux".into(),
            size: crate::client::GameSize { install_size: 1234, download_size: 1000 },
            ..Default::default()
        };
        let info = entry(&paths, &server("mine"), &client(), &game("7", "Shared"), Some(&option));
        assert_eq!(info.install.platform.as_deref(), Some("linux"));
        assert_eq!(info.is_linux_native, Some(true));
        assert_eq!(info.install.install_size.as_deref(), Some("1234"));
    }
}
