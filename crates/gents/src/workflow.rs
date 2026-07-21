//! Workflow-orchestration projection helpers.

use serde::Deserialize;

use crate::defra_node::EmbeddedNode;
use crate::graphql::escape_graphql_string;

pub(crate) const FAN_OUT_AND_SYNTHESIZE_TOOL_NAME: &str = "fan_out_and_synthesize";
pub(crate) const WORKFLOW_ROLE_FAN_OUT_CHILD: &str = "fan_out_child";
pub(crate) const WORKFLOW_ROLE_SYNTHESIS: &str = "synthesis";

/// Fan-out width cap (D6: `1 <= N <= maxBackgroundedPerParent`). This is the
/// SAME quantity as the runtime backgrounded-per-parent ceiling, because fan-out
/// children are background bridges counted against that cap — derived from it so
/// the two cannot silently drift.
pub(crate) const MAX_FAN_OUT_TASKS: usize =
    crate::hook::persistence::MAX_BACKGROUNDED_TOOLS_PER_PARENT;

/// Persisted bridge states that count as terminal for workflow barriers.
pub const WORKFLOW_TERMINAL_TOOL_STATES: &[&str] =
    &["completed", "failed", "timedOut", "cancelled"];

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct FanOutAndSynthesizeArgs {
    #[serde(alias = "fan_out_target")]
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub synthesis_target: Option<String>,
    #[serde(default)]
    pub synthesis_prompt: String,
    #[serde(default)]
    pub tasks: Vec<FanOutTask>,
}

impl FanOutAndSynthesizeArgs {
    pub(crate) fn fan_out_width(&self) -> usize {
        self.tasks.len()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct FanOutTask {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    pub prompt: String,
}

/// Projection predicate for `fan_out_and_synthesize` barrier legality.
///
/// A group is legal when it is non-empty and, if synthesis exists, every
/// fan-out bridge is terminal in the parent-visible `AgentToolCall` lifecycle
/// vocabulary. Without synthesis, the group may still contain running children.
pub fn workflow_barrier_projection_legal<'a>(
    group_states: impl IntoIterator<Item = &'a str>,
    synthesis_present: bool,
) -> bool {
    let states = group_states.into_iter().collect::<Vec<_>>();
    if states.is_empty() {
        return false;
    }
    !synthesis_present
        || states
            .iter()
            .all(|state| WORKFLOW_TERMINAL_TOOL_STATES.contains(state))
}

/// A persisted workflow bridge row, read back from `AgentToolCall` by
/// `workflow_group_id`. This is the durable projection the barrier is proven
/// over and the source for idempotent adoption on a parent reclaim mid-barrier.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowBridgeRow {
    pub tool_call_id: String,
    #[serde(default)]
    pub child_request_id: Option<String>,
    #[serde(default)]
    pub workflow_role: Option<String>,
    #[serde(default)]
    pub lifecycle_state: Option<String>,
}

impl WorkflowBridgeRow {
    pub fn is_role(&self, role: &str) -> bool {
        self.workflow_role.as_deref() == Some(role)
    }
}

/// Load every persisted bridge row in one workflow group (the durable barrier
/// projection surface). Ordered by `started_at` so adopted fan-out indices are
/// stable across a reclaim.
pub async fn load_workflow_group_bridges(
    node: &EmbeddedNode,
    session_id: &str,
    workflow_group_id: &str,
) -> anyhow::Result<Vec<WorkflowBridgeRow>> {
    let escaped_session = escape_graphql_string(session_id);
    let escaped_group = escape_graphql_string(workflow_group_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{escaped_session}" }},
                    workflow_group_id: {{ _eq: "{escaped_group}" }}
                }},
                order: {{ started_at: ASC }}
            ) {{
                tool_call_id
                child_request_id
                workflow_role
                lifecycle_state
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "loading workflow group bridges for group {workflow_group_id} failed: {:?}",
            response.errors
        );
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default())
}

/// Decide whether the fan-out barrier is satisfied over the DURABLE bridge rows
/// of one workflow group: synthesis may proceed only when exactly
/// `expected_fan_out` fan-out-child bridges are present and every one is terminal
/// in the persisted lifecycle vocabulary. Fail-CLOSED: a row with a missing
/// `lifecycle_state` counts as non-terminal (mapped to `""`, not dropped), and a
/// row-count mismatch refuses synthesis — so a NULL state or an unexpected/extra
/// row can never let the barrier pass open. This is the exact gate the engine
/// runs before spawning synthesis; it is pure over the rows so it is hermetically
/// testable without live inference.
pub fn fan_out_barrier_satisfied(rows: &[WorkflowBridgeRow], expected_fan_out: usize) -> bool {
    let fan_out_states: Vec<&str> = rows
        .iter()
        .filter(|row| row.is_role(WORKFLOW_ROLE_FAN_OUT_CHILD))
        .map(|row| row.lifecycle_state.as_deref().unwrap_or(""))
        .collect();
    fan_out_states.len() == expected_fan_out
        && workflow_barrier_projection_legal(fan_out_states.iter().copied(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(role: &str, state: Option<&str>) -> WorkflowBridgeRow {
        WorkflowBridgeRow {
            tool_call_id: "tc".to_string(),
            child_request_id: Some("cr".to_string()),
            workflow_role: Some(role.to_string()),
            lifecycle_state: state.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn barrier_satisfied_only_when_all_fan_out_terminal_and_count_matches() {
        let fan = WORKFLOW_ROLE_FAN_OUT_CHILD;
        let syn = WORKFLOW_ROLE_SYNTHESIS;

        // All fan-out terminal (incl. a failure terminal) and count matches → admit.
        assert!(fan_out_barrier_satisfied(
            &[
                row(fan, Some("completed")),
                row(fan, Some("failed")),
                row(fan, Some("cancelled")),
            ],
            3
        ));
        // A `timedOut` sibling is terminal too.
        assert!(fan_out_barrier_satisfied(
            &[row(fan, Some("completed")), row(fan, Some("timedOut"))],
            2
        ));
        // A still-running sibling holds the barrier closed.
        assert!(!fan_out_barrier_satisfied(
            &[row(fan, Some("completed")), row(fan, Some("running"))],
            2
        ));
        // Fail-CLOSED: a NULL lifecycle_state row is non-terminal, not dropped.
        assert!(!fan_out_barrier_satisfied(
            &[row(fan, Some("completed")), row(fan, None)],
            2
        ));
        // Fail-CLOSED on count: fewer durable fan-out rows than expected.
        assert!(!fan_out_barrier_satisfied(
            &[row(fan, Some("completed"))],
            3
        ));
        // Fail-CLOSED on count: an unexpected extra fan-out row.
        assert!(!fan_out_barrier_satisfied(
            &[
                row(fan, Some("completed")),
                row(fan, Some("completed")),
                row(fan, Some("completed")),
            ],
            2
        ));
        // The synthesis bridge row is not counted as a fan-out child.
        assert!(fan_out_barrier_satisfied(
            &[
                row(fan, Some("completed")),
                row(fan, Some("completed")),
                row(syn, Some("running")),
            ],
            2
        ));
        // Empty group never admits synthesis (vacuous-barrier guard).
        assert!(!fan_out_barrier_satisfied(&[], 0));
    }
}
