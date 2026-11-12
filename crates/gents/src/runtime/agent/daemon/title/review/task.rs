use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;

use gents_documents::Task;
use gents_protocol::row::AgentRequest;

pub(super) struct Policy {
    definition: Task,
    pub(super) text: String,
}

pub(super) async fn load(
    node: &Arc<EmbeddedNode>,
    parent: &AgentRequest,
    selection: &gents_tools::plugin::approval::workspace::ReviewTaskSelection,
) -> Result<Policy> {
    let task_id = selection.task_id.as_str();
    let definition = gents_session::session::message::load_configured_task(
        &gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(&parent.agent_did)?,
        ),
        &parent.node_did,
        task_id,
    )
    .await?
    .context("access review Task is not configured")?;
    anyhow::ensure!(
        gents_tools::plugin::approval::workspace::review_task_matches_selection(
            &definition,
            selection,
            &parent.node_did,
            &parent.agent_did,
        ),
        "The access review policy changed. Select Approve for me again to use the updated policy."
    );
    let text = gents_tools::plugin::approval::workspace::review_task_policy(
        &definition,
        &parent.node_did,
        task_id,
        &parent.agent_did,
        &parent.session_id,
    )?;
    Ok(Policy { definition, text })
}

impl Policy {
    pub(super) async fn is_current(
        &self,
        node: &Arc<EmbeddedNode>,
        parent: &AgentRequest,
    ) -> Result<bool> {
        Ok(gents_session::session::message::load_configured_task(
            &gents_store::config_client::ConfigAccess::local_as(
                node.clone(),
                identity::Did::new(&parent.agent_did)?,
            ),
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
    use gents_documents::TaskHook;

    #[test]
    fn generated_task_policies_bind_the_configured_task_fields() {
        let cases = &gents_lean_vocab::lean_contract_snapshot().access_review_task_cases;
        assert!(!cases.is_empty());
        for case in cases {
            let document = Task {
                node_did: case.document.node_did.clone(),
                task_id: case.document.task_id.clone(),
                display_name: None,
                description: None,
                agent_did: case.document.agent_did.clone(),
                prompt_template: case.document.prompt_template.clone(),
                emit_outcome: case.document.emit_outcome,
                outcome_readers: Vec::new(),
                goal_objective_template: case.document.goal_objective_template.clone(),
                goal_token_budget: case
                    .document
                    .goal_token_budget
                    .map(|budget| i64::try_from(budget).unwrap()),
                hooks: case
                    .document
                    .hooks
                    .iter()
                    .map(|hook| TaskHook {
                        hook_id: hook.hook_id.clone(),
                        phase: serde_json::from_value(serde_json::Value::String(
                            hook.phase.clone(),
                        ))
                        .unwrap(),
                        command: hook.command.clone(),
                        timeout_secs: None,
                        skip_exit_codes: Vec::new(),
                    })
                    .collect(),
                enabled: case.document.enabled,
                output_schema_ref: case.document.output_schema_ref.clone(),
                created_at: None,
                updated_at: None,
                tags: Vec::new(),
            };
            let mut approved = document.clone();
            approved.task_id = case.task_id.clone();
            approved.prompt_template = case.approved_prompt.clone();
            let selection =
                gents_tools::plugin::approval::workspace::ReviewTaskSelection::for_task(&approved);
            assert_eq!(
                gents_tools::plugin::approval::workspace::review_task_matches_selection(
                    &document,
                    &selection,
                    &case.node,
                    &case.agent,
                ),
                case.selected_eligible,
                "{}",
                serde_json::to_string(&document).unwrap(),
            );
            assert_eq!(
                gents_tools::plugin::approval::workspace::review_task_is_eligible(
                    &document,
                    &case.node,
                    &case.task_id,
                    &case.agent
                ),
                case.eligible,
                "{}",
                serde_json::to_string(&document).unwrap()
            );
        }
    }
}
