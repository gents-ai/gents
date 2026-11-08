#[cfg(unix)]
mod process_control_acceptance;

use std::time::Instant;

use axum::http::header;
use gents_protocol::row::{
    AgentReadinessEntry, AgentReadinessState, NodeReadinessProcessState, NodeReadinessRow,
    NodeReadinessSnapshot, NODE_READINESS_FORMAT_VERSION,
};
use serde_json::{json, Value};

use super::metrics::{
    load_p2p_metrics_for_scrape, p2p_metrics_from_status, P2P_METRICS_FETCH_BUDGET,
};
use super::status::{status_handler, STATUS_PROBE_BUDGET};
use super::*;
use crate::http::prometheus::MetricsRuntimeRow;

fn desktop_endpoint(
    url: impl Into<String>,
    identity: &gents_desktop_core::client::PrincipalIdentity,
) -> GraphqlEndpoint {
    GraphqlEndpoint::as_principal(url, identity.did())
        .with_client_signing_capability(
            gents::store::config_client::ClientSigningCapability::from_ed25519_key(
                identity.private_key_bytes(),
            )
            .unwrap(),
        )
        .unwrap()
}

fn state() -> RuntimeHttpState {
    let (activation_runtime, activation_observation) = empty_activation_state();
    RuntimeHttpState {
        graphql: gents::store::config_client::GraphqlEndpoint::anonymous(
            "http://127.0.0.1:9181/api/v0/graphql",
        ),
        p2p_graphql: gents::store::config_client::GraphqlEndpoint::anonymous(
            "http://127.0.0.1:9181/api/v0/graphql",
        ),
        node_name: "amy".to_string(),
        node_did: "did:key:zAgent".to_string(),
        tool_ceiling: "readwrite".to_string(),
        tool_root: Some("/Users/test".to_string()),
        tool_root_error: None,
        cli_tools: Vec::new(),
        home: None,
        started_at: "2026-06-04T00:00:00Z".to_string(),
        started_instant: Instant::now(),
        backend_health: None,
        p2p_admission: None,
        p2p_metrics_cache: std::sync::Arc::new(std::sync::Mutex::new(None)),
        store_size: crate::http::prometheus::StoreSizeCache::new(None),
        codex_shim_health: None,
        enrollment_offer_issuer: crate::http::enrollment::empty_issuer_handle(),
        enrollment_decisions: crate::http::enrollment::empty_decision_service_handle(),
        activation_runtime,
        activation_observation,
        replicated_schema: Default::default(),
        serve_lifecycle: Default::default(),
    }
}

#[tokio::test]
async fn inference_activity_requires_authenticated_identity() {
    let app = Router::new()
        .route(
            gents::observe::inference_observation::INFERENCE_ACTIVITY_PATH,
            post(crate::http::inference_activity::read),
        )
        .with_state(state());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let _server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let response = reqwest::Client::new()
        .post(format!("http://{address}{}", gents::observe::inference_observation::INFERENCE_ACTIVITY_PATH))
        .json(&json!({"node":"node", "session":"session", "request":"request", "generation":"generation"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
}

#[tokio::test]
async fn backend_discovery_requires_authenticated_identity() {
    let app = Router::new()
        .route(
            crate::http::inference_activity::BACKEND_DISCOVERY_PATH,
            post(crate::http::inference_activity::discover_backend),
        )
        .with_state(state());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let _server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let response = reqwest::Client::new()
        .post(format!(
            "http://{address}{}",
            crate::http::inference_activity::BACKEND_DISCOVERY_PATH
        ))
        .json(&json!({"node_did":"node", "backend_id":"backend"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
}

#[tokio::test]
async fn runtime_control_binds_signed_observation_and_retires_only_the_operator_observed_build() {
    use gents_desktop_core::client::{DesktopPaths, PrincipalIdentity};
    use gents_protocol::serve_lifecycle::control::{
        RuntimeControlCommand, RuntimeControlDisposition, RuntimeControlReceipt, RuntimeOperation,
        RUNTIME_CONTROL_PATH, RUNTIME_CONTROL_VERSION,
    };
    let temp = tempfile::tempdir().unwrap();
    // Runtime control admits only the Node's own signer; the enrolled User
    // configures the Agent the runtime starts over its fresh backend admission.
    let home = crate::shared::test_support::ClientHome::new().await;
    let identity = home.home.identity.clone();
    let foreign_paths = DesktopPaths::from_root(temp.path().join("foreign"));
    foreign_paths.ensure_root_dirs().await.unwrap();
    let foreign = PrincipalIdentity::load_or_create(&foreign_paths)
        .await
        .unwrap();
    let owner = identity.did().to_owned();
    let node = home.node().clone();
    let binding = home.agent("fixture").await;
    home.mark_backend_healthy(&binding).await;
    let runtime = gents::Gents::builder()
        .node(node.clone())
        .backend_admission(home.home.backend_admission())
        .identity(identity.clone())
        .tool_ceiling(gents::ToolCeiling::meta_only())
        .agent("fixture")
        .identity_binding(binding.clone())
        .backend_id(format!("{}:backend", binding.agent_did))
        .model_name("test-model")
        .done()
        .build()
        .await
        .unwrap();
    let admission = runtime.service_admission();
    let mut state = state();
    state.node_did = owner.clone();
    let lifecycle = state.serve_lifecycle.clone();
    assert!(state.activation_runtime.set(runtime).is_ok());
    let mut starting_state = state.clone();
    starting_state.serve_lifecycle = Default::default();
    let app = Router::new()
        .route(
            RUNTIME_CONTROL_PATH,
            post(crate::http::runtime_control::runtime_control_handler),
        )
        .with_state(state)
        .merge(
            Router::new()
                .route(
                    "/runtime/operator/starting",
                    post(crate::http::runtime_control::runtime_control_handler),
                )
                .with_state(starting_state),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let url = format!("http://{addr}{RUNTIME_CONTROL_PATH}");
    let graphql = format!("http://{addr}/api/v0/graphql");
    let endpoint = GraphqlEndpoint::as_principal(&graphql, identity.did())
        .with_client_signing_capability(identity.client_signing_capability().unwrap())
        .unwrap();
    let command = |operation| RuntimeControlCommand {
        version: RUNTIME_CONTROL_VERSION,
        node_did: owner.clone(),
        nonce: uuid::Uuid::new_v4().to_string(),
        issued_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        operation,
    };
    let observe = command(RuntimeOperation::Observe);
    let response = client.post(&url).json(&observe).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let starting: RuntimeControlReceipt = response.json().await.unwrap();
    gents::runtime::service::verify_control_receipt(&observe, &starting).unwrap();
    assert_eq!(starting.disposition, RuntimeControlDisposition::Starting);
    lifecycle.mark_ready();
    let observe = command(RuntimeOperation::Observe);
    let receipt: RuntimeControlReceipt = client
        .post(&url)
        .json(&observe)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    gents::runtime::service::verify_control_receipt(&observe, &receipt).unwrap();
    assert_eq!(receipt.disposition, RuntimeControlDisposition::Observed);
    let mut modified = receipt.clone();
    modified.capabilities.insert("request_input".into(), 999);
    assert!(gents::runtime::service::verify_control_receipt(&observe, &modified).is_err());
    let other_challenge = command(RuntimeOperation::Observe);
    assert!(gents::runtime::service::verify_control_receipt(&other_challenge, &receipt).is_err());
    let retire = command(RuntimeOperation::Retire {
        expected_build: receipt.build.clone(),
    });
    assert!(gents::runtime::service::verify_control_receipt(&retire, &receipt).is_err());
    let response = client.post(&url).json(&retire).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = client
        .post(&url)
        .bearer_auth("invalid")
        .json(&retire)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    let foreign_endpoint = desktop_endpoint(&graphql, &foreign);
    let response = foreign_endpoint
        .authorize(client.post(&url))
        .unwrap()
        .json(&retire)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let mut other_node = retire.clone();
    other_node.node_did = foreign.did().to_owned();
    let response = endpoint
        .authorize(client.post(&url))
        .unwrap()
        .json(&other_node)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let changed = command(RuntimeOperation::Retire {
        expected_build: "0".repeat(64),
    });
    let refused: RuntimeControlReceipt = endpoint
        .authorize(client.post(&url))
        .unwrap()
        .json(&changed)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    gents::runtime::service::verify_control_receipt(&changed, &refused).unwrap();
    assert_eq!(refused.disposition, RuntimeControlDisposition::BuildChanged);
    assert!(!admission.is_retiring().unwrap());

    node.shutdown().await.expect("node shutdown");
    let response = endpoint
        .authorize(client.post(format!("http://{addr}/runtime/operator/starting")))
        .unwrap()
        .json(&retire)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let accepted: RuntimeControlReceipt = response.json().await.unwrap();
    gents::runtime::service::verify_control_receipt(&retire, &accepted).unwrap();
    assert_eq!(accepted.disposition, RuntimeControlDisposition::Retiring);
    assert_eq!(
        accepted.lifecycle,
        gents_protocol::serve_lifecycle::ServeLifecycle::Starting
    );
    assert!(admission.is_retiring().unwrap());
    let repeated: RuntimeControlReceipt = endpoint
        .authorize(client.post(&url))
        .unwrap()
        .json(&retire)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    gents::runtime::service::verify_control_receipt(&retire, &repeated).unwrap();
    assert_eq!(repeated.disposition, RuntimeControlDisposition::Stopping);
    drop(server);
}

#[tokio::test]
async fn operator_inventory_authenticates_and_reads_native_metadata() {
    use gents_desktop_core::client::{DesktopPaths, PrincipalIdentity};
    use gents_protocol::file_inventory::{
        DirectFileInventoryRequest, FileInventoryObservation, FileInventoryReadAction,
        FILE_INVENTORY_OPERATOR_PATH,
    };

    let temp = tempfile::tempdir().unwrap();
    // The enrolled User holds the operator grants and configures the Node
    // through its own client-signed endpoint.
    let home = crate::shared::test_support::ClientHome::new().await;
    let identity = home.home.identity.clone();
    let user = &home.user;
    let foreign_paths = DesktopPaths::from_root(temp.path().join("foreign"));
    foreign_paths.ensure_root_dirs().await.unwrap();
    let foreign = PrincipalIdentity::load_or_create(&foreign_paths)
        .await
        .unwrap();
    let node = home.node().clone();
    let owner = identity.did();
    // The foreign client may read documents on this Node, but no session of
    // the User's is visible to it.
    let admin = gents::ConfigAccess::local_as(node.clone(), identity::Did::new(owner).unwrap());
    for permission in [
        gents::store::config_client::NodePermission::DocumentRead,
        gents::store::config_client::NodePermission::DocumentUpdate,
    ] {
        admin
            .grant_node_permission(&identity::Did::new(foreign.did()).unwrap(), permission)
            .await
            .unwrap();
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let access = home.access.clone();
    let session_id = "operator-http-session";
    let binding = home.agent("fixture").await;
    home.mark_backend_healthy(&binding).await;
    home.agent_session(&binding, session_id).await;
    let runtime = gents::Gents::builder()
        .node(node.clone())
        .backend_admission(home.home.backend_admission())
        .identity(identity.clone())
        .tool_ceiling(gents::ToolCeiling::meta_only())
        .agent("fixture")
        .identity_binding(binding.clone())
        .backend_id(format!("{}:backend", binding.agent_did))
        .model_name("test-model")
        .done()
        .build()
        .await
        .unwrap();
    let mut state = state();
    state.node_did = owner.into();
    assert!(state.activation_runtime.set(runtime).is_ok());
    let app = Router::new()
        .route(
            FILE_INVENTORY_OPERATOR_PATH,
            post(crate::http::file_inventory::operator_inventory_handler),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let graphql = format!("http://{addr}/api/v0/graphql");
    let endpoint = crate::shared::test_support::user_endpoint(&graphql, user);
    let foreign_endpoint = desktop_endpoint(&graphql, &foreign);
    let url = format!("http://{addr}{FILE_INVENTORY_OPERATOR_PATH}");
    let demand = DirectFileInventoryRequest {
        node_did: owner.into(),
        session_id: session_id.into(),
        peer_id: "operator-client".into(),
        action: FileInventoryReadAction::default(),
    };
    let response = client.post(&url).json(&demand).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let response = client
        .post(&url)
        .bearer_auth("invalid-token")
        .json(&demand)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    let wrong_audience =
        crate::shared::test_support::user_endpoint("http://other.invalid/api/v0/graphql", user)
        .authorization()
        .unwrap()
        .unwrap();
    let response = client
        .post(&url)
        .header(header::AUTHORIZATION, wrong_audience)
        .json(&demand)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    let response = foreign_endpoint
        .authorize(client.post(&url))
        .unwrap()
        .json(&demand)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .json::<Option<FileInventoryObservation>>()
        .await
        .unwrap()
        .is_none());
    let mut retained = None;
    for _ in 0..2 {
        let response = endpoint
            .authorize(client.post(&url))
            .unwrap()
            .json(&demand)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let reply = response
            .json::<Option<FileInventoryObservation>>()
            .await
            .unwrap()
            .unwrap();
        let page = reply.current.as_ref().unwrap();
        assert_eq!(page.binding.coverage.entries, 0);
        assert_eq!(page.binding.coverage.pending, 0);
        assert_eq!(page.binding.coverage.unknown, 0);
        if let Some(prior) = &retained {
            assert_eq!(&reply, prior);
        }
        retained = Some(reply);
    }
    let mut wrong_runtime = demand.clone();
    wrong_runtime.node_did = foreign.did().into();
    let response = endpoint
        .authorize(client.post(&url))
        .unwrap()
        .json(&wrong_runtime)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let mut missing = demand.clone();
    missing.session_id.push_str("-missing");
    let response = endpoint
        .authorize(client.post(&url))
        .unwrap()
        .json(&missing)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .json::<Option<FileInventoryObservation>>()
        .await
        .unwrap()
        .is_none());
    drop(server);
    drop(access);
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn inference_settings_authenticates_the_client_before_reading_configuration() {
    use gents_desktop_core::{
        client::{DesktopPaths, PrincipalIdentity},
        inference::INFERENCE_SETTINGS_PATH,
    };
    let temp = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(temp.path());
    paths.ensure_root_dirs().await.unwrap();
    let identity = PrincipalIdentity::load_or_create(&paths).await.unwrap();
    let app = Router::new()
        .route(
            INFERENCE_SETTINGS_PATH,
            get(crate::http::inference::inference_settings_handler),
        )
        .with_state(state());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let url = format!("http://{addr}{INFERENCE_SETTINGS_PATH}");
    let client = reqwest::Client::new();
    let response = client.get(&url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let response = client
        .get(&url)
        .bearer_auth("invalid-token")
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    let foreign_audience = desktop_endpoint("http://other.invalid/api/v0/graphql", &identity)
        .authorization()
        .unwrap()
        .unwrap();
    let response = client
        .get(&url)
        .header(header::AUTHORIZATION, foreign_audience)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    let endpoint = desktop_endpoint(format!("http://{addr}/api/v0/graphql"), &identity);
    let response = endpoint
        .authorize(client.get(&url))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"],
        "The runtime is still starting. Try again shortly."
    );
    server.abort();
}

#[test]
fn activation_observation_requires_one_successful_exact_tuple() {
    let observed = RuntimeActivationObservation {
        event: Some((4, "desired".into(), Ok(()))),
        router: Some((4, "desired".into())),
    };
    assert!(observed.successful_for(4, "desired"));
    assert!(!observed.successful_for(5, "desired"));
    assert!(!observed.successful_for(4, "other"));

    let failed = RuntimeActivationObservation {
        event: Some((4, "desired".into(), Err("seed failed".into()))),
        router: Some((4, "desired".into())),
    };
    assert!(!failed.successful_for(4, "desired"));

    let torn = RuntimeActivationObservation {
        event: Some((4, "desired".into(), Ok(()))),
        router: Some((5, "desired".into())),
    };
    assert!(!torn.successful_for(4, "desired"));
}

fn runtime() -> MetricsRuntimeRow {
    MetricsRuntimeRow {
        node_did: "did:key:zAgent".to_string(),
        reconcile_phase: "idle".to_string(),
        last_reconcile_result: "applied".to_string(),
        last_reconcile_completed_at: "2026-06-04T00:00:00Z".to_string(),
    }
}

fn readiness(default_agent_did: &str) -> NodeReadinessRow {
    NodeReadinessRow {
        node_did: "did:key:zAgent".to_string(),
        snapshot_json: serde_json::to_string(&NodeReadinessSnapshot {
            format_version: NODE_READINESS_FORMAT_VERSION,
            process_state: NodeReadinessProcessState::Ready,
            active_generation: 1,
            router_generation: 1,
            default_agent_did: default_agent_did.to_string(),
            agents: vec![AgentReadinessEntry {
                agent_did: default_agent_did.to_string(),
                state: AgentReadinessState::Ready,
                reason: None,
            }],
        })
        .unwrap(),
        updated_at: "2026-06-04T00:00:00Z".to_string(),
    }
}

#[test]
fn p2p_metrics_snapshot_decodes_pinned_live_sync_status() {
    let mut sync_status = gents::P2pSyncStatusSnapshot::default();
    sync_status.push_backlog.queued_items = 7;
    sync_status.push_backlog.stale_head_retirements_total = 17;
    sync_status.push_backlog.peer_capacity_parks_total = 13;
    sync_status.push_backlog.per_peer = vec![gents::P2pPeerBacklogSnapshot {
        peer_id: "peer-a".to_string(),
        consecutive_failures: 3,
        ..Default::default()
    }];
    sync_status.push_updates_coalesced_total = 43;
    sync_status.persisted_pending_dags = 17;
    sync_status.missing_link_retries = 23;
    sync_status.gossip_direction_filtered_total = 47;
    sync_status.pending_dag_capacity_shed = 59;
    sync_status.next_pending_retry_in_ms = Some(71);
    sync_status.pending_dag_terminal_quarantined = 73;
    sync_status.quarantined_pending_dags = 79;
    let snapshot = p2p_metrics_from_status(
        &json!({
            "enabled": true,
            "p2p_connected_peers": ["peer-a", "peer-b"],
            "p2p_replicator_count": 2,
            "p2p_sync_status": serde_json::to_value(sync_status)
                .expect("serialize pinned sync status")
        }),
        None,
    );

    assert_eq!(snapshot.connected_peers, 2);
    assert_eq!(snapshot.replicators, 2);
    let sync = snapshot.sync_status.expect("valid pinned sync status");
    assert_eq!(sync.push_backlog.queued_items, 7);
    assert_eq!(sync.push_backlog.stale_head_retirements_total, 17);
    assert_eq!(sync.push_backlog.peer_capacity_parks_total, 13);
    assert_eq!(sync.push_backlog.per_peer[0].consecutive_failures, 3);
    assert_eq!(sync.push_updates_coalesced_total, 43);
    assert_eq!(sync.persisted_pending_dags, 17);
    assert_eq!(sync.missing_link_retries, 23);
    assert_eq!(sync.gossip_direction_filtered_total, 47);
    assert_eq!(sync.pending_dag_capacity_shed, 59);
    assert_eq!(sync.next_pending_retry_in_ms, Some(71));
    assert_eq!(sync.pending_dag_terminal_quarantined, 73);
    assert_eq!(sync.quarantined_pending_dags, 79);
}

#[tokio::test]
async fn status_lifecycle_is_starting_until_serve_marks_it_ready() {
    async fn lifecycle(state: &RuntimeHttpState) -> Value {
        let response = status_handler(State(state.clone()), &state.p2p_graphql).await;
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("status body");
        let body = serde_json::from_slice::<Value>(&body).expect("status json");
        assert_eq!(
            body[gents::plugin::approval::workspace::STATUS_SUPPORT_FIELD],
            json!(gents::plugin::approval::workspace::support()),
        );
        body[gents_protocol::serve_lifecycle::STATUS_LIFECYCLE_FIELD].clone()
    }
    let mut state = state();
    state.graphql = GraphqlEndpoint::anonymous("http://127.0.0.1:9/api/v0/graphql");
    state.p2p_graphql = state.graphql.clone();

    assert_eq!(lifecycle(&state).await, json!("starting"));
    state.serve_lifecycle.mark_ready();
    assert_eq!(lifecycle(&state).await, json!("ready"));
}

async fn stalled_activity_state(core_delay: Duration) -> RuntimeHttpState {
    use axum::{routing::post, Json, Router};

    let claimed_at = chrono::Utc::now() - chrono::Duration::seconds(120);
    let core = json!({
        "data": {
            "NodeRuntime": [serde_json::to_value(runtime()).unwrap()],
            "NodeReadiness": [serde_json::to_value(readiness("default")).unwrap()],
            "InferenceBackend": [],
            "AgentRequest": [{
                "_docID": "doc-req-1",
                "request_id": "req-1",
                "node_did": "did:key:zAgent",
                "claimed_at": claimed_at.to_rfc3339(),
                "deadline": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            }],
            "AgentToolCall": []
        }
    });
    let mock = Router::new().route(
        "/api/v0/graphql",
        post(move |Json(body): Json<Value>| {
            let core = core.clone();
            async move {
                let query = body["query"].as_str().unwrap_or_default().to_string();
                if query.contains("NodeRuntime") {
                    tokio::time::sleep(core_delay).await;
                    return Ok(Json(core));
                }
                if query.contains("InferenceCall(") {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    return Ok(Json(json!({ "data": {} })));
                }
                Err(StatusCode::NOT_FOUND)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock graphql");
    let addr = listener.local_addr().expect("mock addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, mock).await;
    });
    let mut state = state();
    state.graphql = GraphqlEndpoint::anonymous(format!("http://{addr}/api/v0/graphql"));
    state.p2p_graphql = state.graphql.clone();
    state
}

async fn health_body(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("health body");
    (status, serde_json::from_slice(&bytes).expect("health json"))
}

fn progress_age_ms(body: &Value) -> i64 {
    body.pointer("/liveness/requests/0/last_progress_age_ms")
        .and_then(Value::as_i64)
        .expect("liveness request row")
}

#[tokio::test]
async fn stalled_liveness_activity_read_does_not_hold_health() {
    let state = stalled_activity_state(Duration::ZERO).await;

    let started = std::time::Instant::now();
    let (status, healthz) = health_body(healthz_handler(State(state.clone())).await).await;
    assert!(
        started.elapsed() < Duration::from_millis(1_500),
        "/healthz waited {:?} on a stalled activity read",
        started.elapsed()
    );
    assert_eq!(status, StatusCode::OK, "{healthz}");
    assert_eq!(healthz["ok"], json!(true), "{healthz}");
    assert!((120_000..180_000).contains(&progress_age_ms(&healthz)));

    let started = std::time::Instant::now();
    let (_, status_body) = health_body(status_handler(State(state.clone()), &state.p2p_graphql).await).await;
    assert!(
        started.elapsed() < STATUS_PROBE_BUDGET + P2P_METRICS_FETCH_BUDGET,
        "/status waited {:?}",
        started.elapsed()
    );
    assert_eq!(status_body["ok"], json!(true), "{status_body}");
    assert!((120_000..180_000).contains(&progress_age_ms(&status_body)));
}

#[tokio::test]
async fn late_core_read_leaves_status_ok_when_activity_stalls() {
    let state = stalled_activity_state(Duration::from_millis(1_700)).await;

    let started = std::time::Instant::now();
    let (_, status_body) = health_body(status_handler(State(state.clone()), &state.p2p_graphql).await).await;
    assert!(
        started.elapsed() < STATUS_PROBE_BUDGET + P2P_METRICS_FETCH_BUDGET,
        "/status waited {:?}",
        started.elapsed()
    );
    assert_eq!(status_body["ok"], json!(true), "{status_body}");
    assert!((120_000..180_000).contains(&progress_age_ms(&status_body)));
}

#[tokio::test]
async fn disabled_p2p_metrics_skip_live_status_fetch() {
    let mut state = state();
    state.graphql = GraphqlEndpoint::anonymous("http://127.0.0.1:9/api/v0/graphql");
    state.p2p_graphql = state.graphql.clone();

    let snapshot = load_p2p_metrics_for_scrape(&state).await;

    assert!(!snapshot.enabled);
    assert!(!snapshot.stale);
    assert_eq!(snapshot.connected_peers, 0);
    assert_eq!(snapshot.replicators, 0);
    assert!(snapshot.admission.is_none());
    assert!(snapshot.sync_status.is_none());
}

async fn usage_route(
    state: &RuntimeHttpState,
    command: crate::commands::accounts::UsageReadCommand,
    bearer: Option<&dyn gents::NodeIdentity>,
) -> (StatusCode, Value) {
    let app = Router::new()
        .route("/accounts/usage/read", post(account_usage_read_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let _server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let client = reqwest::Client::new();
    let request = client
        .post(format!("http://{address}/accounts/usage/read"))
        .json(&command);
    let request = if let Some(identity) = bearer {
        GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), identity.did())
            .with_client_signing_capability(identity.client_signing_capability().unwrap())
            .unwrap()
            .authorize(request)
            .unwrap()
    } else {
        request
    };
    let response = request.send().await.unwrap();
    let status = response.status();
    (status, response.json().await.expect("usage json"))
}

fn usage_command(signer_did: &str) -> crate::commands::accounts::UsageReadCommand {
    crate::commands::accounts::UsageReadCommand {
        trigger: gents::inference::usage_observation::UsageTrigger::Open,
        provider: None,
        signer_did: signer_did.to_string(),
        issued_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        nonce: "nonce-usage-1".to_string(),
        sig: vec![0; 64],
    }
}

#[tokio::test]
async fn account_usage_route_is_unavailable_before_activation() {
    let (status, body) = usage_route(&state(), usage_command("did:key:zAgent"), None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"].is_string(), "{body}");
}

#[tokio::test]
async fn account_usage_route_refuses_unsigned_and_foreign_signers() {
    let temp = tempfile::tempdir().unwrap();
    let key = |name: &str| -> Arc<dyn gents::NodeIdentity> {
        Arc::new(gents::KeyIdentity::load_or_create(temp.path().join(name), None).unwrap())
    };
    // The owner is the enrolled User that published the Node; the runtime
    // authenticates its signed reads against the Node it can see.
    let home = crate::shared::test_support::ClientHome::new().await;
    let identity = home.home.identity.clone();
    let owner = home.user_signer();
    let other = key("other.key");
    let node = home.node().clone();
    let mut state = state();
    state.node_did = identity.did().into();
    *state.enrollment_decisions.write().await = Some(
        crate::http::enrollment::EnrollmentDecisionService::new(identity.clone(), node),
    );
    let signed = |identity: &Arc<dyn gents::NodeIdentity>| {
        let identity = identity.clone();
        async move {
            crate::commands::accounts::UsageReadCommand::signed(
                identity.as_ref(),
                gents::inference::usage_observation::UsageTrigger::Open,
                None,
            )
            .await
            .unwrap()
        }
    };

    let (status, body) =
        usage_route(&state, usage_command(owner.did()), Some(owner.as_ref())).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "unsigned: {body}");
    let (status, body) = usage_route(&state, signed(&other).await, Some(other.as_ref())).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "foreign: {body}");

    let command = signed(&owner).await;
    let (status, body) = usage_route(&state, command.clone(), Some(other.as_ref())).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "bearer mismatch: {body}");
    let (status, body) = usage_route(&state, command.clone(), Some(owner.as_ref())).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "signed: {body}");
    let (status, body) = usage_route(&state, command, Some(owner.as_ref())).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "replay: {body}");
}
