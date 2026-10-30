use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::documents::Task;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewTaskSelection {
    pub task_id: String,
    pub policy_digest: String,
}

impl ReviewTaskSelection {
    /// Display metadata does not change consent. Eligibility independently
    /// rejects execution effects; the fingerprint binds the selected template.
    pub fn for_task(task: &Task) -> Self {
        Self {
            task_id: task.task_id.clone(),
            policy_digest: format!("{:x}", Sha256::digest(task.prompt_template.as_bytes())),
        }
    }
}

pub fn review_task_matches_selection(
    task: &Task,
    selection: &ReviewTaskSelection,
    node: &str,
    agent: &str,
) -> bool {
    review_task_is_eligible(task, node, &selection.task_id, agent)
        && ReviewTaskSelection::for_task(task) == *selection
}

pub fn review_task_is_eligible(task: &Task, node: &str, task_id: &str, agent: &str) -> bool {
    let Task {
        node_did,
        task_id: configured_id,
        agent_id,
        prompt_template,
        enabled,
        hooks,
        emit_outcome,
        goal_objective_template,
        goal_token_budget,
        output_schema_ref,
        display_name: _,
        description: _,
        created_at: _,
        updated_at: _,
        tags: _,
    } = task;
    node_did == node
        && configured_id == task_id
        && agent_id == agent
        && !prompt_template.trim().is_empty()
        && *enabled
        && hooks.is_empty()
        && !*emit_outcome
        && goal_objective_template.is_none()
        && goal_token_budget.is_none()
        && output_schema_ref.is_none()
}

pub fn review_task_policy(
    task: &Task,
    node: &str,
    task_id: &str,
    agent: &str,
    session: &str,
) -> Result<String> {
    anyhow::ensure!(
        review_task_is_eligible(task, node, task_id, agent),
        "access review requires an enabled policy Task for this agent without execution side effects"
    );
    let rendered = crate::session::message::render_task_body(task, node, agent, session, None)
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        rendered.goal.is_none() && !rendered.content.trim().is_empty(),
        "access review Task did not render a policy"
    );
    Ok(rendered.content)
}

/// This policy is persisted as an ordinary editable Task. Installation never
/// replaces an existing Task, and a review grants only its exact bound action.
pub fn default_review_task(node: &str, agent: &str) -> Task {
    Task {
        node_did: node.to_owned(),
        task_id: format!("workspace-access-review:{agent}"),
        display_name: Some("Access review".to_owned()),
        description: None,
        agent_id: agent.to_owned(),
        prompt_template: include_str!("access_review.md").to_owned(),
        emit_outcome: false,
        goal_objective_template: None,
        goal_token_budget: None,
        hooks: Vec::new(),
        enabled: true,
        output_schema_ref: None,
        created_at: None,
        updated_at: None,
        tags: Vec::new(),
    }
}
