use super::*;

/// A pack built from the `assets_fixture` fixture, under a coordinate and
/// version generic import/store tests do not otherwise care about.
fn store_test_pack() -> (Vec<u8>, PackHeader) {
    test_pack_named("store_import_fixture", "1.0.0")
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
    let (bytes, header) = store_test_pack();

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
    let (bytes, header) = store_test_pack();
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
    let (bytes, _) = store_test_pack();
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
    let (mut bytes, _) = store_test_pack();
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
    let (bytes, header) = store_test_pack();
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

#[test]
fn release_removes_the_archive_and_its_unpacked_copy() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    let (bytes, header) = store_test_pack();
    let stored = store.import(bytes.as_slice(), None).unwrap();
    store.open(&header.digest).unwrap();
    let unpacked = store
        .unpacked_root()
        .join(crate::pack_archive::digest_hex(&header.digest).unwrap());
    assert!(stored.path.is_file());
    assert!(unpacked.is_dir());

    assert!(store.release(&header.digest).unwrap());
    assert!(!stored.path.exists());
    assert!(!unpacked.exists());

    // Idempotent: nothing left to release the second time.
    assert!(!store.release(&header.digest).unwrap());
}

// --- name index -----------------------------------------------------------

#[test]
fn an_import_indexes_the_pack_by_coordinate_and_version() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    let (bytes, header) = test_pack_named("name_index_a", "1.0.0");
    store.import(bytes.as_slice(), None).unwrap();

    let found = store.lookup("gents", "name_index_a", None).unwrap();
    assert_eq!(
        found,
        Some(StoredName {
            version: "1.0.0".into(),
            digest: header.digest.clone(),
        })
    );
    assert_eq!(
        store
            .lookup("gents", "name_index_a", Some("1.0.0"))
            .unwrap(),
        found
    );
    assert_eq!(
        store
            .lookup("gents", "name_index_a", Some("9.9.9"))
            .unwrap(),
        None
    );
    assert_eq!(
        store.names().unwrap(),
        vec![(
            "gents/name_index_a".to_owned(),
            vec![StoredName {
                version: "1.0.0".into(),
                digest: header.digest,
            }]
        )]
    );
}

#[test]
fn lookup_prefers_the_highest_semver_not_the_lexicographically_largest() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    let (v1, _) = test_pack_named("name_index_b", "1.2.0");
    let (v2, v2_header) = test_pack_named("name_index_b", "1.10.0");
    store.import(v1.as_slice(), None).unwrap();
    store.import(v2.as_slice(), None).unwrap();

    // Lexicographically "1.2.0" > "1.10.0", but 1.10.0 is the newer release.
    let found = store.lookup("gents", "name_index_b", None).unwrap();
    assert_eq!(found.map(|entry| entry.version), Some("1.10.0".to_owned()));
    assert_eq!(
        store
            .lookup("gents", "name_index_b", Some("1.10.0"))
            .unwrap()
            .map(|entry| entry.digest),
        Some(v2_header.digest)
    );
}

#[test]
fn non_semver_versions_sort_after_every_semver_version() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    let (semver_bytes, semver_header) = test_pack_named("name_index_c", "1.0.0");
    let (other_bytes, _) = test_pack_named("name_index_c", "latest");
    store.import(semver_bytes.as_slice(), None).unwrap();
    store.import(other_bytes.as_slice(), None).unwrap();

    assert_eq!(
        store
            .lookup("gents", "name_index_c", None)
            .unwrap()
            .map(|entry| entry.digest),
        Some(semver_header.digest)
    );
}

#[test]
fn a_version_with_a_path_separator_is_stored_but_not_indexed() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    let (bytes, header) = test_pack_named("name_index_d", "../escape");
    let stored = store.import(bytes.as_slice(), None).unwrap();

    assert_eq!(stored.header.digest, header.digest);
    assert!(
        store.contains(&header.digest).unwrap(),
        "still content-addressed"
    );
    assert_eq!(store.lookup("gents", "name_index_d", None).unwrap(), None);
    assert!(store.names().unwrap().is_empty());
}

#[test]
fn a_bare_dot_or_dot_dot_version_is_stored_but_not_indexed() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    for (name, version) in [("name_index_f", ".."), ("name_index_g", ".")] {
        let (bytes, header) = test_pack_named(name, version);
        store.import(bytes.as_slice(), None).unwrap();
        assert!(store.contains(&header.digest).unwrap());
        assert_eq!(store.lookup("gents", name, None).unwrap(), None);
    }
    // Neither entry escaped its own `by-name/gents/<name>` directory to
    // land as a sibling of it or higher up the tree.
    assert!(store.names().unwrap().is_empty());
}

#[test]
fn release_deletes_the_matching_name_index_entry_and_keeps_the_rest() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    let (v1_bytes, v1_header) = test_pack_named("name_index_e", "1.0.0");
    let (v2_bytes, v2_header) = test_pack_named("name_index_e", "2.0.0");
    store.import(v1_bytes.as_slice(), None).unwrap();
    store.import(v2_bytes.as_slice(), None).unwrap();
    let v1_entry = store.by_name_root().join("gents/name_index_e/1.0.0");
    let v2_entry = store.by_name_root().join("gents/name_index_e/2.0.0");
    assert!(v1_entry.is_file(), "indexed before release");
    assert!(v2_entry.is_file(), "indexed before release");

    store.release(&v1_header.digest).unwrap();

    // The index file itself is gone, not merely unresolvable because
    // `lookup` filters out entries whose archive disappeared: deleting
    // `remove_name_entries_for_digest`/`remove_indexed_entry` entirely would
    // make `lookup` alone insufficient to catch the regression.
    assert!(!v1_entry.exists(), "release deleted the index entry itself");
    assert!(v2_entry.is_file(), "the other version's entry is untouched");

    assert_eq!(
        store
            .lookup("gents", "name_index_e", Some("1.0.0"))
            .unwrap(),
        None,
        "the released version's archive is gone, so it no longer resolves"
    );
    assert_eq!(
        store
            .lookup("gents", "name_index_e", Some("2.0.0"))
            .unwrap()
            .map(|entry| entry.digest),
        Some(v2_header.digest)
    );
}

/// When the archive is already gone before `release` runs (a prior release,
/// or the file was removed out from under the store), there is no header
/// left to read the coordinate and version from; `release` must still find
/// and delete the index entry, by falling back to a full scan.
#[test]
fn releasing_a_digest_whose_archive_is_already_gone_still_cleans_its_index_entry() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    let (bytes, header) = test_pack_named("name_index_h", "1.0.0");
    store.import(bytes.as_slice(), None).unwrap();
    let entry = store.by_name_root().join("gents/name_index_h/1.0.0");
    assert!(entry.is_file());

    std::fs::remove_file(store.path(&header.digest).unwrap()).unwrap();
    assert!(
        !store.release(&header.digest).unwrap(),
        "nothing left in the content-addressed store to remove"
    );

    assert!(
        !entry.exists(),
        "the index entry is cleaned up even though the archive was gone first"
    );
}

#[test]
fn versions_differing_only_in_case_index_resolve_and_stay_distinct() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    let (upper_bytes, upper) = test_pack_named("name_index_i", "1.0.0-RC1");
    let (lower_bytes, lower) = test_pack_named("name_index_i", "1.0.0-rc1");
    store.import(upper_bytes.as_slice(), None).unwrap();
    store.import(lower_bytes.as_slice(), None).unwrap();
    assert_ne!(upper.digest, lower.digest);

    for (version, header) in [("1.0.0-RC1", &upper), ("1.0.0-rc1", &lower)] {
        assert_eq!(
            store
                .lookup("gents", "name_index_i", Some(version))
                .unwrap()
                .map(|entry| (entry.version, entry.digest)),
            Some((version.to_owned(), header.digest.clone()))
        );
    }
    let names = store.names().unwrap();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].1.len(), 2, "two distinct entries");
}

#[test]
fn releasing_a_pack_with_an_unindexable_version_leaves_other_files_alone() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    let (bytes, header) = test_pack_named("escape", "../escape");
    store.import(bytes.as_slice(), None).unwrap();
    let decoy = store.by_name_root().join("gents/escape");
    std::fs::create_dir_all(decoy.parent().unwrap()).unwrap();
    std::fs::write(&decoy, &header.digest).unwrap();

    assert!(store.release(&header.digest).unwrap());
    assert!(
        decoy.is_file(),
        "release never follows an unindexed version"
    );
}

#[test]
fn a_damaged_pack_can_still_be_released_with_its_index_entry() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    let (bytes, header) = test_pack_named("name_index_j", "1.0.0");
    store.import(bytes.as_slice(), None).unwrap();
    let entry = store.by_name_root().join("gents/name_index_j/1.0.0");
    assert!(entry.is_file());
    let archive = store.path(&header.digest).unwrap();
    std::fs::write(&archive, b"not a pack").unwrap();

    assert!(store.release(&header.digest).unwrap());
    assert!(!archive.exists());
    assert!(!entry.exists(), "the index entry went with the archive");
}

#[test]
fn indexing_an_unchanged_entry_does_not_rewrite_it() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    let (bytes, header) = test_pack_named("name_index_k", "1.0.0");
    store.import(bytes.as_slice(), None).unwrap();
    let entry = store.by_name_root().join("gents/name_index_k/1.0.0");
    let before = std::fs::metadata(&entry).unwrap().modified().unwrap();
    let inode = |path: &Path| {
        #[cfg(unix)]
        {
            std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(path).unwrap())
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            0u64
        }
    };
    let ino = inode(&entry);
    store.index(&header).unwrap();
    assert_eq!(
        std::fs::metadata(&entry).unwrap().modified().unwrap(),
        before
    );
    assert_eq!(inode(&entry), ino);
}

#[test]
fn lookup_refuses_a_coordinate_that_is_not_a_pack_name() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    assert_eq!(store.lookup("gents", "/etc", None).unwrap(), None);
    assert_eq!(store.lookup("..", "x", None).unwrap(), None);
}
