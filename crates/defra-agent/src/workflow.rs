//! Workflow-orchestration projection helpers.

use serde::Deserialize;

pub(crate) const FAN_OUT_AND_SYNTHESIZE_TOOL_NAME: &str = "fan_out_and_synthesize";
pub(crate) const WORKFLOW_ROLE_FAN_OUT_CHILD: &str = "fan_out_child";
pub(crate) const WORKFLOW_ROLE_SYNTHESIS: &str = "synthesis";
pub(crate) const MAX_FAN_OUT_TASKS: usize = 8;

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
