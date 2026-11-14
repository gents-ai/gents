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
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        node.node_identity_did().unwrap().parse().unwrap(),
    );
    let triggers = vec![serde_json::from_value::<Trigger>(serde_json::json!({"node_did":"did:task-history","trigger_id":"task-trigger","task_id":"task","source":{"kind":"event","event_source_id":"source"}})).unwrap()];
    let rows =
        complete_access_snapshot_requests(&access, "did:task-history", &[], &triggers, Vec::new())
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

    let patch = fetch_doc_patch(
        &gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(node.node_identity_did().expect("fixture reader identity"))
                .expect("fixture DID"),
        ),
        "did:test:agent",
        AGENT_MESSAGE_NAME,
        &["never-existed"],
    )
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

    let patch = fetch_doc_patch(
        &gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(node.node_identity_did().expect("fixture reader identity"))
                .expect("fixture DID"),
        ),
        "did:test:agent",
        AGENT_MESSAGE_NAME,
        &[],
    )
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
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(node.node_identity_did().expect("fixture reader identity"))
            .expect("fixture DID"),
    );
    let selected = access
        .read_view(
            &gents_documents::views::tasks::TriggerList::new("did:test:patch")
                .trigger_ids(vec!["trigger-present".to_owned()]),
        )
        .await
        .expect("read trigger document id");
    let doc_id = selected.first().expect("created trigger").doc_id.as_str();
    let patch = fetch_doc_patch(
        &access,
        "did:test:patch",
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

    let result = fetch_doc_patch(
        &gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(node.node_identity_did().expect("fixture reader identity"))
                .expect("fixture DID"),
        ),
        "did:test:agent",
        "NotARealCollection",
        &["x"],
    )
    .await;
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

    let runtimes = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        node.node_identity_did().unwrap().parse().unwrap(),
    )
    .read_view(&gents_store::views::runtime::NodeRuntimeList::new(
        "did:key:runtime-capacity",
    ))
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
                    node_did: "did:test:agent",
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

    let tool_calls = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        node.node_identity_did().unwrap().parse().unwrap(),
    )
    .read_view(&gents_session::views::transcript::ToolCallRows::new(
        gents_session::views::transcript::Scope::Node("did:test:agent".to_owned()),
    ))
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
            node_did: "did:test:agent",
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

    let tool_calls = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        node.node_identity_did().unwrap().parse().unwrap(),
    )
    .read_view(&gents_session::views::transcript::ToolCallRows::new(
        gents_session::views::transcript::Scope::Node("did:test:agent".to_owned()),
    ))
    .await
    .expect("load agent tool calls");
    let tool_call = tool_calls
        .iter()
        .find(|row| row.tool_call_key == "session-1:job-1")
        .expect("created tool call");
    assert_eq!(tool_call.job_pin.as_ref(), Some(&pin));
    assert_eq!(tool_call.job_progress.as_ref(), Some(&progress));
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

    let store = load_node_snapshot_on(
        &gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            node.node_identity_did().unwrap().parse().unwrap(),
        ),
        "did:alpha",
    )
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
