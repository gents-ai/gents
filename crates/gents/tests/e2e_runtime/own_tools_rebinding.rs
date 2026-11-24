use std::sync::Arc;

use gents_store::identity::NodeIdentity;
use serde_json::{json, Value};

use crate::support::accepted_turn::boot_prepared_accepted_turn;
use crate::support::live_inference::request_terminal_rows;
use crate::support::signed_mailbox::{prepare_signed_mailbox_turn, SignedMailboxTurn};
use crate::support::test_db;
use gents_inference::test_support::scripted_backend::{StreamChunk, StreamResponse};

const BACKEND_ID: &str = "own-tools-backend";
const MODEL: &str = "own-tools-model";
const PROMPT: &str = "own-tools-rebinding-prompt";

fn agent_new_targets(body: &Value) -> Option<Value> {
    body["tools"]
        .as_array()?
        .iter()
        .find(|tool| tool["function"]["name"] == "agent_new")
        .map(|tool| tool["function"]["parameters"]["properties"]["agent"]["enum"].clone())
}

/// The receipt the config call `call_id` returned, as the next turn reads it.
fn receipt(body: &Value, call_id: &str) -> Value {
    let message = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool" && message["tool_call_id"] == call_id)
        .unwrap_or_else(|| panic!("the turn carries {call_id}: {body}"));
    serde_json::from_str(message["content"].as_str().unwrap()).unwrap()
}

/// The Engineer's config call outside any request, as it runs before boot.
async fn configure(tools: &[Box<dyn gents_loop::tool::ToolDyn>], call: Value) -> Value {
    let config = tools
        .iter()
        .find(|tool| tool.name() == gents_protocol::tool_names::CONFIG_TOOL_NAME)
        .unwrap();
    let text = config
        .call(
            call.to_string(),
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await
        .unwrap_or_else(|error| panic!("{call}: {error:#}"));
    serde_json::from_str(&text).unwrap()
}

/// The Engineer, holding the Node administration init gives the default
/// Agent, creates two helper agents and an agent target for each: `writer`
/// runs, `helper` it disables.
async fn seed_agent_targets(db: &crate::support::TestDb, turn: &SignedMailboxTurn) {
    let node_did = turn.node_did.clone();
    let engineer = turn.agent.did().to_owned();
    turn.client
        .transact_write("test.own_tools.node_administration", |txn| {
            let (node_did, engineer) = (&node_did, &engineer);
            Box::pin(async move {
                gents_documents::grant_node_administration(txn, node_did, engineer).await
            })
        })
        .await
        .unwrap();
    let tools = gents_tools::builtin::self_config::build_runtime_self_config_tools(
        Arc::default(),
        db.node.clone(),
        turn.agent.clone(),
        db.node_identity.clone(),
        &gents_tools::surface::SelfConfigToolConfig {
            enabled: true,
            agent_did: engineer.clone(),
            categories: gents_documents::patch::SELF_CONFIG_CATEGORIES
                .iter()
                .map(|category| category.to_string())
                .collect(),
            no_lockout: true,
            ..Default::default()
        },
        Arc::new(gents_toolcalls::plugin::executor::PluginExecutor::default()),
        Arc::new(gents_automation::triggers::task_run::TaskRuns),
    )
    .await
    .unwrap();
    let profile = format!("{engineer}:profile");
    for (name, enabled) in [("writer", true), ("helper", false)] {
        let created = configure(
            &tools,
            json!({"argv": ["agent", "create"], "options": {
                "display-name": name, "system-prompt": "Help.", "profile": profile}}),
        )
        .await;
        let agent_did = created["agent_did"].as_str().unwrap().to_owned();
        if !enabled {
            configure(
                &tools,
                json!({"argv": ["agent", "update"], "target_id": agent_did,
                    "set": {"enabled": false}}),
            )
            .await;
        }
        configure(
            &tools,
            json!({"argv": ["agent-target", "create"], "target_id": format!("{name}-target"),
                "set": {"agent_did": agent_did, "name": name}}),
        )
        .await;
    }
}

fn offered(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_owned))
        .collect()
}

/// The Engineer, holding the policy-delegate set init issues the default
/// Agent, turns on Goal creation, turns off the sessions tool and selects the
/// targets of the agents it created in its own Tools, then creates a Goal in
/// the same request: the turn after the config call is sent the new toolset
/// and its `create_goal` call runs. The receipt names what the next turn
/// gained, and the target whose agent does not run as waiting, not in effect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_calls_a_tool_it_granted_itself_in_the_same_request() {
    let db = test_db("own-tools-rebinding").await;
    let request_id = "own-tools-request";
    let grant = serde_json::json!({
        "argv": ["tools", "update"],
        "set": {
            "built_ins": {
                "enable_session_history_tool": false,
                "enable_goal_tools": true,
                "enable_goal_creation": true,
            },
            "agents": {"enabled": true, "target_ids": ["writer-target", "helper-target"]},
        },
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
    seed_agent_targets(&db, &turn).await;
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
    assert_eq!(agent_new_targets(before), None, "{:?}", offered(before));
    for body in [after, last] {
        assert!(
            offered(body).iter().any(|name| name == create_goal)
                && !offered(body).iter().any(|name| name == sessions),
            "later turns have the rebound toolset: {:?}",
            offered(body)
        );
        assert_eq!(agent_new_targets(body), Some(json!(["writer"])));
    }
    let next_turn = &receipt(after, "call-grant")["next_turn"];
    for name in [create_goal, "agent_new", "agent_new agent=writer"] {
        assert!(
            next_turn["added"]
                .as_array()
                .unwrap()
                .iter()
                .any(|added| added == name),
            "{next_turn}"
        );
    }
    assert_eq!(
        next_turn["waiting"][0]["name"], "agent_new agent=helper",
        "{next_turn}"
    );

    runtime.shutdown().await;
}
