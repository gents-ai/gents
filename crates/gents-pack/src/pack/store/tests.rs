use super::*;
use gents_lean_vocab::families::pack_store as family;

#[test]
fn version_listing_uses_installed_digest_and_keeps_other_stored_artifacts() {
    let installed = StoredName {
        version: "1.0.0".into(),
        digest: "sha256:installed".into(),
    };
    let revised = StoredName {
        version: "1.0.0".into(),
        digest: "sha256:revised".into(),
    };
    let newer = StoredName {
        version: "1.1.0".into(),
        digest: "sha256:newer".into(),
    };
    let listed = list_pack_versions(
        &[newer.clone(), revised.clone(), installed.clone()],
        Some(installed.clone()),
    );
    assert_eq!(listed.installed, Some(installed.clone()));
    assert_eq!(listed.other_stored, vec![newer, revised]);
    let absent = list_pack_versions(&[], Some(installed.clone()));
    assert_eq!(absent.installed, Some(installed));
    assert!(absent.other_stored.is_empty());
}

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
fn lookup_refuses_a_coordinate_that_is_not_a_pack_name() {
    let home = tempfile::tempdir().unwrap();
    let store = PackStore::new(home.path());
    assert_eq!(store.lookup("gents", "/etc", None).unwrap(), None);
    assert_eq!(store.lookup("..", "x", None).unwrap(), None);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Receive,
    ReceiveExpectingAnother,
    OpenPack,
    Release,
    LoseArchive,
    Damage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum StepResult {
    Stored,
    Refused,
    Opened,
    Released,
    NothingToRelease,
    Done,
}

fn modelled_pack(pack: &family::Pack) -> (Vec<u8>, PackHeader) {
    let read = semver::Version::parse(&pack.version.text)
        .ok()
        .map(|version| family::Semver {
            major: version.major,
            minor: version.minor,
            patch: version.patch,
            pre: (!version.pre.is_empty()).then(|| version.pre.to_string()),
        });
    assert_eq!(
        serde_json::to_value(read).unwrap(),
        serde_json::to_value(&pack.version.semver).unwrap(),
        "the model reads {} as semver does",
        pack.version.text
    );
    test_pack_named(&pack.name, &pack.version.text)
}

#[test]
fn the_store_conforms_to_the_model() {
    family::replay::<Action, StepResult>(|script| {
        let home = tempfile::tempdir().unwrap();
        let store = PackStore::new(home.path());
        let results = script
            .steps
            .iter()
            .map(|step| {
                let (bytes, header) = modelled_pack(&step.pack);
                let archive = store.path(&header.digest).unwrap();
                match step.action {
                    Action::Receive => {
                        store.import(bytes.as_slice(), None).expect(&script.name);
                        StepResult::Stored
                    }
                    Action::ReceiveExpectingAnother => {
                        let other = format!("sha256:{}", "0".repeat(64));
                        match store.import(bytes.as_slice(), Some(&other)) {
                            Ok(_) => StepResult::Stored,
                            Err(_) => StepResult::Refused,
                        }
                    }
                    Action::OpenPack => match store.open(&header.digest) {
                        Ok(_) => StepResult::Opened,
                        Err(_) => StepResult::Refused,
                    },
                    Action::Release => {
                        if store.release(&header.digest).expect(&script.name) {
                            StepResult::Released
                        } else {
                            StepResult::NothingToRelease
                        }
                    }
                    Action::LoseArchive => {
                        std::fs::remove_file(&archive).ok();
                        StepResult::Done
                    }
                    Action::Damage => {
                        if archive.exists() {
                            std::fs::write(&archive, b"not a pack").unwrap();
                        }
                        StepResult::Done
                    }
                }
            })
            .collect();

        let mut watched: Vec<&family::Pack> = Vec::new();
        for pack in script
            .steps
            .iter()
            .map(|step| &step.pack)
            .chain(&script.watch)
        {
            if !watched
                .iter()
                .any(|seen| seen.name == pack.name && seen.version.text == pack.version.text)
            {
                watched.push(pack);
            }
        }
        let mut names: Vec<&str> = Vec::new();
        for pack in &watched {
            if !names.contains(&pack.name.as_str()) {
                names.push(&pack.name);
            }
        }
        family::StoreObservation {
            results,
            archives: std::fs::read_dir(&store.root).map_or(0, |entries| entries.count() as u64),
            packs: watched
                .iter()
                .map(|pack| {
                    let (_, header) = modelled_pack(pack);
                    let found = store
                        .lookup("gents", &pack.name, Some(&pack.version.text))
                        .unwrap();
                    family::PackView {
                        pack: (*pack).clone(),
                        contains: store.contains(&header.digest).unwrap(),
                        found: found
                            == Some(StoredName {
                                version: pack.version.text.clone(),
                                digest: header.digest.clone(),
                            }),
                        indexed: store.index_entry_path(&header).is_some_and(|entry| {
                            std::fs::read_to_string(entry)
                                .is_ok_and(|recorded| recorded.trim() == header.digest)
                        }),
                        unpacked: store
                            .unpacked_root()
                            .join(digest_hex(&header.digest).unwrap())
                            .is_dir(),
                    }
                })
                .collect(),
            latest: names
                .iter()
                .map(|name| family::Latest {
                    name: (*name).to_owned(),
                    version: store
                        .lookup("gents", name, None)
                        .unwrap()
                        .map(|stored| stored.version),
                })
                .collect(),
            names: store
                .names()
                .unwrap()
                .into_iter()
                .map(|(coordinate, versions)| family::Listing {
                    name: coordinate
                        .strip_prefix("gents/")
                        .expect("a test pack coordinate")
                        .to_owned(),
                    versions: versions.into_iter().map(|stored| stored.version).collect(),
                })
                .collect(),
        }
    });
}
