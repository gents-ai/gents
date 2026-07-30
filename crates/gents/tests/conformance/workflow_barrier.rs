use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::workflow::{fan_out_barrier_satisfied, load_workflow_group_bridges};

use crate::support::test_db;

#[allow(clippy::too_many_arguments)]
async fn seed_bridge(
    node: &EmbeddedNode,
    session_id: &str,
    seq: u32,
    tool_call_id: &str,
    workflow_group_id: &str,
    workflow_role: &str,
    lifecycle_state: &str,
) {
    let session = escape_graphql_string(session_id);
    let tcid = escape_graphql_string(tool_call_id);
    let group = escape_graphql_string(workflow_group_id);
    let role = escape_graphql_string(workflow_role);
    let state = escape_graphql_string(lifecycle_state);
    let key = format!("{session}:{tcid}");
    let now = "2026-06-18T00:00:00Z";
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{key}",
                session_id: "{session}",
                message_sequence: {seq},
                tool_name: "spawn_subagent",
                tool_call_id: "{tcid}",
                args: "{{}}",
                result: "",
                status: "called",
                lifecycle_state: "{state}",
                workflow_group_id: "{group}",
                workflow_role: "{role}",
                started_at: "{now}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(!resp.has_errors(), "seed_bridge failed: {:?}", resp.errors);
}

#[tokio::test]
async fn durable_barrier_gate_refuses_synthesis_until_all_fan_out_terminal() {
    let db = test_db("workflow-barrier-durable").await;
    let node = db.node.as_ref();
    let session = "sess-barrier";

    seed_bridge(
        node,
        session,
        1,
        "a-fan-0",
        "group-a",
        "fan_out_child",
        "completed",
    )
    .await;
    seed_bridge(
        node,
        session,
        2,
        "a-fan-1",
        "group-a",
        "fan_out_child",
        "completed",
    )
    .await;
    seed_bridge(
        node,
        session,
        3,
        "a-fan-2",
        "group-a",
        "fan_out_child",
        "running",
    )
    .await;
    seed_bridge(node, session, 4, "a-syn", "group-a", "synthesis", "running").await;

    seed_bridge(
        node,
        session,
        5,
        "b-fan-0",
        "group-b",
        "fan_out_child",
        "completed",
    )
    .await;
    seed_bridge(
        node,
        session,
        6,
        "b-fan-1",
        "group-b",
        "fan_out_child",
        "failed",
    )
    .await;
    seed_bridge(
        node,
        session,
        7,
        "b-fan-2",
        "group-b",
        "fan_out_child",
        "timedOut",
    )
    .await;

    let rows_a = load_workflow_group_bridges(node, session, "group-a")
        .await
        .expect("load group-a");
    assert_eq!(
        rows_a.len(),
        4,
        "group-a has 3 fan-out + 1 synthesis bridge"
    );
    let rows_b = load_workflow_group_bridges(node, session, "group-b")
        .await
        .expect("load group-b");
    assert_eq!(rows_b.len(), 3, "group-b has 3 fan-out bridges");

    assert!(
        !fan_out_barrier_satisfied(&rows_a, 3),
        "synthesis must be refused while a fan-out bridge is still running"
    );
    assert!(
        fan_out_barrier_satisfied(&rows_b, 3),
        "synthesis must be admitted once every fan-out bridge is terminal"
    );

    assert!(
        !fan_out_barrier_satisfied(&rows_b, 4),
        "a row-count shortfall must refuse synthesis, never pass open"
    );
}
