use super::super::*;
use crate::client::schema::ensure_runtime_schemas;
use defra_node::NodeBuilder;
use gents_protocol::schemas::AGENT_MESSAGE_NAME;
use std::sync::Arc;

#[test]
fn snapshot_coalesces_shared_roots_and_splits_strict_projections() {
    let query = snapshot_query(Some("filter: { node_did: { _eq: \"did:alpha\" } }"))
        .expect("snapshot query");
    assert_eq!(query.matches("triggers: Trigger(").count(), 1);
    assert_eq!(
        query
            .matches("inference_backends: InferenceBackend(")
            .count(),
        1
    );
    assert!(query.contains("_eq: \"did:alpha\""));
    assert_eq!(
        TRIGGER_FIELDS,
        gents::config_client::config_projection(gents::Collection::Trigger, None)
            .unwrap()
            .0
            .join(" ")
    );
    assert_eq!(
        {
            let mut fields = INFERENCE_BACKEND_FIELDS
                .split_whitespace()
                .collect::<Vec<_>>();
            fields.sort_unstable();
            fields
        },
        {
            let mut fields =
                gents::config_client::config_projection(gents::Collection::InferenceBackend, None)
                    .unwrap()
                    .0
                    .to_vec();
            fields.sort_unstable();
            fields
        }
    );

    let data = serde_json::json!({"triggers": [{
        "node_did": "did:alpha", "trigger_id": "trigger-1", "task_id": "task-1",
        "source": {"kind": "event", "event_source_id": "source-1"},
        "last_status": "ok", "last_attempt_at": null,
        "last_fired_source_doc_id": null, "last_error": null, "fire_count": 1,
        "next_run_at": null
    }, {
        "trigger_id": "malformed-config-but-valid-observation", "last_status": "ok",
        "last_attempt_at": null, "last_fired_source_doc_id": null, "last_error": null,
        "fire_count": 2, "next_run_at": null
    }]});
    let trigger: Vec<Trigger> = parse_projected_query_rows(
        &data,
        "triggers",
        TRIGGER_FIELDS,
        &format!("{TRIGGER_OBSERVATION_FIELDS} {SCHEDULE_OBSERVATION_FIELDS}"),
    )
    .expect("strict config projection");
    let observation: Vec<TriggerObservation> =
        parse_observation_query_rows(&data, "triggers", TRIGGER_OBSERVATION_FIELDS)
            .expect("independent observation projection");
    let schedule_observation: Vec<ScheduleObservation> =
        parse_observation_query_rows(&data, "triggers", SCHEDULE_OBSERVATION_FIELDS)
            .expect("independent schedule observation projection");
    assert_eq!(trigger.len(), 1);
    assert_eq!(observation.len(), 2);
    assert_eq!(observation[1].last_status.as_deref(), Some("ok"));
    assert_eq!(schedule_observation.len(), 2);
}

#[test]
fn combined_snapshot_keeps_unknown_config_fields_rejected_and_observations_independent() {
    let data = serde_json::json!({
        "triggers": [{
            "node_did": "did:alpha", "trigger_id": "t", "task_id": "task",
            "source": {"kind": "event", "event_source_id": "source"},
            "future_trigger_config": true, "last_status": "ok", "last_attempt_at": null,
            "last_fired_source_doc_id": null, "last_error": null, "fire_count": 1
        }],
        "inference_backends": [{
            "node_did": "did:alpha", "backend_id": "b", "name": "backend",
            "provider_kind": "OpenAiCompatible", "openai_wire_api": "chat_completions",
            "endpoint": "http://localhost:8000/v1", "auth": {"kind": "unauthenticated"},
            "enabled": true, "future_backend_config": true,
            "catalogs": [], "last_probe": null, "probe_status": "ready"
        }]
    });

    let triggers: Vec<Trigger> = parse_projected_query_rows(
        &data,
        "triggers",
        TRIGGER_FIELDS,
        &format!("{TRIGGER_OBSERVATION_FIELDS} {SCHEDULE_OBSERVATION_FIELDS}"),
    )
    .expect("row-level malformed config is skipped");
    let trigger_observations: Vec<TriggerObservation> =
        parse_observation_query_rows(&data, "triggers", TRIGGER_OBSERVATION_FIELDS)
            .expect("trigger observation remains independently decodable");
    let backends: Vec<InferenceBackend> = parse_projected_query_rows(
        &data,
        "inference_backends",
        INFERENCE_BACKEND_FIELDS,
        INFERENCE_BACKEND_OBSERVATION_FIELDS,
    )
    .expect("row-level malformed config is skipped");
    let mut valid_backend_data = data.clone();
    valid_backend_data["inference_backends"][0]
        .as_object_mut()
        .expect("backend row")
        .remove("future_backend_config");
    let valid_backends: Vec<InferenceBackend> = parse_projected_query_rows(
        &valid_backend_data,
        "inference_backends",
        INFERENCE_BACKEND_FIELDS,
        INFERENCE_BACKEND_OBSERVATION_FIELDS,
    )
    .expect("canonical backend fixture");
    let backend_observations: Vec<InferenceBackendObservation> = parse_observation_query_rows(
        &data,
        "inference_backends",
        INFERENCE_BACKEND_OBSERVATION_FIELDS,
    )
    .expect("backend observation remains independently decodable");

    assert!(
        triggers.is_empty(),
        "unknown Trigger fields must not be stripped"
    );
    assert_eq!(trigger_observations[0].last_status.as_deref(), Some("ok"));
    assert!(
        backends.is_empty(),
        "unknown backend fields must not be stripped"
    );
    assert_eq!(
        valid_backends.len(),
        1,
        "control backend fixture must decode"
    );
    assert_eq!(
        backend_observations[0].probe_status.as_deref(),
        Some("ready")
    );
}

#[test]
fn malformed_default_backend_keeps_operator_config_authority_unresolved() {
    let mut data = serde_json::json!({
        "nodes": [], "agents": [], "runtimes": [], "node_readiness": [],
        "contexts": [], "tools": [], "inference_profiles": [], "inference_sampling": [],
        "inference_execution": [], "tasks": [], "schedules": [], "triggers": [],
        "skills": [], "compactions": [], "tool_service_registries": [],
        "event_sources": [], "agent_targets": [], "datastore_tool_surfaces": [],
        "chain_key_bindings": [], "sessions": [], "requests": [],
        "inference_backends": [{
            "node_did": "did:alpha", "backend_id": "b", "name": "backend",
            "provider_kind": "OpenAiCompatible", "openai_wire_api": "chat_completions",
            "endpoint": "http://localhost:8000/v1", "auth": {"kind": "unauthenticated"},
            "enabled": true, "future_backend_config": true, "catalogs": [],
            "last_probe": null, "probe_status": "ready"
        }]
    });

    let partial = operator_config_store_from_data(&data, "did:alpha")
        .expect("malformed config rows remain a query result");
    assert!(partial.inference_backends.is_empty());
    assert_eq!(partial.backend_observations.len(), 1);
    assert!(!partial
        .operator_config_authoritative_node_dids
        .contains("did:alpha"));
    let local = ClientStore::from_rows(ClientStoreRows {
        operator_config_authoritative_node_dids: ["did:alpha".to_owned()].into_iter().collect(),
        ..ClientStoreRows::default()
    });
    let projected = local.overlay_node_operator_config("did:alpha", &partial);
    assert!(!projected
        .operator_config_authoritative_node_dids
        .contains("did:alpha"));

    data["inference_backends"][0]
        .as_object_mut()
        .expect("backend row")
        .remove("future_backend_config");
    let complete =
        operator_config_store_from_data(&data, "did:alpha").expect("canonical config rows decode");
    assert_eq!(complete.inference_backends.len(), 1);
    assert!(complete
        .operator_config_authoritative_node_dids
        .contains("did:alpha"));
    let projected = local.overlay_node_operator_config("did:alpha", &complete);
    assert!(projected
        .operator_config_authoritative_node_dids
        .contains("did:alpha"));
}

#[test]
fn patch_observation_provenance_stays_aligned_for_repeated_logical_ids() {
    let data = serde_json::json!({
        "triggers": [
        {
            "node_did": "did:alpha", "trigger_id": "shared", "future_config": true,
            "last_status": "alpha", "last_attempt_at": null,
            "last_fired_source_doc_id": null, "last_error": null, "fire_count": 1
        },
        {
            "node_did": "did:beta", "trigger_id": "shared", "future_config": true,
            "last_status": "beta", "last_attempt_at": null,
            "last_fired_source_doc_id": null, "last_error": null, "fire_count": 2
        }
        ],
        "inference_backends": [
            {"node_did": "did:alpha", "backend_id": "shared", "catalogs": [], "last_probe": null, "probe_status": "alpha"},
            {"node_did": "did:beta", "backend_id": "shared", "catalogs": [], "last_probe": null, "probe_status": "beta"}
        ]
    });
    let (observations, source_node_dids): (Vec<TriggerObservation>, Vec<Option<String>>) =
        parse_observation_query_rows_with_source(
            &data,
            "triggers",
            TRIGGER_OBSERVATION_FIELDS,
            Some("node_did"),
        )
        .expect("observation rows");

    assert_eq!(observations[0].last_status.as_deref(), Some("alpha"));
    assert_eq!(observations[1].last_status.as_deref(), Some("beta"));
    assert_eq!(
        source_node_dids,
        [Some("did:alpha".into()), Some("did:beta".into())]
    );
    let (backend_observations, backend_source_node_dids): (
        Vec<InferenceBackendObservation>,
        Vec<Option<String>>,
    ) = parse_observation_query_rows_with_source(
        &data,
        "inference_backends",
        INFERENCE_BACKEND_OBSERVATION_FIELDS,
        Some("node_did"),
    )
    .expect("backend observations");
    assert_eq!(
        backend_observations[0].probe_status.as_deref(),
        Some("alpha")
    );
    assert_eq!(
        backend_observations[1].probe_status.as_deref(),
        Some("beta")
    );
    assert_eq!(
        backend_source_node_dids,
        [Some("did:alpha".into()), Some("did:beta".into())]
    );
}

#[test]
fn access_query_data_keeps_the_owned_payload_allocation() {
    let body = serde_json::json!({
        "data": {"AgentOutputSegment": [{"payload": "x".repeat(65_536)}]},
        "errors": [],
        "extensions": {"trace": "query"},
    });
    let payload = body["data"]["AgentOutputSegment"][0]["payload"]
        .as_str()
        .unwrap()
        .as_ptr();
    let data = into_access_graphql_data(body, "query").unwrap();
    assert_eq!(
        data["AgentOutputSegment"][0]["payload"]
            .as_str()
            .unwrap()
            .as_ptr(),
        payload
    );
}

#[test]
fn access_query_data_preserves_response_errors_and_null_data() {
    let partial = serde_json::json!({
        "data": {"AgentMessage": []},
        "errors": [{"message": "denied"}],
    });
    let error = into_access_graphql_data(partial, "session page").unwrap_err();
    assert!(error.to_string().contains("session page returned errors"));
    assert!(error.to_string().contains("denied"));
    for body in [serde_json::json!({}), Value::Null, serde_json::json!([])] {
        assert_eq!(
            into_access_graphql_data(body, "session page")
                .unwrap_err()
                .to_string(),
            "session page returned no data"
        );
    }
    for errors in [Value::Null, serde_json::json!([]), serde_json::json!({})] {
        assert_eq!(
            into_access_graphql_data(serde_json::json!({"data": null, "errors": errors}), "query")
                .unwrap(),
            Value::Null
        );
    }
}

#[tokio::test]
async fn fetch_doc_patch_returns_empty_store_for_no_matches() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let patch = fetch_doc_patch(node.as_ref(), AGENT_MESSAGE_NAME, &["never-existed"])
        .await
        .expect("fetch_doc_patch");
    assert_eq!(patch.store.transcript_messages.len(), 0);
    assert_eq!(patch.observed_documents, 0);
}

#[tokio::test]
async fn fetch_doc_patch_empty_input_is_no_op() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let patch = fetch_doc_patch(&node, AGENT_MESSAGE_NAME, &[])
        .await
        .expect("fetch_doc_patch");
    assert_eq!(patch.store.row_count(), 0);
    assert_eq!(patch.observed_documents, 0);
}

#[tokio::test]
async fn trigger_patch_counts_physical_documents_for_mixed_present_and_missing_ids() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    gents::config_client::ConfigAccess::write_local(
        &node,
        "test.trigger_patch",
        r#"mutation {
            create_Trigger(input: {
                node_did: "did:test:patch",
                trigger_id: "trigger-present",
                task_id: "task",
                source: {kind: "schedule", schedule_id: "schedule"},
                enabled: true
            }) { _docID }
        }"#,
    )
    .await
    .expect("create trigger");
    let data = execute_local_graphql_query(
        node.as_ref(),
        r#"{ Trigger(filter: { node_did: { _eq: "did:test:patch" }, trigger_id: { _eq: "trigger-present" } }) { _docID } }"#,
        "read trigger fixture",
    )
    .await
    .expect("read trigger document id");
    let doc_id = data["Trigger"][0]["_docID"]
        .as_str()
        .expect("created trigger document id");

    let patch = fetch_doc_patch(
        node.as_ref(),
        TRIGGER_NAME,
        &[doc_id, "missing-trigger-document"],
    )
    .await
    .expect("mixed trigger patch");
    assert_eq!(patch.store.triggers.len(), 1);
    assert_eq!(patch.store.trigger_observations.len(), 1);
    assert_eq!(patch.store.schedule_observations.len(), 1);
    assert_eq!(patch.observed_documents, 1);
}

#[tokio::test]
async fn fetch_doc_patch_unknown_collection_errors() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let result = fetch_doc_patch(node.as_ref(), "NotARealCollection", &["x"]).await;
    assert!(result.is_err());
}

#[test]
fn doc_patch_support_excludes_pairing_control_collections() {
    assert!(supports_doc_patch_collection(INFERENCE_BACKEND_NAME));
    assert!(supports_doc_patch_collection(TOOL_SERVICE_REGISTRY_NAME));
    for collection in [
        "AgentContext",
        "CompactionConfig",
        "InferenceSampling",
        "InferenceExecution",
        "EventSource",
        "AgentTarget",
        "DatastoreToolSurface",
        "ChainKeyBinding",
    ] {
        assert!(supports_doc_patch_collection(collection), "{collection}");
    }
    assert!(!supports_doc_patch_collection("PeerPairingApplied"));
    assert!(!supports_doc_patch_collection("SessionHydrationRequest"));
}

#[test]
fn node_runtime_queries_cannot_reintroduce_readiness_authority() {
    let fields = NODE_RUNTIME_FIELDS.split_whitespace().collect::<Vec<_>>();
    for forbidden in [
        "process_state",
        "active_generation",
        "router_generation",
        "default_agent_id",
        "runnable_agent_count",
        "unavailable_agent_count",
    ] {
        assert!(!fields.contains(&forbidden), "forbidden field: {forbidden}");
    }
}

#[tokio::test]
async fn load_node_runtimes_hydrates_executor_capacity_and_queue_depth() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let response = node
        .execute(
            r#"mutation {
                create_NodeRuntime(input: {
                    node_did: "did:key:runtime-capacity",
                    agent_executor_capacity: 7,
                    agent_executor_queue_depth: 3
                }) { node_did }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let runtimes = load_node_runtimes(node.as_ref())
        .await
        .expect("load node runtimes");
    let runtime = runtimes
        .iter()
        .find(|row| row.node_did == "did:key:runtime-capacity")
        .expect("created runtime");
    assert_eq!(runtime.agent_executor_capacity, Some(7));
    assert_eq!(runtime.agent_executor_queue_depth, Some(3));
}

#[tokio::test]
async fn load_agent_tool_calls_hydrates_background_projection_fields() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let response = node
        .execute(
            r#"mutation {
                create_AgentToolCall(input: {
                    tool_call_key: "session-1:start-1",
                    request_id: "parent-1",
                    session_id: "session-1",
                    message_sequence: 1,
                    tool_name: "agent_new",
                    tool_call_id: "start-1",
                    status: "called",
                    lifecycle_state: "running",
                    await_mode: "background",
                    started_at: "2026-07-29T00:00:00Z"
                }) { tool_call_key }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let tool_calls = load_agent_tool_calls(node.as_ref())
        .await
        .expect("load agent tool calls");
    let tool_call = tool_calls
        .iter()
        .find(|row| row.tool_call_key == "session-1:start-1")
        .expect("created tool call");
    assert_eq!(tool_call.await_mode.as_deref(), Some("background"));
}

#[tokio::test]
async fn load_node_scoped_snapshot_excludes_other_nodes() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let mutation = r#"mutation {
        alpha: create_AgentSession(input: {
            session_id: "alpha-1",
            node_did: "did:alpha",
            agent_id: "default",
            title: { text: "alpha", source: "user" },
            created_at: "2026-05-07T00:00:00Z"
        }) { _docID }
        beta: create_AgentSession(input: {
            session_id: "beta-1",
            node_did: "did:beta",
            agent_id: "default",
            title: { text: "beta", source: "user" },
            created_at: "2026-05-07T00:00:00Z"
        }) { _docID }
    }"#;
    let response = node.execute(mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let goal_mutation = r#"mutation {
        alpha: create_Goal(input: {
            goal_id: "alpha-goal",
            session_id: "alpha-goal-only",
            node_did: "did:alpha",
            objective: "goal-only session",
            status: "active",
            created_at: "2026-05-07T00:00:00Z"
        }) { _docID }
        beta: create_Goal(input: {
            goal_id: "beta-goal",
            session_id: "beta-goal-only",
            node_did: "did:beta",
            objective: "other agent",
            status: "active",
            created_at: "2026-05-07T00:00:00Z"
        }) { _docID }
    }"#;
    let response = node.execute(goal_mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let store = load_node_scoped_snapshot(node.as_ref(), "did:alpha")
        .await
        .expect("load_node_scoped_snapshot");

    let dids: Vec<&str> = store
        .sessions
        .iter()
        .map(|session| session.node_did.as_str())
        .collect();
    assert_eq!(
        dids.len(),
        1,
        "the owning session must survive the scope filter"
    );
    assert!(
        dids.iter().all(|d| *d == "did:alpha"),
        "expected only did:alpha sessions; got {dids:?}"
    );
    assert_eq!(store.goals.len(), 1);
    assert_eq!(store.goals[0].session_id, "alpha-goal-only");
}
