use std::sync::Arc;
use std::time::Duration;

use gents::config::{MaxTurnsProvenance, ResolvedBehavior, DEFAULT_MAX_TURNS};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{AgentIdentity, DocumentRuntimeOptions, Gents, ToolCeiling};
use gents_protocol::row::AgentRequestRow;
use serde_json::json;

use crate::support::accepted_turn::{
    boot_prepared_accepted_turn, prepare_accepted_turn, AcceptedTurnRuntime, AcceptedTurnSpec,
};
use crate::support::fixtures::{configure_subagent_behavior, subagent_target};
use crate::support::streaming_backend::{StreamChunk, StreamPlan, StreamResponse, StreamScript};
use crate::support::{first_optional_row, test_db, TestDb};

const PARENT_BEHAVIOR_ID: &str = "e2e-child-limit-parent";
const CHILD_BEHAVIOR_ID: &str = "e2e-child-limit-child";
const BACKEND_ID: &str = "e2e-child-limit-backend";
const MODEL: &str = "e2e-child-limit-model";
const PARENT_TOOL_CALL_ID: &str = "e2e-child-limit-tool-call";
const PARENT_PROMPT: &str = "e2e-child-limit-parent-prompt";
const CHILD_PROMPT: &str = "e2e-child-limit-child-prompt";
const EXPLICIT_CHILD_MAX_TURNS: i64 = 7;

async fn configure_spawn_chain(db: &TestDb) {
    let agent_did = db.node_identity.did().to_string();
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        CHILD_BEHAVIOR_ID,
        "e2e-child-limit-child-tools",
        Vec::new(),
        false,
        false,
        None,
    )
    .await;
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        PARENT_BEHAVIOR_ID,
        "e2e-child-limit-parent-tools",
        vec![subagent_target(
            &agent_did,
            CHILD_BEHAVIOR_ID,
            &agent_did,
            CHILD_BEHAVIOR_ID,
        )],
        true,
        true,
        None,
    )
    .await;
}

async fn bind_child_execution_max_turns(node: &EmbeddedNode, agent_did: &str, max_turns: i64) {
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
                let profile_id = format!("{CHILD_BEHAVIOR_ID}-inference");
                let execution_id = format!("{CHILD_BEHAVIOR_ID}-execution");
                let (_, mut profile) =
                    read(txn, Collection::InferenceProfile, agent_did, &profile_id)
                        .await?
                        .expect("child inference profile");
                profile["execution_id"] = execution_id.clone().into();
                let execution = json!({
                    "agent_did": agent_did,
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

async fn wait_for_child_row(node: &EmbeddedNode, parent_request_id: &str) -> AgentRequestRow {
    let escaped = escape_graphql_string(parent_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ caused_by_parent_request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                request_id
                behavior_id
                caused_by_parent_request_id
            }}
        }}"#
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(row) =
            first_optional_row::<AgentRequestRow>(&node.execute(&query).await, "AgentRequest")
        {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the child spawned by {parent_request_id}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn spawn_child(
    db: &TestDb,
    request_id: &str,
    session_id: &str,
) -> (AgentRequestRow, Arc<ResolvedBehavior>, AcceptedTurnRuntime) {
    let prepared = prepare_accepted_turn(
        db,
        AcceptedTurnSpec {
            backend_id: BACKEND_ID,
            model: MODEL,
            parent_behavior_id: PARENT_BEHAVIOR_ID,
            configured_behavior_ids: &[PARENT_BEHAVIOR_ID, CHILD_BEHAVIOR_ID],
            request_id,
            session_id,
            prompt: PARENT_PROMPT,
            accepted_chunks: vec![StreamChunk::tool_call(
                PARENT_TOOL_CALL_ID,
                "spawn_subagent",
                json!({
                    "name": CHILD_BEHAVIOR_ID,
                    "prompt": CHILD_PROMPT,
                    "await_mode": "background"
                })
                .to_string(),
            )],
            child_plans: vec![StreamPlan::new(
                CHILD_PROMPT,
                vec![StreamResponse::Stream(StreamScript::paused(
                    CHILD_PROMPT,
                    ["child started"],
                ))],
            )],
            valid_until: None,
            subagent_depth: Some(0),
            request_setup: None,
        },
    )
    .await;

    let identity: Arc<dyn AgentIdentity> = db.node_identity.clone();
    let agent = Gents::from_default_behavior_documents(
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
        .behaviors()
        .iter()
        .find(|behavior| behavior.behavior_id == CHILD_BEHAVIOR_ID)
        .expect("child behavior resolved from documents")
        .clone();

    let runtime = boot_prepared_accepted_turn(db, prepared, agent).await;
    let child = wait_for_child_row(db.node.as_ref(), request_id).await;
    (child, child_behavior, runtime)
}

#[tokio::test]
async fn spawned_child_without_an_execution_document_runs_at_the_default_turn_limit() {
    let db = test_db("e2e-child-limit-default").await;
    configure_spawn_chain(&db).await;

    let (child, child_behavior, runtime) = spawn_child(
        &db,
        "e2e-child-limit-default-request",
        "e2e-child-limit-default-session",
    )
    .await;

    assert_eq!(child.behavior_id.as_deref(), Some(CHILD_BEHAVIOR_ID));
    assert_eq!(child_behavior.max_turns, DEFAULT_MAX_TURNS);
    assert_eq!(child_behavior.max_turns, 1_000);
    assert_eq!(
        child_behavior.max_turns_provenance,
        MaxTurnsProvenance::Default
    );

    runtime.runtime.shutdown().await;
}

#[tokio::test]
async fn spawned_child_runs_at_its_execution_documents_turn_limit() {
    let db = test_db("e2e-child-limit-explicit").await;
    configure_spawn_chain(&db).await;
    bind_child_execution_max_turns(
        db.node.as_ref(),
        &db.node_identity.did().to_string(),
        EXPLICIT_CHILD_MAX_TURNS,
    )
    .await;

    let (child, child_behavior, runtime) = spawn_child(
        &db,
        "e2e-child-limit-explicit-request",
        "e2e-child-limit-explicit-session",
    )
    .await;

    assert_eq!(child.behavior_id.as_deref(), Some(CHILD_BEHAVIOR_ID));
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
