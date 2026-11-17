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
    let result = super::super::stage::prepare_with_probe(
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
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("untrusted candidate must be refused"),
    };
    assert!(matches!(
        error.downcast_ref::<PreflightError>(),
        Some(PreflightError::StoreKey { .. })
    ));
    let text = format!("{error:#}");
    for fragment in [
        "without interaction",
        "approve this build",
        "Runtime and service unchanged",
        "account `injected-reader-no-keychain-item`",
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

fn write_fixture(home: &Path) {
    crate::home_state::write_init_config(
        home,
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
            store_encryption: Some(keychain_record()),
        },
    )
    .unwrap();
}

struct ChildRunner {
    home: PathBuf,
    output: crate::native_service::CommandOutput,
    calls: std::sync::atomic::AtomicUsize,
}
impl CommandRunner for ChildRunner {
    fn run(
        &self,
        program: &std::ffi::OsStr,
        args: &[std::ffi::OsString],
    ) -> Result<crate::native_service::CommandOutput> {
        assert!(Path::new(program).is_file());
        assert_eq!(
            args,
            [
                "service".into(),
                "update-check".into(),
                "--home".into(),
                self.home.as_os_str().to_owned()
            ]
        );
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(crate::native_service::CommandOutput {
            success: self.output.success,
            stdout: self.output.stdout.clone(),
            stderr: self.output.stderr.clone(),
        })
    }
}

fn prepared_probe(
    home: &Path,
    output: crate::native_service::CommandOutput,
    keychain: bool,
) -> Result<(tempfile::TempDir, Phase)> {
    let root = home.join("probe-copies");
    std::fs::create_dir_all(&root).unwrap();
    let candidate = std::env::current_exe().unwrap();
    let runner = ChildRunner {
        home: home.into(),
        output,
        calls: Default::default(),
    };
    let result = super::super::stage::prepare_with_preflight(
        &root,
        &candidate,
        &super::super::files::digest(&candidate).unwrap(),
        home,
        keychain,
        &mut |copy, home, keychain, original| {
            probe(&runner, copy, home, keychain, original).map_err(Into::into)
        },
    );
    assert_eq!(runner.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    if result.is_err() {
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
    }
    assert!(!home
        .join(gents_store::local::home::SERVICE_UPDATES_DIR_NAME)
        .exists());
    result
}

#[test]
fn real_probe_refuses_nonzero_and_missing_attestation_and_admits_attestation() {
    let home = tempfile::tempdir().unwrap();
    write_fixture(home.path());
    let mut metadata = super::super::metadata(true);
    let error = prepared_probe(
        home.path(),
        crate::native_service::CommandOutput {
            success: false,
            stdout: metadata.to_string(),
            stderr: "schema migration failed on Linux".into(),
        },
        true,
    )
    .unwrap_err();
    let text = error.to_string();
    assert!(text.contains("schema migration failed on Linux"));
    assert!(!text.contains("Keychain"));
    assert!(!text.contains("approve"));
    let error = prepared_probe(
        home.path(),
        crate::native_service::CommandOutput {
            success: true,
            stdout: metadata.to_string(),
            stderr: String::new(),
        },
        true,
    )
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<PreflightError>(),
        Some(PreflightError::MissingStoreKeyProof { .. })
    ));
    metadata["keychain_store_key_checked"] = serde_json::json!(true);
    let (_, phase) = prepared_probe(
        home.path(),
        crate::native_service::CommandOutput {
            success: true,
            stdout: metadata.to_string(),
            stderr: String::new(),
        },
        true,
    )
    .unwrap();
    assert_eq!(phase, Phase::Staged);
    assert_eq!(phase.step(Action::Register(true)), Some(Phase::Registered));
}

#[test]
fn real_probe_preserves_structural_store_key_cause_and_original_path() {
    let home = tempfile::tempdir().unwrap();
    write_fixture(home.path());
    let child_error: anyhow::Error = store_key_error(
        home.path(),
        Path::new("/deleted/stage/gents"),
        "untrusted-child-label".into(),
        StoreKeyCause::Candidate {
            detail: "authorization denied (-25293)".into(),
        },
    )
    .into();
    let error = prepared_probe(
        home.path(),
        crate::native_service::CommandOutput {
            success: false,
            stdout: failure_metadata(&child_error).unwrap().to_string(),
            stderr: child_error.to_string(),
        },
        true,
    )
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<PreflightError>(),
        Some(PreflightError::StoreKey { .. })
    ));
    let text = format!("{error:#}");
    assert!(text.contains(&std::env::current_exe().unwrap().display().to_string()));
    assert!(text.contains("account `injected-reader-no-keychain-item`"));
    assert!(text.contains("com.source-inc.gents.store-key"));
    assert!(text.contains("-25293"));
    assert!(text.contains("Runtime and service unchanged"));
    assert!(!text.contains("/deleted/stage"));
    assert!(!text.contains("untrusted-child-label"));
    assert!(!text.contains("re-sign"));
}

#[test]
fn candidate_metadata_cannot_skip_the_store_key_reader() {
    if !in_child(concat!(
        module_path!(),
        "::candidate_metadata_cannot_skip_the_store_key_reader"
    )) {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    write_fixture(home.path());
    let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = called.clone();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let error = runtime
        .block_on(super::super::check_metadata_with_reader(
            Some(home.path().into()),
            move |home, record| {
                observed.store(true, std::sync::atomic::Ordering::SeqCst);
                check_store_key_with(home, record, |_, _, data| {
                    assert_no_keychain_interaction();
                    Err(StoreKeyUnavailable {
                        data_path: data.into(),
                        code: -25293,
                    }
                    .into())
                })
            },
        ))
        .unwrap_err();
    assert!(called.load(std::sync::atomic::Ordering::SeqCst));
    assert!(failure_metadata(&error).is_some());
    // If check_metadata replaces the reader with literal true, it instead hits
    // the absent runtime endpoint: both assertions above fail (no identity read).
}

#[test]
fn real_probe_and_receipt_admission_order_match_lean() {
    gents_lean_vocab::families::update_handoff_preflight::replay::<Phase>(|i| {
        let home = tempfile::tempdir().unwrap();
        let mut metadata = super::super::metadata(i.verified);
        metadata["keychain_store_key_checked"] =
            serde_json::json!(i.key_readable_without_interaction);
        let observed = prepared_probe(
            home.path(),
            crate::native_service::CommandOutput {
                success: true,
                stdout: metadata.to_string(),
                stderr: String::new(),
            },
            i.keychain_custody,
        )
        .ok();
        let admitted = observed.as_ref().map(|(_, phase)| *phase);
        gents_lean_vocab::families::update_handoff_preflight::PreflightVerdict {
            admitted,
            registered: admitted.and_then(|phase| phase.step(Action::Register(true))),
        }
    });
}
