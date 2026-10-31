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
async fn background_control_authenticates_the_owning_operator_before_runtime_access() {
    use gents_desktop_core::client::{DesktopPaths, PrincipalIdentity};
    use gents_protocol::job::{JobControlKind, OperatorJobControlRequest, JOB_CONTROL_PATH};
    use gents_protocol::process_control::{OperatorProcessControlRequest, PROCESS_CONTROL_PATH};
    let temp = tempfile::tempdir().unwrap();
    let owner_paths = DesktopPaths::from_root(temp.path().join("owner"));
    owner_paths.ensure_root_dirs().await.unwrap();
    let owner = PrincipalIdentity::load_or_create(&owner_paths)
        .await
        .unwrap();
    let foreign_paths = DesktopPaths::from_root(temp.path().join("foreign"));
    foreign_paths.ensure_root_dirs().await.unwrap();
    let foreign = PrincipalIdentity::load_or_create(&foreign_paths)
        .await
        .unwrap();
    let mut state = state();
    state.node_did = owner.did().to_owned();
    let app = Router::new()
        .route(
            JOB_CONTROL_PATH,
            post(crate::http::job_control::job_control_handler),
        )
        .route(
            PROCESS_CONTROL_PATH,
            post(crate::http::process_control::process_control_handler),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let client = reqwest::Client::new();
    let graphql = format!("http://{address}/api/v0/graphql");
    let command = OperatorJobControlRequest {
        node_did: owner.did().into(),
        tool_call_doc_id: "physical-job".into(),
        action: JobControlKind::Stop,
    };
    let process = OperatorProcessControlRequest {
        node_did: owner.did().into(),
        session_id: "session".into(),
        tool_call_id: "native".into(),
        tool_call_doc_id: "physical-native".into(),
    };
    for (path, command) in [
        (JOB_CONTROL_PATH, serde_json::to_value(command).unwrap()),
        (PROCESS_CONTROL_PATH, serde_json::to_value(process).unwrap()),
    ] {
        let url = format!("http://{address}{path}");
        let response = client.post(&url).json(&command).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let endpoint = GraphqlEndpoint::as_principal(&graphql, foreign.did());
        let response = endpoint
            .authorize(client.post(&url))
            .unwrap()
            .json(&command)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let endpoint = GraphqlEndpoint::as_principal(&graphql, owner.did());
        let mut other = command.clone();
        other["nodeDid"] = serde_json::json!(foreign.did());
        let response = endpoint
            .authorize(client.post(&url))
            .unwrap()
            .json(&other)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let response = endpoint
            .authorize(client.post(&url))
            .unwrap()
            .json(&command)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
    drop(server);
}

#[tokio::test]
async fn runtime_control_binds_signed_observation_and_retires_only_the_operator_observed_build() {
    use gents::store::config_client::ConfigAccess;
    use gents_desktop_core::client::{DesktopPaths, PrincipalIdentity};
    use gents_protocol::serve_lifecycle::control::{
        RuntimeControlCommand, RuntimeControlDisposition, RuntimeControlReceipt, RuntimeOperation,
        RUNTIME_CONTROL_PATH, RUNTIME_CONTROL_VERSION,
    };
    let temp = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(temp.path().join("operator"));
    paths.ensure_root_dirs().await.unwrap();
    let identity = Arc::new(PrincipalIdentity::load_or_create(&paths).await.unwrap());
    let foreign_paths = DesktopPaths::from_root(temp.path().join("foreign"));
    foreign_paths.ensure_root_dirs().await.unwrap();
    let foreign = PrincipalIdentity::load_or_create(&foreign_paths)
        .await
        .unwrap();
    let owner = identity.did().to_owned();
    let node = Arc::new(
        gents::defra_node::EmbeddedNode::builder()
            .with_node_identity_did(&owner)
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
    ConfigAccess::transact_local_write(&node, None, "test.runtime_control.setup", |txn| {
        let owner = &owner;
        Box::pin(async move {
            txn.execute_with_variables(
                "mutation($node: NodeMutationInputArg!, $backend: InferenceBackendMutationInputArg!) { create_Node(input: $node) { _docID } create_InferenceBackend(input: $backend) { _docID } }",
                &json!({
                    "node": {"node_did": owner, "default_agent_id": "fixture", "enabled": true},
                    "backend": {"node_did": owner, "backend_id": "fixture", "name": "Fixture",
                        "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:9/v1",
                        "auth": {"kind": "unauthenticated"}, "enabled": true, "probe_status": "healthy"},
                }),
            ).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
    let runtime = gents::Gents::builder()
        .node(node.clone())
        .identity(identity.clone())
        .tool_ceiling(gents::ToolCeiling::meta_only())
        .agent("fixture")
        .backend_id("fixture")
        .model_name("fixture")
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
    let endpoint = GraphqlEndpoint::as_principal(&graphql, &owner);
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
    let foreign_endpoint = GraphqlEndpoint::as_principal(&graphql, foreign.did());
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

    node.shutdown().await;
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
async fn operator_inventory_authenticates_and_replays_native_metadata() {
    use gents::store::config_client::ConfigAccess;
    use gents_desktop_core::client::{DesktopPaths, PrincipalIdentity};
    use gents_protocol::file_inventory::{
        FileInventoryAdmission, FileInventoryPageDemand, FileInventoryRefusalReason,
        FileInventoryReply, FILE_INVENTORY_OPERATOR_PATH,
    };
    use gents_protocol::session_hydration::HydrationRequest;
    let temp = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(temp.path().join("operator"));
    paths.ensure_root_dirs().await.unwrap();
    let identity = Arc::new(PrincipalIdentity::load_or_create(&paths).await.unwrap());
    let foreign_paths = DesktopPaths::from_root(temp.path().join("foreign"));
    foreign_paths.ensure_root_dirs().await.unwrap();
    let foreign = PrincipalIdentity::load_or_create(&foreign_paths)
        .await
        .unwrap();
    let node = Arc::new(
        gents::defra_node::EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
    let owner = identity.did();
    let session_id = "operator-http-session";
    let session_doc_id = ConfigAccess::transact_local_write(&node, None, "test.inventory.http_setup", |txn| {
        Box::pin(async move {
            let result = txn.execute_with_variables(
                "mutation($node: NodeMutationInputArg!, $backend: InferenceBackendMutationInputArg!, $session: AgentSessionMutationInputArg!) { create_Node(input: $node) { _docID } create_InferenceBackend(input: $backend) { _docID } create_AgentSession(input: $session) { _docID } }",
                &json!({
                    "node": {"node_did": owner, "default_agent_id": "fixture", "enabled": true},
                    "backend": {"node_did": owner, "backend_id": "fixture", "name": "Fixture",
                        "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:9/v1",
                        "auth": {"kind": "unauthenticated"}, "enabled": true, "probe_status": "healthy"},
                    "session": {"session_id": session_id, "node_did": owner, "requester_did": owner,
                        "agent_id": "fixture", "created_at": "2026-10-24T00:00:00Z"},
                })).await?;
            gents::store::graphql::created_doc_id(&result, "AgentSession")
        })
    }).await.unwrap();
    let runtime = gents::Gents::builder()
        .node(node.clone())
        .identity(identity.clone())
        .tool_ceiling(gents::ToolCeiling::meta_only())
        .agent("fixture")
        .backend_id("fixture")
        .model_name("fixture")
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
    let endpoint = GraphqlEndpoint::as_principal(&graphql, owner);
    let url = format!("http://{addr}{FILE_INVENTORY_OPERATOR_PATH}");
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let demand = FileInventoryPageDemand {
        admission: FileInventoryAdmission {
            request: HydrationRequest {
                request_key: format!("operator-client:{session_id}"),
                peer_id: "operator-client".into(),
                requester_did: owner.into(),
                node_did: owner.into(),
                session_id: session_id.into(),
            },
            session_doc_id,
        },
        attempt_id: "operator-http-attempt".into(),
        cursor: None,
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
        gents::store::identity::defradb_bearer_authorization(owner, "other.invalid").unwrap();
    let response = client
        .post(&url)
        .header(header::AUTHORIZATION, wrong_audience)
        .json(&demand)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    let foreign_endpoint = GraphqlEndpoint::as_principal(&graphql, foreign.did());
    let response = foreign_endpoint
        .authorize(client.post(&url))
        .unwrap()
        .json(&demand)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let access = ConfigAccess::local(node.clone());
    assert!(
        access
            .execute("{ FileInventoryPage { _docID } }")
            .await
            .unwrap()["data"]["FileInventoryPage"]
            .as_array()
            .unwrap()
            .is_empty(),
        "unauthorized reads must not prepare metadata"
    );
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
        let reply: FileInventoryReply = response.json().await.unwrap();
        gents::file_inventory::verify_reply(&demand, &reply, identity.as_ref())
            .await
            .unwrap();
        let FileInventoryReply::Page(page) = &reply else {
            panic!("operator received a refusal");
        };
        assert_eq!(page.page.binding.coverage.entries, 0);
        assert_eq!(page.page.binding.coverage.pending, 0);
        assert_eq!(page.page.binding.coverage.unknown, 0);
        if let Some(prior) = &retained {
            assert_eq!(&reply, prior);
        }
        retained = Some(reply);
    }
    let mut cross_source = demand.clone();
    cross_source.admission.request.node_did = foreign.did().into();
    let response = endpoint
        .authorize(client.post(&url))
        .unwrap()
        .json(&cross_source)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let mut missing = demand.clone();
    missing.admission.request.session_id.push_str("-missing");
    missing.attempt_id.push_str("-missing");
    let response = endpoint
        .authorize(client.post(&url))
        .unwrap()
        .json(&missing)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let refused: FileInventoryReply = response.json().await.unwrap();
    gents::file_inventory::verify_reply(&missing, &refused, identity.as_ref())
        .await
        .unwrap();
    assert!(matches!(refused, FileInventoryReply::Refused(receipt)
        if receipt.reason == FileInventoryRefusalReason::Unavailable));
    drop(server);
    drop(access);
    node.shutdown().await;
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
    let foreign_audience =
        gents::store::identity::defradb_bearer_authorization(identity.did(), "other.invalid")
            .unwrap();
    let response = client
        .get(&url)
        .header(header::AUTHORIZATION, foreign_audience)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    let endpoint =
        GraphqlEndpoint::as_principal(format!("http://{addr}/api/v0/graphql"), identity.did());
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

fn readiness(default_agent_id: &str) -> NodeReadinessRow {
    NodeReadinessRow {
        node_did: "did:key:zAgent".to_string(),
        snapshot_json: serde_json::to_string(&NodeReadinessSnapshot {
            format_version: NODE_READINESS_FORMAT_VERSION,
            process_state: NodeReadinessProcessState::Ready,
            active_generation: 1,
            router_generation: 1,
            default_agent_id: default_agent_id.to_string(),
            agents: vec![AgentReadinessEntry {
                agent_id: default_agent_id.to_string(),
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
        let response = status_handler(State(state.clone())).await;
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
    let (_, status_body) = health_body(status_handler(State(state)).await).await;
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
    let (_, status_body) = health_body(status_handler(State(state)).await).await;
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
) -> (StatusCode, Value) {
    let response = account_usage_read_handler(State(state.clone()), axum::Json(command)).await;
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("usage body");
    (status, serde_json::from_slice(&body).expect("usage json"))
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
    let (status, body) = usage_route(&state(), usage_command("did:key:zAgent")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"].is_string(), "{body}");
}

#[tokio::test]
async fn account_usage_route_refuses_unsigned_and_foreign_signers() {
    let temp = tempfile::tempdir().unwrap();
    let key = |name: &str| -> Arc<dyn gents::NodeIdentity> {
        Arc::new(gents::KeyIdentity::load_or_create(temp.path().join(name), None).unwrap())
    };
    let (identity, other) = (key("runtime.key"), key("other.key"));
    let node = Arc::new(
        crate::shared::test_support::node_builder()
            .build()
            .await
            .unwrap(),
    );
    gents::store::schema::migration::ensure_all_runtime_migrations(node.clone())
        .await
        .unwrap();
    let state = state();
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

    let (status, body) = usage_route(&state, usage_command(identity.did())).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "unsigned: {body}");
    let (status, body) = usage_route(&state, signed(&other).await).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "foreign: {body}");

    let command = signed(&identity).await;
    let (status, body) = usage_route(&state, command.clone()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "signed: {body}");
    let (status, body) = usage_route(&state, command).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "replay: {body}");
}
