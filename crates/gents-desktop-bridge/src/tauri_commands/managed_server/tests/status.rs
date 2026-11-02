use super::*;

#[test]
fn status_priority_is_starting_then_failed_then_stopped_then_disabled() {
    let stored = StoredManagedServer {
        node_name: "local".to_string(),
        tool_ceiling: Some(ManagedServerToolCeiling::Readwrite),
        tool_root: Some("/Users/test".to_string()),
        reviewed_for: None,
    };
    let mut runtime = ManagedServerRuntimeState {
        starting: true,
        last_error: Some("boom".to_string()),
        ..Default::default()
    };
    assert_eq!(
        status_from(&runtime, Some(&stored), None, None, None).state,
        ManagedServerState::Starting
    );
    runtime.starting = false;
    assert_eq!(
        status_from(&runtime, Some(&stored), None, None, None).state,
        ManagedServerState::Failed
    );
    runtime.last_error = None;
    let installed = gents_server::native_service::NativeServiceStatus {
        installed: true,
        running: false,
        job_loaded: false,
        enabled: true,
        requires_approval: false,
        background_item: Default::default(),
        failed: false,
        detail: None,
    };
    assert_eq!(
        status_from(&runtime, Some(&stored), Some(&installed), None, None).state,
        ManagedServerState::Stopped
    );
    assert_eq!(
        status_from(&runtime, None, None, None, None).state,
        ManagedServerState::Disabled
    );
}

#[test]
fn idle_status_reprobes_and_preserves_an_external_runtime_identity() {
    let external = project_external_status(
        ManagedServerStatus {
            state: ManagedServerState::External,
            auto_start: false,
            node_name: Some("local".to_string()),
            node_did: Some("did:key:preserved".to_string()),
            graphql: Some("http://127.0.0.1:9191/graphql".to_string()),
            effective_tool_ceiling: Some(ManagedServerToolCeiling::Readwrite),
            effective_tool_root: Some("/Users/test".to_string()),
            suggested_tool_root: Some("/Users/test".to_string()),
            pairing_ready: false,
            approval_required: false,
            runtime_booting: false,
            error: None,
            error_code: None,
        },
        &gents_server::native_service::NativeServiceStatus {
            installed: true,
            running: false,
            job_loaded: false,
            enabled: true,
            requires_approval: false,
            background_item: Default::default(),
            failed: false,
            detail: None,
        },
    );

    assert_eq!(external.state, ManagedServerState::External);
    assert_eq!(external.node_did.as_deref(), Some("did:key:preserved"));
    assert!(external.auto_start);
}

#[test]
fn native_running_without_endpoint_readiness_is_starting() {
    let native = gents_server::native_service::NativeServiceStatus {
        installed: true,
        running: true,
        job_loaded: true,
        enabled: true,
        requires_approval: false,
        background_item: Default::default(),
        failed: false,
        detail: None,
    };
    let status = status_from(
        &ManagedServerRuntimeState::default(),
        None,
        Some(&native),
        None,
        None,
    );
    assert_eq!(status.state, ManagedServerState::Starting);

    let observed = project_external_status(
        ManagedServerStatus {
            state: ManagedServerState::External,
            auto_start: false,
            node_name: Some("local".to_string()),
            node_did: Some("did:key:ready".to_string()),
            graphql: Some("http://127.0.0.1:9191/graphql".to_string()),
            effective_tool_ceiling: Some(ManagedServerToolCeiling::MetaOnly),
            effective_tool_root: None,
            suggested_tool_root: None,
            pairing_ready: false,
            approval_required: false,
            runtime_booting: false,
            error: None,
            error_code: None,
        },
        &native,
    );
    assert_eq!(observed.state, ManagedServerState::Running);
}

#[test]
fn loaded_native_job_owns_a_ready_endpoint_between_process_states() {
    let native = gents_server::native_service::NativeServiceStatus {
        installed: true,
        running: false,
        job_loaded: true,
        enabled: false,
        requires_approval: false,
        background_item: Default::default(),
        failed: false,
        detail: None,
    };
    let observed = project_external_status(
        ManagedServerStatus {
            state: ManagedServerState::External,
            auto_start: true,
            node_name: Some("local".to_string()),
            node_did: Some("did:key:ready".to_string()),
            graphql: Some("http://127.0.0.1:9191/graphql".to_string()),
            effective_tool_ceiling: Some(ManagedServerToolCeiling::MetaOnly),
            effective_tool_root: None,
            suggested_tool_root: None,
            pairing_ready: false,
            approval_required: false,
            runtime_booting: false,
            error: None,
            error_code: None,
        },
        &native,
    );
    assert_eq!(observed.state, ManagedServerState::Running);
    assert!(!observed.auto_start);
}

#[test]
fn stale_local_failure_still_allows_runtime_readiness_probe() {
    let runtime = ManagedServerRuntimeState {
        last_error: Some("an earlier start failed".to_string()),
        ..Default::default()
    };
    let status = status_from(&runtime, None, None, None, None);
    assert_eq!(status.state, ManagedServerState::Failed);
}

#[tokio::test]
async fn failed_native_stop_does_not_reprovision_authority() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let provisioned = AtomicBool::new(false);
    let preference_written = AtomicBool::new(false);
    let result = stop_before_reprovision(
        || async { Err(BridgeError::untyped("stop failed")) },
        || async {
            provisioned.store(true, Ordering::SeqCst);
            Ok(())
        },
        || async {
            preference_written.store(true, Ordering::SeqCst);
            Ok(())
        },
    )
    .await;

    assert!(result.is_err());
    assert!(!provisioned.load(Ordering::SeqCst));
    assert!(!preference_written.load(Ordering::SeqCst));
}

#[tokio::test]
async fn reviewed_preference_is_written_before_native_restart_attempt() {
    use std::sync::{Arc, Mutex};

    let order = Arc::new(Mutex::new(Vec::new()));
    stop_before_reprovision(
        {
            let order = Arc::clone(&order);
            move || async move {
                order.lock().unwrap().push("stop");
                Ok(())
            }
        },
        {
            let order = Arc::clone(&order);
            move || async move {
                order.lock().unwrap().push("provision");
                Ok(())
            }
        },
        {
            let order = Arc::clone(&order);
            move || async move {
                order.lock().unwrap().push("preference");
                Ok(())
            }
        },
    )
    .await
    .unwrap();
    order.lock().unwrap().push("native-start-failed");

    assert_eq!(
        *order.lock().unwrap(),
        ["stop", "provision", "preference", "native-start-failed"]
    );
}

#[test]
fn external_server_rejects_a_different_initialized_identity() {
    let error = ensure_matching_identity(Some("did:key:local"), "did:key:other", 9191)
        .expect_err("different identity must be rejected");
    assert_eq!(error.code, BridgeErrorCode::InvalidArgument);
    assert!(error.message.contains("port 9191"));
    ensure_matching_identity(Some("did:key:local"), "did:key:local", 9191).unwrap();
    assert!(ensure_matching_identity(None, "did:key:local", 9191).is_err());
    assert!(ensure_matching_identity(Some(""), "did:key:local", 9191).is_err());
    assert!(ensure_matching_identity(Some("did:key:local"), "", 9191).is_err());
}

#[test]
fn managed_stop_rejects_a_manually_owned_endpoint() {
    let ours = || PortReadiness::Ready(ready_status("did:key:local"));
    assert!(ensure_native_owns_running_endpoint(&ours(), false).is_err());
    assert!(ensure_native_owns_running_endpoint(&ours(), true).is_ok());
    assert!(ensure_native_owns_running_endpoint(&PortReadiness::NotListening, false).is_ok());
}

#[test]
fn ready_runtime_requires_live_status_to_carry_reviewed_authority() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("init.json"),
        r#"{"node_did":"did:key:zReady"}"#,
    )
    .unwrap();
    let authority = EffectiveManagedAuthority {
        tool_ceiling: ManagedServerToolCeiling::Readwrite,
        tool_root: Some(PathBuf::from("/Users/test")),
    };
    let missing_authority = ManagedServerStatus {
        state: ManagedServerState::Running,
        auto_start: false,
        node_name: Some("Mandrake".into()),
        node_did: Some("did:key:zReady".into()),
        graphql: Some("http://127.0.0.1:9191/api/v0/graphql".into()),
        effective_tool_ceiling: None,
        effective_tool_root: None,
        suggested_tool_root: None,
        pairing_ready: false,
        approval_required: false,
        runtime_booting: false,
        error: None,
        error_code: None,
    };
    let error = validate_ready_runtime(&missing_authority, &authority, temp.path())
        .expect_err("missing /status authority must fail closed");
    assert!(error.to_string().contains("live did="));

    let ready = ManagedServerStatus {
        effective_tool_ceiling: Some(ManagedServerToolCeiling::Readwrite),
        effective_tool_root: Some("/Users/test".into()),
        ..missing_authority
    };
    validate_ready_runtime(&ready, &authority, temp.path()).unwrap();
}

#[test]
fn request_visibility_race_is_retriable() {
    use gents_protocol::enrollment::EnrollmentDecisionRefusalCode;
    let rejected = |status, code| {
        anyhow::Error::new(gents_server::server_host::EnrollmentDecisionRejected {
            status,
            message: "refused".into(),
            code,
        })
        .context("approving desktop enrollment on managed runtime")
    };
    assert!(enrollment_request_is_not_visible_yet(&rejected(
        reqwest::StatusCode::BAD_REQUEST,
        Some(EnrollmentDecisionRefusalCode::NotPending),
    )));
    assert!(!enrollment_request_is_not_visible_yet(&rejected(
        reqwest::StatusCode::BAD_REQUEST,
        Some(EnrollmentDecisionRefusalCode::Conflicted),
    )));
    assert!(!enrollment_request_is_not_visible_yet(&rejected(
        reqwest::StatusCode::FORBIDDEN,
        None,
    )));
}

#[test]
fn restart_command_validates_reviewed_authority_through_tauri_ipc() {
    use crate::config::{
        AppMeta, BootstrapPolicy, BridgeConfig, HomePolicy, ManagedServerPolicy, NodeHomePolicy,
    };
    use crate::snapshot::projection::SnapshotGrants;
    use crate::state::{resolve_policy, DesktopAppState};
    use tauri::ipc::InvokeBody;
    use tauri::webview::InvokeRequest;

    let temp = tempfile::tempdir().expect("temporary bridge roots");
    let policy = resolve_policy(
        &BridgeConfig {
            home: HomePolicy::FixedRoot(temp.path().join("desktop")),
            bootstrap: BootstrapPolicy::LocalRuntimeAllowed {
                node_home: NodeHomePolicy::Fixed(temp.path().join("agent")),
            },
            app_meta: AppMeta {
                app_name: "managed-restart-test".to_string(),
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            snapshot_grants: SnapshotGrants::core_only(),
            managed_server: ManagedServerPolicy::Allowed,
            client_host: false,
            store_key_custody: gents::store::local::store_key::StoreKeyCustodyChoice::File,
        },
        None,
    )
    .expect("fixed bridge policy");
    let app = tauri::test::mock_builder()
        .manage(DesktopAppState::new(policy))
        .invoke_handler(tauri::generate_handler![desktop_managed_server_restart])
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("mock desktop bridge app");
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("mock webview");
    let response = tauri::test::get_ipc_response(
        &webview,
        InvokeRequest {
            cmd: "desktop_managed_server_restart".into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: "http://tauri.localhost".parse().expect("invoke URL"),
            body: InvokeBody::Json(serde_json::json!({
                "request": {
                    "nodeName": "Workshop Agent",
                    "toolCeiling": "readwrite",
                    "toolRoot": "relative/path"
                }
            })),
            headers: Default::default(),
            invoke_key: tauri::test::INVOKE_KEY.to_string(),
        },
    )
    .expect_err("an unvalidated restart root must be rejected");

    let serialized = response.to_string();
    assert!(
        serialized.contains("absolute directory"),
        "restart IPC returned an unexpected error: {response:?}"
    );
}
