//! Single-node aspirational e2e for `fan_out_and_synthesize` (issue #378, cut 0).
//!
//! NORTH-STAR SKELETON. The `fan_out_and_synthesize` orchestration primitive
//! does not exist yet (it lands in cut 1). This file's CUT-0 GATE is purely
//! "compiles + appears in `--list` + skipped by default" — it COMPILES with the
//! package and is skipped in normal runs (`#[ignore]` + an early-return unless
//! `DEFRA_AGENT_LIVE_WORKFLOW=1`). It deliberately queries only `AgentToolCall`
//! fields that exist today (it never selects `workflow_group_id` /
//! `workflow_role` before cut 1 adds those columns), so it cannot raise a
//! "field does not exist" GraphQL error.
//!
//! Cut 0 does NOT configure inference or submit a request, so an explicit
//! `DEFRA_AGENT_LIVE_WORKFLOW=1` run here fails only trivially (empty DB). The
//! MEANINGFUL first wall — "no `fan_out_and_synthesize` tool call observed"
//! *after a real orchestrator request ran* — is realized in cut 1 (plan Task
//! 1.5 Step 1), which boots + configures + submits + waits, then asserts and
//! extends to the full barrier projection.
//!
//! Target shape (cut 1 fills in the marked setup and extends the assertion to
//! the full barrier projection):
//!   1. one node + an orchestrator behavior whose ToolSelection has
//!      orchestration_enabled = subagent_spawn_enabled = subagent_background_enabled = true;
//!   2. a researcher behavior (fan-out target) + a synthesizer behavior;
//!   3. drive ONE `fan_out_and_synthesize` call over N=3 sub-questions;
//!   4. assert from durable rows: exactly 3 `fan_out_child` bridges in group G
//!      all terminal, 1 `synthesis` bridge, and
//!      `synthesis.started_at >= max(fan_out_child.completed_at)`
//!      (barrier-completeness — see the design doc §6).
//!
//! To run:
//! ```bash
//! DEFRA_AGENT_LIVE_WORKFLOW=1 \
//!   cargo test -p defra-agent --test e2e_live \
//!   workflow_orchestration_live -- --ignored --nocapture
//! ```

use anyhow::Result;
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use serde::Deserialize;

use crate::support::{test_db, TestDb};

/// The orchestration tool the runtime will expose in cut 1. Referenced here by
/// string only — no Rust symbol dependency, so the skeleton compiles today.
const ORCH_TOOL: &str = "fan_out_and_synthesize";

fn live_enabled() -> bool {
    std::env::var("DEFRA_AGENT_LIVE_WORKFLOW").as_deref() == Ok("1")
}

/// A `fan_out_and_synthesize` bridge row, read over fields that exist today.
/// Cut 1 extends this with `workflow_group_id` / `workflow_role` once the
/// schema carries them (design D4).
#[derive(Debug, Deserialize)]
struct OrchToolCallRow {
    #[allow(dead_code)]
    tool_call_id: String,
    #[allow(dead_code)]
    lifecycle_state: Option<String>,
    #[allow(dead_code)]
    started_at: Option<String>,
    #[allow(dead_code)]
    completed_at: Option<String>,
    #[allow(dead_code)]
    child_request_id: Option<String>,
}

/// Cut-0 STAGED query: all `fan_out_and_synthesize` tool calls on a session,
/// selecting only existing columns. Returns every matching row so cut 1 can
/// layer the `workflow_group_id` / `workflow_role` barrier assertions on top.
async fn fetch_orchestration_tool_calls(
    node: &EmbeddedNode,
    session_id: &str,
) -> Vec<OrchToolCallRow> {
    let session = escape_graphql_string(session_id);
    let tool = escape_graphql_string(ORCH_TOOL);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{session}" }}, tool_name: {{ _eq: "{tool}" }} }},
                order: {{ started_at: ASC }}
            ) {{ tool_call_id lifecycle_state started_at completed_at child_request_id }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(!resp.has_errors(), "AgentToolCall query failed: {:?}", resp.errors);
    resp.data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|rows| rows.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| serde_json::from_value(row.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set DEFRA_AGENT_LIVE_WORKFLOW=1 and pass --ignored"]
async fn fan_out_and_synthesize_barrier_live() -> Result<()> {
    if !live_enabled() {
        eprintln!("DEFRA_AGENT_LIVE_WORKFLOW != 1; skipping workflow orchestration e2e");
        return Ok(());
    }

    // Boot a single node so the staged query runs against a real control plane.
    let db: TestDb = test_db("workflow-fanout-live").await;
    let session_id = "session-workflow-fanout-live";

    // ---- CUT 1 FILLS IN (the north-star setup) --------------------------------
    // - ensure principal + configure orchestrator/researcher/synthesizer behaviors
    //   (orchestration_enabled ∧ subagent_spawn_enabled ∧ subagent_background_enabled);
    // - create_runtime_request(... session_id ...) with a constrained prompt that
    //   elicits exactly one `fan_out_and_synthesize` over N=3 sub-questions;
    // - wait for the orchestrator request to terminalize.
    // Until then, no orchestration tool call is ever written — which is the wall.
    // ---------------------------------------------------------------------------

    // STAGED ASSERTION (cut-0 wall): the orchestration primitive is not built, so
    // no `fan_out_and_synthesize` tool call exists. Fails meaningfully on an
    // explicit run, naming cut 1's first affordance.
    let orch_calls = fetch_orchestration_tool_calls(db.node.as_ref(), session_id).await;
    assert!(
        !orch_calls.is_empty(),
        "no `{ORCH_TOOL}` tool call observed — the orchestration primitive is not built yet (cut 1). \
         Once cut 1 lands, extend this to the full barrier projection: 3 `fan_out_child` bridges in \
         one workflow_group_id all terminal, then exactly 1 `synthesis` bridge with \
         started_at >= max(fan_out_child.completed_at)."
    );

    Ok(())
}
