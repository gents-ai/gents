//! Hermetic (always-run) fence for the durable workflow barrier path (#378).
//!
//! `barrier-1` wires `fan_out_and_synthesize` synthesis behind
//! `workflow_barrier_projection_legal` evaluated over the DURABLE `AgentToolCall`
//! bridge rows of a `workflow_group_id`. This test seeds those rows directly and
//! drives the *exact* engine surface — `load_workflow_group_bridges` +
//! `fan_out_barrier_satisfied` — so a regression that deleted the durable re-read,
//! inverted the role filter, or dropped a NULL-state row is caught under the
//! default `cargo test -p gents` gate (not only the env-gated live e2e).

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

    // Group A: 3 fan-out children, one still running, + a synthesis bridge.
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

    // Group B: 3 fan-out children, all terminal (incl. a failure terminal).
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

    // The durable query is group-scoped: each group sees only its own rows.
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

    // Barrier is enforced from the durable rows: group-a has a running child, so
    // synthesis is refused; group-b is all-terminal, so it is admitted.
    assert!(
        !fan_out_barrier_satisfied(&rows_a, 3),
        "synthesis must be refused while a fan-out bridge is still running"
    );
    assert!(
        fan_out_barrier_satisfied(&rows_b, 3),
        "synthesis must be admitted once every fan-out bridge is terminal"
    );

    // Fail-closed: if the engine expected more fan-out bridges than the durable
    // rows show (a partial/lost cohort), the barrier refuses synthesis.
    assert!(
        !fan_out_barrier_satisfied(&rows_b, 4),
        "a row-count shortfall must refuse synthesis, never pass open"
    );
}
