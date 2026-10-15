use std::sync::Arc;
use std::time::Duration;

use gents::config::{MaxTurnsProvenance, ResolvedAgent, DEFAULT_MAX_TURNS};
use gents::defra_node::EmbeddedNode;
use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use gents::{DocumentRuntimeOptions, Gents, NodeIdentity, ToolCeiling};
use serde::Deserialize;
use serde_json::json;

use crate::support::accepted_turn::{
    boot_prepared_accepted_turn, prepare_accepted_turn, AcceptedTurnRuntime, AcceptedTurnSpec,
};
use crate::support::fixtures::{agent_target, configure_child_agent};
use crate::support::streaming_backend::{StreamChunk, StreamPlan, StreamResponse};
use crate::support::{test_db, TestDb};

const PARENT_AGENT_ID: &str = "e2e-session-limit-parent";
const CHILD_AGENT_ID: &str = "e2e-session-limit-child";
const BACKEND_ID: &str = "e2e-session-limit-backend";
const MODEL: &str = "e2e-session-limit-model";
const PARENT_TOOL_CALL_ID: &str = "e2e-session-limit-tool-call";
const PARENT_PROMPT: &str = "e2e-session-limit-parent-prompt";
const CHILD_PROMPT: &str = "e2e-session-limit-child-prompt";
/// Its decimal is not a prefix of the built-in default (1,000) nor of any other
/// limit a mis-wired child could run instead, and it is low enough that the
/// child reaches its turn limit within the eight invalid dispatches
/// `gents_loop::loop_stream::invalid_tool_progress` allows per execution.
const EXPLICIT_CHILD_MAX_TURNS: i64 = 3;
/// rig's max-turns display, matched as a fixed byte string by
/// `scripts/harbor/run_gents.sh`.
const PINNED_PREFIX: &str = "agent stream failed: PromptError: MaxTurnError: ";

#[derive(Debug, Deserialize)]
struct ChildRequestRow {
    request_id: String,
    agent_id: Option<String>,
    lifecycle_state: Option<String>,
    failure_reason: Option<String>,
}

async fn configure_session_chain(db: &TestDb) {
    let node_did = db.node_identity.did().to_string();
    configure_child_agent(
        db.node.as_ref(),
        &node_did,
        CHILD_AGENT_ID,
        "e2e-session-limit-child-tools",
        Vec::new(),
        false,
    )
    .await;
    configure_child_agent(
        db.node.as_ref(),
        &node_did,
        PARENT_AGENT_ID,
        "e2e-session-limit-parent-tools",
        vec![agent_target(
            &node_did,
            CHILD_AGENT_ID,
            &node_did,
            CHILD_AGENT_ID,
        )],
        true,
    )
    .await;
}

async fn bind_child_execution_max_turns(node: &EmbeddedNode, node_did: &str, max_turns: i64) {
    use gents::config_client::{
        read_desired_state_record_in_txn as read, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    use gents::Collection;

    gents::config_client::ConfigAccess::transact_local(
        node,
        None,
        "test.bind_child_execution_max_turns",
        |txn| {
            Box::pin(async move {
                let profile_id = format!("{CHILD_AGENT_ID}-inference");
                let execution_id = format!("{CHILD_AGENT_ID}-execution");
                let (_, mut profile) =
                    read(txn, Collection::InferenceProfile, node_did, &profile_id)
                        .await?
                        .expect("child inference profile");
                profile["execution_id"] = execution_id.clone().into();
                let execution = json!({
                    "node_did": node_did,
                    "execution_id": execution_id,
                    "max_turns": max_turns,
                });
                let plan = DesiredStateApplyPlan::new(vec![
                    DesiredStateApplyDocument {
                        collection: Collection::InferenceExecution,
                        add: execution.clone(),
                        update: execution,
                    },
                    DesiredStateApplyDocument {
                        collection: Collection::InferenceProfile,
                        add: profile.clone(),
                        update: profile,
                    },
                ])?;
                gents::config_client::apply_desired_state_plan(txn, &plan).await
            })
        },
    )
    .await
    .expect("bind child execution limit");
}

/// The caller's completion wake may share the caller's lineage, so the started
/// session's request is selected by its behavior rather than by taking the
/// first row.
async fn child_of(node: &EmbeddedNode, parent_request_id: &str) -> Option<ChildRequestRow> {
    let escaped = escape_graphql_string(parent_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ caused_by_parent_request_id: {{ _eq: "{escaped}" }} }}
            ) {{
                request_id
                agent_id
                lifecycle_state
                failure_reason
            }}
        }}"#
    );
    let response = graphql_with_transaction_retry(node, &query, "test.read_child_request")
        .await
        .expect("read child request");
    assert!(!response.has_errors(), "{:?}", response.errors);
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .into_iter()
        .flatten()
        .cloned()
        .map(|row| serde_json::from_value::<ChildRequestRow>(row).expect("decode AgentRequest row"))
        .find(|row| row.agent_id.as_deref() == Some(CHILD_AGENT_ID))
}

async fn wait_for_terminal_child(
    node: &EmbeddedNode,
    parent_request_id: &str,
    runtime: &AcceptedTurnRuntime,
) -> ChildRequestRow {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(child) = child_of(node, parent_request_id).await {
            if matches!(
                child.lifecycle_state.as_deref(),
                Some("completed") | Some("failed")
            ) {
                return child;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for a terminal child of {parent_request_id}; provider bodies: {:?}",
            runtime.backend.observed_completion_bodies()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn child_tool_turn(index: usize) -> StreamResponse {
    StreamResponse::streams(
        CHILD_PROMPT,
        vec![StreamChunk::tool_call(
            format!("e2e-session-limit-turn-{index}"),
            "discover_tools",
            "{}".to_string(),
        )],
    )
}

async fn start_child_session(
    db: &TestDb,
    request_id: &str,
    session_id: &str,
    child_responses: Vec<StreamResponse>,
) -> (Arc<ResolvedAgent>, AcceptedTurnRuntime) {
    let prepared = prepare_accepted_turn(
        db,
        AcceptedTurnSpec {
            backend_id: BACKEND_ID,
            model: MODEL,
            parent_agent_id: PARENT_AGENT_ID,
            configured_agent_ids: &[PARENT_AGENT_ID, CHILD_AGENT_ID],
            request_id,
            session_id,
            prompt: PARENT_PROMPT,
            accepted_chunks: vec![StreamChunk::tool_call(
                PARENT_TOOL_CALL_ID,
                gents::toolset::AGENT_NEW_TOOL_NAME,
                json!({
                    "agent": CHILD_AGENT_ID,
                    "prompt": CHILD_PROMPT,
                })
                .to_string(),
            )],
            child_plans: vec![StreamPlan::new(CHILD_PROMPT, child_responses)],
            valid_until: None,
            request_hop: Some(0),
            request_setup: None,
        },
    )
    .await;

    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let agent = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .expect("build child-limit runtime");
    let child_behavior = agent
        .agents()
        .iter()
        .find(|behavior| behavior.agent_id == CHILD_AGENT_ID)
        .expect("child behavior resolved from documents")
        .clone();

    let runtime = boot_prepared_accepted_turn(db, prepared, agent).await;
    (child_behavior, runtime)
}

/// `gents_loop::loop_stream` guards on `current_turn > config.max_turns + 1`
/// from a zero start, so a limit of N executes N + 2 turns: a child that
/// completes seven turns ran under a limit of at least five, bounding the
/// applied limit by execution rather than by reading the configuration back.
/// The count cannot grow much further — this child has no callable tool, so
/// every scripted turn spends one of the eight invalid dispatches
/// `gents_loop::loop_stream::invalid_tool_progress` allows per execution, and
/// exhausting those fails the child ahead of any turn limit.
#[tokio::test]
async fn started_session_without_an_execution_document_runs_past_a_low_turn_limit() {
    let db = test_db("e2e-session-limit-default").await;
    configure_session_chain(&db).await;

    let mut responses = (0..6).map(child_tool_turn).collect::<Vec<_>>();
    responses.push(StreamResponse::completes(CHILD_PROMPT, ["child done"]));
    let (child_behavior, runtime) = start_child_session(
        &db,
        "e2e-session-limit-default-request",
        "e2e-session-limit-default-session",
        responses,
    )
    .await;

    let child = wait_for_terminal_child(
        db.node.as_ref(),
        "e2e-session-limit-default-request",
        &runtime,
    )
    .await;
    assert_eq!(child.agent_id.as_deref(), Some(CHILD_AGENT_ID));
    assert_eq!(
        child.lifecycle_state.as_deref(),
        Some("completed"),
        "child {} must finish every scripted turn, not exhaust a limit: {:?}",
        child.request_id,
        child.failure_reason
    );

    assert_eq!(child_behavior.max_turns, DEFAULT_MAX_TURNS);
    assert_eq!(child_behavior.max_turns, 1_000);
    assert_eq!(
        child_behavior.max_turns_provenance,
        MaxTurnsProvenance::Default
    );

    runtime.runtime.shutdown().await;
}

/// `PromptError::MaxTurnsError` reports `config.max_turns`, the bound the
/// child's own loop enforced, so the persisted failure names the applied limit
/// and the applied provenance rather than the configuration they came from.
#[tokio::test]
async fn started_session_loop_enforces_its_execution_documents_turn_limit() {
    let db = test_db("e2e-session-limit-explicit").await;
    configure_session_chain(&db).await;
    bind_child_execution_max_turns(
        db.node.as_ref(),
        &db.node_identity.did().to_string(),
        EXPLICIT_CHILD_MAX_TURNS,
    )
    .await;

    let (child_behavior, runtime) = start_child_session(
        &db,
        "e2e-session-limit-explicit-request",
        "e2e-session-limit-explicit-session",
        (0..8).map(child_tool_turn).collect(),
    )
    .await;

    let child = wait_for_terminal_child(
        db.node.as_ref(),
        "e2e-session-limit-explicit-request",
        &runtime,
    )
    .await;
    assert_eq!(child.agent_id.as_deref(), Some(CHILD_AGENT_ID));
    assert_eq!(child.lifecycle_state.as_deref(), Some("failed"));
    let reason = child.failure_reason.unwrap_or_default();
    assert!(
        reason.starts_with(PINNED_PREFIX),
        "child failure must keep the pinned prefix: {reason}"
    );
    assert!(
        reason.contains(&format!(
            "reached max turn limit: {EXPLICIT_CHILD_MAX_TURNS})"
        )),
        "child loop must enforce the configured limit, not another one: {reason}"
    );
    assert!(
        reason.contains(MaxTurnsProvenance::ExecutionProfile.describe()),
        "the failure must name the knob that set the applied limit: {reason}"
    );
    assert!(
        !reason.contains(MaxTurnsProvenance::Default.describe()),
        "a document-bound limit must not report the built-in default: {reason}"
    );

    assert_eq!(
        child_behavior.max_turns,
        usize::try_from(EXPLICIT_CHILD_MAX_TURNS).unwrap()
    );
    assert_eq!(
        child_behavior.max_turns_provenance,
        MaxTurnsProvenance::ExecutionProfile
    );

    runtime.runtime.shutdown().await;
}
