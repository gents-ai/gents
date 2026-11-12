use super::super::*;
use crate::client::schema::ensure_runtime_schemas;
use gents_protocol::schemas::AGENT_MESSAGE_NAME;
use std::sync::Arc;

#[tokio::test]
async fn bounded_task_history_preserves_physical_twins_at_the_cutoff() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
    ensure_runtime_schemas(&node).await.expect("schemas");
    let mut docs = Vec::new();
    for index in 0..24 {
        let input = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
            "node_did":"did:task-history", "request_id":"same-logical-request",
            "session_id":"task-session", "caused_by_trigger_id":"task-trigger",
            "created_at":"2026-10-15T00:00:00Z", "lifecycle_state":"completed",
            "purpose":"normal", "content":format!("physical twin {index}")
        }))
        .unwrap();
        let result = gents_store::config_client::ConfigAccess::write_local(
            &node,
            "test.task_history_twins",
            &format!("mutation {{ create_AgentRequest(input: {input}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
        docs.push(gents_store::graphql::created_doc_id(&result, "AgentRequest").unwrap());
    }
    for index in 0..9 {
        let input = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
            "node_did":"did:task-history", "request_id":format!("legacy-{index}"),
            "session_id":"task-session", "caused_by_trigger_id":"task-trigger",
            "created_at":"2026-10-16T00:00:00Z", "lifecycle_state":"completed",
            "purpose":null
        }))
        .unwrap();
        gents_store::config_client::ConfigAccess::write_local(
            &node,
            "test.task_history_nonpublic",
            &format!("mutation {{ create_AgentRequest(input: {input}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
    }
    docs.sort();
    let data = serde_json::json!({
        "requests":[], "sessions":[],
        "triggers":[{"node_did":"did:task-history", "trigger_id":"task-trigger"}]
    });
    let rows = complete_local_snapshot_requests(&node, &data, "did:task-history")
        .await
        .expect("bounded task history");
    assert!(
        rows.len() <= 16,
        "only candidates and cutoff refinements are loaded"
    );
    let mut actual = rows
        .iter()
        .map(|row| row.doc_id.clone().unwrap())
        .collect::<Vec<_>>();
    actual.sort();
    actual.truncate(8);
    assert_eq!(
        actual,
        docs[..8],
        "same top eight as complete history, including physical ties"
    );
    node.shutdown().await.unwrap();
}

#[test]
fn complete_request_projection_rejects_mixed_valid_and_malformed_rows() {
    let data = serde_json::json!({"requests":[
        {"request_id":"valid", "lifecycle_state":"pending"},
        {"request_id":"malformed", "lifecycle_state":42}
    ]});
    assert!(complete_request_rows(&data, "requests").is_err());
}

#[test]
fn session_head_seed_requires_observed_physical_and_logical_identity() {
    let rows = vec![
        serde_json::from_value::<gents_protocol::row::AgentRequestRow>(serde_json::json!({
            "_docID": "request-doc",
            "request_id": "observed-request",
            "purpose": "normal",
            "node_did": "did:alpha",
            "session_id": "session-a",
            "requester_did": "did:requester"
        }))
        .expect("valid observed row"),
        serde_json::from_value::<gents_protocol::row::AgentRequestRow>(serde_json::json!({
            "_docID": "request-doc",
            "request_id": "other-logical-request",
            "purpose": "normal",
            "node_did": "did:alpha",
            "session_id": "session-a",
            "requester_did": "did:requester"
        }))
        .expect("valid mismatched row"),
    ];
    let scopes = vec![(
        "request-doc".to_owned(),
        "did:alpha".to_owned(),
        "session-a".to_owned(),
        Some("did:requester".to_owned()),
        "observed-request".to_owned(),
    )];

    let accepted = scoped_session_heads(rows, &scopes);
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0].request_id, "observed-request");
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
            "endpoint": "http://localhost:8000/v1", "auth_kind": "unauthenticated",
            "enabled": true, "future_backend_config": true
        }],
        "backend_observations": [{"node_did":"did:alpha", "backend_id":"b", "catalogs":[], "last_probe":null, "probe_status":"ready"}]
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
    let backends: Vec<gents_documents::InferenceBackendMetadata> =
        parse_projected_query_rows(&data, "inference_backends", INFERENCE_BACKEND_FIELDS, "")
            .expect("row-level malformed config is skipped");
    let mut valid_backend_data = data.clone();
    valid_backend_data["inference_backends"][0]
        .as_object_mut()
        .expect("backend row")
        .remove("future_backend_config");
    let valid_backends: Vec<gents_documents::InferenceBackendMetadata> =
        parse_projected_query_rows(
            &valid_backend_data,
            "inference_backends",
            INFERENCE_BACKEND_FIELDS,
            "",
        )
        .expect("canonical backend fixture");
    let backend_observations: Vec<InferenceBackendObservation> = parse_observation_query_rows(
        &data,
        "backend_observations",
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
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
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
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
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
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    gents_store::config_client::ConfigAccess::write_local(
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
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
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
        "default_agent_did",
        "runnable_agent_count",
        "unavailable_agent_count",
    ] {
        assert!(!fields.contains(&forbidden), "forbidden field: {forbidden}");
    }
}

#[tokio::test]
async fn load_node_runtimes_hydrates_executor_capacity_and_queue_depth() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
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
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
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
async fn load_agent_tool_calls_hydrates_job_pin_and_progress() {
    use gents_protocol::job::{JobBudget, JobCeiling, JobOrigin, JobPin, JobProgress, JobState};
    use gents_store::config_client::ConfigAccess;

    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let pin = JobPin {
        script_id: "fixture-script".into(),
        source_digest: "sha256:source".into(),
        origin: JobOrigin::Installed,
        engine_digest: "sha256:engine".into(),
        plugin_ref: "fixture/workflow".into(),
        agent_budget: 4,
        handle: "job-1".into(),
        cwd: "/fixture/cwd".into(),
        adopted_at: 1_780_000_000_000,
        args_digest: "sha256:args".into(),
        chain_root_doc_id: "root-doc".into(),
        resume_of: None,
        replayed_from: None,
        ceiling_at_pin: JobCeiling {
            targets: vec!["general".into()],
            max_live: 2,
            max_spawns: 8,
            max_messages_per_slot: 4,
            result_bytes: 65_536,
            max_wall_secs: 600,
        },
    };
    let progress = JobProgress {
        title: "Job one".into(),
        state: JobState::Running,
        pause_reason: None,
        phases: Vec::new(),
        active: Vec::new(),
        spawned: 2,
        budget: JobBudget {
            total: 4,
            spent: 2,
            remaining: 2,
        },
        last_event: "seeded".into(),
        last_event_at: None,
        question: None,
        observed_terminal_children: 0,
        revision: 1,
    };
    let mutation = r#"mutation($job_pin: JSON, $job_progress: JSON) {
        create_AgentToolCall(input: {
            tool_call_key: "session-1:job-1",
            request_id: "parent-1",
            session_id: "session-1",
            message_sequence: 1,
            tool_name: "workflow",
            tool_call_id: "job-1",
            status: "running",
            lifecycle_state: "running",
            await_mode: "background",
            started_at: "2026-07-29T00:00:00Z",
            job_pin: $job_pin,
            job_progress: $job_progress
        }) { tool_call_key }
    }"#;
    let variables = serde_json::json!({ "job_pin": pin, "job_progress": progress });
    ConfigAccess::local(node.clone())
        .transact_write("test.fixture.variables", |txn| {
            let variables = variables.clone();
            Box::pin(async move { txn.execute_with_variables(mutation, &variables).await })
        })
        .await
        .expect("seed job row");

    let tool_calls = load_agent_tool_calls(node.as_ref())
        .await
        .expect("load agent tool calls");
    let tool_call = tool_calls
        .iter()
        .find(|row| row.tool_call_key == "session-1:job-1")
        .expect("created tool call");
    assert_eq!(tool_call.job_pin.as_ref(), Some(&pin));
    assert_eq!(tool_call.job_progress.as_ref(), Some(&progress));
    assert!(!AGENT_TOOL_CALL_FIELDS.contains("job_step_state"));
}

#[tokio::test]
async fn load_node_scoped_snapshot_excludes_other_nodes() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let mutation = r#"mutation {
        alpha: create_AgentSession(input: {
            session_id: "alpha-1",
            node_did: "did:alpha",
            agent_did: "default",
            title: { text: "alpha", source: "user" },
            created_at: "2026-05-07T00:00:00Z"
        }) { _docID }
        beta: create_AgentSession(input: {
            session_id: "beta-1",
            node_did: "did:beta",
            agent_did: "default",
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

#[test]
fn catalog_freshness_failure_keeps_other_backend_observations() {
    let data = serde_json::json!({"inference_backends":[
        {"backend_id":"bad", "node_did":"did:bad", "catalogs":null,
            "catalog_freshness":{"version":255,"entries":[]}},
        {"backend_id":"good", "node_did":"did:good", "catalogs":null}
    ]});
    let observations = parse_backend_observation_rows(&data, "inference_backends").unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].backend_id, "good");
}
