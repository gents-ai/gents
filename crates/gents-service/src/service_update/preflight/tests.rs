use super::*;
use gents_store::local::store_key::{StoreKeyUnavailable, STORE_ENCRYPTION_VERSION};

fn keychain_record() -> StoreEncryption {
    StoreEncryption {
        version: STORE_ENCRYPTION_VERSION,
        custody: StoreKeyCustody::MacosKeychain {
            keychain_label: "injected-reader-no-keychain-item".into(),
        },
    }
}

fn in_child(test: &str) -> bool {
    let test = test
        .split_once("::")
        .expect("test module has a crate prefix")
        .1;
    const MARKER: &str = "GENTS_UPDATE_KEY_PREFLIGHT_TEST";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(MARKER, test)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    false
}

fn assert_no_keychain_interaction() {
    #[cfg(target_os = "macos")]
    assert!(
        !security_framework::os::macos::keychain::SecKeychain::user_interaction_allowed().unwrap()
    );
}

#[test]
fn candidate_keychain_refusal_precedes_staging_without_touching_runtime() {
    if !in_child(concat!(
        module_path!(),
        "::candidate_keychain_refusal_precedes_staging_without_touching_runtime"
    )) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(temp.path()).unwrap();
    let encryption = keychain_record();
    crate::home_state::write_init_config(
        &home,
        &crate::state::StoredInitConfig {
            home: home.to_string_lossy().into_owned(),
            node_name: "preflight-fixture".into(),
            node_did: "fixture-node".into(),
            key_path: None,
            identity_backend: Some("file".into()),
            keychain_label: None,
            secure_enclave_label: None,
            tool_package: None,
            tool_ceiling: crate::args::ToolCeilingArg::Readwrite,
            tool_root: Some(home.to_string_lossy().into_owned()),
            store_encryption: Some(encryption.clone()),
        },
    )
    .unwrap();
    let runtime = home.join("runtime-sentinel");
    let service = home.join("service-sentinel");
    std::fs::write(&runtime, b"running prior runtime").unwrap();
    std::fs::write(&service, b"installed prior service").unwrap();
    let init_before = std::fs::read(home.join("init.json")).unwrap();
    let candidate = std::env::current_exe().unwrap();
    let id = uuid::Uuid::new_v4();
    let args = crate::args::ServiceUpdateArgs {
        home: Some(home.clone()),
        executable: Some(candidate.clone()),
        sha256: Some(super::super::files::digest(&candidate).unwrap()),
        operation_id: Some(id),
        network: Default::default(),
        environment_file: None,
        drain_timeout_secs: 300,
    };
    let mut reads = 0;
    let result = super::super::stage::prepare_with_preflight(
        home.clone(),
        id,
        args,
        None,
        |executable, checked_home, keychain, original| {
            assert_eq!(original, candidate);
            assert_ne!(executable, candidate);
            assert_eq!(
                super::super::files::digest(executable).unwrap(),
                super::super::files::digest(&candidate).unwrap()
            );
            assert_eq!(checked_home, home);
            assert!(keychain);
            check_store_key_with(checked_home, Some(&encryption), |_, _, data| {
                reads += 1;
                assert_no_keychain_interaction();
                Err(StoreKeyUnavailable {
                    data_path: data.to_path_buf(),
                    code: -25293,
                }
                .into())
            })?;
            panic!("an unreadable candidate must not pass preflight")
        },
    );
    let error = result.err().expect("untrusted candidate must be refused");
    assert!(matches!(
        error.downcast_ref::<PreflightError>(),
        Some(PreflightError::StoreKey { .. })
    ));
    let text = format!("{error:#}");
    for fragment in [
        "Keychain prompt",
        "not be trusted",
        "Runtime and service unchanged",
        "Keychain Access",
        "service update-check --home",
        "-25293",
    ] {
        assert!(text.contains(fragment), "{text}");
    }
    assert_eq!(reads, 1);
    assert_eq!(std::fs::read(&runtime).unwrap(), b"running prior runtime");
    assert_eq!(std::fs::read(&service).unwrap(), b"installed prior service");
    assert_eq!(std::fs::read(home.join("init.json")).unwrap(), init_before);
    let root = home.join(gents_store::local::home::SERVICE_UPDATES_DIR_NAME);
    let entries: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, [std::ffi::OsString::from("submission.lock")]);
    assert!(!super::super::operation_dir(&home, id).exists());
    assert!(!gents_store::local::store_key::home_key_file(&home).exists());
}

#[test]
fn keychain_reader_is_noninteractive_and_discards_the_key() {
    if !in_child(concat!(
        module_path!(),
        "::keychain_reader_is_noninteractive_and_discards_the_key"
    )) {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let record = keychain_record();
    assert!(
        check_store_key_with(home.path(), Some(&record), |_, key_file, data| {
            assert_no_keychain_interaction();
            assert_eq!(
                key_file,
                gents_store::local::store_key::home_key_file(home.path())
            );
            assert_eq!(
                data,
                gents_store::local::home::default_data_dir(home.path())
            );
            StoreKey::from_host(&[7; 32])
        })
        .unwrap()
    );
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn other_custody_does_not_read_or_create_a_key() {
    let record = StoreEncryption {
        version: STORE_ENCRYPTION_VERSION,
        custody: StoreKeyCustody::File,
    };
    let home = tempfile::tempdir().unwrap();
    for encryption in [None, Some(&record)] {
        assert!(!check_store_key_with(home.path(), encryption, |_, _, _| {
            panic!("non-Keychain custody must not invoke the reader")
        })
        .unwrap());
    }
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn old_candidate_metadata_cannot_attest_to_keychain_readability() {
    let home = Path::new("/fixture-home");
    let mut metadata = super::super::metadata(true);
    assert!(matches!(
        admit(&metadata, home, true),
        Err(PreflightError::MissingStoreKeyProof { .. })
    ));
    assert_eq!(admit(&metadata, home, false).unwrap(), Phase::Staged);
    metadata["keychain_store_key_checked"] = serde_json::json!(false);
    assert!(admit(&metadata, home, true).is_err());
    metadata["keychain_store_key_checked"] = serde_json::json!(true);
    assert_eq!(admit(&metadata, home, true).unwrap(), Phase::Staged);
    metadata["schema_checked"] = serde_json::json!(false);
    assert!(matches!(
        admit(&metadata, home, true),
        Err(PreflightError::Incompatible)
    ));
}

#[test]
fn remedy_command_quotes_executable_and_actual_home() {
    assert_eq!(
        check_command(
            Path::new("/candidate dir/gents"),
            Path::new("/home/John's node")
        ),
        "'/candidate dir/gents' runtime service update-check --home '/home/John'\"'\"'s node'"
    );
}
