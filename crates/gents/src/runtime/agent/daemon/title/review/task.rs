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
