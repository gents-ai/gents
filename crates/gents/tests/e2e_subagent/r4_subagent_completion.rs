use std::time::Duration;

use gents::background_completion::{
    project_background_subagent_completion, BackgroundCompletionOutcome,
};
use gents::config_client::{
    apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::defra_node::EmbeddedNode;
use gents::document_config::{
    AgentBehavior, AgentContext, SubagentTargetDocument, SubagentTools, Tools,
};
use gents::fetch_interrupt_requested_at;
use gents::graphql::escape_graphql_string;
use gents::llm::message::{Message, Text, UserContent};
use gents::llm::ToolCallHookAction;
use gents::tool_call_lifecycle::{AwaitMode, ToolCallLifecycle};
use gents::{AgentIdentity, DocumentRuntimeOptions, Gents, ToolCeiling};
use gents_protocol::output::{OutputSource, OutputWriter, TerminalOutput};
use gents_protocol::request_input::{QueuePolicy, QueueSource};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;
use serde_json::json;

use crate::support::accepted_turn::{
    boot_accepted_turn, boot_accepted_turn_with_backend_capacity,
    boot_accepted_turn_with_backend_capacity_and_dynamic_followups, AcceptedTurnRuntime,
    AcceptedTurnSpec,
};
use crate::support::fixtures::bind_behavior_backend;
use crate::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use crate::support::streaming_backend::{
    MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse, StreamScript,
};
use crate::support::{first_row, test_db};

const PARENT_BEHAVIOR_ID: &str = "r4-completion-parent";
const CHILD_BEHAVIOR_ID: &str = "r4-completion-child";
const BACKEND_ID: &str = "r4-completion-backend";
/// Stable endpoint used only to satisfy backend connectivity config; this
/// fixture drives persistence seams, never a live model call.
const BACKEND_ENDPOINT: &str = "http://127.0.0.1:1/v1";

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    lifecycle_state: Option<String>,
    await_mode: Option<String>,
}

#[derive(Debug, PartialEq, Deserialize)]
struct MessageRow {
    sequence: u32,
    role: String,
    #[serde(default)]
    content: String,
    request_doc_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChildRequestStateRow {
    lifecycle_state: Option<String>,
    failure_reason: Option<String>,
    execution_generation: Option<String>,
    terminal_output: Option<TerminalOutput>,
}

/// Install the canonical configuration bundle and parent request/session for
/// every scenario through the shared desired-state owners: an explicit
/// `SubagentTargetDocument` (owner `agent_did`, destination
/// `target_agent_did`), a canonical `Tools` document selecting it by
/// `target_id`, an `AgentContext` binding that Tools doc, and an
/// `AgentBehavior` binding `context_id` + `inference_profile_id`. Inference
/// selection comes from `support::fixtures::bind_behavior_backend`, the
/// existing shared owner; no implicit bootstrap defaults are invented.
async fn install_canonical_behavior_bundle(node: &EmbeddedNode, agent_did: &str) {
    // Seed both explicit behavior chains through the shared fixture owner.
    // The parent and child may share a backend, but each behavior owns an
    // explicit profile and never relies on an inferred bootstrap default.
    bind_behavior_backend(
        node,
        agent_did,
        PARENT_BEHAVIOR_ID,
        BACKEND_ID,
        BACKEND_ENDPOINT,
        "test-model",
    )
    .await;
    bind_behavior_backend(
        node,
        agent_did,
        CHILD_BEHAVIOR_ID,
        BACKEND_ID,
        BACKEND_ENDPOINT,
        "test-model",
    )
    .await;
    let parent_profile_id = format!("{PARENT_BEHAVIOR_ID}-inference");
    let child_profile_id = format!("{CHILD_BEHAVIOR_ID}-inference");

    let target = SubagentTargetDocument {
        target_id: format!("{PARENT_BEHAVIOR_ID}:{CHILD_BEHAVIOR_ID}"),
        agent_did: agent_did.to_string(),
        target_agent_did: agent_did.to_string(),
        behavior_id: CHILD_BEHAVIOR_ID.to_string(),
        name: CHILD_BEHAVIOR_ID.to_string(),
        description: None,
        tags: Vec::new(),
    };
    let tools = Tools {
        tools_id: format!("{PARENT_BEHAVIOR_ID}:tools"),
        agent_did: agent_did.to_string(),
        subagents: Some(SubagentTools {
            target_ids: vec![target.target_id.clone()],
            spawn_enabled: Some(true),
            background_enabled: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };
    let context = AgentContext {
        context_id: format!("{PARENT_BEHAVIOR_ID}:context"),
        agent_did: agent_did.to_string(),
        display_name: None,
        description: None,
        system_prompt: None,
        tools_id: Some(tools.tools_id.clone()),
        compaction_id: None,
        skill_ids: Vec::new(),
        tags: Vec::new(),
    };
    let parent_behavior = AgentBehavior {
        behavior_id: PARENT_BEHAVIOR_ID.to_string(),
        agent_did: agent_did.to_string(),
        display_name: Some("R4 completion parent".to_string()),
        description: None,
        context_id: Some(context.context_id.clone()),
        inference_profile_id: parent_profile_id,
        enabled: true,
        tags: Vec::new(),
        created_at: Some("2026-05-12T00:00:00Z".to_string()),
    };
    let child_behavior = AgentBehavior {
        behavior_id: CHILD_BEHAVIOR_ID.to_string(),
        agent_did: agent_did.to_string(),
        display_name: Some("R4 completion child".to_string()),
        description: None,
        context_id: None,
        inference_profile_id: child_profile_id,
        enabled: true,
        tags: Vec::new(),
        created_at: Some("2026-05-12T00:00:01Z".to_string()),
    };
    // One desired-state transaction installs the whole scoped bundle so
    // reference validation sees the complete same-owner closure. Every
    // document is the complete canonical replacement, addressed by owner DID.
    ConfigAccess::transact_local(node, None, "r4_completion.canonical_bundle", |txn| {
        let target = target.clone();
        let tools = tools.clone();
        let context = context.clone();
        let parent_behavior = parent_behavior.clone();
        let child_behavior = child_behavior.clone();
        Box::pin(async move {
            let mut documents = Vec::new();
            for (collection, value) in [
                (
                    gents::Collection::SubagentTarget,
                    serde_json::to_value(&target)?,
                ),
                (gents::Collection::Tools, serde_json::to_value(&tools)?),
                (
                    gents::Collection::AgentContext,
                    serde_json::to_value(&context)?,
                ),
                (
                    gents::Collection::AgentBehavior,
                    serde_json::to_value(&parent_behavior)?,
                ),
                (
                    gents::Collection::AgentBehavior,
                    serde_json::to_value(&child_behavior)?,
                ),
            ] {
                documents.push(DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                });
            }
            apply_desired_state_plan(txn, &DesiredStateApplyPlan::new(documents)?).await
        })
    })
    .await
    .unwrap();
}

async fn setup_fixture(test_name: &str) -> (crate::support::TestDb, String, String) {
    let db = test_db(test_name).await;
    let agent_did = db.node_identity.did().to_string();
    install_canonical_behavior_bundle(db.node.as_ref(), &agent_did).await;

    let session_id = format!("{test_name}-parent-session");
    let request_id = format!("{test_name}-parent-request");
    (db, session_id, request_id)
}

struct AcceptedChild<'a> {
    tool_call_id: &'a str,
    await_mode: AwaitMode,
    final_response: &'a str,
}

async fn start_accepted_children(
    db: &crate::support::TestDb,
    session_id: &str,
    request_id: &str,
    children: &[AcceptedChild<'_>],
) -> (AcceptedTurnRuntime, Vec<(String, String)>) {
    let chunks = children
        .iter()
        .map(|child| {
            StreamChunk::tool_call(
                child.tool_call_id,
                "spawn_subagent",
                json!({
                    "name": CHILD_BEHAVIOR_ID,
                    "prompt": format!("prompt for {}", child.tool_call_id),
                    "await_mode": child.await_mode.as_str(),
                })
                .to_string(),
            )
        })
        .collect();
    let plans = children
        .iter()
        .map(|child| {
            let prompt = format!("prompt for {}", child.tool_call_id);
            let text: &'static str = Box::leak(child.final_response.to_string().into_boxed_str());
            StreamPlan::new(
                prompt.clone(),
                vec![StreamResponse::Stream(StreamScript::paused(prompt, [text]))],
            )
        })
        .collect();
    let spec = AcceptedTurnSpec {
        backend_id: BACKEND_ID,
        model: "test-model",
        parent_behavior_id: PARENT_BEHAVIOR_ID,
        configured_behavior_ids: &[PARENT_BEHAVIOR_ID, CHILD_BEHAVIOR_ID],
        request_id,
        session_id,
        prompt: "parent prompt",
        accepted_chunks: chunks,
        child_plans: plans,
        valid_until: None,
        subagent_depth: None,
        request_setup: None,
    };
    let options = DocumentRuntimeOptions {
        tool_ceiling: ToolCeiling::meta_only(),
        ..Default::default()
    };
    let runtime = if children.len() > 1 {
        // Multi-child completion/interleaving premises require both children
        // executing while the parent still holds its own worker. The default
        // one-worker backend remains covered by capacity-one tests elsewhere.
        boot_accepted_turn_with_backend_capacity(db, spec, options, children.len() + 1).await
    } else {
        boot_accepted_turn(db, spec, options).await
    };
    let mut ids = Vec::with_capacity(children.len());
    for child in children {
        let ids_for_child =
            wait_for_child_for_tool(db.node.as_ref(), request_id, child.tool_call_id).await;
        assert_exact_child_binding(
            db.node.as_ref(),
            request_id,
            session_id,
            &ids_for_child.0,
            db.node_identity.did(),
        )
        .await;
        ids.push(ids_for_child);
    }
    (runtime, ids)
}

async fn release_child_and_wait(
    db: &crate::support::TestDb,
    runtime: &AcceptedTurnRuntime,
    child_request_id: &str,
    tool_call_id: &str,
) {
    runtime
        .backend
        .release(&format!("prompt for {tool_call_id}"));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if fetch_child_request_state(db.node.as_ref(), child_request_id)
            .await
            .lifecycle_state
            .as_deref()
            == Some("completed")
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "child did not complete: {:?}",
            fetch_child_request_state(db.node.as_ref(), child_request_id).await
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_child_generation(node: &EmbeddedNode, request_id: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(generation) = fetch_child_request_state(node, request_id)
            .await
            .execution_generation
        {
            return generation;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "child never acquired an execution generation"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn child_execution_expiry(node: &EmbeddedNode, request_id: &str) -> (String, String) {
    let request_id = escape_graphql_string(request_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{ lifecycle_state execution_generation execution_lease_expires_at }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row = &response.data.expect("child execution expiry data")["AgentRequest"][0];
    assert_eq!(row["lifecycle_state"], "processing");
    (
        row["execution_generation"]
            .as_str()
            .expect("child execution generation")
            .to_owned(),
        row["execution_lease_expires_at"]
            .as_str()
            .expect("child execution lease expiry")
            .to_owned(),
    )
}

async fn wait_for_child_lease_relinquishment(
    node: &EmbeddedNode,
    request_id: &str,
    generation: &str,
    original_expiry: &str,
) {
    let timeout = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (observed_generation, expiry) = child_execution_expiry(node, request_id).await;
        assert_eq!(observed_generation, generation);
        let expiry_at = chrono::DateTime::parse_from_rfc3339(&expiry)
            .unwrap()
            .with_timezone(&chrono::Utc);
        if expiry != original_expiry && expiry_at <= chrono::Utc::now() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < timeout,
            "crashed child did not relinquish its exact execution lease: original={original_expiry}; current={expiry}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn setup_runtime_fixture(test_name: &str) -> (crate::support::TestDb, String, String) {
    let db = test_db(test_name).await;
    let agent_did = db.node_identity.did().to_string();
    install_canonical_behavior_bundle(db.node.as_ref(), &agent_did).await;
    let session_id = format!("{test_name}-parent-session");
    let request_id = format!("{test_name}-parent-request");
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        PARENT_BEHAVIOR_ID,
        &request_id,
        &session_id,
        "parent prompt",
    )
    .await;
    (db, session_id, request_id)
}

struct CanonicalCompletionRuntime {
    runtime: BootedAgent,
    _backend: MockStreamingBackend,
}

impl CanonicalCompletionRuntime {
    async fn shutdown(self) {
        self.runtime.shutdown().await;
    }
}

async fn run_canonical_background_spawn(
    db: &crate::support::TestDb,
    args: &str,
    provider_call_id: &str,
) -> CanonicalCompletionRuntime {
    const MODEL: &str = "r4-completion-scripted";
    const BACKEND: &str = "r4-completion-scripted-backend";
    let child_prompt = serde_json::from_str::<serde_json::Value>(args)
        .expect("spawn arguments")
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .expect("spawn prompt")
        .to_string();
    let backend = MockStreamingBackend::start_with_plans(
        MODEL,
        vec![
            StreamPlan::new(
                "parent prompt",
                vec![
                    StreamResponse::streams(
                        "parent prompt",
                        vec![StreamChunk::tool_call(
                            provider_call_id,
                            "spawn_subagent",
                            args,
                        )],
                    ),
                    StreamResponse::completes("parent prompt", ["parent complete"]),
                ],
            ),
            StreamPlan::new(
                child_prompt.clone(),
                vec![StreamResponse::completes(
                    child_prompt,
                    ["fast background child done"],
                )],
            ),
        ],
    )
    .expect("start completion scripted backend");
    for behavior_id in [PARENT_BEHAVIOR_ID, CHILD_BEHAVIOR_ID] {
        bind_behavior_backend(
            db.node.as_ref(),
            db.node_identity.did(),
            behavior_id,
            BACKEND,
            backend.endpoint(),
            MODEL,
        )
        .await;
    }
    let identity: std::sync::Arc<dyn AgentIdentity> = db.node_identity.clone();
    let agent = Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .expect("build completion scripted runtime");
    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;
    CanonicalCompletionRuntime {
        runtime: BootedAgent::new(shutdown_tx, handle, agent_did),
        _backend: backend,
    }
}

async fn run_canonical_parent_prompt(
    db: &crate::support::TestDb,
    session_id: &str,
    request_id: &str,
    prompt: &str,
) -> CanonicalCompletionRuntime {
    const MODEL: &str = "r4-completion-resume";
    const BACKEND: &str = "r4-completion-resume-backend";
    let backend = MockStreamingBackend::start(
        MODEL,
        vec![crate::support::streaming_backend::StreamScript::completes(
            prompt.to_string(),
            ["resumed"],
        )],
    )
    .expect("start resume backend");
    bind_behavior_backend(
        db.node.as_ref(),
        db.node_identity.did(),
        PARENT_BEHAVIOR_ID,
        BACKEND,
        backend.endpoint(),
        MODEL,
    )
    .await;
    create_runtime_request(
        db.node.as_ref(),
        db.node_identity.did(),
        PARENT_BEHAVIOR_ID,
        request_id,
        session_id,
        prompt,
    )
    .await;
    let identity: std::sync::Arc<dyn AgentIdentity> = db.node_identity.clone();
    let agent = Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .expect("build resume runtime");
    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;
    CanonicalCompletionRuntime {
        runtime: BootedAgent::new(shutdown_tx, handle, agent_did),
        _backend: backend,
    }
}

async fn wait_for_child_for_tool(
    node: &EmbeddedNode,
    _parent_request_id: &str,
    tool_call_id: &str,
) -> (String, String) {
    let call = escape_graphql_string(tool_call_id);
    let bridge_query = format!(
        r#"{{ AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{call}" }} }}, limit: 1) {{ child_request_id }} }}"#
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let bridge = node.execute(&bridge_query).await;
        if let Some(child_request_id) =
            crate::support::first_optional_row::<AcceptedBridgeRow>(&bridge, "AgentToolCall")
                .and_then(|row| row.child_request_id)
        {
            let child = escape_graphql_string(&child_request_id);
            let response = node.execute(&format!(r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{child}" }} }}, limit: 1) {{ request_id session_id }} }}"#)).await;
            if let Some(row) =
                crate::support::first_optional_row::<ChildForToolRow>(&response, "AgentRequest")
            {
                return (row.request_id, row.session_id);
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for child AgentRequest for tool call {tool_call_id}; bridge facts: {}",
            node.execute(&format!(r#"{{ AgentToolCall(filter: {{ tool_name: {{ _eq: "spawn_subagent" }} }}) {{ tool_call_id lifecycle_state tool_failure_class denial_reason child_request_id }} }}"#)).await.data.unwrap_or(serde_json::Value::Null)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[derive(Debug, Deserialize)]
struct AcceptedBridgeRow {
    child_request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChildForToolRow {
    request_id: String,
    session_id: String,
}

#[derive(Debug, Deserialize)]
struct ChildBindingRow {
    agent_did: String,
    requester_did: Option<String>,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_request_doc_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
    caused_by_parent_tool_call_doc_id: Option<String>,
}

async fn assert_exact_child_binding(
    node: &EmbeddedNode,
    parent_request_id: &str,
    parent_session_id: &str,
    child_request_id: &str,
    expected_did: &str,
) {
    let parent_doc_id = crate::support::exact_request_doc_id(node, parent_request_id).await;
    let session = escape_graphql_string(parent_session_id);
    let child = escape_graphql_string(child_request_id);
    let tool_response = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }}, child_request_id: {{ _eq: "{child}" }} }}, limit: 1) {{ _docID agent_did requester_did tool_call_id cancel_policy }} }}"#
        ))
        .await;
    assert!(!tool_response.has_errors(), "{:?}", tool_response.errors);
    let tool_data = tool_response.data.expect("parent tool call data");
    let tool = &tool_data["AgentToolCall"][0];
    let tool_doc_id = tool["_docID"]
        .as_str()
        .expect("parent tool call physical identity")
        .to_string();
    assert_eq!(tool["agent_did"].as_str(), Some(expected_did));
    assert_eq!(tool["requester_did"].as_str(), Some(expected_did));
    assert_eq!(tool["cancel_policy"].as_str(), Some("cascade"));
    let internal_tool_call_id = tool["tool_call_id"]
        .as_str()
        .expect("internal tool call id");
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{child}" }} }}, limit: 1) {{ agent_did requester_did caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let binding: ChildBindingRow = first_row(&response, "AgentRequest");
    assert_eq!(binding.agent_did, expected_did);
    assert_eq!(binding.requester_did.as_deref(), Some(expected_did));
    assert_eq!(
        binding.caused_by_parent_request_id.as_deref(),
        Some(parent_request_id)
    );
    assert_eq!(
        binding.caused_by_parent_request_doc_id.as_deref(),
        Some(parent_doc_id.as_str())
    );
    assert_eq!(
        binding.caused_by_parent_tool_call_id.as_deref(),
        Some(internal_tool_call_id)
    );
    assert_eq!(
        binding.caused_by_parent_tool_call_doc_id.as_deref(),
        Some(tool_doc_id.as_str())
    );
}

fn skip_reason(action: ToolCallHookAction) -> String {
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("expected Skip action, got {action:?}");
    };
    reason
}

/// The accepted-turn runtime is still writing this request (its own
/// terminalization and bookkeeping), so the fixture write goes through the
/// transaction owner, whose conflict retry re-runs the whole update.
async fn set_request_lifecycle(node: &EmbeddedNode, request_id: &str, state: &str) {
    let request_id = escape_graphql_string(request_id);
    let state = escape_graphql_string(state);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{ lifecycle_state: "{state}" }}
            ) {{ _docID }}
        }}"#
    );
    ConfigAccess::transact_local(node, None, "test.set_request_lifecycle", |txn| {
        let mutation = mutation.clone();
        Box::pin(async move { txn.execute(&mutation).await.map(|_| ()) })
    })
    .await
    .unwrap_or_else(|error| panic!("set request lifecycle failed: {error:#}"));
}

async fn set_child_processing_deadline(
    node: &EmbeddedNode,
    request_id: &str,
    deadline: chrono::DateTime<chrono::Utc>,
    execution_generation: &str,
) {
    let request_id = escape_graphql_string(request_id);
    let deadline = escape_graphql_string(&deadline.to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{ deadline: "{deadline}" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "set child processing deadline failed: {:?}",
        response.errors
    );
    assert!(
        response
            .data
            .as_ref()
            .and_then(|data| data["update_AgentRequest"].as_array())
            .is_some_and(|rows| rows.len() == 1),
        "set child processing deadline did not match exactly one request: {:?}",
        response.data
    );
    let state = fetch_child_request_state(node, request_id.as_str()).await;
    assert_eq!(state.lifecycle_state.as_deref(), Some("processing"));
    assert_eq!(
        state.execution_generation.as_deref(),
        Some(execution_generation)
    );
}

async fn child_provider_source_rows(
    node: &EmbeddedNode,
    request_doc_id: &str,
) -> Vec<serde_json::Value> {
    let request_doc_id = escape_graphql_string(request_doc_id);
    let response = node.execute(&format!(r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ _docID agent_did requester_did source writer payload close }} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let expected_source = serde_json::to_value(OutputSource::ProviderTurn {
        scope: gents_protocol::rendered_request::CaptureScope {
            kind: gents_protocol::rendered_request::CaptureScopeKind::Inference,
            seq: 1,
        },
        turn_index: 0,
        attempt: 0,
    })
    .unwrap();
    response.data.expect("child output segment data")["AgentOutputSegment"]
        .as_array()
        .expect("child output segment rows")
        .iter()
        .filter(|row| row["source"] == expected_source)
        .cloned()
        .collect()
}

async fn fetch_child_request_state(
    node: &EmbeddedNode,
    child_request_id: &str,
) -> ChildRequestStateRow {
    let child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{child_request_id}" }} }},
                limit: 1
            ) {{
                lifecycle_state
                failure_reason
                execution_generation
                terminal_output
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentRequest")
}

async fn fetch_tool_call(node: &EmbeddedNode, session_id: &str, tool_call_id: &str) -> ToolCallRow {
    let session_id = escape_graphql_string(session_id);
    let tool_call_id = escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    tool_call_id: {{ _eq: "{tool_call_id}" }}
                }},
                limit: 1
            ) {{ lifecycle_state await_mode }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentToolCall")
}

async fn fetch_parent_messages(node: &EmbeddedNode, session_id: &str) -> Vec<MessageRow> {
    let header_doc_ids = parent_message_header_ids(node, session_id).await;
    load_parent_messages(node, session_id, &header_doc_ids).await
}

/// Physical header identities of the parent transcript, in sequence order.
async fn parent_message_header_ids(node: &EmbeddedNode, session_id: &str) -> Vec<String> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "message query failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|row| {
            row["_docID"]
                .as_str()
                .expect("AgentMessage physical identity")
                .to_owned()
        })
        .collect()
}

/// Reconstruct each named header through the canonical message owner. Each
/// row's metadata and content come from the same immutable header, so a
/// publication racing this observation can add headers but never pair one
/// header's metadata with another's content.
async fn load_parent_messages(
    node: &EmbeddedNode,
    session_id: &str,
    header_doc_ids: &[String],
) -> Vec<MessageRow> {
    let (agent_did, requester_did) = request_scope_for_session(node, session_id).await;
    let mut rows = Vec::with_capacity(header_doc_ids.len());
    for header_doc_id in header_doc_ids {
        let (header, message) = gents::session::load_canonical_message_from_node(
            node,
            header_doc_id,
            &agent_did,
            requester_did.as_deref(),
        )
        .await
        .unwrap_or_else(|error| panic!("reconstruct parent message {header_doc_id}: {error:#}"));
        assert_eq!(
            header.session_id, session_id,
            "header left the parent session"
        );
        let mut content = serde_json::to_string(&message).expect("serialize native message");
        // Keep the structured rendering for tool IDs and function names,
        // while exposing user-authored notification text without JSON
        // string escaping to the status/body assertions below.
        if let Message::User { content: items } = &message {
            for item in items {
                if let UserContent::Text(Text { text }) = item {
                    content.push('\n');
                    content.push_str(text);
                }
            }
        }
        rows.push(MessageRow {
            sequence: header.sequence,
            role: serde_json::to_value(header.role)
                .expect("serialize message role")
                .as_str()
                .expect("message role string")
                .to_owned(),
            content,
            request_doc_id: header.request_doc_id,
        });
    }
    rows
}

async fn fetch_background_notifications(node: &EmbeddedNode, session_id: &str) -> Vec<MessageRow> {
    fetch_parent_messages(node, session_id)
        .await
        .into_iter()
        .filter(|message| message.content.contains("<subagent-notification"))
        .collect()
}

async fn request_scope_for_session(
    node: &EmbeddedNode,
    session_id: &str,
) -> (String, Option<String>) {
    let session_id = escape_graphql_string(session_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{session_id}" }} }}, limit: 1) {{ agent_did requester_did }} }}"#
        ))
        .await;
    let data = response.data.expect("session owner data");
    let session = &data["AgentSession"][0];
    let agent_did = session["agent_did"]
        .as_str()
        .expect("session owner DID")
        .to_string();
    let requester_did = session["requester_did"].as_str().map(str::to_owned);
    (agent_did, requester_did)
}

/// Read the durable background-completion wake rows through the canonical
/// typed input: select the bare `input` field, decode
/// `gents_protocol::row::AgentRequestRow`, and match the coalescing owner's
/// `QueueSource::BackgroundCompletion` source with the coalesce policy. The
/// legacy `metadata` bag reader is retired; missing or malformed typed input
/// fails decoding loudly instead of silently passing the count assertions.
async fn fetch_scheduled_wakes(node: &EmbeddedNode, session_id: &str) -> Vec<AgentRequestRow> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                request_id
                session_id
                content
                lifecycle_state
                execution_origin
                input
                created_at
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "wake query failed: {:?}",
        response.errors
    );
    let rows = gents::graphql::rows::<AgentRequestRow>(&response, "AgentRequest")
        .expect("decode wake AgentRequest rows");
    rows.into_iter()
        .filter(|row| {
            row.session_id.as_deref() == Some(session_id)
                && row.execution_origin.as_deref() == Some("scheduled")
                && row
                    .input
                    .as_ref()
                    .and_then(|input| input.queue.as_ref())
                    .is_some_and(|queue| {
                        queue.source == QueueSource::BackgroundCompletion
                            && queue.policy == QueuePolicy::Coalesce
                    })
        })
        .collect()
}

#[tokio::test]
async fn background_completion_projects_bridge_notifies_and_enqueues_wake() {
    let (db, session_id, parent_request_id) = setup_fixture("background_completion_project").await;
    let (runtime, children) = start_accepted_children(
        &db,
        &session_id,
        &parent_request_id,
        &[AcceptedChild {
            tool_call_id: "spawn-bg-1",
            await_mode: AwaitMode::Background,
            final_response: "child final answer <ok>",
        }],
    )
    .await;
    let (child_request_id, _) = &children[0];
    release_child_and_wait(&db, &runtime, child_request_id, "spawn-bg-1").await;

    let outcome = project_background_subagent_completion(
        db.node.clone(),
        child_request_id,
        db.node_identity.did(),
    )
    .await
    .unwrap();
    assert!(matches!(
        &outcome,
        BackgroundCompletionOutcome::Projected { .. }
            | BackgroundCompletionOutcome::AlreadyProjected
    ));

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "spawn-bg-1").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("completed"));
    assert_eq!(tool.await_mode.as_deref(), Some("background"));

    let messages = fetch_background_notifications(db.node.as_ref(), &session_id).await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, "user");
    assert!(messages[0].content.contains(r#"<subagent-notification"#));
    assert!(messages[0].content.contains(r#"status="completed""#));
    assert!(messages[0]
        .content
        .contains("child final answer &lt;ok&gt;"));

    let wakes = fetch_scheduled_wakes(db.node.as_ref(), &session_id).await;
    assert_eq!(wakes.len(), 1);

    let again = project_background_subagent_completion(
        db.node.clone(),
        child_request_id,
        db.node_identity.did(),
    )
    .await
    .unwrap();
    assert_eq!(again, BackgroundCompletionOutcome::AlreadyProjected);
    assert_eq!(
        fetch_background_notifications(db.node.as_ref(), &session_id)
            .await
            .len(),
        1
    );
    let wakes = fetch_scheduled_wakes(db.node.as_ref(), &session_id).await;
    assert_eq!(wakes.len(), 1);
    runtime.shutdown().await;
}

#[tokio::test]
async fn background_completion_recovers_side_effects_after_bridge_already_projected() {
    let (db, session_id, parent_request_id) = setup_fixture("background_completion_recovery").await;
    let (runtime, children) = start_accepted_children(
        &db,
        &session_id,
        &parent_request_id,
        &[AcceptedChild {
            tool_call_id: "spawn-bg-recover",
            await_mode: AwaitMode::Background,
            final_response: "child completed before observer side effects",
        }],
    )
    .await;
    let (child_request_id, _) = &children[0];
    release_child_and_wait(&db, &runtime, child_request_id, "spawn-bg-recover").await;

    let mut lifecycle = ToolCallLifecycle::load(db.node.clone(), &session_id, "spawn-bg-recover")
        .await
        .unwrap()
        .expect("bridge should exist");
    lifecycle
        .bridge_complete("child completed before observer side effects".to_string())
        .await
        .unwrap();

    let outcome = project_background_subagent_completion(
        db.node.clone(),
        child_request_id,
        db.node_identity.did(),
    )
    .await
    .unwrap();
    assert!(matches!(
        &outcome,
        BackgroundCompletionOutcome::Projected { .. }
            | BackgroundCompletionOutcome::AlreadyProjected
    ));
    // The live observer may win notification projection after the bridge CAS.
    // Exact one-shot side effects and replay are checked below either way.
    let messages = fetch_background_notifications(db.node.as_ref(), &session_id).await;
    assert_eq!(messages.len(), 1);
    assert!(messages[0]
        .content
        .contains("child completed before observer side effects"));
    let wakes = fetch_scheduled_wakes(db.node.as_ref(), &session_id).await;
    assert_eq!(wakes.len(), 1);

    let again = project_background_subagent_completion(
        db.node.clone(),
        child_request_id,
        db.node_identity.did(),
    )
    .await
    .unwrap();
    assert_eq!(again, BackgroundCompletionOutcome::AlreadyProjected);
    assert_eq!(
        fetch_background_notifications(db.node.as_ref(), &session_id)
            .await
            .len(),
        1
    );
    assert_eq!(
        fetch_scheduled_wakes(db.node.as_ref(), &session_id)
            .await
            .len(),
        1
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn background_notification_sorts_after_reserved_spawn_tool_result() {
    let (db, session_id, _parent_request_id) =
        setup_runtime_fixture("background_completion_order").await;
    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "background child can complete quickly",
        "await_mode": "background"
    })
    .to_string();
    let runtime = run_canonical_background_spawn(&db, &args, "model-call-order").await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let messages = loop {
        let messages = fetch_parent_messages(db.node.as_ref(), &session_id).await;
        if messages
            .iter()
            .any(|message| message.content.contains("<subagent-notification"))
        {
            break messages;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for projected background notification"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    runtime.shutdown().await;

    let spawn = messages
        .iter()
        .find(|message| message.role == "assistant" && message.content.contains("spawn_subagent"))
        .expect("accepted spawn header");
    let receipt = messages
        .iter()
        .find(|message| message.role == "user" && message.content.contains("child_request_id"))
        .expect("spawn tool result");
    let notification = messages
        .iter()
        .find(|message| message.content.contains("<subagent-notification"))
        .expect("background notification");
    assert!(receipt.sequence > spawn.sequence);
    assert!(notification.sequence > receipt.sequence);
    let wakes = fetch_scheduled_wakes(db.node.as_ref(), &session_id).await;
    assert_eq!(wakes.len(), 1);
    assert_eq!(
        notification.request_doc_id.as_deref(),
        wakes[0].doc_id.as_deref()
    );
}

#[tokio::test]
async fn background_completion_compacts_multibyte_summary_without_panicking() {
    let (db, session_id, parent_request_id) = setup_fixture("background_completion_unicode").await;
    let final_response = "é".repeat(3000);
    let (runtime, children) = start_accepted_children(
        &db,
        &session_id,
        &parent_request_id,
        &[AcceptedChild {
            tool_call_id: "spawn-bg-unicode",
            await_mode: AwaitMode::Background,
            final_response: &final_response,
        }],
    )
    .await;
    let (child_request_id, _) = &children[0];
    release_child_and_wait(&db, &runtime, child_request_id, "spawn-bg-unicode").await;

    let outcome = project_background_subagent_completion(
        db.node.clone(),
        child_request_id,
        db.node_identity.did(),
    )
    .await
    .unwrap();
    assert!(matches!(
        &outcome,
        BackgroundCompletionOutcome::Projected { .. }
            | BackgroundCompletionOutcome::AlreadyProjected
    ));
    let messages = fetch_background_notifications(db.node.as_ref(), &session_id).await;
    assert_eq!(messages.len(), 1);
    assert!(messages[0].content.contains("<subagent-notification"));
    assert!(messages[0].content.contains("..."));
    runtime.shutdown().await;
}

#[tokio::test]
async fn multiple_background_completions_append_notifications_and_coalesce_wake() {
    let (db, session_id, parent_request_id) = setup_fixture("background_completion_coalesce").await;
    let (runtime, children) = start_accepted_children(
        &db,
        &session_id,
        &parent_request_id,
        &[
            AcceptedChild {
                tool_call_id: "spawn-bg-a",
                await_mode: AwaitMode::Background,
                final_response: "child A done",
            },
            AcceptedChild {
                tool_call_id: "spawn-bg-b",
                await_mode: AwaitMode::Background,
                final_response: "child B done",
            },
        ],
    )
    .await;
    let (child_a, _) = &children[0];
    let (child_b, _) = &children[1];
    runtime.backend.release("prompt for spawn-bg-a");
    runtime.backend.release("prompt for spawn-bg-b");
    release_child_and_wait(&db, &runtime, child_a, "spawn-bg-a").await;
    release_child_and_wait(&db, &runtime, child_b, "spawn-bg-b").await;

    let first =
        project_background_subagent_completion(db.node.clone(), child_a, db.node_identity.did())
            .await
            .unwrap();
    let second =
        project_background_subagent_completion(db.node.clone(), child_b, db.node_identity.did())
            .await
            .unwrap();
    assert!(matches!(
        first,
        BackgroundCompletionOutcome::Projected { .. }
            | BackgroundCompletionOutcome::AlreadyProjected
    ));
    assert!(matches!(
        second,
        BackgroundCompletionOutcome::Projected { .. }
            | BackgroundCompletionOutcome::AlreadyProjected
    ));
    let messages = fetch_background_notifications(db.node.as_ref(), &session_id).await;
    assert_eq!(messages.len(), 2);
    assert!(
        messages
            .iter()
            .any(|message| message.content.contains("child A done")),
        "missing A completion notification: {messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|message| message.content.contains("child B done")),
        "missing B completion notification: {messages:?}"
    );
    let wakes = fetch_scheduled_wakes(db.node.as_ref(), &session_id).await;
    assert!(
        !wakes.is_empty(),
        "both notifications require a wake: {wakes:#?}"
    );
    assert!(
        wakes
            .iter()
            .filter(|wake| wake.lifecycle_state == Some(RequestLifecycleState::Pending))
            .count()
            <= 1,
        "coalescing must not leave duplicate pending wakes: {wakes:#?}"
    );
    assert!(
        wakes
            .iter()
            .filter(|wake| {
                matches!(
                    wake.lifecycle_state,
                    Some(RequestLifecycleState::Claimed | RequestLifecycleState::Processing)
                )
            })
            .count()
            <= 1,
        "session must not have duplicate active wakes: {wakes:#?}"
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn background_completion_does_not_interrupt_active_foreground_parent() {
    let (db, session_id, parent_request_id) =
        setup_fixture("background_completion_interleave").await;
    let runtime = boot_accepted_turn_with_backend_capacity_and_dynamic_followups(
        &db,
        AcceptedTurnSpec {
            backend_id: BACKEND_ID,
            model: "test-model",
            parent_behavior_id: PARENT_BEHAVIOR_ID,
            configured_behavior_ids: &[PARENT_BEHAVIOR_ID, CHILD_BEHAVIOR_ID],
            request_id: &parent_request_id,
            session_id: &session_id,
            prompt: "parent prompt",
            accepted_chunks: vec![StreamChunk::tool_call(
                "spawn-bg-b",
                "spawn_subagent",
                json!({
                    "name": CHILD_BEHAVIOR_ID,
                    "prompt": "prompt for spawn-bg-b",
                    "await_mode": "background",
                })
                .to_string(),
            )],
            child_plans: vec![
                StreamPlan::new(
                    "prompt for spawn-bg-b",
                    vec![StreamResponse::Stream(StreamScript::paused(
                        "prompt for spawn-bg-b",
                        ["background child B done"],
                    ))],
                ),
                StreamPlan::new(
                    "prompt for spawn-fg-a",
                    vec![StreamResponse::Stream(StreamScript::paused(
                        "prompt for spawn-fg-a",
                        ["foreground child A done"],
                    ))],
                ),
            ],
            valid_until: None,
            subagent_depth: None,
            request_setup: None,
        },
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
        3,
        "parent prompt",
    )
    .await;
    let (background_child, _) =
        wait_for_child_for_tool(db.node.as_ref(), &parent_request_id, "spawn-bg-b").await;
    assert_exact_child_binding(
        db.node.as_ref(),
        &parent_request_id,
        &session_id,
        &background_child,
        db.node_identity.did(),
    )
    .await;
    wait_for_child_generation(db.node.as_ref(), &background_child).await;
    runtime.backend.enqueue_response(
        "parent prompt",
        StreamResponse::streams(
            "parent prompt",
            vec![StreamChunk::tool_call(
                "spawn-fg-a",
                "spawn_subagent",
                json!({
                    "name": CHILD_BEHAVIOR_ID,
                    "prompt": "prompt for spawn-fg-a",
                    "await_mode": "foreground",
                })
                .to_string(),
            )],
        ),
    );
    let (foreground_child, _) =
        wait_for_child_for_tool(db.node.as_ref(), &parent_request_id, "spawn-fg-a").await;
    assert_exact_child_binding(
        db.node.as_ref(),
        &parent_request_id,
        &session_id,
        &foreground_child,
        db.node_identity.did(),
    )
    .await;
    wait_for_child_generation(db.node.as_ref(), &foreground_child).await;
    release_child_and_wait(&db, &runtime, &background_child, "spawn-bg-b").await;

    // This test keeps the foreground bridge active while the daemon's real
    // completion observer projects the background sibling. Wait for that
    // owner rather than racing it with a second terminal-source close.
    let projection_deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let bridge = fetch_tool_call(db.node.as_ref(), &session_id, "spawn-bg-b").await;
        let notifications = fetch_background_notifications(db.node.as_ref(), &session_id).await;
        if bridge.lifecycle_state.as_deref() == Some("completed") && notifications.len() == 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < projection_deadline,
            "daemon did not project completed background sibling: bridge={bridge:?}, notifications={notifications:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let outcome = project_background_subagent_completion(
        db.node.clone(),
        &background_child,
        db.node_identity.did(),
    )
    .await
    .unwrap();
    assert_eq!(outcome, BackgroundCompletionOutcome::AlreadyProjected);
    assert_eq!(
        fetch_background_notifications(db.node.as_ref(), &session_id)
            .await
            .len(),
        1
    );

    let wakes = fetch_scheduled_wakes(db.node.as_ref(), &session_id).await;
    assert_eq!(wakes.len(), 1);
    let interrupt = fetch_interrupt_requested_at(db.node.as_ref(), &foreground_child)
        .await
        .unwrap();
    assert!(interrupt.is_none());
    runtime.shutdown().await;
}

#[tokio::test]
async fn recovery_leaves_running_background_bridge_after_clean_parent_completion() {
    let (db, session_id, parent_request_id) =
        setup_fixture("background_completion_recovery_skip").await;
    let (runtime, children) = start_accepted_children(
        &db,
        &session_id,
        &parent_request_id,
        &[AcceptedChild {
            tool_call_id: "spawn-bg-recovery-skip",
            await_mode: AwaitMode::Background,
            final_response: "unused",
        }],
    )
    .await;
    let (child_request_id, _) = &children[0];
    set_request_lifecycle(db.node.as_ref(), &parent_request_id, "completed").await;

    let report = ToolCallLifecycle::recover_all(&db.node, db.node_identity.did())
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 0);

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "spawn-bg-recovery-skip").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("running"));
    assert_eq!(tool.await_mode.as_deref(), Some("background"));
    let interrupt = fetch_interrupt_requested_at(db.node.as_ref(), child_request_id)
        .await
        .unwrap();
    assert!(
        interrupt.is_none(),
        "clean parent completion must not interrupt its linked background child"
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn recovery_terminalizes_expired_background_child_before_projection() {
    let (db, session_id, parent_request_id) =
        setup_fixture("background_completion_expired_child").await;
    let (runtime, children) = start_accepted_children(
        &db,
        &session_id,
        &parent_request_id,
        &[AcceptedChild {
            tool_call_id: "spawn-bg-expired-child",
            await_mode: AwaitMode::Background,
            final_response: "partial child output",
        }],
    )
    .await;
    let (child_request_id, _) = &children[0];
    let original_generation = wait_for_child_generation(db.node.as_ref(), child_request_id).await;
    let processing_timeout = tokio::time::Instant::now() + Duration::from_secs(5);
    while fetch_child_request_state(db.node.as_ref(), child_request_id)
        .await
        .lifecycle_state
        .as_deref()
        != Some("processing")
    {
        assert!(
            tokio::time::Instant::now() < processing_timeout,
            "child did not enter owned processing before crash"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let (observed_generation, original_expiry) =
        child_execution_expiry(db.node.as_ref(), child_request_id).await;
    assert_eq!(observed_generation, original_generation);
    let request_doc_id = crate::support::exact_request_doc_id(&db.node, child_request_id).await;
    let output_timeout = tokio::time::Instant::now() + Duration::from_secs(5);
    let original_output = loop {
        let rows = child_provider_source_rows(db.node.as_ref(), &request_doc_id).await;
        if rows.len() == 1
            && rows[0]["payload"] == "partial child output"
            && rows[0]["close"].is_null()
        {
            break rows[0].clone();
        }
        assert!(
            tokio::time::Instant::now() < output_timeout,
            "child did not flush one open provider source: {rows:#?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    runtime.crash().await;
    // Dropping an active owned execution asynchronously relinquishes its
    // physical lease. Wait for that exact same-generation write before taking
    // the startup recovery snapshot, so the revocation CAS sees stable facts.
    wait_for_child_lease_relinquishment(
        db.node.as_ref(),
        child_request_id,
        &original_generation,
        &original_expiry,
    )
    .await;
    let expired_deadline = chrono::Utc::now() - chrono::Duration::seconds(1);
    set_child_processing_deadline(
        db.node.as_ref(),
        child_request_id,
        expired_deadline,
        &original_generation,
    )
    .await;
    let child_id = escape_graphql_string(child_request_id);
    let evidence = db
        .node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{child_id}" }} }}, limit: 1) {{ _docID agent_did requester_did lifecycle_state deadline execution_generation execution_lease_expires_at }} }}"#
        ))
        .await;
    assert!(!evidence.has_errors(), "{:?}", evidence.errors);
    let recovery_evidence = evidence.data.expect("child recovery evidence");
    let did = escape_graphql_string(db.node_identity.did());
    let bridge_evidence = db
        .node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ lifecycle_state: {{ _eq: "running" }}, agent_did: {{ _eq: "{did}" }} }}) {{ _docID agent_did request_id child_request_id tool_call_id lifecycle_state }} }}"#
        ))
        .await;
    assert!(
        !bridge_evidence.has_errors(),
        "{:?}",
        bridge_evidence.errors
    );
    let bridge_evidence = bridge_evidence
        .data
        .expect("running bridge recovery evidence");

    let report = ToolCallLifecycle::recover_all(&db.node, db.node_identity.did())
        .await
        .unwrap();
    let child = fetch_child_request_state(db.node.as_ref(), child_request_id).await;
    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "spawn-bg-expired-child").await;
    assert_eq!(
        report.tool_calls_recovered, 1,
        "expired current-generation recovery: report={report:?}; evidence={recovery_evidence:?}; bridge_evidence={bridge_evidence:?}; child={child:?}; bridge={tool:?}"
    );
    assert_eq!(child.lifecycle_state.as_deref(), Some("dead"));
    assert!(
        child
            .failure_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("child request deadline exceeded")),
        "child failure reason should explain deadline expiry: {:?}",
        child.failure_reason
    );

    assert_ne!(
        child.execution_generation.as_deref(),
        Some(original_generation.as_str())
    );
    // Child-deadline revocation selects from existing header metadata. This
    // fixture has only an open segment, so it must not invent a transcript
    // header or a Partial close. Expired-lease partial salvage is covered by
    // e2e_lifecycle::lifecycle_recovery.
    assert!(matches!(
        child.terminal_output,
        Some(TerminalOutput::NoMessage)
    ));
    let source_rows = child_provider_source_rows(db.node.as_ref(), &request_doc_id).await;
    assert_eq!(
        source_rows.len(),
        1,
        "exact retained source rows={source_rows:#?}"
    );
    assert_eq!(
        source_rows[0], original_output,
        "deadline revocation must retain the exact physical open segment"
    );
    assert_eq!(source_rows[0]["agent_did"], db.node_identity.did());
    assert_eq!(source_rows[0]["requester_did"], db.node_identity.did());
    assert_eq!(
        source_rows[0]["writer"],
        serde_json::to_value(OutputWriter::RequestExecution {
            execution_generation: original_generation.clone(),
        })
        .unwrap()
    );
    assert!(
        source_rows[0]["close"].is_null(),
        "deadline revocation must not invent a close for this source: {source_rows:#?}"
    );

    let tool = fetch_tool_call(db.node.as_ref(), &session_id, "spawn-bg-expired-child").await;
    assert_eq!(tool.lifecycle_state.as_deref(), Some("failed"));
    assert_eq!(tool.await_mode.as_deref(), Some("background"));

    let messages = fetch_background_notifications(db.node.as_ref(), &session_id).await;
    assert_eq!(messages.len(), 1);
    assert!(messages[0].content.contains(r#"<subagent-notification"#));
    assert!(messages[0].content.contains(r#"status="dead""#));
    assert!(messages[0].content.contains(child_request_id.as_str()));

    let wakes = fetch_scheduled_wakes(db.node.as_ref(), &session_id).await;
    assert_eq!(wakes.len(), 1);
}

#[tokio::test]
async fn stale_hook_sequence_does_not_overwrite_background_notification() {
    let (db, session_id, _parent_request_id) =
        setup_runtime_fixture("background_completion_hook_sequence").await;
    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "notification must survive",
        "await_mode": "background"
    })
    .to_string();
    let first_runtime = run_canonical_background_spawn(&db, &args, "model-call-stale-hook").await;
    let notification_deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let messages = fetch_parent_messages(db.node.as_ref(), &session_id).await;
        if messages
            .iter()
            .any(|message| message.content.contains("notification must survive"))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < notification_deadline,
            "timed out waiting for canonical background notification"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    first_runtime.shutdown().await;

    let runtime = run_canonical_parent_prompt(
        &db,
        &session_id,
        "background-completion-resume-request",
        "parent hook resumes",
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let messages = fetch_parent_messages(db.node.as_ref(), &session_id).await;
        if messages
            .iter()
            .any(|message| message.content.contains("parent hook resumes"))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for canonically authored resumed prompt"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    runtime.shutdown().await;

    let messages = fetch_parent_messages(db.node.as_ref(), &session_id).await;
    let notification = messages
        .iter()
        .find(|message| message.content.contains("notification must survive"))
        .expect("background notification");
    let resumed = messages
        .iter()
        .find(|message| message.content.contains("parent hook resumes"))
        .expect("resumed authored prompt");
    assert!(resumed.sequence > notification.sequence);
}

/// #1806 pinned-identity regression for `load_parent_messages`: identities
/// captured while the runtime is stopped still reconstruct to exactly their
/// own rows after a later publication. This does not drive
/// `fetch_parent_messages` itself across the race; that composition is covered
/// only by repeated integration runs.
#[tokio::test]
async fn parent_message_observation_is_coherent_across_publication() {
    let (db, session_id, _parent_request_id) =
        setup_runtime_fixture("parent_message_observation_publication").await;
    let args = json!({
        "name": CHILD_BEHAVIOR_ID,
        "prompt": "observed before publication",
        "await_mode": "background"
    })
    .to_string();
    let first_runtime = run_canonical_background_spawn(&db, &args, "model-call-observation").await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !fetch_parent_messages(db.node.as_ref(), &session_id)
        .await
        .iter()
        .any(|message| message.content.contains("observed before publication"))
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for canonical background notification"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    first_runtime.shutdown().await;

    let before_ids = parent_message_header_ids(db.node.as_ref(), &session_id).await;
    let before = load_parent_messages(db.node.as_ref(), &session_id, &before_ids).await;

    let runtime = run_canonical_parent_prompt(
        &db,
        &session_id,
        "observation-publication-request",
        "published between reads",
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let after = loop {
        let messages = fetch_parent_messages(db.node.as_ref(), &session_id).await;
        if messages
            .iter()
            .any(|message| message.content.contains("published between reads"))
        {
            break messages;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the publication between reads"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    runtime.shutdown().await;

    // The identities read before the publication still reconstruct to exactly
    // their own rows; the new header is simply not part of that observation.
    let straddling = load_parent_messages(db.node.as_ref(), &session_id, &before_ids).await;
    assert!(after.len() > before_ids.len());
    assert_eq!(straddling.len(), before_ids.len());
    for (observed, original) in straddling.iter().zip(&before) {
        assert_eq!(observed.sequence, original.sequence);
        assert_eq!(observed.role, original.role);
        assert_eq!(observed.request_doc_id, original.request_doc_id);
        assert_eq!(observed.content, original.content);
    }
    assert!(straddling
        .iter()
        .all(|message| !message.content.contains("published between reads")));
    assert_eq!(&after[..before.len()], &before[..]);
}

async fn persist_unresolvable_message(node: &EmbeddedNode, request_id: &str) {
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, PayloadRef, TranscriptMessage,
    };
    #[derive(Deserialize)]
    struct Scope {
        #[serde(rename = "_docID")]
        doc_id: String,
        agent_did: String,
        requester_did: Option<String>,
        session_id: String,
    }
    let request = escape_graphql_string(request_id);
    let scope: Scope = first_row(
        &node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request}" }} }}, limit: 1) {{ _docID agent_did requester_did session_id }} }}"#
            ))
            .await,
        "AgentRequest",
    );
    let message = TranscriptMessage {
        message_key: format!("unresolvable:{request_id}"),
        session_id: scope.session_id,
        agent_did: scope.agent_did,
        requester_did: scope.requester_did,
        request_doc_id: Some(scope.doc_id),
        publication: MessagePublication::RequestExecution {
            execution_generation: "unrelated".into(),
        },
        outcome: OutputOutcome::Complete,
        sequence: 10_000,
        role: MessageRole::Assistant,
        native_id: None,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        blocks: vec![MessageBlock::ToolCall {
            tool_call_doc_id: "unrelated-tool".into(),
            id: "unrelated-call".into(),
            call_id: None,
            name: "bash".into(),
            arguments: PayloadRef {
                close_doc_id: "missing-close".into(),
                stream: 0,
            },
            signature: None,
            additional_params: None,
        }],
    };
    let variables =
        gents::session::canonical_rows::transcript_message_create_variables(&message).unwrap();
    ConfigAccess::transact_local(node, None, "test.unresolvable_sibling_message", |txn| {
        let variables = variables.clone();
        Box::pin(async move {
            txn.execute_with_variables(
                gents::session::canonical_rows::CREATE_AGENT_MESSAGE_MUTATION,
                &variables,
            )
            .await
            .map(|_| ())
        })
    })
    .await
    .expect("persist unresolvable sibling message");
}

/// A spawn is admitted from its own accepted message (#1807). Reading every
/// sibling transcript made materialization and claim scale with the run; an
/// unresolvable sibling message now cannot delay a later child at all.
#[tokio::test]
async fn later_child_is_claimed_without_reading_sibling_transcripts() {
    let (db, session_id, request_id) = setup_fixture("claim_independent_of_siblings").await;
    let spawn = |id: &str| {
        StreamChunk::tool_call(
            id,
            "spawn_subagent",
            json!({
                "name": CHILD_BEHAVIOR_ID,
                "prompt": format!("prompt for {id}"),
                "await_mode": "background",
            })
            .to_string(),
        )
    };
    let paused = |id: &str| {
        let prompt = format!("prompt for {id}");
        StreamPlan::new(
            prompt.clone(),
            vec![StreamResponse::Stream(StreamScript::paused(
                prompt,
                ["done"],
            ))],
        )
    };
    let runtime = boot_accepted_turn_with_backend_capacity_and_dynamic_followups(
        &db,
        AcceptedTurnSpec {
            backend_id: BACKEND_ID,
            model: "test-model",
            parent_behavior_id: PARENT_BEHAVIOR_ID,
            configured_behavior_ids: &[PARENT_BEHAVIOR_ID, CHILD_BEHAVIOR_ID],
            request_id: &request_id,
            session_id: &session_id,
            prompt: "parent prompt",
            accepted_chunks: vec![spawn("spawn-sibling")],
            child_plans: vec![paused("spawn-sibling"), paused("spawn-later")],
            valid_until: None,
            subagent_depth: None,
            request_setup: None,
        },
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
        3,
        "parent prompt",
    )
    .await;
    let (sibling, _) =
        wait_for_child_for_tool(db.node.as_ref(), &request_id, "spawn-sibling").await;
    wait_for_child_generation(db.node.as_ref(), &sibling).await;
    persist_unresolvable_message(db.node.as_ref(), &sibling).await;

    runtime.backend.enqueue_response(
        "parent prompt",
        StreamResponse::streams("parent prompt", vec![spawn("spawn-later")]),
    );
    let (later, _) = wait_for_child_for_tool(db.node.as_ref(), &request_id, "spawn-later").await;
    wait_for_child_generation(db.node.as_ref(), &later).await;
    runtime.shutdown().await;
}
