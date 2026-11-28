use crate::client_runtime::ClientRuntime;
use gents_store::client_journal::paths::DesktopPaths;

use crate::types::TaskDeleteRequest;
use gents_store::client_error::BridgeError;
use gents_store::client_error::BridgeErrorCode;

struct DenyingRuntime {
    core: std::sync::Arc<ClientRuntime>,
    node_did: String,
    _server: wiremock::MockServer,
    _home: tempfile::TempDir,
}

impl DenyingRuntime {
    async fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        let core = ClientRuntime::start_with_paths_and_options(
            DesktopPaths::from_root(home.path()),
            crate::client_test_support::file_journal_options(),
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
        core.connect_runtime(gents_protocol::runtime_connection::RuntimeConnection {
            node_did: node_did.to_string(),
            label: ("Denying runtime").to_string(),
            graphql: graphql.to_string(),
            local_node_home: Some((runtime_home.to_str().unwrap()).to_string()),
            access_request: None,
            access_state: None,
        })
        .await
        .unwrap();
        wiremock::Mock::given(wiremock::matchers::any())
            .respond_with(wiremock::ResponseTemplate::new(403).set_body_string("denied"))
            .mount(&server)
            .await;
        Self {
            core: std::sync::Arc::new(core),
            node_did,
            _server: server,
            _home: home,
        }
    }

    fn assert_denied(error: anyhow::Error) {
        let bridge = BridgeError::classify_transport_error(error);
        assert_eq!(bridge.code, BridgeErrorCode::PermissionDenied, "{bridge}");
        assert!(!bridge.retryable);
    }

    async fn shutdown(self) {
        self.core.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn denied_config_writes_session_reads_and_stops_keep_their_permission_code() {
    let runtime = DenyingRuntime::start().await;
    let (core, node) = (runtime.core.as_ref(), runtime.node_did.as_str());

    let error = crate::commands::delete_task_config(
        &runtime.core,
        TaskDeleteRequest {
            task_id: "task".into(),
            node_did: runtime.node_did.clone(),
        },
    )
    .await
    .unwrap_err();
    DenyingRuntime::assert_denied(error);
    let log = std::fs::read_to_string(crate::logging::desktop_error_log(
        runtime.core.paths().root(),
    ))
    .expect("denied desktop mutation writes desktop-errors.log");
    assert!(log.contains("delete task failed:"), "unexpected log: {log}");
    assert!(
        log.contains("403"),
        "expected real HTTP status in log: {log}"
    );

    DenyingRuntime::assert_denied(
        crate::commands::session_goal(core, node, "session")
            .await
            .unwrap_err(),
    );
    DenyingRuntime::assert_denied(core.request_timeline(node, "request").await.unwrap_err());

    let stop_error = crate::interrupt::interrupt_request(
        &runtime.core,
        &crate::types::DesktopInterruptRequest {
            request_id: "request".into(),
            node_did: Some(runtime.node_did.clone()),
            cause: "userCancelled".into(),
        },
    )
    .await
    .unwrap_err();
    DenyingRuntime::assert_denied(stop_error.into());

    runtime.shutdown().await;
}
