use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;

use crate::document_config::Task;
use crate::watcher::AgentRequest;

pub(super) struct Policy {
    definition: Task,
    pub(super) text: String,
}

fn eligible(task: &Task, node: &str, task_id: &str, agent: &str) -> bool {
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

pub(super) async fn load(
    node: &Arc<EmbeddedNode>,
    parent: &AgentRequest,
    task_id: &str,
) -> Result<Policy> {
    let definition = crate::session_message::load_configured_task(node, &parent.node_did, task_id)
        .await?
        .context("access review Task is not configured")?;
    anyhow::ensure!(
        eligible(&definition, &parent.node_did, task_id, &parent.agent_id),
        "access review requires an enabled policy Task for this agent without execution side effects"
    );
    let rendered = crate::session_message::render_task_body(
        &definition,
        &parent.node_did,
        &parent.agent_id,
        &parent.session_id,
        None,
    )
    .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        rendered.goal.is_none() && !rendered.content.trim().is_empty(),
        "access review Task did not render a policy"
    );
    Ok(Policy {
        definition,
        text: rendered.content,
    })
}

impl Policy {
    pub(super) async fn is_current(
        &self,
        node: &Arc<EmbeddedNode>,
        parent: &AgentRequest,
    ) -> Result<bool> {
        Ok(crate::session_message::load_configured_task(
            node,
            &parent.node_did,
            &self.definition.task_id,
        )
        .await?
        .as_ref()
            == Some(&self.definition))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Case {
        node: String,
        task_id: String,
        agent: String,
        document: Task,
        eligible: bool,
    }

    #[test]
    fn generated_task_policies_bind_the_configured_task_fields() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().access_review_task_cases;
        assert!(!cases.is_empty());
        for case in cases {
            let case: Case = serde_json::from_value(case.clone()).unwrap();
            assert_eq!(
                eligible(&case.document, &case.node, &case.task_id, &case.agent),
                case.eligible,
                "{}",
                serde_json::to_string(&case.document).unwrap()
            );
        }
    }
}
