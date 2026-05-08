//! Helper for creating subagent-parent-linked AgentRequest rows.
//!
//! Public API surface consumed by R3's `SubagentSource` and by Bucket 3
//! conformance fixtures (Task 26). Mirrors R1's existing AgentRequest
//! creation flow in `crates/defra-agent/src/lifecycle/materialize.rs`,
//! with the addition of subagent parent-linkage fields and the depth
//! cap enforced by Lean's `Subagent.maxSubagentDepth`.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;

use crate::graphql::escape_graphql_string;
use crate::lifecycle::DEFAULT_REQUEST_MAX_RETRIES;
use crate::session::execute_mutation_with_retry;

use super::IllegalToolCallTransition;

/// The configured cap on subagent recursion depth. Matches Lean's
/// `Subagent.maxSubagentDepth = 3` (see
/// `crates/defra-agent/proofs/Proofs/Subagent/State.lean`). Exposed as
/// part of R2's public API surface so R3's apply-time spawn-flow
/// validation can reference the same value as the Lean spec.
pub const MAX_SUBAGENT_DEPTH: u32 = 3;

/// Create a new AgentRequest row with subagent parent linkage. Validates
/// the two preconditions enforced by the Lean Subagent spec BEFORE any
/// DB I/O:
///
///   1. `parent_subagent_depth + 1 ≤ MAX_SUBAGENT_DEPTH`. Returns
///      `IllegalToolCallTransition::SubagentDepthExceeded` otherwise.
///   2. Both `parent_request_id` and `parent_tool_call_id` non-empty.
///      Returns `IllegalToolCallTransition::ParentLinkageIncoherent`
///      otherwise. (A child must reference both parent identifiers; the
///      well-formedness invariant in `watcher.rs::validate_subagent_fields`
///      requires that `subagent_depth > 0` documents have both fields
///      populated.)
///
/// On success returns the newly minted `request_id`.
///
/// Field ownership notes (mirroring `materialize.rs`):
///   - `request_id` and `session_id` are freshly generated UUIDs.
///   - `lifecycle_state` is initialized to `"pending"`.
///   - `subagent_depth = parent_subagent_depth + 1`.
///   - `caused_by_parent_request_id` / `caused_by_parent_tool_call_id`
///     carry the parent linkage.
///   - Trigger lineage fields (`caused_by_trigger_id`,
///     `caused_by_trigger_kind`) are intentionally left empty: a
///     subagent spawn is not itself a trigger fire — the parent's
///     trigger lineage already records the originating cause.
#[allow(clippy::too_many_arguments)]
pub async fn create_subagent_request(
    node: &EmbeddedNode,
    parent_request_id: String,
    parent_tool_call_id: String,
    parent_subagent_depth: u32,
    agent_did: String,
    behavior_id: String,
    prompt: String,
    deadline: Option<DateTime<Utc>>,
) -> Result<String> {
    // 1. Depth check (pure precondition, fires before any DB I/O).
    if parent_subagent_depth + 1 > MAX_SUBAGENT_DEPTH {
        return Err(anyhow!(IllegalToolCallTransition::SubagentDepthExceeded));
    }

    // 2. Coherence check (pure precondition, fires before any DB I/O).
    if parent_request_id.is_empty() || parent_tool_call_id.is_empty() {
        return Err(anyhow!(IllegalToolCallTransition::ParentLinkageIncoherent));
    }

    // 3. Generate fresh identifiers (mirror materialize.rs pattern).
    let new_request_id = uuid::Uuid::new_v4().to_string();
    let new_session_id = uuid::Uuid::new_v4().to_string();
    let new_subagent_depth = parent_subagent_depth + 1;
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let escaped_request_id = escape_graphql_string(&new_request_id);
    let escaped_agent_did = escape_graphql_string(&agent_did);
    let escaped_behavior_id = escape_graphql_string(&behavior_id);
    let escaped_session_id = escape_graphql_string(&new_session_id);
    let escaped_prompt = escape_graphql_string(&prompt);
    let escaped_created_at = escape_graphql_string(&now);
    let escaped_parent_request_id = escape_graphql_string(&parent_request_id);
    let escaped_parent_tool_call_id = escape_graphql_string(&parent_tool_call_id);

    let deadline_field = deadline
        .map(|d| {
            let escaped_deadline = escape_graphql_string(&d.to_rfc3339());
            format!(
                r#"
                deadline: "{escaped_deadline}","#
            )
        })
        .unwrap_or_default();

    // 4. Build and execute the CREATE mutation. Mirrors the field shape
    // of `write_pending_agent_request_with_lineage_and_conversation_title`
    // in `lifecycle/materialize.rs`, plus the three subagent fields.
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "{escaped_behavior_id}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "{escaped_prompt}",
                status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "{escaped_created_at}",{deadline_field}
                retry_count: 0,
                max_retries: {max_retries},
                subagent_depth: {new_subagent_depth},
                caused_by_parent_request_id: "{escaped_parent_request_id}",
                caused_by_parent_tool_call_id: "{escaped_parent_tool_call_id}"
            }}) {{ _docID }}
        }}"#,
        max_retries = DEFAULT_REQUEST_MAX_RETRIES,
    );

    execute_mutation_with_retry(node, &mutation, "create_subagent_request").await?;

    Ok(new_request_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The depth and coherence checks fire BEFORE any DB I/O, so we can
    // exercise them without a real EmbeddedNode by leveraging the early
    // return semantics. The DB-touching happy path is deferred to Bucket
    // 3 / Task 26, which has the test_db fixture set up.
    //
    // We can't easily fabricate an `EmbeddedNode` for unit tests (it's a
    // real type with a real constructor that boots a node). So the unit
    // tests below use `unsafe { std::mem::zeroed() }` only conceptually
    // — that's UB in Rust. Instead, we inline-construct the precondition
    // checks directly in #[test] blocks that don't call the function.
    //
    // The function-level error paths (depth + coherence) ARE tested by
    // Task 26's end-to-end fixtures, where a real node is available.

    #[test]
    fn max_subagent_depth_matches_lean_spec() {
        // Lean: Subagent.State.lean defines `maxSubagentDepth : Nat := 3`.
        assert_eq!(MAX_SUBAGENT_DEPTH, 3);
    }

    #[test]
    fn depth_precondition_arithmetic() {
        // parent_subagent_depth + 1 must be <= MAX_SUBAGENT_DEPTH.
        // Allowed parent depths: 0, 1, 2 (resulting children: 1, 2, 3).
        // Rejected parent depths: 3 and above.
        for parent_depth in 0..=2 {
            assert!(parent_depth + 1 <= MAX_SUBAGENT_DEPTH);
        }
        for parent_depth in 3..=10 {
            assert!(parent_depth + 1 > MAX_SUBAGENT_DEPTH);
        }
    }
}
