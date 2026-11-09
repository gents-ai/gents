use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};

use crate::error::{BridgeError, BridgeErrorCode};
use crate::types::TaskDeleteRequest;

/// A runtime that refuses every request from this client, as a runtime whose
/// ACP does not grant it the document does.
#[tokio::test]
async fn a_denied_config_write_keeps_its_permission_code() {
    let home = tempfile::tempdir().unwrap();
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(home.path()),
        ClientCoreOptions::local_simulated_route(),
    )
    .await
    .unwrap();
    let server = wiremock::MockServer::start().await;
    let node_did = core.node_identity().did().to_owned();
    let runtime_home = home.path().join("runtime");
    std::fs::create_dir_all(&runtime_home).unwrap();
    let graphql = format!("{}/api/v0/graphql", server.uri());
    std::fs::write(
        runtime_home.join("runtime.json"),
        serde_json::json!({
            "graphql": graphql, "node_name": "Denying runtime", "node_did": node_did,
            "p2p_transport": "iroh"
        })
        .to_string(),
    )
    .unwrap();
    core.add_local_standard_peer_route_for_test(
        "Denying runtime",
        "127.0.0.1:56000/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb",
        &node_did,
        &graphql,
        runtime_home.to_str().unwrap(),
    )
    .await
    .unwrap();
    wiremock::Mock::given(wiremock::matchers::any())
        .respond_with(wiremock::ResponseTemplate::new(403).set_body_string("denied"))
        .mount(&server)
        .await;

    let error = crate::commands::delete_task_config(
        &core,
        TaskDeleteRequest {
            task_id: "task".into(),
            node_did: node_did.clone(),
        },
    )
    .await
    .unwrap_err();
    let bridge = BridgeError::classify_transport_error(error);
    assert_eq!(bridge.code, BridgeErrorCode::PermissionDenied, "{bridge}");
    assert!(!bridge.retryable);
    std::sync::Arc::new(core).shutdown().await.unwrap();
}
