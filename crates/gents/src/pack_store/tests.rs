use super::*;
use crate::pack::declared_paths;

fn mailbox_pack() -> (Vec<u8>, PackHeader) {
    let pack = crate::pack::resolve_pack("mailbox").expect("a bundled pack");
    let dir = tempfile::tempdir().expect("tempdir");
    for path in declared_paths(&pack.manifest) {
        let target = dir.path().join(&path);
        std::fs::create_dir_all(target.parent().expect("a parent")).expect("mkdir");
        std::fs::write(&target, pack.asset(&path).expect("asset")).expect("write");
    }
    crate::pack_archive::pack_dir(dir.path()).expect("packing")
}

fn store_files(store: &PackStore) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(&store.root)
        .expect("store dir")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn an_imported_pack_is_stored_under_its_digest_and_reopens_verified() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    let (bytes, header) = mailbox_pack();

    let stored = store
        .import(bytes.as_slice(), Some(&header.digest))
        .expect("import");
    assert_eq!(stored.header, header);
    assert_eq!(stored.path, store.path(&header.digest).unwrap());
    assert_eq!(
        std::fs::read(&stored.path).unwrap(),
        bytes,
        "stored byte for byte"
    );
    assert!(stored.path.ends_with(format!(
        "packs/store/sha256/{}.pack",
        digest_hex(&header.digest).unwrap()
    )));

    assert_eq!(store.open(&header.digest).unwrap().digest(), header.digest);
    assert_eq!(store.verify(&header.digest).unwrap(), header);
    assert!(store.contains(&header.digest).unwrap());
    assert_eq!(
        store_files(&store).len(),
        1,
        "no staging file is left behind"
    );
}

#[test]
fn importing_the_same_pack_twice_keeps_one_file() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    let (bytes, header) = mailbox_pack();
    store.import(bytes.as_slice(), None).unwrap();
    store
        .import(bytes.as_slice(), Some(&header.digest))
        .unwrap();
    assert_eq!(store_files(&store).len(), 1);
}

#[test]
fn a_pack_with_another_digest_than_asked_for_is_not_stored() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    let (bytes, _) = mailbox_pack();
    let wanted = format!("sha256:{}", "0".repeat(64));
    let error = store
        .import(bytes.as_slice(), Some(&wanted))
        .expect_err("refused");
    assert!(
        format!("{error:#}").contains("nothing was stored"),
        "{error:#}"
    );
    assert!(store_files(&store).is_empty(), "{:?}", store_files(&store));
}

#[test]
fn a_file_that_is_not_a_pack_is_not_stored() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    assert!(store.import(&b"definitely not gzip"[..], None).is_err());
    let (mut bytes, _) = mailbox_pack();
    bytes.extend_from_slice(b"trailing");
    let error = store.import(bytes.as_slice(), None).expect_err("refused");
    assert!(
        format!("{error:#}").contains("data after the end"),
        "{error:#}"
    );
    assert!(store_files(&store).is_empty(), "{:?}", store_files(&store));
}

#[test]
fn a_stored_file_whose_content_no_longer_matches_its_name_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    let (bytes, header) = mailbox_pack();
    let stored = store.import(bytes.as_slice(), None).unwrap();
    let mut damaged = bytes.clone();
    let middle = damaged.len() / 2;
    damaged[middle] ^= 0xff;
    std::fs::write(&stored.path, &damaged).unwrap();
    assert!(store.open(&header.digest).is_err());
    assert!(store.verify(&header.digest).is_err());
}

#[test]
fn only_a_sha256_digest_names_a_stored_pack() {
    let store = PackStore::new(Path::new("/nonexistent"));
    assert!(store.path("mailbox").is_err());
    assert!(store.path("sha256:../../etc").is_err());
}
