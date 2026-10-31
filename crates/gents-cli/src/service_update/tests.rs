use super::*;

#[test]
fn listener_binding_rejects_redirects_other_hosts_and_public_wildcards() {
    let settings = NetworkSettings {
        http_port: Some(9191),
        ..Default::default()
    };
    assert!(execute::endpoint("http://127.0.0.1:9191/api/v0/graphql", &settings).is_ok());
    for raw in [
        "http://localhost:9191/api/v0/graphql",
        "http://127.0.0.1:9191/api/v0/graphql?x=1",
        "http://127.0.0.1:9192/api/v0/graphql",
        "http://192.0.2.7:9191/api/v0/graphql",
        "http://user@127.0.0.1:9191/api/v0/graphql",
        "http://0.0.0.0:9191/api/v0/graphql",
    ] {
        assert!(execute::endpoint(raw, &settings).is_err(), "{raw}");
    }
    let settings = NetworkSettings {
        http_addr: Some("192.0.2.7".parse().unwrap()),
        http_port: Some(9191),
        ..Default::default()
    };
    assert!(execute::endpoint("http://192.0.2.7:9191/api/v0/graphql", &settings).is_ok());
}

#[tokio::test]
async fn durable_receipts_enforce_acknowledgement_and_backup_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut receipt = Receipt {
        operation_id: uuid::Uuid::new_v4(),
        node_did: "node".into(),
        session_id: Some("session".into()),
        request_id: Some("request".into()),
        phase: Phase::Staged,
        message: String::new(),
        updated_at: String::new(),
        directory: dir.path().into(),
    };
    files::write_json(&dir.path().join("receipt.json"), &receipt).unwrap();
    assert!(receipt
        .transition(Action::Retire(true), "unsafe")
        .await
        .is_err());
    receipt
        .transition(Action::Register(true), "registered")
        .await
        .unwrap();
    assert!(receipt
        .transition(Action::Retire(true), "unsafe")
        .await
        .is_err());
    receipt
        .transition(Action::Acknowledge, "accepted")
        .await
        .unwrap();
    receipt
        .transition(Action::Retire(true), "idle")
        .await
        .unwrap();
    assert!(receipt
        .transition(Action::Install(true), "unsafe")
        .await
        .is_err());
    let saved: Receipt = files::read_json(&dir.path().join("receipt.json")).unwrap();
    assert_eq!(saved.phase, Phase::Retired);
    assert_eq!(saved.session_id.as_deref(), Some("session"));
}

#[tokio::test]
async fn stale_acknowledgement_cannot_resurrect_a_failed_operation() {
    let dir = tempfile::tempdir().unwrap();
    let mut receipt = Receipt {
        operation_id: uuid::Uuid::new_v4(),
        node_did: "node".into(),
        session_id: Some("session".into()),
        request_id: Some("request".into()),
        phase: Phase::Registered,
        message: String::new(),
        updated_at: String::new(),
        directory: dir.path().into(),
    };
    files::write_json(&dir.path().join("receipt.json"), &receipt).unwrap();
    let mut stale = receipt.clone();
    receipt
        .transition(Action::Fail, "acknowledgement expired")
        .await
        .unwrap();
    assert!(stale
        .transition(Action::Acknowledge, "accepted")
        .await
        .is_err());
    assert_eq!(stale.phase, Phase::Registered);
    let saved: Receipt = files::read_json(&dir.path().join("receipt.json")).unwrap();
    assert_eq!(saved.phase, Phase::Failed);
    assert_eq!(saved.message, "acknowledgement expired");
}

#[test]
fn operation_lock_survives_receipt_replacement_and_excludes_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("helper.lock");
    let first = files::lock(&path).unwrap();
    assert!(files::lock(&path).is_err());
    files::write_json(
        &dir.path().join("receipt.json"),
        &serde_json::json!({"phase":"accepted"}),
    )
    .unwrap();
    assert!(files::lock(&path).is_err());
    drop(first);
    assert!(files::lock(&path).is_ok());
}

#[test]
fn supported_service_commands_refuse_disruption_only_for_the_calling_home() {
    let home = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    for action in [
        ServiceAction::Stop {
            keep_enabled: false,
        },
        ServiceAction::Restart,
        ServiceAction::Install {
            network: Default::default(),
            environment_file: None,
        },
        ServiceAction::Uninstall,
    ] {
        assert!(refuse_origin_disruption(home.path(), Some(home.path()), &action).is_err());
        assert!(refuse_origin_disruption(home.path(), Some(other.path()), &action).is_ok());
        assert!(refuse_origin_disruption(home.path(), None, &action).is_ok());
    }
    assert!(
        refuse_origin_disruption(home.path(), Some(home.path()), &ServiceAction::Status).is_ok()
    );
}

#[cfg(unix)]
#[test]
fn cold_copy_preserves_key_permissions_and_does_not_follow_symlinks() {
    use std::os::unix::fs::{symlink, PermissionsExt as _};
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("key"), b"retained key").unwrap();
    std::fs::set_permissions(source.join("key"), std::fs::Permissions::from_mode(0o600)).unwrap();
    symlink("/outside/key", source.join("external")).unwrap();
    let backup = dir.path().join("backup");
    files::copy_tree(&source, &backup).unwrap();
    assert_eq!(std::fs::read(backup.join("key")).unwrap(), b"retained key");
    assert_eq!(
        std::fs::metadata(backup.join("key"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::read_link(backup.join("external")).unwrap(),
        PathBuf::from("/outside/key")
    );
    assert!(std::fs::symlink_metadata(source.join("key"))
        .unwrap()
        .is_file());
}

#[tokio::test]
async fn broken_acknowledgement_cannot_release_the_updater() {
    let dir = tempfile::tempdir().unwrap();
    let mut receipt = Receipt {
        operation_id: uuid::Uuid::new_v4(),
        node_did: "node".into(),
        session_id: Some("session".into()),
        request_id: Some("request".into()),
        phase: Phase::Registered,
        message: "registered".into(),
        updated_at: String::new(),
        directory: dir.path().into(),
    };
    files::write_json(&dir.path().join("receipt.json"), &receipt).unwrap();
    let (mut writer, reader) = tokio::io::duplex(1024);
    drop(reader);
    assert!(acknowledge(&mut receipt, &mut writer).await.is_err());
    assert_eq!(
        files::read_json::<Receipt>(&dir.path().join("receipt.json"))
            .unwrap()
            .phase,
        Phase::Registered
    );
    let (mut writer, mut reader) = tokio::io::duplex(4096);
    acknowledge(&mut receipt, &mut writer).await.unwrap();
    drop(writer);
    use tokio::io::AsyncReadExt as _;
    let mut output = Vec::new();
    reader.read_to_end(&mut output).await.unwrap();
    let returned: Receipt = serde_json::from_slice(&output).unwrap();
    assert_eq!(returned.phase, Phase::Accepted);
    assert_eq!(
        files::read_json::<Receipt>(&dir.path().join("receipt.json"))
            .unwrap()
            .phase,
        Phase::Accepted
    );
}

#[test]
fn candidate_preflight_rejects_scripts_before_execution() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("gents");
    std::fs::write(&script, b"#!/bin/sh\necho fake managed update metadata\n").unwrap();
    assert!(files::validate_native_executable(&script).is_err());
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        assert!(files::validate_native_executable(&std::env::current_exe().unwrap()).is_ok());
    }
}

fn fixture_plan(home: &Path) -> Plan {
    let id = uuid::Uuid::new_v4();
    let directory = operation_dir(home, id);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("gents"), b"fixture").unwrap();
    Plan {
        version: 1,
        id,
        home: home.into(),
        node_did: "fixture-node".into(),
        session_id: Some("original-session".into()),
        request_id: Some("original-request".into()),
        endpoint: "http://127.0.0.1:9191/api/v0/graphql".into(),
        prior_build: "0".repeat(64),
        intent: Intent {
            digest: files::digest(&directory.join("gents")).unwrap(),
            executable_update: false,
            network: Default::default(),
            environment: None,
            drain_timeout_secs: 1,
        },
        prior_network: Default::default(),
        prior_definition: b"prior".to_vec(),
        next_definition: b"next".to_vec(),
        enabled: false,
        accepted_deadline: (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
        p2p_enabled: false,
        prior_peer_id: None,
        service_config_dir: home.join("missing-service"),
        user_home: home.into(),
    }
}
fn initialize_fixture_identity(plan: &mut Plan) {
    use gents::identity::NodeIdentity as _;
    let key = plan.home.join("fixture.key");
    let identity = gents::identity::KeyIdentity::load_or_create(&key, None).unwrap();
    plan.node_did = identity.did().to_owned();
    crate::home_state::write_init_config(
        &plan.home,
        &crate::shared::StoredInitConfig {
            home: plan.home.to_string_lossy().into_owned(),
            node_name: "fixture".into(),
            node_did: plan.node_did.clone(),
            key_path: Some(key.to_string_lossy().into_owned()),
            identity_backend: Some("file".into()),
            keychain_label: None,
            secure_enclave_label: None,
            tool_package: None,
            tool_ceiling: crate::cli::args::ToolCeilingArg::Readwrite,
            tool_root: Some(plan.home.to_string_lossy().into_owned()),
            store_encryption: None,
        },
    )
    .unwrap();
}

fn fixture_receipt(plan: &Plan, phase: Phase) -> Receipt {
    Receipt {
        operation_id: plan.id,
        node_did: plan.node_did.clone(),
        session_id: plan.session_id.clone(),
        request_id: plan.request_id.clone(),
        phase,
        message: String::new(),
        updated_at: String::new(),
        directory: operation_dir(&plan.home, plan.id),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn startup_failure_retains_cold_backup_and_session_outcome_without_resetting_data() {
    let home = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(home.path()).unwrap();
    let mut plan = fixture_plan(&home);
    initialize_fixture_identity(&mut plan);
    let directory = operation_dir(&home, plan.id);
    let executable = directory.join("gents");
    plan.prior_definition = if cfg!(target_os = "macos") {
        format!(r#"<?xml version="1.0"?><plist version="1.0"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array><string>{}</string><string>server</string><string>--home</string><string>{}</string></array></dict></plist>"#, crate::native_service::SERVICE_LABEL, executable.display(), home.display()).into_bytes()
    } else {
        format!(
            "[Service]\nExecStart=\"{}\" \"server\" \"--home\" \"{}\"\n",
            executable.display(),
            home.display()
        )
        .into_bytes()
    };
    plan.next_definition = plan.prior_definition.clone();

    std::fs::create_dir_all(directory.join("backup/data")).unwrap();
    std::fs::write(directory.join("backup/data/retained"), b"cold store").unwrap();
    std::fs::create_dir(home.join("data")).unwrap();
    std::fs::write(home.join("data/retained"), b"live store").unwrap();
    files::write_json(&directory.join("plan.json"), &plan).unwrap();
    files::write_json(
        &directory.join("receipt.json"),
        &fixture_receipt(&plan, Phase::Installed),
    )
    .unwrap();
    run(home.clone(), plan.id).await.unwrap();
    let outcome: Receipt = files::read_json(&directory.join("receipt.json")).unwrap();
    assert_eq!(outcome.phase, Phase::Failed);
    assert_eq!(outcome.session_id.as_deref(), Some("original-session"));
    assert!(outcome.message.contains("No data reset"));
    assert!(!outcome.message.contains("initialized home missing"));
    assert!(
        outcome
            .message
            .contains("reading installed service definition before startup"),
        "{}",
        outcome.message
    );
    assert!(directory.join("startup-attempted.json").is_file());
    assert_eq!(
        std::fs::read(home.join("data/retained")).unwrap(),
        b"live store"
    );
    assert_eq!(
        std::fs::read(directory.join("backup/data/retained")).unwrap(),
        b"cold store"
    );
    run(home, plan.id).await.unwrap();
    assert_eq!(
        files::read_json::<Receipt>(&directory.join("receipt.json"))
            .unwrap()
            .updated_at,
        outcome.updated_at
    );
}

#[tokio::test]
async fn missing_acknowledgement_expires_without_retirement_or_store_access() {
    let home = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(home.path()).unwrap();
    let plan = fixture_plan(&home);
    let directory = operation_dir(&home, plan.id);
    files::write_json(&directory.join("plan.json"), &plan).unwrap();
    files::write_json(
        &directory.join("receipt.json"),
        &fixture_receipt(&plan, Phase::Registered),
    )
    .unwrap();
    run(home.clone(), plan.id).await.unwrap();
    assert_eq!(
        files::read_json::<Receipt>(&directory.join("receipt.json"))
            .unwrap()
            .phase,
        Phase::Failed
    );
    assert!(!home.join("data").exists());
    assert!(!directory.join("backup").exists());
}

#[tokio::test]
async fn helper_recovery_cannot_extend_a_busy_or_unhealthy_operation_forever() {
    let dir = tempfile::tempdir().unwrap();
    let first = execute::deadline(dir.path(), "drain", 1).await.unwrap();
    assert_eq!(
        first,
        execute::deadline(dir.path(), "drain", 3600).await.unwrap()
    );
    let expired = chrono::Utc::now() - chrono::Duration::seconds(1);
    files::write_json(
        &dir.path().join("health-deadline.json"),
        &expired.to_rfc3339(),
    )
    .unwrap();
    assert!(execute::deadline(dir.path(), "health", 120).await.unwrap() < chrono::Utc::now());
}

#[tokio::test]
async fn replacement_cannot_back_up_or_install_over_a_claimed_store() {
    let home = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(home.path()).unwrap();
    let mut plan = fixture_plan(&home);
    initialize_fixture_identity(&mut plan);
    let directory = operation_dir(&home, plan.id);
    std::fs::create_dir(home.join("data")).unwrap();
    let _store = gents::home::lock_home_store(&home).unwrap();
    files::write_json(&directory.join("plan.json"), &plan).unwrap();
    files::write_json(
        &directory.join("receipt.json"),
        &fixture_receipt(&plan, Phase::Released),
    )
    .unwrap();
    run(home, plan.id).await.unwrap();
    let outcome: Receipt = files::read_json(&directory.join("receipt.json")).unwrap();
    assert_eq!(outcome.phase, Phase::Failed);
    assert!(
        outcome.message.contains("already using"),
        "{}",
        outcome.message
    );
    assert!(
        outcome
            .message
            .contains(&format!("process {}", std::process::id())),
        "{}",
        outcome.message
    );
    assert!(!outcome.message.contains("initialized home missing"));
    assert!(!directory.join("backup").exists());
    assert!(!plan.service_config_dir.exists());
}

#[cfg(unix)]
#[test]
fn cold_backup_copies_the_claimed_store_even_when_its_directory_is_a_symlink() {
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(data.path().join("retained"), b"actual store bytes").unwrap();
    std::os::unix::fs::symlink(data.path(), home.path().join("data")).unwrap();
    let plan = fixture_plan(home.path());
    let directory = operation_dir(home.path(), plan.id);
    let _store = gents::home::lock_home_store(home.path()).unwrap();
    files::cold_backup(&plan, &directory).unwrap();
    assert!(std::fs::symlink_metadata(directory.join("backup/data"))
        .unwrap()
        .is_dir());
    assert_eq!(
        std::fs::read(directory.join("backup/data/retained")).unwrap(),
        b"actual store bytes"
    );
    assert_eq!(
        std::fs::read(data.path().join("retained")).unwrap(),
        b"actual store bytes"
    );
}

#[test]
fn login_enablement_can_change_without_disrupting_the_calling_runtime() {
    let home = tempfile::tempdir().unwrap();
    for action in [ServiceAction::Enable, ServiceAction::Disable] {
        assert!(refuse_origin_disruption(home.path(), Some(home.path()), &action).is_ok());
    }
}

#[test]
fn unavailable_peer_status_cannot_report_a_healthy_isolated_node() {
    assert!(!execute::peers_reconnected(
        &[],
        Err(anyhow::anyhow!("peer API unavailable"))
    ));
    assert!(execute::peers_reconnected(&[], Ok(Vec::new())));
    assert!(!execute::peers_reconnected(
        &["expected".into()],
        Ok(Vec::new())
    ));
    assert!(execute::peers_reconnected(
        &["expected".into()],
        Ok(vec!["expected".into()])
    ));
}

#[tokio::test]
async fn acceptance_requires_the_registered_helper_to_acknowledge_startup() {
    let home = tempfile::tempdir().unwrap();
    let plan = fixture_plan(home.path());
    assert!(wait_for_helper(&plan, Duration::from_millis(1))
        .await
        .is_err());
    let directory = operation_dir(home.path(), plan.id);
    files::write_json(
        &directory.join("helper-ready.json"),
        &HelperReady {
            operation_id: uuid::Uuid::new_v4(),
            pid: 1,
        },
    )
    .unwrap();
    assert!(wait_for_helper(&plan, Duration::from_secs(1))
        .await
        .is_err());
    files::write_json(
        &directory.join("helper-ready.json"),
        &HelperReady {
            operation_id: plan.id,
            pid: 1,
        },
    )
    .unwrap();
    wait_for_helper(&plan, Duration::from_secs(1))
        .await
        .unwrap();
    assert!(!home.path().join("data").exists());
}

#[test]
fn abandoned_unpublished_staging_does_not_block_updates() {
    let root = tempfile::tempdir().unwrap();
    let orphan = root.path().join("stage-unpublished");
    std::fs::create_dir(&orphan).unwrap();
    std::fs::write(orphan.join("receipt.json"), "invalid partial JSON").unwrap();
    stage::reject_pending(root.path()).unwrap();
}

#[test]
fn abandoned_pre_acceptance_expires_but_accepted_operations_remain_owned() {
    let home = tempfile::tempdir().unwrap();
    let plan = fixture_plan(home.path());
    let directory = operation_dir(home.path(), plan.id);
    files::write_json(&directory.join("plan.json"), &plan).unwrap();
    files::write_json(
        &directory.join("receipt.json"),
        &fixture_receipt(&plan, Phase::Registered),
    )
    .unwrap();
    let root = directory.parent().unwrap();
    stage::reject_pending(root).unwrap();
    assert_eq!(
        files::read_json::<Receipt>(&directory.join("receipt.json"))
            .unwrap()
            .phase,
        Phase::Failed
    );
    files::write_json(
        &directory.join("receipt.json"),
        &fixture_receipt(&plan, Phase::Accepted),
    )
    .unwrap();
    assert!(stage::reject_pending(root).is_err());
    assert_eq!(
        files::read_json::<Receipt>(&directory.join("receipt.json"))
            .unwrap()
            .phase,
        Phase::Accepted
    );
}

#[tokio::test]
async fn recovery_refuses_any_candidate_start_attempt_even_without_readiness() {
    let home = tempfile::tempdir().unwrap();
    let plan = fixture_plan(home.path());
    let directory = operation_dir(home.path(), plan.id);
    files::write_json(&directory.join("startup-attempted.json"), &true).unwrap();
    let mut reservation = Some(
        gents::home::reserve_runtime_startup(home.path())
            .await
            .unwrap(),
    );
    assert!(
        !recovery::restore_unstarted(&plan, Phase::Installed, &mut reservation)
            .await
            .unwrap()
    );
    assert!(reservation.is_some());
    assert!(!plan.service_config_dir.exists());
}

#[tokio::test]
async fn file_notifications_observe_atomic_receipt_replacement() {
    let root = tempfile::tempdir().unwrap();
    let mut changes = files::Changes::watch(root.path()).await.unwrap();
    files::write_json(&root.path().join("receipt.json"), &true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), changes.changed())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn exposed_candidate_is_a_rollback_fence_even_without_a_startup_marker() {
    let home = tempfile::tempdir().unwrap();
    let mut plan = fixture_plan(home.path());
    plan.intent.executable_update = true;
    let directory = operation_dir(home.path(), plan.id);
    files::write_json(&directory.join("candidate-exposed.json"), &true).unwrap();
    let mut reservation = Some(
        gents::home::reserve_runtime_startup(home.path())
            .await
            .unwrap(),
    );
    assert!(
        !recovery::restore_unstarted(&plan, Phase::BackedUp, &mut reservation)
            .await
            .unwrap()
    );
    assert!(reservation.is_some());
    assert!(!plan.service_config_dir.exists());
}

#[tokio::test]
async fn health_resume_gets_one_durable_bounded_grace_not_an_unbounded_extension() {
    let directory = tempfile::tempdir().unwrap();
    let expired = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    files::write_json(&directory.path().join("health-deadline.json"), &expired).unwrap();
    assert!(
        execute::health_deadline(directory.path(), false)
            .await
            .unwrap()
            < chrono::Utc::now()
    );
    let first = execute::health_deadline(directory.path(), true)
        .await
        .unwrap();
    assert!(first > chrono::Utc::now());
    assert_eq!(
        first,
        execute::health_deadline(directory.path(), true)
            .await
            .unwrap()
    );
    files::write_json(
        &directory.path().join("health-resume-deadline.json"),
        &expired,
    )
    .unwrap();
    assert!(
        execute::health_deadline(directory.path(), true)
            .await
            .unwrap()
            < chrono::Utc::now()
    );
}

#[test]
fn legacy_runtime_cannot_begin_an_unfenced_managed_handoff() {
    let mut capabilities = gents_protocol::serve_lifecycle::control::current_capabilities();
    execute::require_startup_fence(&capabilities).unwrap();
    capabilities.remove(gents_protocol::serve_lifecycle::control::NATIVE_STARTUP_FENCE_CAPABILITY);
    let error = execute::require_startup_fence(&capabilities).unwrap_err();
    assert!(error.to_string().contains("external administration"));
    capabilities.insert(
        gents_protocol::serve_lifecycle::control::NATIVE_STARTUP_FENCE_CAPABILITY.into(),
        2,
    );
    assert!(execute::require_startup_fence(&capabilities).is_err());
}
