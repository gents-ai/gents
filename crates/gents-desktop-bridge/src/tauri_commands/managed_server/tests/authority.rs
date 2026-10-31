use super::*;

#[test]
fn exit_stops_active_service_without_a_desktop_preference_but_respects_login_enablement() {
    let active = gents_server::native_service::NativeServiceStatus {
        installed: true,
        running: true,
        job_loaded: true,
        enabled: false,
        requires_approval: false,
        background_item: gents_server::native_service::BackgroundItemStatus::Unavailable,
        failed: false,
        detail: None,
    };
    assert!(should_stop_managed_service_on_exit(&active));

    let keep_running = gents_server::native_service::NativeServiceStatus {
        enabled: true,
        ..active.clone()
    };
    assert!(!should_stop_managed_service_on_exit(&keep_running));

    let absent = gents_server::native_service::NativeServiceStatus {
        installed: false,
        running: false,
        job_loaded: false,
        enabled: false,
        requires_approval: false,
        background_item: gents_server::native_service::BackgroundItemStatus::Unavailable,
        failed: false,
        detail: None,
    };
    assert!(!should_stop_managed_service_on_exit(&absent));
}

fn write_home(node_home: &Path, did: &str, ceiling: &str, root: Option<&str>) {
    let init = serde_json::json!({
        "home": node_home.display().to_string(),
        "node_name": "Forge",
        "node_did": did,
        "key_path": null,
        "tool_ceiling": ceiling,
        "tool_root": root,
    });
    write(&node_home.join("init.json"), &init.to_string());
}

async fn remember_forge_grant(state: &DesktopAppState, node_home: &Path, root: &str) {
    save_confirmed_preference(
        state,
        node_home,
        &StoredManagedServer {
            node_name: "Forge".to_string(),
            tool_ceiling: Some(ManagedServerToolCeiling::Readwrite),
            tool_root: Some(root.to_string()),
            reviewed_for: None,
        },
    )
    .await
    .unwrap();
    assert!(load_preference(state)
        .await
        .unwrap()
        .unwrap()
        .reviewed_for
        .is_some());
}

fn invoke_on_forge_home(cmd: &str) -> (tempfile::TempDir, Vec<u8>, String) {
    use tauri::ipc::InvokeBody;
    use tauri::webview::InvokeRequest;

    let (temp, state) = orchestration_state();
    let node_home = state.policy.node_home.clone().expect("node home");
    let desktop_root = state.policy.desktop_paths.root().to_path_buf();
    write_home(&node_home, "did:key:forge", "MetaOnly", None);
    let before = std::fs::read(node_home.join("init.json")).unwrap();
    let root = temp.path().join("work");
    std::fs::create_dir_all(&root).unwrap();
    let app = tauri::test::mock_builder()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            desktop_managed_server_start,
            desktop_managed_server_restart
        ])
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("mock desktop bridge app");
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("mock webview");
    let error = tauri::test::get_ipc_response(
        &webview,
        InvokeRequest {
            cmd: cmd.into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: "http://tauri.localhost".parse().expect("invoke URL"),
            body: InvokeBody::Json(serde_json::json!({
                "request": {
                    "nodeName": "Scout",
                    "toolCeiling": "readwrite",
                    "toolRoot": root.display().to_string(),
                }
            })),
            headers: Default::default(),
            invoke_key: tauri::test::INVOKE_KEY.to_string(),
        },
    )
    .expect_err("a request naming another agent must be refused");
    assert!(!desktop_root.join(MANAGED_SERVER_CONFIG).exists());
    let after = std::fs::read(node_home.join("init.json")).unwrap();
    assert_eq!(after, before, "{cmd} rewrote the initialized home");
    (temp, after, error.to_string())
}

#[test]
fn start_naming_another_agent_leaves_the_home_untouched() {
    let (_temp, _, error) = invoke_on_forge_home("desktop_managed_server_start");
    assert!(
        error.contains("already has a local Node named Forge"),
        "{error}"
    );
}

#[test]
fn restart_naming_another_agent_leaves_the_home_untouched() {
    let (_temp, _, error) = invoke_on_forge_home("desktop_managed_server_restart");
    assert!(
        error.contains("already has a local Node named Forge"),
        "{error}"
    );
}

#[tokio::test]
async fn a_remembered_grant_reconnects_only_the_home_it_was_reviewed_for() {
    let (temp, state) = orchestration_state();
    let node_home = state.policy.node_home.clone().expect("node home");
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let root = root.to_string_lossy().into_owned();
    write_home(&node_home, "did:key:forge", "Readwrite", Some(&root));
    remember_forge_grant(&state, &node_home, &root).await;

    let bound = load_bound_preference(&state).await.unwrap();
    let authority = start_authority(None, None, bound.as_ref()).expect("reviewed home");
    assert_eq!(
        authority.stored(),
        (ManagedServerToolCeiling::Readwrite, Some(root.clone()))
    );
}

#[tokio::test]
async fn a_replaced_or_missing_home_needs_a_fresh_review_and_is_not_rewritten() {
    let (temp, state) = orchestration_state();
    let node_home = state.policy.node_home.clone().expect("node home");
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let root = root.to_string_lossy().into_owned();
    write_home(&node_home, "did:key:forge", "Readwrite", Some(&root));
    remember_forge_grant(&state, &node_home, &root).await;

    write_home(&node_home, "did:key:other", "MetaOnly", None);
    let before = std::fs::read(node_home.join("init.json")).unwrap();
    let bound = load_bound_preference(&state).await.unwrap();
    assert_eq!(bound.as_ref().unwrap().tool_ceiling, None);
    let error = start_authority(None, None, bound.as_ref()).unwrap_err();
    assert_eq!(error.code, BridgeErrorCode::InvalidArgument);
    assert_eq!(std::fs::read(node_home.join("init.json")).unwrap(), before);

    std::fs::remove_file(node_home.join("init.json")).unwrap();
    let bound = load_bound_preference(&state).await.unwrap();
    assert!(start_authority(None, None, bound.as_ref()).is_err());
    assert!(!node_home.join("init.json").exists());
}

#[tokio::test]
async fn a_home_whose_authority_changed_needs_a_fresh_review() {
    let (temp, state) = orchestration_state();
    let node_home = state.policy.node_home.clone().expect("node home");
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let root = root.to_string_lossy().into_owned();
    write_home(&node_home, "did:key:forge", "Readwrite", Some(&root));
    remember_forge_grant(&state, &node_home, &root).await;
    let remembered = load_preference(&state).await.unwrap();

    write_home(&node_home, "did:key:forge", "Readonly", Some(&root));
    assert_eq!(
        load_preference(&state).await.unwrap().unwrap().tool_ceiling,
        remembered.unwrap().tool_ceiling
    );
    let bound = load_bound_preference(&state).await.unwrap();
    assert!(start_authority(None, None, bound.as_ref()).is_err());
}

#[tokio::test]
async fn reprovisioning_with_another_name_leaves_the_home_and_preference_alone() {
    let (_temp, state) = orchestration_state();
    let node_home = state.policy.node_home.clone().expect("node home");
    let init = serde_json :: json ! ({ "home" : node_home . display () . to_string () , "node_name" : "Forge" , "node_did" : "did:key:forge" , "key_path" : null , "tool_ceiling" : "Readwrite" , "tool_root" : null , });
    write(&node_home.join("init.json"), &init.to_string());
    let forge = StoredManagedServer {
        node_name: "Forge".to_string(),
        tool_ceiling: Some(ManagedServerToolCeiling::Readwrite),
        tool_root: None,
        reviewed_for: None,
    };
    save_preference(&state, &forge).await.unwrap();
    gents_server::server_host::ensure_standard_home(
        &crate::tests::support::runtime_handle(),
        gents_server::server_host::ProvisionOptions {
            home: node_home.clone(),
            node_name: "Scout".to_string(),
            tool_ceiling: runtime_tool_ceiling(ManagedServerToolCeiling::Readwrite),
            tool_root: None,
            store_key_custody: gents::store::local::store_key::StoreKeyCustodyChoice::File,
        },
    )
    .await
    .unwrap();
    save_confirmed_preference(
        &state,
        &node_home,
        &StoredManagedServer {
            node_name: "Scout".to_string(),
            ..forge.clone()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        read_initialized_name(&node_home).await.as_deref(),
        Some("Forge")
    );
    let stored = load_preference(&state).await.unwrap().expect("preference");
    assert_eq!(stored.node_name, "Forge");
    save_confirmed_preference(&state, &node_home, &forge)
        .await
        .unwrap();
    assert_eq!(
        load_preference(&state).await.unwrap().unwrap().node_name,
        "Forge"
    );
}

#[test]
fn root_validation_canonicalizes_symlinks_and_preserves_spaces() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("a folder");
    std::fs::create_dir(&real).unwrap();
    #[cfg(unix)]
    {
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        assert_eq!(
            validate_tool_root(&alias).unwrap(),
            std::fs::canonicalize(&real).unwrap()
        );
    }
    assert_eq!(
        validate_tool_root(&real).unwrap(),
        std::fs::canonicalize(&real).unwrap()
    );
}

#[test]
fn root_validation_rejects_relative_missing_and_filesystem_root() {
    assert!(validate_tool_root(Path::new("relative")).is_err());
    let temp = tempfile::tempdir().unwrap();
    assert!(validate_tool_root(&temp.path().join("missing")).is_err());
    let root = temp.path().ancestors().last().unwrap();
    assert!(validate_tool_root(root).is_err());
}

#[test]
fn authority_requires_exact_root_shape_for_the_ceiling() {
    assert!(
        EffectiveManagedAuthority::from_request(ManagedServerToolCeiling::Readwrite, None,)
            .is_err()
    );
    assert!(EffectiveManagedAuthority::from_request(
        ManagedServerToolCeiling::MetaOnly,
        Some("/tmp"),
    )
    .is_err());
    assert_eq!(
        EffectiveManagedAuthority::from_request(ManagedServerToolCeiling::MetaOnly, None,).unwrap(),
        EffectiveManagedAuthority {
            tool_ceiling: ManagedServerToolCeiling::MetaOnly,
            tool_root: None,
        }
    );
}
