use std::sync::Arc;

use gents_store::identity::NodeIdentity;
use serde_json::Value;

use crate::support::accepted_turn::boot_prepared_accepted_turn;
use crate::support::live_inference::request_terminal_rows;
use crate::support::signed_mailbox::prepare_signed_mailbox_turn;
use crate::support::streaming_backend::{StreamChunk, StreamResponse};
use crate::support::test_db;

const BACKEND_ID: &str = "own-tools-backend";
const MODEL: &str = "own-tools-model";
const PROMPT: &str = "own-tools-rebinding-prompt";

fn offered(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_owned))
        .collect()
}

/// The Engineer, holding the policy-delegate set init issues the default
/// Agent, turns on Goal creation and turns off the sessions tool in its own
/// Tools, then creates a Goal in the same request: the turn after the config
/// call is sent the new toolset and its `create_goal` call runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_calls_a_tool_it_granted_itself_in_the_same_request() {
    let db = test_db("own-tools-rebinding").await;
    let request_id = "own-tools-request";
    let grant = serde_json::json!({
        "argv": ["tools", "update"],
        "set": {"built_ins": {
            "enable_session_history_tool": false,
            "enable_goal_tools": true,
            "enable_goal_creation": true,
        }},
    });
    let (prepared, turn) = prepare_signed_mailbox_turn(
        &db,
        BACKEND_ID,
        MODEL,
        request_id,
        "own-tools-session",
        PROMPT,
        vec![StreamChunk::tool_call(
            "call-grant",
            gents_protocol::tool_names::CONFIG_TOOL_NAME,
            grant.to_string(),
        )],
    )
    .await
    .unwrap();
    gents_store::identity::GrantSet::PolicyDelegate
        .grant(
            &gents_store::config_client::ConfigAccess::local_as(
                db.node.clone(),
                identity::Did::new(turn.node_did.clone()).unwrap(),
            ),
            &identity::Did::new(turn.agent.did()).unwrap(),
        )
        .await
        .unwrap();
    prepared.backend.enable_dynamic_followups(PROMPT);
    prepared.backend.enqueue_response(
        PROMPT,
        StreamResponse::streams(
            PROMPT,
            vec![StreamChunk::tool_call(
                "call-goal",
                gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME,
                serde_json::json!({"objective": "Ship the configured node"}).to_string(),
            )],
        ),
    );
    prepared
        .backend
        .enqueue_response(PROMPT, StreamResponse::completes(PROMPT, ["configured"]));
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;

    let rows = request_terminal_rows(&turn.client, request_id).await;
    assert_eq!(
        rows["AgentRequest"][0]["lifecycle_state"], "completed",
        "{rows}"
    );
    let calls = rows["AgentToolCall"].as_array().unwrap();
    let goal = calls
        .iter()
        .find(|call| call["tool_name"] == gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME)
        .unwrap_or_else(|| panic!("the granted tool was called: {rows}"));
    assert_eq!(goal["lifecycle_state"], "completed", "{rows}");

    let bodies = runtime.backend.observed_turn_completion_bodies();
    let [before, after, last] = bodies.as_slice() else {
        panic!("three turns: {bodies:?}");
    };
    let sessions = gents_protocol::tool_names::SESSION_HISTORY_TOOL_NAME;
    let create_goal = gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME;
    assert!(
        offered(before).iter().any(|name| name == sessions)
            && !offered(before).iter().any(|name| name == create_goal),
        "the first turn has the claimed toolset: {:?}",
        offered(before)
    );
    for body in [after, last] {
        assert!(
            offered(body).iter().any(|name| name == create_goal)
                && !offered(body).iter().any(|name| name == sessions),
            "later turns have the rebound toolset: {:?}",
            offered(body)
        );
    }

    runtime.shutdown().await;
}
