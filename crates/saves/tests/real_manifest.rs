//! Reads the real manifest when `SAVES_REAL_MANIFEST` names a copy of it.

use heronite_saves::manifest;

#[test]
fn reads_the_real_manifest() {
    let Some(path) = std::env::var_os("SAVES_REAL_MANIFEST") else { return };
    let started = std::time::Instant::now();
    let games = manifest::parse(std::fs::File::open(path).unwrap()).unwrap();
    eprintln!("{} games in {:?}", games.len(), started.elapsed());
    assert!(games.len() > 10_000);
    let ix = manifest::index_of(games);
    let hades = ix.find(Some("1145360"), None, &[]).unwrap();
    assert_eq!(hades.title, "Hades");
    assert!(!hades.files.is_empty());
}
