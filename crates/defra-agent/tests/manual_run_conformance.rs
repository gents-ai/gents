//! Task 18 — conformance tests for manual task runs.
//!
//! # Scope
//!
//! These tests lock down the externally-observable contract between the
//! shared manual-run helper (`defra_agent::write_manual_agent_request`) and
//! DefraDB. The helper is the single entry point both the CLI (`config task
//! run`) and the desktop "Run Now" button use; asserting its persistence
//! behavior here keeps both surfaces honest through one checkpoint.
//!
//! Covered:
//!
//! * `manual_run_materializes_agent_request_with_lineage` — a successful
//!   call persists an `AgentRequest` with the manual lineage tuple
//!   (`caused_by_trigger_id = null`, `caused_by_trigger_kind = "manual"`),
//!   `execution_origin = "interactive"`, and `lifecycle_state = "pending"`.
//! * `manual_run_renders_args_scope` — `args.*` template variables
//!   substitute into the rendered prompt.
//! * `manual_run_bypasses_serial_in_flight_check` — an in-flight manual
//!   `AgentRequest` does NOT prevent a second manual run from materializing
//!   (Manual concurrency is `Parallel` by construction).
//!
//! # Out of scope here — covered elsewhere
//!
//! * `ManualTriggerHandle::run_task_now` error cases (unknown task, disabled
//!   task via empty-snapshot): `ManualTriggerHandle` and
//!   `ActiveRuntimeSnapshot` are both `pub(crate)` in `defra-agent`, so they
//!   cannot be constructed from an integration-test crate. The equivalent
//!   contract is pinned in-crate by
//!   `src/trigger_engine/tests.rs::manual_source_run_task_now_rejects_unknown_task`
//!   and siblings (PR 3 Task 2). A disabled task drops out of
//!   `snapshot.active_tasks()` during resolve, so the same "not in the active
//!   snapshot" path fires — no separate case needed.
//! * End-to-end CLI coverage of the `config task run` command: pinned by
//!   `crates/defra-agent-cli/tests/cli_config_task_run.rs` (Task 9), which
//!   drives the CLI binary against an embedded node and asserts the same
//!   pending-lifecycle landing this file asserts at the helper surface.

use defra_agent::graphql::escape_graphql_string;
use defra_agent::write_manual_agent_request;
use serde_json::Value;

mod support;

use support::{test_db, AGENT_DID, AGENT_NAME};

/// Fetch the full row shape the helper writes so tests can inspect lineage,
/// execution origin, lifecycle state, and the rendered content without
/// reconstructing the filter in every case.
async fn fetch_manual_row(
    node: &defra_agent::defra_node::EmbeddedNode,
    doc_id: &str,
) -> Value {
    let escaped = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                content
                status
                lifecycle_state
                execution_origin
                caused_by_trigger_id
                caused_by_trigger_kind
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "fetch_manual_row query failed: {:?}",
        resp.errors
    );
    resp.data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("exactly one AgentRequest for the given doc_id")
}

/// Count how many `AgentRequest` rows carry the manual lineage tuple (null
/// trigger id, `"manual"` trigger kind). Used by the parallel-concurrency
/// case to confirm a second row really did land.
async fn count_manual_agent_requests(node: &defra_agent::defra_node::EmbeddedNode) -> usize {
    let query = r#"{
        AgentRequest(filter: { caused_by_trigger_kind: { _eq: "manual" } }) {
            _docID
        }
    }"#;
    let resp = node.execute(query).await;
    assert!(
        !resp.has_errors(),
        "count_manual_agent_requests query failed: {:?}",
        resp.errors
    );
    resp.data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .map(|rows| rows.len())
        .unwrap_or(0)
}

// -----------------------------------------------------------------------------
// Case 1: lineage tuple + pending lifecycle
// -----------------------------------------------------------------------------

/// A successful `write_manual_agent_request` call lands a pending
/// `AgentRequest` with the full manual lineage tuple. The helper is the
/// shared entry point for both the CLI (`config task run`) and desktop "Run
/// Now"; asserting the persistence shape here locks down the contract both
/// consumers inherit.
#[tokio::test]
async fn manual_run_materializes_agent_request_with_lineage() {
    let db = test_db("manual-run-lineage").await;

    let doc_id = write_manual_agent_request(
        db.node.as_ref(),
        AGENT_DID,
        AGENT_NAME,
        "task-manual-lineage",
        "manual body",
        serde_json::json!({}),
    )
    .await
    .expect("write_manual_agent_request should succeed on a fresh node");
    assert!(!doc_id.is_empty(), "helper must return a non-empty doc id");

    let row = fetch_manual_row(db.node.as_ref(), &doc_id).await;

    // Lineage tuple: trigger_id is null, trigger_kind is "manual".
    assert!(
        row["caused_by_trigger_id"].is_null(),
        "caused_by_trigger_id must be null for manual runs; got {:?}",
        row["caused_by_trigger_id"]
    );
    assert_eq!(
        row["caused_by_trigger_kind"].as_str(),
        Some("manual"),
        "caused_by_trigger_kind must be \"manual\""
    );
    // Execution origin: interactive (manual runs land on the
    // human-driven execution path).
    assert_eq!(
        row["execution_origin"].as_str(),
        Some("interactive"),
        "execution_origin must be \"interactive\" for manual runs"
    );
    // Lifecycle + status: pending. The CLI path writes pending rather than
    // claimed so the agent's normal intake loop picks the row up.
    assert_eq!(
        row["lifecycle_state"].as_str(),
        Some("pending"),
        "manual runs must land at lifecycle_state=pending"
    );
    assert_eq!(
        row["status"].as_str(),
        Some("pending"),
        "manual runs must land at status=pending"
    );
    assert_eq!(
        row["content"].as_str(),
        Some("manual body"),
        "rendered prompt template must land in content"
    );
}

// -----------------------------------------------------------------------------
// Case 2: args.* template scope
// -----------------------------------------------------------------------------

/// `args.*` template variables in the task's `prompt_template` substitute at
/// helper time against the caller-supplied `args` JSON. Pins the
/// manual-specific half of the trigger engine's template scope contract
/// (`event.*`, `doc.*`, `args.*`) for the out-of-engine helper path.
#[tokio::test]
async fn manual_run_renders_args_scope() {
    let db = test_db("manual-run-args-scope").await;

    let doc_id = write_manual_agent_request(
        db.node.as_ref(),
        AGENT_DID,
        AGENT_NAME,
        "task-args",
        "hi {{ args.name }}",
        serde_json::json!({"name": "Amy"}),
    )
    .await
    .expect("write_manual_agent_request should render args.* templates");

    let row = fetch_manual_row(db.node.as_ref(), &doc_id).await;
    assert_eq!(
        row["content"].as_str(),
        Some("hi Amy"),
        "args.* substitution must produce the rendered prompt in content"
    );
}

// -----------------------------------------------------------------------------
// Case 5: Parallel concurrency — manual runs do NOT queue behind each other
// -----------------------------------------------------------------------------

/// Manual runs are `Parallel` concurrency by construction (see
/// `ManualTriggerHandle::run_task_now`, which sets `ConcurrencyMode::Parallel`
/// on every manual `FireIntent`, and the shared helper, which does not
/// consult the in-flight gate). An operator pressing "Run Now" while a
/// previous manual run is still in-flight must therefore get a *second*
/// `AgentRequest` materialized — not a skip.
///
/// We seed an in-flight manual row directly via the helper (its
/// freshly-materialized row is the "in-flight" in-question), then call the
/// helper a second time and assert two rows now carry the manual lineage.
#[tokio::test]
async fn manual_run_bypasses_serial_in_flight_check() {
    let db = test_db("manual-run-parallel").await;

    // First manual run — still pending after this call, so it counts as
    // in-flight from the concurrency gate's perspective.
    let first = write_manual_agent_request(
        db.node.as_ref(),
        AGENT_DID,
        AGENT_NAME,
        "task-parallel",
        "one",
        serde_json::json!({}),
    )
    .await
    .expect("first manual run should materialize");
    assert_eq!(
        count_manual_agent_requests(db.node.as_ref()).await,
        1,
        "sanity: one manual AgentRequest after the first call"
    );

    // Second manual run — must NOT be gated by the first being in-flight.
    let second = write_manual_agent_request(
        db.node.as_ref(),
        AGENT_DID,
        AGENT_NAME,
        "task-parallel",
        "two",
        serde_json::json!({}),
    )
    .await
    .expect("second manual run must materialize even with a prior in-flight manual row");
    assert_ne!(
        first, second,
        "second manual run must produce a fresh doc_id, not alias the first"
    );
    assert_eq!(
        count_manual_agent_requests(db.node.as_ref()).await,
        2,
        "Parallel concurrency: two back-to-back manual runs must yield two AgentRequest rows"
    );

    // Both rows must individually carry the manual lineage tuple — i.e.
    // Parallel fan-out does not corrupt the lineage on either side.
    for (label, doc_id) in [("first", &first), ("second", &second)] {
        let row = fetch_manual_row(db.node.as_ref(), doc_id).await;
        assert!(
            row["caused_by_trigger_id"].is_null(),
            "{label}: trigger_id must remain null under Parallel fan-out"
        );
        assert_eq!(
            row["caused_by_trigger_kind"].as_str(),
            Some("manual"),
            "{label}: trigger_kind must remain \"manual\" under Parallel fan-out"
        );
    }
}
