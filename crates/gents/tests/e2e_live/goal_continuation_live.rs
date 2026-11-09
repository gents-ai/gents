use std::time::Duration;

use gents::goal::{load_canonical_goal_in_txn, set_goal, GoalStatus, UPDATE_GOAL_TOOL_NAME};
use gents::store::graphql::escape_graphql_string;
use serde::Deserialize;

use crate::support::agent_requests::canonical_now;
use crate::support::builder_runtime::submit_request;
use crate::support::live_inference::{
    bind_target, boot_live_agent, live_target, wait_for_assistant_answer, wait_for_request_terminal,
};
use crate::support::snapshots::SnapshotReader;
use crate::support::test_db;

#[derive(Debug, Deserialize)]
struct GoalChildRow {
    request_id: String,
    session_id: String,
}

async fn wait_for_goal_child<R>(reader: &R, goal_id: &str, timeout: Duration) -> GoalChildRow
where
    R: SnapshotReader,
{
    let deadline = tokio::time::Instant::now() + timeout;
    let goal_id = escape_graphql_string(goal_id);
    loop {
        let data = reader
            .read_data(&format!(
                r#"{{
                    AgentRequest(
                        filter: {{
                            caused_by_trigger_id: {{ _eq: "{goal_id}" }},
                            caused_by_trigger_kind: {{ _eq: "goal" }}
                        }},
                        order: [{{ created_at: ASC }}],
                        limit: 1
                    ) {{ request_id session_id }}
                }}"#
            ))
            .await;
        if let Some(row) = data["AgentRequest"]
            .as_array()
            .and_then(|rows| rows.first())
        {
            return serde_json::from_value(row.clone()).expect("decode goal child");
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for automatic goal continuation"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: set GENTS_EVAL_TARGET and pass --ignored"]
async fn durable_goal_continues_with_real_inference_until_model_completes() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,gents=info")),
        )
        .with_test_writer()
        .try_init();
    let target = live_target();

    let db = test_db("durable-goal-real-inference").await;
    let actors = bind_target(&db, &target).await;
    let node_did = actors.node_did().to_owned();
    let reader = &actors.fixture.access;
    let agent = boot_live_agent(&db, db.node_identity.clone())
        .await
        .expect("boot real-inference goal agent");
    let session_id = "session-durable-goal-real";
    // The Goal belongs to an existing session and is set by its working Agent.
    actors
        .create_session_with(serde_json::json!({
            "node_did": node_did,
            "session_id": session_id,
            "requester_did": actors.agent_did(),
            "agent_did": actors.agent_did(),
            "created_at": canonical_now(),
            "relationships_pending": false,
            "title": gents_protocol::session::SessionTitle {
                text: "generated-title".into(),
                source: gents_protocol::session::SessionTitleSource::Generated,
            },
        }))
        .await;
    let goal = set_goal(
        &db.node,
        actors.agent().as_ref(),
        &node_did,
        session_id,
        Some(
            "When—and only when—you receive a message explicitly stating that you are running under the durable goal controller, call update_goal with status complete and reason 'real inference continuation verified', then reply with the exact marker DURABLE_GOAL_COMPLETE.",
        ),
        Some(GoalStatus::Active),
        Some(Some(20_000)),
    )
    .await
    .expect("create durable goal");

    let initial_request_id = "request-durable-goal-initial";
    submit_request(
        &actors,
        initial_request_id,
        session_id,
        "interactive",
        "This is the initial human turn, not a durable-controller continuation. Do not call update_goal on this turn. Reply with exactly INITIAL_GOAL_PROGRESS.",
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(reader, initial_request_id, Duration::from_secs(120)).await,
        "completed"
    );
    assert!(
        !wait_for_assistant_answer(reader, initial_request_id, Duration::from_secs(30))
            .await
            .trim()
            .is_empty()
    );

    let child = wait_for_goal_child(reader, &goal.goal_id, Duration::from_secs(30)).await;
    assert_eq!(child.session_id, session_id);
    assert_eq!(
        wait_for_request_terminal(reader, &child.request_id, Duration::from_secs(120)).await,
        "completed"
    );
    let answer =
        wait_for_assistant_answer(reader, &child.request_id, Duration::from_secs(30)).await;
    assert!(
        answer.contains("DURABLE_GOAL_COMPLETE"),
        "real continuation response omitted completion marker: {answer:?}"
    );

    let persisted = reader
        .transact_readonly("test.live_goal.load", |txn| {
            let node_did = &node_did;
            Box::pin(async move { load_canonical_goal_in_txn(txn, node_did, session_id).await })
        })
        .await
        .expect("load durable goal")
        .expect("durable goal exists");
    assert_eq!(persisted.parsed_status(), Some(GoalStatus::Complete));

    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{ call_state: {{ _eq: "completed" }}, node_did: {{ _eq: "{}" }} }}
            ) {{ call_id }}
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "{}" }} }}
            ) {{ lifecycle_state }}
        }}"#,
        escape_graphql_string(&node_did),
        escape_graphql_string(session_id),
        UPDATE_GOAL_TOOL_NAME,
    );
    let data = reader.read_data(&query).await;
    let inference_count = data
        .get("InferenceCall")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or_default();
    assert!(
        inference_count >= 2,
        "expected initial and automatic real inference calls, got {inference_count}"
    );
    let tool_count = data
        .get("AgentToolCall")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or_default();
    assert_eq!(
        tool_count, 1,
        "completion must come from the model goal tool"
    );
    agent.shutdown().await;
}
