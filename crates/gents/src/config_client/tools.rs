use anyhow::{Context, Result};

use crate::collection::Collection;
use crate::document_config::Tools;

use super::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};

/// Replace a complete Tools document through the common, owner-scoped writer.
/// Omitted groups revoke their capabilities. Sparse edits belong to the patch API.
pub async fn write_tools_document(access: &ConfigAccess, tools: &Tools) -> Result<String> {
    tools.validate()?;
    access
        .transact("config.tools.write", |txn| {
            Box::pin(async move {
                let value = serde_json::to_value(tools)?;
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: Collection::Tools,
                    add: value.clone(),
                    update: value,
                }])?;
                super::apply_desired_state_plan(txn, &plan).await?;
                super::desired_state::read_record(
                    txn,
                    Collection::Tools,
                    &tools.agent_did,
                    &tools.tools_id,
                )
                .await?
                .map(|(doc_id, _)| doc_id)
                .context("replaced Tools document missing")
            })
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use defra_node::EmbeddedNode;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn replacement_revokes_omitted_groups_without_changing_other_owner() -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::Local(node);
        let mut tools: Tools = serde_json::from_value(json!({
            "agent_did":"did:key:owner", "tools_id":"same",
            "host":{"files":{"mode":"ReadOnly"}},
            "built_ins":{"enable_goal_tools":true}, "tags":["old"]
        }))?;
        let first_id = write_tools_document(&access, &tools).await?;
        let mut foreign = tools.clone();
        foreign.agent_did = "did:key:other".into();
        assert_ne!(write_tools_document(&access, &foreign).await?, first_id);
        tools.host = None;
        tools.built_ins = None;
        tools.tags.clear();
        assert_eq!(write_tools_document(&access, &tools).await?, first_id);
        access
            .transact("test.tools.read", |txn| {
                let tools = &tools;
                let foreign = &foreign;
                Box::pin(async move {
                    for (owner, expected) in [("did:key:owner", tools), ("did:key:other", foreign)]
                    {
                        let (_, value) = super::super::desired_state::read_record(
                            txn,
                            Collection::Tools,
                            owner,
                            "same",
                        )
                        .await?
                        .context("missing tools")?;
                        assert_eq!(serde_json::from_value::<Tools>(value)?, *expected);
                    }
                    Ok(())
                })
            })
            .await
    }
}
