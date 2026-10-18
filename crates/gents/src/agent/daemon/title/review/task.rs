use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;

use crate::document_config::Task;
use crate::watcher::AgentRequest;

pub(super) struct Policy {
    definition: Task,
    pub(super) text: String,
}

pub(super) async fn load(
    node: &Arc<EmbeddedNode>,
    parent: &AgentRequest,
    task_id: &str,
) -> Result<Policy> {
    let definition = crate::session_message::load_configured_task(node, &parent.node_did, task_id)
        .await?
        .context("access review Task is not configured")?;
    let text = crate::plugin::approval::workspace::review_task_policy(
        &definition,
        &parent.node_did,
        task_id,
        &parent.agent_id,
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
                crate::plugin::approval::workspace::review_task_is_eligible(
                    &case.document,
                    &case.node,
                    &case.task_id,
                    &case.agent
                ),
                case.eligible,
                "{}",
                serde_json::to_string(&case.document).unwrap()
            );
        }
    }
}
