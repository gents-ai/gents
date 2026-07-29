use std::sync::Arc;
use std::time::Duration;

use gents::goal::{load_canonical_goal, set_goal, GoalStatus, UPDATE_GOAL_TOOL_NAME};
use gents::graphql::escape_graphql_string;
use gents::AgentIdentity;
use serde::Deserialize;

use super::steward_loop_live::{
    bind_d4f_backend, boot_d4f_agent, wait_for_assistant_answer, wait_for_request_terminal,
};
use crate::support::fixtures::test_identity;
use crate::support::interrupt::create_runtime_request;
use crate::support::{first_optional_row, test_db};

#[derive(Debug, Deserialize)]
struct GoalChildRow {
    request_id: String,
    session_id: String,
}

async fn wait_for_goal_child(
    node: &gents::defra_node::EmbeddedNode,
    goal_id: &str,
    timeout: Duration,
) -> GoalChildRow {
    let deadline = tokio::time::Instant::now() + timeout;
    let goal_id = escape_graphql_string(goal_id);
    loop {
        let response = node
            .execute(&format!(
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
        assert!(
            !response.has_errors(),
            "query goal child: {:?}",
            response.errors
        );
        if let Some(row) = first_optional_row::<GoalChildRow>(&response, "AgentRequest") {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for automatic goal continuation"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: set GENTS_D4F_LIVE=1 and pass --ignored"]
async fn durable_goal_continues_with_real_inference_until_model_completes() {
    assert!(
        std::env::var("GENTS_D4F_LIVE").as_deref() == Ok("1"),
        "set GENTS_D4F_LIVE=1 and pass --ignored to run the durable-goal live qualification"
    );

    let db = test_db("durable-goal-real-inference").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("durable-goal-real-inference"));
    let (agent_did, behavior_id) = bind_d4f_backend(db.node.as_ref(), identity.as_ref()).await;
    let agent = boot_d4f_agent(&db, identity)
        .await
        .expect("boot real-inference goal agent");
    let session_id = "session-durable-goal-real";
    let goal = set_goal(
        db.node.as_ref(),
        &agent_did,
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
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        initial_request_id,
        session_id,
        "This is the initial human turn, not a durable-controller continuation. Do not call update_goal on this turn. Reply with exactly INITIAL_GOAL_PROGRESS.",
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(
            db.node.as_ref(),
            initial_request_id,
            Duration::from_secs(120)
        )
        .await,
        "completed"
    );
    assert!(!wait_for_assistant_answer(
        db.node.as_ref(),
        initial_request_id,
        Duration::from_secs(30)
    )
    .await
    .trim()
    .is_empty());

    let child = wait_for_goal_child(db.node.as_ref(), &goal.goal_id, Duration::from_secs(30)).await;
    assert_eq!(child.session_id, session_id);
    assert_eq!(
        wait_for_request_terminal(
            db.node.as_ref(),
            &child.request_id,
            Duration::from_secs(120)
        )
        .await,
        "completed"
    );
    let answer =
        wait_for_assistant_answer(db.node.as_ref(), &child.request_id, Duration::from_secs(30))
            .await;
    assert!(
        answer.contains("DURABLE_GOAL_COMPLETE"),
        "real continuation response omitted completion marker: {answer:?}"
    );

    let persisted = load_canonical_goal(db.node.as_ref(), &agent_did, session_id)
        .await
        .expect("load durable goal")
        .expect("durable goal exists");
    assert_eq!(persisted.parsed_status(), Some(GoalStatus::Complete));

    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{ call_state: {{ _eq: "completed" }}, agent_did: {{ _eq: "{}" }} }}
            ) {{ call_id }}
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "{}" }} }}
            ) {{ lifecycle_state }}
        }}"#,
        escape_graphql_string(&agent_did),
        escape_graphql_string(session_id),
        UPDATE_GOAL_TOOL_NAME,
    );
    let response = db.node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "query live evidence: {:?}",
        response.errors
    );
    let inference_count = response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or_default();
    assert!(
        inference_count >= 2,
        "expected initial and automatic real inference calls, got {inference_count}"
    );
    let tool_count = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or_default();
    assert_eq!(
        tool_count, 1,
        "completion must come from the model goal tool"
    );
    agent.shutdown().await;
}
