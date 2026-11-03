use std::sync::{Arc, Mutex};

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use clap::Parser as _;
use gents::{KeyIdentity, NodeIdentity as _};

use super::*;

type SeenCommands = Arc<Mutex<Vec<RuntimeControlCommand>>>;
type ControlState = (Arc<KeyIdentity>, SeenCommands, String);

#[test]
fn custody_refusals_preserve_key_access_without_reclassifying_authorization() {
    for error in [
        anyhow::Error::new(gents::store::local::store_key::StoreKeyUnavailable {
            data_path: std::path::PathBuf::from("fixture/data"),
            code: -25308,
        }),
        anyhow::Error::new(gents::store::identity::IdentityKeyUnavailable {
            label: "fixture-identity".to_owned(),
            code: -25308,
        }),
    ] {
        assert_eq!(
            refusal_reason(&error.context("loading recorded runtime keys")),
            RuntimePreparationRefusal::KeyAccess
        );
    }
    assert_eq!(
        refusal_reason(&refusal(
            RuntimePreparationRefusal::Credentials,
            "operator authorization refused"
        )),
        RuntimePreparationRefusal::Credentials
    );
    assert_eq!(
        refusal_reason(&anyhow::anyhow!("key access refused")),
        RuntimePreparationRefusal::Failed
    );
}

struct ControlNode {
    endpoint: reqwest::Url,
    identity: Arc<KeyIdentity>,
    seen: SeenCommands,
    server: tokio::task::JoinHandle<()>,
    _home: tempfile::TempDir,
}

impl Drop for ControlNode {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn control_node(tamper: bool) -> ControlNode {
    control_node_with_build(tamper, "b".repeat(64)).await
}

async fn control_node_with_build(tamper: bool, build: String) -> ControlNode {
    let home = tempfile::tempdir().unwrap();
    let identity = Arc::new(KeyIdentity::load_or_create(home.path().join("key"), None).unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/status",
            get(|State((identity, _, _)): State<ControlState>| async move {
                Json(serde_json::json!({
                    "node_did": identity.did(),
                    "tool_ceiling": "meta-only",
                    "tool_root_error": null,
                    "cli_tools": [],
                }))
            }),
        )
        .route(
            RUNTIME_CONTROL_PATH,
            post(
                move |State((identity, seen, build)): State<ControlState>,
                      Json(command): Json<RuntimeControlCommand>| async move {
                    seen.lock().unwrap().push(command.clone());
                    let disposition = match command.operation {
                        RuntimeOperation::Observe => RuntimeControlDisposition::Observed,
                        RuntimeOperation::Retire { .. } => RuntimeControlDisposition::Busy,
                    };
                    let mut receipt = RuntimeControlReceipt {
                        version: RUNTIME_CONTROL_VERSION,
                        node_did: identity.did().to_owned(),
                        command_nonce: command.nonce.clone(),
                        command_digest: command.digest().unwrap(),
                        build,
                        capabilities: current_capabilities(),
                        replicated_schema: None,
                        lifecycle: ServeLifecycle::Ready,
                        retiring: false,
                        disposition,
                        observed_at: chrono::Utc::now().to_rfc3339(),
                        signature: Vec::new(),
                    };
                    gents::runtime::service::sign_control_receipt(identity.as_ref(), &mut receipt)
                        .await
                        .unwrap();
                    if tamper {
                        receipt.build = "a".repeat(64);
                    }
                    Json(receipt)
                },
            ),
        )
        .with_state((identity.clone(), seen.clone(), build));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = reqwest::Url::parse(&format!(
        "http://{}/api/v0/graphql",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    ControlNode {
        endpoint,
        identity,
        seen,
        server,
        _home: home,
    }
}

#[test]
fn preparation_endpoint_excludes_remote_hosts_credentials_and_ambiguous_routes() {
    for endpoint in [
        "http://192.0.2.1:9191/api/v0/graphql",
        "https://127.0.0.1:9191/api/v0/graphql",
        "http://user:password@127.0.0.1:9191/api/v0/graphql",
        "http://127.0.0.1:9191/api/v0/graphql?redirect=elsewhere",
        "http://127.0.0.1:9191/api/v0/graphql#other",
        "http://127.0.0.1:9191/other",
    ] {
        assert!(local_endpoint(endpoint).is_err(), "{endpoint}");
    }
    assert!(local_endpoint("http://127.0.0.1:9191/api/v0/graphql").is_ok());
    assert!(local_endpoint("http://[::1]:9191/api/v0/graphql").is_ok());
}

#[tokio::test]
async fn preparation_control_verifies_live_receipts_and_rejects_tampering() {
    let client = reqwest::Client::new();
    for tamper in [false, true] {
        let node = control_node(tamper).await;
        let result = control(
            &client,
            &node.endpoint,
            node.identity.did(),
            RuntimeOperation::Observe,
            None,
        )
        .await;
        if tamper {
            assert_eq!(
                result
                    .unwrap_err()
                    .downcast_ref::<PreparationFailure>()
                    .unwrap()
                    .reason,
                RuntimePreparationRefusal::InvalidHome
            );
        } else {
            assert_eq!(result.unwrap().unwrap().build, "b".repeat(64));
        }
        assert_eq!(node.seen.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn preparation_control_refuses_a_signed_receipt_for_another_node() {
    let node = control_node(false).await;
    let foreign = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(foreign.path().join("key"), None).unwrap();
    let error = control(
        &reqwest::Client::new(),
        &node.endpoint,
        identity.did(),
        RuntimeOperation::Observe,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<PreparationFailure>().unwrap().reason,
        RuntimePreparationRefusal::InvalidHome
    );
    assert_eq!(node.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn preparation_control_refuses_a_runtime_without_the_control_endpoint() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = reqwest::Url::parse(&format!(
        "http://{}/api/v0/graphql",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new()).await.unwrap();
    });
    let result = control(
        &reqwest::Client::new(),
        &endpoint,
        "did:key:fixture",
        RuntimeOperation::Observe,
        None,
    )
    .await;
    server.abort();
    let _ = server.await;
    assert_eq!(
        result
            .unwrap_err()
            .downcast_ref::<PreparationFailure>()
            .unwrap()
            .reason,
        RuntimePreparationRefusal::Unsupported
    );
}

#[tokio::test]
async fn preparation_busy_refusal_leaves_identity_store_and_saved_route_unchanged() {
    let node = control_node(false).await;
    let root = tempfile::tempdir().unwrap();
    let home = root.path().canonicalize().unwrap();
    record_home(&node, &home);
    let key = node._home.path().join("key");
    let init_before = std::fs::read(home.join("init.json")).unwrap();
    let state_before = std::fs::read(home.join("runtime.json")).unwrap();
    let key_before = std::fs::read(&key).unwrap();
    let store_claim = gents::store::local::home::lock_home_store(&home).unwrap();
    let cli = crate::cli::Cli::try_parse_from([
        "gents",
        "prepare-runtime",
        "--home",
        home.to_str().unwrap(),
    ])
    .unwrap();
    let crate::cli::Command::PrepareRuntime(args) = cli.command else {
        panic!("prepare-runtime command");
    };
    let result = prepare_inner(args, crate::commands::eval::testing::test_runtime_handle()).await;
    assert_eq!(
        result
            .unwrap_err()
            .downcast_ref::<PreparationFailure>()
            .unwrap()
            .reason,
        RuntimePreparationRefusal::Busy
    );
    let commands = node.seen.lock().unwrap();
    assert_eq!(commands.len(), 2);
    assert!(matches!(commands[0].operation, RuntimeOperation::Observe));
    assert!(
        matches!(&commands[1].operation, RuntimeOperation::Retire { expected_build } if expected_build == &"b".repeat(64))
    );
    assert_ne!(commands[0].nonce, commands[1].nonce);
    assert_eq!(std::fs::read(home.join("init.json")).unwrap(), init_before);
    assert_eq!(
        std::fs::read(home.join("runtime.json")).unwrap(),
        state_before
    );
    assert_eq!(std::fs::read(key).unwrap(), key_before);
    assert!(!home.join("data").exists());
    drop(store_claim);
}

#[tokio::test]
async fn changing_root_does_not_modify_a_busy_runtime() {
    let build = gents::runtime::service::executable_build_id()
        .await
        .unwrap();
    let node = control_node_with_build(false, build).await;
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let home = home.path().canonicalize().unwrap();
    record_home(&node, &home);
    let init_before = std::fs::read(home.join("init.json")).unwrap();
    let state_before = std::fs::read(home.join("runtime.json")).unwrap();
    let cli = crate::cli::Cli::try_parse_from([
        "gents",
        "prepare-runtime",
        "--home",
        home.to_str().unwrap(),
        "--change-tool-root",
        root.path().to_str().unwrap(),
    ])
    .unwrap();
    let crate::cli::Command::PrepareRuntime(args) = cli.command else {
        panic!("prepare-runtime command");
    };
    let error = prepare_inner(args, crate::commands::eval::testing::test_runtime_handle())
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<PreparationFailure>().unwrap().reason,
        RuntimePreparationRefusal::Busy
    );
    assert_eq!(std::fs::read(home.join("init.json")).unwrap(), init_before);
    assert_eq!(
        std::fs::read(home.join("runtime.json")).unwrap(),
        state_before
    );
    assert_eq!(node.seen.lock().unwrap().len(), 2);
}

#[test]
fn changed_root_requires_an_existing_absolute_directory() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("file");
    std::fs::write(&file, "fixture").unwrap();
    assert!(validate_requested_root("relative".into()).is_err());
    assert!(validate_requested_root(root.path().join("missing")).is_err());
    assert!(validate_requested_root(file).is_err());
    assert_eq!(
        validate_requested_root(root.path().to_path_buf()).unwrap(),
        root.path().canonicalize().unwrap()
    );
}

#[tokio::test]
async fn live_authority_must_match_the_saved_ceiling_before_retirement() {
    let node = control_node(false).await;
    let home = tempfile::tempdir().unwrap();
    let config = record_home(&node, home.path());
    let valid = serde_json::json!({
        "node_did": config.node_did.as_str(),
        "tool_ceiling": "meta-only",
        "tool_root_error": null,
        "cli_tools": [],
    });
    assert!(validate_running_authority(&valid, &config).is_ok());
    for field in ["node_did", "tool_ceiling", "tool_root_error", "cli_tools"] {
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            validate_running_authority(&missing, &config).is_err(),
            "{field}"
        );
    }
    let mut mismatch = valid.clone();
    mismatch["tool_ceiling"] = serde_json::json!("readonly");
    assert!(validate_running_authority(&mismatch, &config).is_err());
    mismatch["tool_ceiling"] = serde_json::json!("meta-only");
    mismatch["cli_tools"] = serde_json::json!(["custom"]);
    assert!(validate_running_authority(&mismatch, &config).is_err());
}

#[tokio::test]
async fn changed_root_does_not_retire_a_runtime_with_stricter_live_access() {
    let node = control_node(false).await;
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let home = home.path().canonicalize().unwrap();
    let mut config = record_home(&node, &home);
    config.tool_ceiling = crate::cli::ToolCeilingArg::Readwrite;
    crate::home_state::write_init_config(&home, &config).unwrap();
    let before = std::fs::read(home.join("init.json")).unwrap();
    let cli = crate::cli::Cli::try_parse_from([
        "gents",
        "prepare-runtime",
        "--home",
        home.to_str().unwrap(),
        "--change-tool-root",
        root.path().to_str().unwrap(),
    ])
    .unwrap();
    let crate::cli::Command::PrepareRuntime(args) = cli.command else {
        panic!("prepare-runtime command");
    };
    let error = prepare_inner(args, crate::commands::eval::testing::test_runtime_handle())
        .await
        .unwrap_err();
    assert_eq!(
        refusal_reason(&error),
        RuntimePreparationRefusal::Unavailable
    );
    assert_eq!(std::fs::read(home.join("init.json")).unwrap(), before);
    let commands = node.seen.lock().unwrap();
    assert_eq!(commands.len(), 1);
    assert!(matches!(commands[0].operation, RuntimeOperation::Observe));
}

#[test]
fn managed_root_change_rejects_conflicting_server_overrides() {
    for flag in ["--tool-root", "--tool-ceiling"] {
        let value = if flag == "--tool-root" {
            "/other"
        } else {
            "readonly"
        };
        assert!(crate::cli::Cli::try_parse_from([
            "gents",
            "prepare-runtime",
            "--change-tool-root",
            "/workspace",
            flag,
            value,
        ])
        .is_err());
    }
}

#[tokio::test]
async fn changing_root_without_the_store_key_preserves_the_initialized_home() {
    let node = control_node(false).await;
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let home = home.path().canonicalize().unwrap();
    let mut config = record_home(&node, &home);
    config.store_encryption = Some(gents::store::local::store_key::StoreEncryption {
        version: gents::store::local::store_key::STORE_ENCRYPTION_VERSION,
        custody: gents::store::local::store_key::StoreKeyCustody::File,
    });
    crate::home_state::write_init_config(&home, &config).unwrap();
    std::fs::remove_file(home.join("runtime.json")).unwrap();
    let before = std::fs::read(home.join("init.json")).unwrap();
    let cli = crate::cli::Cli::try_parse_from([
        "gents",
        "prepare-runtime",
        "--home",
        home.to_str().unwrap(),
        "--change-tool-root",
        root.path().to_str().unwrap(),
    ])
    .unwrap();
    let crate::cli::Command::PrepareRuntime(args) = cli.command else {
        panic!("prepare-runtime command");
    };
    let error = prepare_inner(args, crate::commands::eval::testing::test_runtime_handle())
        .await
        .unwrap_err();
    assert_eq!(
        refusal_reason(&error),
        RuntimePreparationRefusal::Credentials
    );
    assert_eq!(std::fs::read(home.join("init.json")).unwrap(), before);
    assert!(!home.join("runtime.json").exists());
    assert!(node.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn changed_home_configuration_is_not_overwritten_at_readiness() {
    let node = control_node(false).await;
    let home = tempfile::tempdir().unwrap();
    let original = record_home(&node, home.path());
    let mut requested = original.clone();
    requested.tool_root = Some("/workspace".into());
    let mut concurrent = original.clone();
    concurrent.node_name = "updated by operator".into();
    crate::home_state::write_init_config(home.path(), &concurrent).unwrap();
    let before = std::fs::read(home.path().join("init.json")).unwrap();
    assert!(commit_root_change(home.path(), &original, &requested).is_err());
    assert_eq!(
        std::fs::read(home.path().join("init.json")).unwrap(),
        before
    );
}

fn record_home(node: &ControlNode, home: &Path) -> StoredInitConfig {
    let key = node._home.path().join("key");
    let config = StoredInitConfig {
        home: home.to_string_lossy().into_owned(),
        node_name: "fixture".into(),
        node_did: node.identity.did().to_owned(),
        key_path: Some(key.to_string_lossy().into_owned()),
        identity_backend: Some("file".into()),
        keychain_label: None,
        secure_enclave_label: None,
        tool_package: None,
        tool_ceiling: crate::cli::ToolCeilingArg::MetaOnly,
        tool_root: None,
        store_encryption: None,
    };
    crate::home_state::write_init_config(home, &config).unwrap();
    crate::home_state::write_runtime_state(
        home,
        &StoredRuntimeState {
            home: config.home.clone(),
            graphql: node.endpoint.to_string(),
            node_name: config.node_name.clone(),
            node_did: config.node_did.clone(),
            default_agent_id: "fixture".into(),
            p2p_transport: "none".into(),
            p2p_peer_id: None,
            p2p_listen_addresses: Vec::new(),
            p2p_admission: None,
        },
    )
    .unwrap();
    config
}

#[tokio::test]
async fn preparation_missing_store_key_refuses_before_requesting_retirement() {
    let node = control_node(false).await;
    let root = tempfile::tempdir().unwrap();
    let home = root.path().canonicalize().unwrap();
    let mut config = record_home(&node, &home);
    config.store_encryption = Some(gents::store::local::store_key::StoreEncryption {
        version: gents::store::local::store_key::STORE_ENCRYPTION_VERSION,
        custody: gents::store::local::store_key::StoreKeyCustody::File,
    });
    crate::home_state::write_init_config(&home, &config).unwrap();
    let init_before = std::fs::read(home.join("init.json")).unwrap();
    let state_before = std::fs::read(home.join("runtime.json")).unwrap();
    let cli = crate::cli::Cli::try_parse_from([
        "gents",
        "prepare-runtime",
        "--home",
        home.to_str().unwrap(),
    ])
    .unwrap();
    let crate::cli::Command::PrepareRuntime(args) = cli.command else {
        panic!("prepare-runtime command");
    };
    let error = prepare_inner(args, crate::commands::eval::testing::test_runtime_handle())
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<PreparationFailure>().unwrap().reason,
        RuntimePreparationRefusal::Credentials
    );
    assert_eq!(node.seen.lock().unwrap().len(), 1);
    assert_eq!(std::fs::read(home.join("init.json")).unwrap(), init_before);
    assert_eq!(
        std::fs::read(home.join("runtime.json")).unwrap(),
        state_before
    );
    assert!(!gents::store::local::store_key::home_key_file(&home).exists());
    assert!(!home.join("data").exists());
}

#[tokio::test]
async fn unavailable_observation_is_absent_but_cannot_authorize_retirement() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = reqwest::Url::parse(&format!(
        "http://{}/api/v0/graphql",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                RUNTIME_CONTROL_PATH,
                post(|| async { axum::http::StatusCode::SERVICE_UNAVAILABLE }),
            ),
        )
        .await
        .unwrap();
    });
    let client = reqwest::Client::new();
    let observed = control(
        &client,
        &endpoint,
        "did:key:fixture",
        RuntimeOperation::Observe,
        None,
    )
    .await
    .unwrap();
    assert!(observed.is_none());
    let retired = control(
        &client,
        &endpoint,
        "did:key:fixture",
        RuntimeOperation::Retire {
            expected_build: "fixture".into(),
        },
        None,
    )
    .await;
    assert_eq!(
        refusal_reason(&retired.unwrap_err()),
        RuntimePreparationRefusal::Unavailable
    );
    server.abort();
    let _ = server.await;
}
