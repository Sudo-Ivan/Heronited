//! Copying a server's games into the Collection.

use std::collections::HashSet;

use anyhow::Result;
use serde_json::{json, Value};

use heronite_data::json_store;
use heronite_data::models::{CollectionGameMetadata, GameInfo, LocalGameMeta, LocalMetaFile, MetadataFile};
use heronite_data::DataPaths;

use crate::{app_name, Client, Game, Server, GAME_KEY, LAUNCH_KIND, PROVIDER_KEY, SERVER_KEY, SOURCE};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    pub added: usize,
    pub updated: usize,
    pub missing: usize,
}

fn entry(paths: &DataPaths, server: &Server, client: &Client, game: &Game) -> GameInfo {
    let windows = game.platforms.iter().any(|p| p == "PC_MICROSOFT_WINDOWS");
    let linux = game.platforms.iter().any(|p| p == "LINUX");
    let app = app_name(&server.id, &game.id);
    let cover = image_file(paths, client, game, game.cover.as_ref(), &app, "cover");
    let hero = image_file(paths, client, game, game.header.as_ref(), &app, "hero").or_else(|| cover.clone());
    let mut info = GameInfo {
        runner: "sideload".into(),
        app_name: app,
        title: game.title.clone().unwrap_or_else(|| game.id.clone()),
        art_cover: hero,
        art_square: cover,
        developer: game.developers.first().cloned(),
        is_installed: false,
        is_linux_native: Some(linux && !windows),
        description: game.summary.clone(),
        ..Default::default()
    };
    info.install.platform = Some(if linux && !windows { "linux" } else { "windows" }.into());
    info.install.install_size = game.file_size.map(|s| s.to_string());
    info.rest.insert("canRunOffline".into(), Value::Bool(false));
    info
}

fn image_file(paths: &DataPaths, client: &Client, game: &Game, image: Option<&crate::Image>, app: &str, kind: &str) -> Option<String> {
    let image = image?;
    let dir = paths.images_cache_dir().join("gameyfin/images");
    std::fs::create_dir_all(&dir).ok()?;
    // The API answers images only with the key, so they are cached as files.
    let path = dir.join(format!("{app}-{kind}"));
    for ext in ["jpg", "png", "webp", "gif"] {
        let existing = path.with_extension(ext);
        if existing.exists() {
            return Some(existing.to_string_lossy().into_owned());
        }
    }
    let mut resp = client.get_public(&format!("/v1/games/{}/images/{}", game.id, image.id)).ok()?;
    let bytes = resp.body_mut().with_config().limit(32 * 1024 * 1024).read_to_vec().ok()?;
    let ext = match resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("") {
        s if s.contains("png") => "png",
        s if s.contains("webp") => "webp",
        s if s.contains("gif") => "gif",
        _ => "jpg",
    };
    let path = path.with_extension(ext);
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
        title: game.title.clone().unwrap_or_else(|| game.id.clone()),
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
        title: game.title.clone(),
        description: game.summary.clone(),
        release_date: game.release.clone(),
        developers: game.developers.clone(),
        publishers: game.publishers.clone(),
        genres: game.genres.clone(),
        platforms: game.platforms.clone(),
        ..Default::default()
    }
}

/// Brings `games` of `server` into the Collection. Entries of games removed
/// from the server stay; a game installed here stays installed.
pub fn sync(paths: &DataPaths, server: &Server, client: &Client, games: &[Game], provider: &str) -> Result<Report> {
    let mut report = Report::default();
    let fresh: Vec<(GameInfo, &Game)> = games.iter().map(|g| (entry(paths, server, client, g), g)).collect();
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
            m.extra.insert(PROVIDER_KEY.into(), json!(provider));
            m.extra.insert("gameyfinUpdatedAt".into(), json!(game.updated_at));
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
            m.title = game.title.clone();
            m.description = game.summary.clone();
            m.release_date = game.release.clone();
            m.developers = game.developers.clone();
            m.publishers = game.publishers.clone();
            m.genres = game.genres.clone();
            m.platforms = game.platforms.clone();
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
    use crate::Server;

    fn server(id: &str) -> Server {
        Server {
            id: id.into(),
            name: id.into(),
            url: "http://localhost".into(),
            sealed_key: "x".into(),
            enabled: true,
            last_sync: String::new(),
        }
    }

    fn game(id: &str, title: &str) -> Game {
        Game { id: id.into(), title: Some(title.into()), platforms: vec!["PC_MICROSOFT_WINDOWS".into()], ..Default::default() }
    }

    fn library(paths: &DataPaths) -> Value {
        json_store::read_value(&paths.sideload_library()).unwrap()
    }

    #[test]
    fn namespaces_the_same_game_on_two_servers() {
        let data = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(data.path());
        let client = Client::new("http://localhost", "x").unwrap();
        sync(&paths, &server("mine"), &client, &[game("7", "Shared")], "files").unwrap();
        sync(&paths, &server("friend"), &client, &[game("7", "Shared")], "files").unwrap();
        let lib = library(&paths);
        let names: Vec<_> = lib["games"].as_array().unwrap().iter().filter_map(|g| g["app_name"].as_str()).collect();
        assert!(names.contains(&"gameyfin_mine_7"));
        assert!(names.contains(&"gameyfin_friend_7"));
        let meta: LocalMetaFile = json_store::read(&paths.local_meta()).unwrap();
        assert_eq!(meta.games["gameyfin_mine_7"].extra[SERVER_KEY], json!("mine"));
        assert_eq!(meta.games["gameyfin_friend_7"].extra[SERVER_KEY], json!("friend"));
    }

    #[test]
    fn updates_server_metadata_and_keeps_installed_state_when_missing() {
        let data = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(data.path());
        let client = Client::new("http://localhost", "x").unwrap();
        let s = server("mine");
        sync(&paths, &s, &client, &[game("7", "Old")], "files").unwrap();
        let report = sync(&paths, &s, &client, &[game("7", "New")], "files").unwrap();
        assert_eq!(report.updated, 1);
        assert_eq!(library(&paths)["games"][0]["title"], json!("New"));

        json_store::update_value_if_changed(&paths.sideload_library(), |doc| {
            doc["games"][0]["is_installed"] = json!(true);
            Ok(())
        })
        .unwrap();
        let report = sync(&paths, &s, &client, &[], "files").unwrap();
        assert_eq!(report.missing, 1);
        assert_eq!(library(&paths)["games"][0]["is_installed"], json!(true));
    }

    #[test]
    fn removing_a_server_removes_only_its_uninstalled_entries() {
        let data = tempfile::tempdir().unwrap();
        let paths = DataPaths::at(data.path());
        let client = Client::new("http://localhost", "x").unwrap();
        let s = server("mine");
        sync(&paths, &s, &client, &[game("7", "One"), game("8", "Two")], "files").unwrap();
        json_store::update_value_if_changed(&paths.sideload_library(), |doc| {
            doc["games"][0]["is_installed"] = json!(true);
            Ok(())
        })
        .unwrap();
        assert_eq!(crate::remove_server_entries(&paths, "mine").unwrap(), 1);
        let lib = library(&paths);
        let names: Vec<_> = lib["games"].as_array().unwrap().iter().filter_map(|g| g["app_name"].as_str()).collect();
        assert_eq!(names, ["gameyfin_mine_7"]);
        let meta: LocalMetaFile = json_store::read(&paths.local_meta()).unwrap();
        assert!(meta.games.contains_key("gameyfin_mine_7"));
        assert!(!meta.games.contains_key("gameyfin_mine_8"));
    }
}
