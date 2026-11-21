use super::*;
use crate::native_service::CommandOutput;
use gents_protocol::operations::install_repair::{IncompatibleStore, IncompatibleStoreKind};
use gents_store::local::store_key::STORE_ENCRYPTION_VERSION;

const LABEL: &str = "injected-reader-no-keychain-item";

fn keychain_record() -> StoreEncryption {
    StoreEncryption {
        version: STORE_ENCRYPTION_VERSION,
        custody: StoreKeyCustody::MacosKeychain {
            keychain_label: LABEL.into(),
        },
    }
}

fn file_record() -> StoreEncryption {
    StoreEncryption {
        version: STORE_ENCRYPTION_VERSION,
        custody: StoreKeyCustody::File,
    }
}

fn write_fixture(home: &Path, encryption: StoreEncryption) {
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
            store_encryption: Some(encryption),
            document_acp: Default::default(),
            client_identity: None,
        },
    )
    .unwrap();
}

fn denied(data: &Path) -> StoreKeyError {
    StoreKeyUnavailable {
        data_path: data.into(),
        code: -25293,
    }
    .into()
}

fn read_or_deny(
    readable: bool,
) -> impl FnOnce(&StoreEncryption, &Path, &Path) -> std::result::Result<StoreKey, StoreKeyError> {
    move |_, _, data| match readable {
        true => StoreKey::from_host(&[7; 32]),
        false => Err(denied(data)),
    }
}

/// The candidate side of the probe: the production store-key check of the
/// candidate's `update-check --home`, with the Keychain read injected.
struct Candidate {
    home: PathBuf,
    verified: bool,
    readable: bool,
}

impl CommandRunner for Candidate {
    fn run(&self, program: &std::ffi::OsStr, args: &[std::ffi::OsString]) -> Result<CommandOutput> {
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
        let config = read_init_config(&self.home)?.unwrap();
        Ok(
            match check_store_key_with(
                &self.home,
                config.store_encryption.as_ref(),
                read_or_deny(self.readable),
            ) {
                Ok(()) => CommandOutput {
                    success: true,
                    stdout: super::super::metadata(self.verified).to_string(),
                    stderr: String::new(),
                },
                Err(error) => CommandOutput {
                    success: false,
                    stdout: String::new(),
                    stderr: format!("{:#}", anyhow::Error::from(error)),
                },
            },
        )
    }
}

fn staged_copies(root: &Path) -> usize {
    std::fs::read_dir(root).unwrap().count()
}

#[test]
fn preflight_admission_and_registration_match_lean() {
    gents_lean_vocab::families::update_handoff_preflight::replay::<Phase>(|i| {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        write_fixture(
            home,
            match i.keychain_custody {
                true => keychain_record(),
                false => file_record(),
            },
        );
        let root = home.join("service-updates");
        std::fs::create_dir(&root).unwrap();
        let candidate = std::env::current_exe().unwrap();
        let config = read_init_config(home).unwrap().unwrap();
        let admitted = super::super::stage::prepare_with_preflight(
            &root,
            &candidate,
            &super::super::files::digest(&candidate).unwrap(),
            home,
            keychain_custody(config.store_encryption.as_ref()),
            &Candidate {
                home: home.into(),
                verified: i.verified,
                readable: i.key_readable_without_interaction,
            },
        )
        .ok();
        let Some((_copy, phase)) = admitted else {
            assert_eq!(staged_copies(&root), 0, "{i:?}");
            return gents_lean_vocab::families::update_handoff_preflight::PreflightVerdict {
                admitted: None,
                registered: None,
            };
        };
        let directory = home.join("operation");
        std::fs::create_dir(&directory).unwrap();
        let mut receipt = Receipt {
            operation_id: uuid::Uuid::new_v4(),
            node_did: config.node_did,
            phase,
            message: String::new(),
            updated_at: String::new(),
            directory: directory.clone(),
        };
        super::super::files::write_json(&directory.join("receipt.json"), &receipt).unwrap();
        let registration = super::super::register_unless_helper_runs(
            &directory,
            super::super::Registrant::Submitter,
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            registration,
            super::super::HelperRegistration::Register,
            "{i:?}"
        );
        let registered = receipt
            .transition_sync(Action::Register(true), "registered")
            .ok()
            .map(|()| {
                super::super::files::read_json::<Receipt>(&directory.join("receipt.json"))
                    .unwrap()
                    .phase
            });
        gents_lean_vocab::families::update_handoff_preflight::PreflightVerdict {
            admitted: Some(phase),
            registered,
        }
    });
}

#[test]
fn an_unreadable_store_key_refuses_before_staging_and_names_the_item() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    write_fixture(home, keychain_record());
    let root = home.join("service-updates");
    std::fs::create_dir(&root).unwrap();
    let candidate = std::env::current_exe().unwrap();
    let error = super::super::stage::prepare_with_preflight(
        &root,
        &candidate,
        &super::super::files::digest(&candidate).unwrap(),
        home,
        true,
        &Candidate {
            home: home.into(),
            verified: true,
            readable: false,
        },
    )
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<PreflightError>(),
        Some(PreflightError::CandidateRefused { .. })
    ));
    let text = format!("{error:#}");
    for fragment in [
        "without interaction",
        "approve this build",
        "runtime and service unchanged",
        &format!("account `{LABEL}`"),
        MACOS_KEYCHAIN_SERVICE,
        "-25293",
        "retry your update command",
    ] {
        assert!(text.contains(fragment), "{fragment}: {text}");
    }
    assert_eq!(
        MACOS_KEYCHAIN_SERVICE,
        "com.source-inc.gents.test.store-key"
    );
    assert_eq!(staged_copies(&root), 0);
    assert!(!gents_store::local::store_key::home_key_file(home).exists());
}

#[test]
fn keychain_reader_is_noninteractive_and_discards_the_key() {
    const CHILD: &str = "GENTS_UPDATE_KEY_PREFLIGHT_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "service_update::preflight::tests::keychain_reader_is_noninteractive_and_discards_the_key",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let home = tempfile::tempdir().unwrap();
    check_store_key_with(
        home.path(),
        Some(&keychain_record()),
        |_, key_file, data| {
            #[cfg(target_os = "macos")]
            assert!(
                !security_framework::os::macos::keychain::SecKeychain::user_interaction_allowed()
                    .unwrap()
            );
            assert_eq!(
                key_file,
                gents_store::local::store_key::home_key_file(home.path())
            );
            assert_eq!(
                data,
                gents_store::local::home::default_data_dir(home.path())
            );
            StoreKey::from_host(&[7; 32])
        },
    )
    .unwrap();
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn other_custody_does_not_read_or_create_a_key() {
    let home = tempfile::tempdir().unwrap();
    for encryption in [None, Some(&file_record())] {
        check_store_key_with(home.path(), encryption, |_, _, _| {
            panic!("non-Keychain custody must not invoke the reader")
        })
        .unwrap();
    }
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn candidate_metadata_cannot_skip_the_store_key_reader() {
    let home = tempfile::tempdir().unwrap();
    write_fixture(home.path(), keychain_record());
    let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = called.clone();
    let error = super::super::check_metadata(Some(home.path().into()), move |home, record| {
        observed.store(true, std::sync::atomic::Ordering::SeqCst);
        check_store_key_with(home, record, read_or_deny(false))
    })
    .await
    .unwrap_err();
    assert!(called.load(std::sync::atomic::Ordering::SeqCst));
    assert!(matches!(
        error.downcast_ref::<PreflightError>(),
        Some(PreflightError::StoreKey { .. })
    ));
}

fn non_access_errors(home: &Path) -> Vec<StoreKeyError> {
    let data = gents_store::local::home::default_data_dir(home);
    let mut foreign = keychain_record();
    foreign.version = STORE_ENCRYPTION_VERSION + 1;
    vec![
        StoreKeyError::Incompatible(IncompatibleStore {
            kind: IncompatibleStoreKind::MissingStoreKey,
            data_path: data.clone(),
        }),
        foreign
            .load(&gents_store::local::store_key::home_key_file(home), &data)
            .unwrap_err(),
        StoreKey::from_host(&[0; 31]).unwrap_err(),
    ]
}

#[tokio::test]
async fn non_access_key_errors_keep_their_text_without_approval_advice() {
    let home = tempfile::tempdir().unwrap();
    write_fixture(home.path(), keychain_record());
    for source in non_access_errors(home.path()) {
        let expected = source.to_string();
        let error = super::super::check_metadata(Some(home.path().into()), move |home, record| {
            check_store_key_with(home, record, |_, _, _| Err(source))
        })
        .await
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<PreflightError>(),
            Some(PreflightError::StoreKeyRead(_))
        ));
        assert_eq!(error.to_string(), expected);
        assert!(!format!("{error:#}").contains("approve"));
    }
}

#[tokio::test]
async fn repair_live_update_propagates_missing_and_foreign_key_errors() {
    let home = tempfile::tempdir().unwrap();
    write_fixture(home.path(), keychain_record());
    for source in non_access_errors(home.path()) {
        let expected = source.to_string();
        let error = super::super::repair_preflight(home.path(), move |home, record| {
            check_store_key_with(home, record, |_, _, _| Err(source))
        })
        .await
        .expect_err("repair must stop, not silently skip an unusable store key");
        assert_eq!(error.to_string(), expected);
    }
    let mut config = read_init_config(home.path()).unwrap().unwrap();
    config.store_encryption.as_mut().unwrap().version = STORE_ENCRYPTION_VERSION + 1;
    crate::home_state::write_init_config(home.path(), &config).unwrap();
    let error = super::super::repair_preflight(home.path(), check_store_key)
        .await
        .expect_err("repair must stop, not silently skip a foreign key format");
    assert!(matches!(
        error.downcast_ref::<PreflightError>(),
        Some(PreflightError::StoreKeyRead(source))
            if source.incompatible().is_some_and(|store| store.kind == IncompatibleStoreKind::ForeignVersion)
    ));
}
