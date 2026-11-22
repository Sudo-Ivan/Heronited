//! Against a real `savesd` when `SAVES_E2E_SERVER` is set (for example
//! `http://127.0.0.1:18787` of a server with open registration).

use std::fs;

use heronite_saves::snapshot::{self, LocalFile};
use heronite_saves::{login, Client, SecureKey};

fn file(dir: &std::path::Path, name: &str, bytes: &[u8]) -> LocalFile {
    let full = dir.join(name);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(&full, bytes).unwrap();
    LocalFile { path: name.to_string(), full }
}

#[test]
fn push_dedupe_history_and_verified_restore() {
    let Ok(server) = std::env::var("SAVES_E2E_SERVER") else { return };
    let key = SecureKey::generate().unwrap();
    let signed = login(&server, &key, "e2e").unwrap();
    let client = Client::new(&server, &signed.token).unwrap();
    let me = client.me().unwrap();
    let limits = me.effective_limits();
    let d = tempfile::tempdir().unwrap();
    let big: Vec<u8> = (0..(3 << 20) + 5).map(|i| (i % 253) as u8).collect();
    let files = |small: &[u8]| vec![file(d.path(), "home/slot1.sav", small), file(d.path(), "home/big.bin", &big)];

    let prepared = snapshot::prepare(files(b"one"), &limits, "pc-a", "first", None).unwrap();
    let out = snapshot::push(&client, &me, "game:e2e", "default", prepared, &|_, _| {}).unwrap();
    let first = out.snapshot.expect("first push makes a snapshot");
    assert!(out.uploaded >= big.len() as u64);

    let again = snapshot::prepare(files(b"one"), &limits, "pc-a", "second", None).unwrap();
    let out = snapshot::push(&client, &me, "game:e2e", "default", again, &|_, _| {}).unwrap();
    assert!(out.snapshot.is_none(), "identical content is not sent again");

    let changed = snapshot::prepare(files(b"two"), &limits, "pc-a", "third", None).unwrap();
    let out = snapshot::push(&client, &me, "game:e2e", "default", changed, &|_, _| {}).unwrap();
    let second = out.snapshot.expect("a change makes a snapshot");
    assert!(out.uploaded < 1000, "only the small file is new, uploaded {}", out.uploaded);

    let list = client.snapshots("game:e2e", "default").unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].id, second);
    assert_eq!(list[1].id, first);

    let m = client.manifest("game:e2e", "default", &first).unwrap();
    let small = m.files.iter().find(|f| f.path == "home/slot1.sav").unwrap();
    assert_eq!(client.read_file("game:e2e", small).unwrap(), b"one");
    let bigf = m.files.iter().find(|f| f.path == "home/big.bin").unwrap();
    assert_eq!(client.read_file("game:e2e", bigf).unwrap(), big);

    let other = Client::new(&server, &login(&server, &SecureKey::parse(&key.export()).unwrap(), "pc-b").unwrap().token).unwrap();
    assert_eq!(other.snapshots("game:e2e", "default").unwrap().len(), 2, "the same key reaches the same saves");
    let stranger = Client::new(&server, &login(&server, &SecureKey::generate().unwrap(), "pc-c").unwrap().token).unwrap();
    assert!(stranger.snapshots("game:e2e", "default").unwrap().is_empty(), "another key sees nothing");

    client.delete_snapshot("game:e2e", "default", &first).unwrap();
    assert_eq!(client.snapshots("game:e2e", "default").unwrap().len(), 1);
}

#[test]
fn refuses_before_uploading_when_space_is_short() {
    let Ok(server) = std::env::var("SAVES_E2E_SERVER") else { return };
    let key = SecureKey::generate().unwrap();
    let client = Client::new(&server, &login(&server, &key, "e2e").unwrap().token).unwrap();
    let mut me = client.me().unwrap();
    me.usage.quota_bytes = 10_000;
    me.usage.used_bytes = 9_900;
    let d = tempfile::tempdir().unwrap();
    let prepared = snapshot::prepare(vec![file(d.path(), "a.bin", &vec![7u8; 500_000])], &me.effective_limits(), "pc", "", None).unwrap();
    let err = snapshot::push(&client, &me, "game:e2e-full", "default", prepared, &|_, _| {}).unwrap_err();
    assert!(err.to_string().contains("Not enough space"), "{err}");
    assert!(client.snapshots("game:e2e-full", "default").unwrap().is_empty());
}
