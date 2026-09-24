use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::llm::message::{
    AssistantContent, Message, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent,
    UserContent,
};
use gents::llm::ToolCallHookAction;
use gents::tool_call_lifecycle::{
    CancelCause, CascadeDispatch, ToolCallLifecycle, MAX_SUBAGENT_DEPTH,
};
use gents::{
    fetch_interrupt_requested_at, interrupt_request, load_history, AgentIdentity, DefraSessionHook,
    DocumentRuntimeOptions, FailurePolicy, Gents, ToolCeiling,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::support::accepted_turn::{
    boot_accepted_turn, boot_accepted_turn_with_backend_capacity, boot_prepared_accepted_turn,
    enqueue_local_accepted_request, enqueue_local_accepted_request_until, prepare_accepted_turn,
    AcceptedTurnRuntime, AcceptedTurnSpec,
};
use crate::support::fixtures::{
    bind_behavior_backend, configure_subagent_behavior, spawn_subagent_source, subagent_target,
    SubagentSourceGuard,
};
use crate::support::interrupt::{wait_for_runtime_ready, BootedAgent};
use crate::support::streaming_backend::{
    MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse, StreamScript,
};
use crate::support::{first_optional_row, first_row, test_db};

const PARENT_BEHAVIOR_ID: &str = "r4-parent";
const CHILD_BEHAVIOR_ID: &str = "r4-child";

struct SpawnFixture {
    db: crate::support::TestDb,
    hook: DefraSessionHook,
    session_id: String,
    request_id: String,
    parent_subagent_depth: u32,
    extra_parent_fields: String,
    operator_tool_root: Option<std::path::PathBuf>,
    parent_deadline: chrono::DateTime<chrono::Utc>,
    agent_did: String,
    legacy_source: std::sync::Mutex<Option<SubagentSourceGuard>>,
}

/// Drive one spawn invocation through the production owned loop. The provider
/// header is published before hook dispatch, and a background receipt is
/// authored before the second provider turn can observe it.
async fn run_canonical_spawn_turn(
    fixture: &SpawnFixture,
    provider_call_id: &str,
    args: &str,
) -> AcceptedTurnRuntime {
    run_canonical_spawn_turn_with_child_response(
        fixture,
        provider_call_id,
        args,
        "child held for fixture observation",
        1,
    )
    .await
}

async fn run_canonical_spawn_turn_with_child_response(
    fixture: &SpawnFixture,
    provider_call_id: &str,
    args: &str,
    child_response: &'static str,
    backend_capacity: usize,
) -> AcceptedTurnRuntime {
    // The canonical runtime owns subagent-source reconciliation. Direct-hook
    // fixtures used a separate test source; running both creates duplicate
    // physical child requests for one accepted bridge.
    drop(
        fixture
            .legacy_source
            .lock()
            .expect("legacy source mutex")
            .take(),
    );
    run_canonical_tool_turn_with_child_response(
        fixture,
        provider_call_id,
        "spawn_subagent",
        args,
        child_response,
        backend_capacity,
    )
    .await
}

async fn run_canonical_tool_turn(
    fixture: &SpawnFixture,
    provider_call_id: &str,
    tool_name: &str,
    args: &str,
) -> AcceptedTurnRuntime {
    run_canonical_tool_turn_with_child_response(
        fixture,
        provider_call_id,
        tool_name,
        args,
        "child held for fixture observation",
        1,
    )
    .await
}

async fn run_canonical_tool_turn_with_child_response(
    fixture: &SpawnFixture,
    provider_call_id: &str,
    tool_name: &str,
    args: &str,
    child_response: &'static str,
    backend_capacity: usize,
) -> AcceptedTurnRuntime {
    const MODEL: &str = "r4-scripted-spawn";
    const BACKEND: &str = "r4-scripted-spawn-backend";
    let request_id = escape_graphql_string(&fixture.request_id);
    let removed = fixture
        .db
        .node
        .execute(&format!(
            r#"mutation {{ delete_AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(
        !removed.has_errors(),
        "replace legacy parent fixture: {:?}",
        removed.errors
    );
    // `create_runtime_request` owns canonical session/request admission. The
    // direct-hook fixture predates that boundary and its session has no
    // authenticated requester ancestry, so replace both documents together.
    let session_id = escape_graphql_string(&fixture.session_id);
    let removed = fixture
        .db
        .node
        .execute(&format!(
            r#"mutation {{ delete_AgentSession(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(
        !removed.has_errors(),
        "replace legacy parent session fixture: {:?}",
        removed.errors
    );
    let valid_until = fixture
        .parent_deadline
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let workspace_id = parent_field(&fixture.extra_parent_fields, "workspace_id");
    let workspace_authority = parent_field(&fixture.extra_parent_fields, "workspace_authority");
    // Canonical request admission is authenticated by this node's DID. The
    // older direct-mutation fixture supplied an arbitrary owner string before
    // its node existed; retain the field's presence, but bind its value to
    // the admitted principal for a real owned request.
    let workspace_owner = parent_field(&fixture.extra_parent_fields, "workspace_owner_agent_did")
        .map(|_| fixture.db.node_identity.did().to_owned());
    let workspace_seal_hash = parent_field(&fixture.extra_parent_fields, "workspace_seal_hash");
    let request_setup = (!fixture.extra_parent_fields.trim().is_empty()).then(|| {
        Box::new(
            move |request: &mut gents_protocol::request_admission::AgentRequestCreate| {
                request.workspace_id = workspace_id;
                request.workspace_authority = workspace_authority;
                request.workspace_owner_agent_did = workspace_owner;
                request.workspace_seal_hash = workspace_seal_hash;
            },
        ) as Box<dyn FnOnce(&mut gents_protocol::request_admission::AgentRequestCreate)>
    });
    let child_plans = serde_json::from_str::<serde_json::Value>(args)
        .ok()
        .and_then(|value| value["prompt"].as_str().map(str::to_owned))
        .map(|prompt| {
            vec![StreamPlan::new(
                prompt.clone(),
                vec![StreamResponse::Stream(StreamScript::paused(
                    prompt,
                    [child_response],
                ))],
            )]
        })
        .unwrap_or_default();
    let spec = AcceptedTurnSpec {
        backend_id: BACKEND,
        model: MODEL,
        parent_behavior_id: PARENT_BEHAVIOR_ID,
        configured_behavior_ids: &[PARENT_BEHAVIOR_ID, CHILD_BEHAVIOR_ID],
        request_id: &fixture.request_id,
        session_id: &fixture.session_id,
        prompt: "parent prompt",
        accepted_chunks: vec![StreamChunk::tool_call(provider_call_id, tool_name, args)],
        child_plans,
        valid_until: Some(&valid_until),
        subagent_depth: Some(fixture.parent_subagent_depth),
        request_setup,
    };
    let options = DocumentRuntimeOptions {
        tool_ceiling: fixture
            .operator_tool_root
            .clone()
            .map(ToolCeiling::readonly_at)
            .unwrap_or_else(ToolCeiling::meta_only),
        ..Default::default()
    };
    let runtime = if backend_capacity == 1 {
        boot_accepted_turn(&fixture.db, spec, options).await
    } else {
        boot_accepted_turn_with_backend_capacity(&fixture.db, spec, options, backend_capacity).await
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let response = fixture
            .db
            .node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{ lifecycle_state failure_reason }} }}"#
            ))
            .await;
        let row = response.data.as_ref().and_then(|data| {
            data["AgentRequest"]
                .as_array()
                .and_then(|rows| rows.first())
        });
        let state = row.and_then(|row| row["lifecycle_state"].as_str());
        if state == Some("completed") {
            break;
        }
        assert_ne!(
            state,
            Some("failed"),
            "canonical spawn parent failed: {}",
            row.and_then(|row| row["failure_reason"].as_str())
                .unwrap_or("missing failure reason")
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for canonical spawn parent; state={state:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    runtime
}

async fn run_canonical_followup_tool_turn(
    fixture: &SpawnFixture,
    request_id: &str,
    provider_call_id: &str,
    tool_name: &str,
    args: &str,
) {
    let valid_until = fixture
        .parent_deadline
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let runtime = boot_accepted_turn(
        &fixture.db,
        AcceptedTurnSpec {
            backend_id: "r4-scripted-followup-backend",
            model: "r4-scripted-followup",
            parent_behavior_id: PARENT_BEHAVIOR_ID,
            configured_behavior_ids: &[PARENT_BEHAVIOR_ID],
            request_id,
            session_id: &fixture.session_id,
            prompt: "followup parent prompt",
            accepted_chunks: vec![StreamChunk::tool_call(provider_call_id, tool_name, args)],
            child_plans: Vec::new(),
            valid_until: Some(&valid_until),
            subagent_depth: Some(fixture.parent_subagent_depth),
            request_setup: None,
        },
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await;
    wait_for_request_terminal(fixture.db.node.as_ref(), request_id, "completed").await;
    runtime.shutdown().await;
}

async fn boot_canonical_followup_wait_turn(
    fixture: &SpawnFixture,
    request_id: &str,
    provider_call_id: &str,
    args: &str,
    child_prompt: &str,
    child_response: &'static str,
) -> AcceptedTurnRuntime {
    let valid_until = fixture
        .parent_deadline
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    boot_accepted_turn(
        &fixture.db,
        AcceptedTurnSpec {
            backend_id: "r4-scripted-followup-wait-backend",
            model: "r4-scripted-followup-wait",
            parent_behavior_id: PARENT_BEHAVIOR_ID,
            configured_behavior_ids: &[PARENT_BEHAVIOR_ID, CHILD_BEHAVIOR_ID],
            request_id,
            session_id: &fixture.session_id,
            prompt: "followup wait prompt",
            accepted_chunks: vec![StreamChunk::tool_call(
                provider_call_id,
                "wait_subagent",
                args,
            )],
            child_plans: vec![StreamPlan::new(
                child_prompt,
                vec![StreamResponse::Stream(StreamScript::paused(
                    child_prompt,
                    [child_response],
                ))],
            )],
            valid_until: Some(&valid_until),
            subagent_depth: Some(fixture.parent_subagent_depth),
            request_setup: None,
        },
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
}

async fn complete_existing_child_request(
    fixture: &SpawnFixture,
    child_request_id: &str,
    child_prompt: &str,
    final_response: &'static str,
) {
    let backend = MockStreamingBackend::start_with_plans(
        "r4-scripted-child-completion",
        vec![StreamPlan::new(
            child_prompt,
            vec![StreamResponse::completes(child_prompt, [final_response])],
        )],
    )
    .expect("start child completion backend");
    bind_behavior_backend(
        fixture.db.node.as_ref(),
        &fixture.agent_did,
        CHILD_BEHAVIOR_ID,
        "r4-scripted-child-completion-backend",
        backend.endpoint(),
        "r4-scripted-child-completion",
    )
    .await;
    let identity: std::sync::Arc<dyn AgentIdentity> = fixture.db.node_identity.clone();
    let agent = Gents::from_default_behavior_documents(
        fixture.db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .expect("build child completion runtime");
    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(fixture.db.node.as_ref(), &agent_did).await;
    let runtime = BootedAgent::new(shutdown_tx, handle, agent_did);
    wait_for_request_terminal(fixture.db.node.as_ref(), child_request_id, "completed").await;
    runtime.shutdown().await;
    drop(backend);
}

async fn configure_remote_spawn_target(fixture: &SpawnFixture, remote_did: &str) {
    bind_behavior_backend(
        fixture.db.node.as_ref(),
        remote_did,
        CHILD_BEHAVIOR_ID,
        "r4-remote-unmaterialized-backend",
        "http://127.0.0.1:1/v1",
        "test-model",
    )
    .await;
    gents::upsert_agent_behavior(
        fixture.db.node.as_ref(),
        &gents::AgentBehaviorDocument {
            behavior_id: CHILD_BEHAVIOR_ID.to_string(),
            agent_did: remote_did.to_string(),
            display_name: Some("Remote unmaterialized child".to_string()),
            description: None,
            context_id: None,
            inference_profile_id: format!("{CHILD_BEHAVIOR_ID}-inference"),
            enabled: true,
            tags: Vec::new(),
            created_at: Some("2026-05-14T00:00:00Z".to_string()),
        },
    )
    .await
    .expect("publish remote child behavior");
    configure_subagent_behavior(
        fixture.db.node.as_ref(),
        &fixture.agent_did,
        PARENT_BEHAVIOR_ID,
        "r4-parent-tools",
        vec![subagent_target(
            &fixture.agent_did,
            CHILD_BEHAVIOR_ID,
            remote_did,
            CHILD_BEHAVIOR_ID,
        )],
        true,
        true,
        Some(true),
    )
    .await;
}

fn parent_field(fields: &str, name: &str) -> Option<String> {
    let suffix = fields.split_once(&format!(r#"{name}: ""#))?.1;
    Some(suffix.split('"').next()?.to_string())
}

async fn run_canonical_foreground_spawn_turn(
    fixture: &SpawnFixture,
    provider_call_id: &str,
    args: &str,
    child_prompt: &str,
    child_response: StreamResponse,
) {
    let runtime = boot_canonical_foreground_spawn_turn(
        fixture,
        provider_call_id,
        args,
        child_prompt,
        child_response,
    )
    .await;
    wait_for_parent_terminal(fixture, "completed").await;
    runtime.shutdown().await;
}

async fn boot_canonical_foreground_spawn_turn(
    fixture: &SpawnFixture,
    provider_call_id: &str,
    args: &str,
    child_prompt: &str,
    child_response: StreamResponse,
) -> AcceptedTurnRuntime {
    boot_canonical_foreground_spawn_turn_inner(
        fixture,
        provider_call_id,
        args,
        child_prompt,
        child_response,
        None,
        None,
    )
    .await
}

async fn boot_canonical_foreground_spawn_turn_with_backend_capacity(
    fixture: &SpawnFixture,
    provider_call_id: &str,
    args: &str,
    child_prompt: &str,
    child_response: StreamResponse,
    max_concurrent: usize,
) -> AcceptedTurnRuntime {
    boot_canonical_foreground_spawn_turn_inner(
        fixture,
        provider_call_id,
        args,
        child_prompt,
        child_response,
        None,
        Some(max_concurrent),
    )
    .await
}

async fn boot_canonical_foreground_spawn_turn_with_execution_deadline(
    fixture: &SpawnFixture,
    provider_call_id: &str,
    args: &str,
    child_prompt: &str,
    child_response: StreamResponse,
    deadline_secs: i64,
) -> AcceptedTurnRuntime {
    boot_canonical_foreground_spawn_turn_inner(
        fixture,
        provider_call_id,
        args,
        child_prompt,
        child_response,
        Some(deadline_secs),
        // The paused child must be able to acquire an inference slot while
        // the foreground parent waits on its bridge. This fixture tests the
        // bridge deadline, not capacity-one provider serialization.
        Some(2),
    )
    .await
}

async fn boot_canonical_foreground_spawn_turn_inner(
    fixture: &SpawnFixture,
    provider_call_id: &str,
    args: &str,
    child_prompt: &str,
    child_response: StreamResponse,
    execution_deadline_secs: Option<i64>,
    backend_capacity: Option<usize>,
) -> AcceptedTurnRuntime {
    const MODEL: &str = "r4-scripted-foreground-spawn";
    const BACKEND: &str = "r4-scripted-foreground-spawn-backend";
    let request_id = escape_graphql_string(&fixture.request_id);
    for mutation in [
        format!(
            r#"mutation {{ delete_AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ _docID }} }}"#
        ),
        format!(
            r#"mutation {{ delete_AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(&fixture.session_id)
        ),
    ] {
        let response = fixture.db.node.execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "replace legacy foreground fixture: {:?}",
            response.errors
        );
    }
    let valid_until = fixture
        .parent_deadline
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let prepared = prepare_accepted_turn(
        &fixture.db,
        AcceptedTurnSpec {
            backend_id: BACKEND,
            model: MODEL,
            parent_behavior_id: PARENT_BEHAVIOR_ID,
            configured_behavior_ids: &[PARENT_BEHAVIOR_ID, CHILD_BEHAVIOR_ID],
            request_id: &fixture.request_id,
            session_id: &fixture.session_id,
            prompt: "parent prompt",
            accepted_chunks: vec![StreamChunk::tool_call(
                provider_call_id,
                "spawn_subagent",
                args,
            )],
            child_plans: vec![StreamPlan::new(child_prompt, vec![child_response])],
            // Admission TTL is not the claimed execution deadline. The
            // deadline-specific fixture configures that through the parent
            // InferenceExecution owner below.
            valid_until: execution_deadline_secs
                .is_none()
                .then_some(valid_until.as_str()),
            subagent_depth: Some(fixture.parent_subagent_depth),
            request_setup: None,
        },
    )
    .await;
    if let Some(deadline_secs) = execution_deadline_secs {
        assert!(deadline_secs > 1);
        let did = fixture.db.node_identity.did().to_owned();
        gents::ConfigAccess::transact_local(
            fixture.db.node.as_ref(),
            None,
            "test.foreground_parent_execution_deadline",
            |txn| {
                let did = did.clone();
                Box::pin(async move {
                    use gents::config_client::{
                        apply_desired_state_plan, read_desired_state_record_in_txn,
                        DesiredStateApplyDocument, DesiredStateApplyPlan,
                    };
                    let (_, behavior) = read_desired_state_record_in_txn(
                        txn,
                        gents::Collection::AgentBehavior,
                        &did,
                        PARENT_BEHAVIOR_ID,
                    )
                    .await?
                    .expect("canonical parent behavior");
                    let profile_id = behavior["inference_profile_id"]
                        .as_str()
                        .expect("canonical parent inference profile");
                    let (_, mut profile) = read_desired_state_record_in_txn(
                        txn,
                        gents::Collection::InferenceProfile,
                        &did,
                        profile_id,
                    )
                    .await?
                    .expect("canonical parent profile");
                    let execution_id = format!("{profile_id}:deadline-test");
                    profile["execution_id"] = execution_id.clone().into();
                    let execution =
                        serde_json::to_value(gents::document_config::InferenceExecution {
                            agent_did: did,
                            execution_id,
                            stream_liveness_timeout_secs: Some(1),
                            deadline_duration_secs: Some(deadline_secs),
                            ..Default::default()
                        })?;
                    apply_desired_state_plan(
                        txn,
                        &DesiredStateApplyPlan::new(vec![
                            DesiredStateApplyDocument {
                                collection: gents::Collection::InferenceExecution,
                                add: execution.clone(),
                                update: execution,
                            },
                            DesiredStateApplyDocument {
                                collection: gents::Collection::InferenceProfile,
                                add: profile.clone(),
                                update: profile,
                            },
                        ])?,
                    )
                    .await
                    .map(|_| ())
                })
            },
        )
        .await
        .expect("configure canonical parent execution deadline");
    }
    if let Some(max_concurrent) = backend_capacity {
        assert!(max_concurrent > 0);
        let did = fixture.db.node_identity.did().to_owned();
        gents::ConfigAccess::transact_local(
            fixture.db.node.as_ref(),
            None,
            "test.foreground_spawn_backend_capacity",
            |txn| {
                let did = did.clone();
                Box::pin(async move {
                    use gents::config_client::{
                        apply_desired_state_plan, read_desired_state_record_in_txn,
                        DesiredStateApplyDocument, DesiredStateApplyPlan,
                    };
                    let (_, mut backend) = read_desired_state_record_in_txn(
                        txn,
                        gents::Collection::InferenceBackend,
                        &did,
                        BACKEND,
                    )
                    .await?
                    .expect("canonical foreground inference backend");
                    backend["max_concurrent"] = max_concurrent.into();
                    apply_desired_state_plan(
                        txn,
                        &DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                            collection: gents::Collection::InferenceBackend,
                            add: backend.clone(),
                            update: backend,
                        }])?,
                    )
                    .await
                    .map(|_| ())
                })
            },
        )
        .await
        .expect("configure canonical foreground backend capacity");
    }
    let identity: std::sync::Arc<dyn AgentIdentity> = fixture.db.node_identity.clone();
    let agent = Gents::from_default_behavior_documents(
        fixture.db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .expect("build canonical foreground fixture runtime");
    boot_prepared_accepted_turn(&fixture.db, prepared, agent).await
}

async fn wait_for_parent_terminal(fixture: &SpawnFixture, expected: &str) {
    wait_for_request_terminal(fixture.db.node.as_ref(), &fixture.request_id, expected).await;
}

async fn wait_for_request_terminal(node: &EmbeddedNode, request_id: &str, expected: &str) {
    let request_id = escape_graphql_string(request_id);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let response = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{ lifecycle_state failure_reason }} }}"#
            ))
            .await;
        let row = response.data.as_ref().and_then(|data| {
            data["AgentRequest"]
                .as_array()
                .and_then(|rows| rows.first())
        });
        let state = row.and_then(|row| row["lifecycle_state"].as_str());
        if state == Some(expected) {
            break;
        }
        if expected != "failed" {
            assert_ne!(state, Some("failed"), "foreground parent failed: {row:?}");
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for foreground parent; state={state:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    request_id: Option<String>,
    tool_name: Option<String>,
    args: Option<String>,
    result: Option<String>,
    lifecycle_state: Option<String>,
    await_mode: Option<String>,
    cancel_policy: Option<String>,
    cancel_cause: Option<String>,
    child_request_id: Option<String>,
    spawn_target_did: Option<String>,
    spawn_behavior_id: Option<String>,
    unclaimed_deadline_at: Option<String>,
    cancel_cascade_intent_at: Option<String>,
    cancel_pending_remote_ack: Option<bool>,
    #[allow(dead_code)]
    stuck_since: Option<String>,
    tool_failure_class: Option<String>,
}

async fn setup_spawn_fixture(
    test_name: &str,
    targets: Vec<&str>,
    parent_subagent_depth: u32,
    background_enabled: bool,
) -> SpawnFixture {
    setup_spawn_fixture_with_flags(
        test_name,
        targets,
        parent_subagent_depth,
        true,
        background_enabled,
    )
    .await
}

async fn setup_spawn_fixture_with_flags(
    test_name: &str,
    targets: Vec<&str>,
    parent_subagent_depth: u32,
    spawn_enabled: bool,
    background_enabled: bool,
) -> SpawnFixture {
    setup_spawn_fixture_with_flags_and_deadline(
        test_name,
        targets,
        parent_subagent_depth,
        spawn_enabled,
        background_enabled,
        chrono::Utc::now() + chrono::Duration::minutes(5),
    )
    .await
}

async fn setup_spawn_fixture_with_flags_and_deadline(
    test_name: &str,
    targets: Vec<&str>,
    parent_subagent_depth: u32,
    spawn_enabled: bool,
    background_enabled: bool,
    parent_deadline: chrono::DateTime<chrono::Utc>,
) -> SpawnFixture {
    setup_spawn_fixture_with_parent_fields(
        test_name,
        targets,
        parent_subagent_depth,
        spawn_enabled,
        background_enabled,
        parent_deadline,
        "",
    )
    .await
}

async fn setup_spawn_fixture_with_parent_fields(
    test_name: &str,
    targets: Vec<&str>,
    parent_subagent_depth: u32,
    spawn_enabled: bool,
    background_enabled: bool,
    parent_deadline: chrono::DateTime<chrono::Utc>,
    extra_parent_fields: &str,
) -> SpawnFixture {
    let db = test_db(test_name).await;
    let agent_did = db.node_identity.did().to_string();

    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        CHILD_BEHAVIOR_ID,
        "r4-child-tools",
        Vec::new(),
        false,
        false,
        None,
    )
    .await;
    for behavior_id in targets
        .iter()
        .copied()
        .filter(|behavior_id| *behavior_id != CHILD_BEHAVIOR_ID)
    {
        configure_subagent_behavior(
            db.node.as_ref(),
            &agent_did,
            behavior_id,
            &format!("{behavior_id}-tools"),
            Vec::new(),
            false,
            false,
            None,
        )
        .await;
    }
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        PARENT_BEHAVIOR_ID,
        "r4-parent-tools",
        targets
            .into_iter()
            .map(|behavior_id| subagent_target(&agent_did, behavior_id, &agent_did, behavior_id))
            .collect(),
        spawn_enabled,
        background_enabled,
        None,
    )
    .await;

    let source = spawn_subagent_source(
        db.node.clone(),
        &agent_did,
        PARENT_BEHAVIOR_ID,
        CHILD_BEHAVIOR_ID,
    );

    let session_id = format!("{test_name}-session");
    let request_id = format!("{test_name}-parent");
    create_parent_request_with_extra_fields(
        db.node.as_ref(),
        &agent_did,
        &request_id,
        &session_id,
        parent_subagent_depth,
        parent_deadline,
        extra_parent_fields,
    )
    .await;
    crate::support::create_agent_session_in_scope(
        db.node.as_ref(),
        &agent_did,
        &session_id,
        PARENT_BEHAVIOR_ID,
        "2026-05-13T00:00:00Z",
    )
    .await;

    let hook = DefraSessionHook::resume_with_identity_policy(
        db.node.clone(),
        &session_id,
        PARENT_BEHAVIOR_ID,
        &agent_did,
        None,
        FailurePolicy::default(),
    )
    .await
    .unwrap();
    hook.set_active_request_lineage(Some(request_id.clone()), None)
        .await
        .expect("bind persisted request lineage");
    hook.set_request_deadline_at(Some(parent_deadline)).await;

    SpawnFixture {
        db,
        hook,
        session_id,
        request_id,
        parent_subagent_depth,
        extra_parent_fields: extra_parent_fields.to_string(),
        operator_tool_root: None,
        parent_deadline,
        agent_did,
        legacy_source: std::sync::Mutex::new(Some(source)),
    }
}

async fn create_parent_request(
    node: &EmbeddedNode,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    subagent_depth: u32,
    deadline: chrono::DateTime<chrono::Utc>,
) {
    create_parent_request_with_extra_fields(
        node,
        agent_did,
        request_id,
        session_id,
        subagent_depth,
        deadline,
        "",
    )
    .await;
}

async fn create_parent_request_with_extra_fields(
    node: &EmbeddedNode,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    subagent_depth: u32,
    deadline: chrono::DateTime<chrono::Utc>,
    extra_fields: &str,
) {
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let behavior_id = escape_graphql_string(PARENT_BEHAVIOR_ID);
    let agent_did = escape_graphql_string(agent_did);
    let created_at = chrono::Utc::now().to_rfc3339();
    let deadline = deadline.to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                behavior_id: "{behavior_id}",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "parent prompt",
                lifecycle_state: "processing",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "{created_at}",
                deadline: "{deadline}",
                retry_count: 0,
                max_retries: 3,
                subagent_depth: {subagent_depth}
                {extra_fields}
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create parent AgentRequest failed: {:?}",
        response.errors
    );
}

async fn fetch_tool_call(
    node: &std::sync::Arc<EmbeddedNode>,
    session_id: &str,
    provider_tool_call_id: &str,
) -> ToolCallRow {
    let tool_call_doc_id =
        accepted_tool_call_doc_id(node.as_ref(), session_id, provider_tool_call_id).await;
    let escaped_tool_call_doc_id = escape_graphql_string(&tool_call_doc_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ _docID: {{ _eq: "{escaped_tool_call_doc_id}" }} }},
                limit: 1
            ) {{
                request_id
                tool_name
                lifecycle_state
                await_mode
                cancel_policy
                cancel_cause
                child_request_id
                spawn_target_did
                spawn_behavior_id
                unclaimed_deadline_at
                cancel_cascade_intent_at
                cancel_pending_remote_ack
                stuck_since
                tool_failure_class
            }}
        }}"#
    );
    let mut row: ToolCallRow = first_row(&node.execute(&query).await, "AgentToolCall");
    let escaped_session_id = escape_graphql_string(session_id);
    let scope = node
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}, limit: 1) {{ agent_did requester_did }} }}"#
        ))
        .await;
    let scope: serde_json::Value = first_row(&scope, "AgentSession");
    let agent_did = scope["agent_did"].as_str().expect("session agent_did");
    let requester_did = scope["requester_did"].as_str();
    let history = load_history(node.as_ref(), session_id, agent_did, requester_did)
        .await
        .expect("load canonical session history");
    for message in history {
        match message {
            Message::Assistant { content, .. } => {
                for content in content {
                    if let AssistantContent::ToolCall(call) = content {
                        if call.id == provider_tool_call_id
                            || call.call_id.as_deref() == Some(provider_tool_call_id)
                        {
                            row.args = Some(call.function.arguments.to_string());
                        }
                    }
                }
            }
            Message::User { content } => {
                for content in content {
                    if let UserContent::ToolResult(result) = content {
                        if result.id == provider_tool_call_id
                            || result.call_id.as_deref() == Some(provider_tool_call_id)
                        {
                            row.result = result.content.iter().find_map(|content| match content {
                                ToolResultContent::Text(Text { text }) => Some(text.clone()),
                                _ => None,
                            });
                        }
                    }
                }
            }
            Message::System { .. } => {}
        }
    }
    row
}

async fn accepted_tool_call_doc_id(
    node: &EmbeddedNode,
    session_id: &str,
    provider_tool_call_id: &str,
) -> String {
    let escaped_session_id = escape_graphql_string(session_id);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let response = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}, order: {{ sequence: ASC }}) {{ _docID agent_did requester_did }} }}"#
        ))
        .await;
        assert!(
            !response.has_errors(),
            "load accepted headers: {:?}",
            response.errors
        );
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data["AgentMessage"].as_array())
            .expect("AgentMessage header rows");
        for row in rows {
            let header_doc_id = row["_docID"].as_str().expect("header _docID");
            let agent_did = row["agent_did"].as_str().expect("header agent_did");
            let requester_did = row["requester_did"].as_str();
            let (header, _) = gents::session::load_canonical_message_from_node(
                node,
                header_doc_id,
                agent_did,
                requester_did,
            )
            .await
            .expect("reconstruct accepted canonical header");
            for block in header.blocks {
                if let gents_protocol::output::MessageBlock::ToolCall {
                    tool_call_doc_id,
                    id,
                    call_id,
                    ..
                } = block
                {
                    if id == provider_tool_call_id
                        || call_id.as_deref() == Some(provider_tool_call_id)
                    {
                        return tool_call_doc_id;
                    }
                }
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "accepted provider tool call {provider_tool_call_id} missing from session {session_id}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_tool_call_await_mode(
    node: &std::sync::Arc<EmbeddedNode>,
    session_id: &str,
    tool_call_id: &str,
    expected_await_mode: &str,
) -> ToolCallRow {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let row = fetch_tool_call(node, session_id, tool_call_id).await;
        if row.await_mode.as_deref() == Some(expected_await_mode) {
            return row;
        }
        if tokio::time::Instant::now() >= deadline {
            let session = escape_graphql_string(session_id);
            let requests = node
                .execute(&format!(
                    r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ request_id lifecycle_state failure_reason }} }}"#
                ))
                .await;
            panic!(
                "timed out waiting for tool call {tool_call_id} await_mode={expected_await_mode}; row={row:?}; requests={:?}",
                requests.data
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn count_tool_calls_by_name(node: &EmbeddedNode, session_id: &str, tool_name: &str) -> usize {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_tool_name = escape_graphql_string(tool_name);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    tool_name: {{ _eq: "{escaped_tool_name}" }}
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "count AgentToolCall by name failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|rows| rows.as_array())
        .map_or(0, Vec::len)
}

async fn fetch_child_request(node: &EmbeddedNode, child_request_id: &str) -> AgentRequestRow {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                limit: 1
            ) {{
                request_id
                session_id
                behavior_id
                content
                lifecycle_state
                failure_reason
                subagent_depth
                deadline
                valid_until
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
                caused_by_trigger_id
                caused_by_trigger_kind
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentRequest")
}

async fn fetch_child_request_optional(
    node: &EmbeddedNode,
    child_request_id: &str,
) -> Option<AgentRequestRow> {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                limit: 1
            ) {{
                request_id
                session_id
                behavior_id
                content
                lifecycle_state
                failure_reason
                subagent_depth
                deadline
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
                caused_by_trigger_id
                caused_by_trigger_kind
            }}
        }}"#
    );
    first_optional_row(&node.execute(&query).await, "AgentRequest")
}

async fn child_request_for_tool(
    node: &EmbeddedNode,
    session_id: &str,
    provider_tool_call_id: &str,
) -> Option<AgentRequestRow> {
    let tool_call_doc_id = accepted_tool_call_doc_id(node, session_id, provider_tool_call_id).await;
    let escaped_tool_call_doc_id = escape_graphql_string(&tool_call_doc_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ caused_by_parent_tool_call_doc_id: {{ _eq: "{escaped_tool_call_doc_id}" }} }},
                limit: 1
            ) {{
                request_id session_id behavior_id content lifecycle_state failure_reason
                subagent_depth deadline caused_by_parent_request_id
                caused_by_parent_tool_call_id caused_by_trigger_id caused_by_trigger_kind
            }}
        }}"#
    );
    first_optional_row(&node.execute(&query).await, "AgentRequest")
}

async fn wait_for_child_request_for_tool(
    node: &EmbeddedNode,
    session_id: &str,
    parent_tool_call_id: &str,
) -> AgentRequestRow {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(row) = child_request_for_tool(node, session_id, parent_tool_call_id).await {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for child AgentRequest for tool call {parent_tool_call_id}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_child_session_id(node: &EmbeddedNode, child_request_id: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(child) = fetch_child_request_optional(node, child_request_id).await {
            if let Some(session_id) = child.session_id.filter(|value| !value.is_empty()) {
                return session_id;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for child AgentRequest {child_request_id} session id"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn persist_child_terminal(
    node: &EmbeddedNode,
    child_request_id: &str,
    lifecycle_state: &str,
    failure_reason: Option<&str>,
) {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let escaped_lifecycle_state = escape_graphql_string(lifecycle_state);
    let failure_reason_field = failure_reason
        .map(|reason| {
            let escaped = escape_graphql_string(reason);
            format!(r#", failure_reason: "{escaped}""#)
        })
        .unwrap_or_default();
    let update_request = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                input: {{
                    lifecycle_state: "{escaped_lifecycle_state}"
                    {failure_reason_field}
                }}
            ) {{ _docID }}
        }}"#
    );
    // The child runtime may renew its execution lease while this fixture
    // forces a terminal state, which can race a DefraDB transaction
    // conflict. `graphql::graphql_with_transaction_retry` is a read-only
    // owner that rejects mutations outright, so route this one mutation
    // through the existing auto-commit transaction-conflict retry owner
    // instead; unrelated GraphQL errors still fail the test below.
    gents::ConfigAccess::write_local(node, "test.persist_child_terminal", &update_request)
        .await
        .unwrap_or_else(|error| {
            panic!("update child AgentRequest {lifecycle_state} failed: {error:#}")
        });
}

fn skip_reason_json(action: ToolCallHookAction) -> Value {
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("expected Skip action, got {action:?}");
    };
    serde_json::from_str(&reason).expect("skip reason should be JSON")
}

fn persisted_tool_result_json(tool: &ToolCallRow) -> Value {
    serde_json::from_str(tool.result.as_deref().expect("persisted tool result JSON"))
        .expect("persisted tool result should be JSON")
}

async fn canonical_tool_payload_json(fixture: &SpawnFixture, provider_call_id: &str) -> Value {
    canonical_tool_payload_json_for_scope(
        fixture.db.node.as_ref(),
        &fixture.session_id,
        fixture.db.node_identity.did(),
        Some(fixture.db.node_identity.did()),
        provider_call_id,
    )
    .await
}

async fn canonical_tool_payload_json_for_scope(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    provider_call_id: &str,
) -> Value {
    let history = load_history(node, session_id, agent_did, requester_did)
        .await
        .expect("load canonical parent history");
    history
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => content.iter().find_map(|content| match content {
                UserContent::ToolResult(result)
                    if result.id == provider_call_id
                        || result.call_id.as_deref() == Some(provider_call_id) =>
                {
                    result.content.iter().find_map(|content| match content {
                        ToolResultContent::Text(Text { text }) => serde_json::from_str(text).ok(),
                        _ => None,
                    })
                }
                _ => None,
            }),
            _ => None,
        })
        .next()
        .expect("canonical tool-result payload")
}

#[path = "r4_subagent_tools_cases/background_cancel.rs"]
mod background_cancel;
#[path = "r4_subagent_tools_cases/cancel_subagent.rs"]
mod cancel_subagent;
#[path = "r4_subagent_tools_cases/foreground_spawn.rs"]
mod foreground_spawn;
#[path = "r4_subagent_tools_cases/spawn_validation.rs"]
mod spawn_validation;
#[path = "r4_subagent_tools_cases/spawn_workspace.rs"]
mod spawn_workspace;
#[path = "r4_subagent_tools_cases/wait_subagent.rs"]
mod wait_subagent;
