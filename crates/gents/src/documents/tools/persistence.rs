use anyhow::{Context, Result};

use super::Tools;
use crate::store::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
use crate::Collection;

/// Replace a complete Tools document; omitted groups revoke their capabilities.
pub async fn write_tools_document(access: &ConfigAccess, tools: &Tools) -> Result<String> {
    tools.validate()?;
    access
        .transact_write("config.tools.write", |txn| {
            Box::pin(async move {
                let value = serde_json::to_value(tools)?;
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: Collection::Tools,
                    add: value.clone(),
                    update: value,
                }])?;
                crate::documents::apply_desired_state_plan(txn, &plan).await?;
                crate::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    Collection::Tools,
                    &tools.node_did,
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

    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn replacement_revokes_omitted_groups_without_changing_other_owner() -> Result<()> {
        let node = Arc::new(crate::test_support::node_builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(
                node.node_identity_did()
                    .expect("fixture node has an identity"),
            )
            .unwrap(),
        );
        crate::test_support::bootstrap_signed_test_node(&access, "did:key:owner").await;
        crate::test_support::bootstrap_signed_test_node(&access, "did:key:other").await;
        let mut tools: Tools = serde_json::from_value(json!({
            "node_did":"did:key:owner", "tools_id":"same",
            "host":{"files":{"mode":"ReadOnly"}},
            "built_ins":{"enable_goal_tools":true}, "tags":["old"]
        }))?;
        let first_id = write_tools_document(&access, &tools).await?;
        let mut foreign = tools.clone();
        foreign.node_did = "did:key:other".into();
        assert_ne!(write_tools_document(&access, &foreign).await?, first_id);
        tools.host = None;
        tools.built_ins = None;
        tools.tags.clear();
        assert_eq!(write_tools_document(&access, &tools).await?, first_id);
        access
            .transact_write("test.tools.read", |txn| {
                let tools = &tools;
                let foreign = &foreign;
                Box::pin(async move {
                    for (owner, expected) in [("did:key:owner", tools), ("did:key:other", foreign)]
                    {
                        let (_, value) =
                            crate::store::config_client::read_desired_state_record_in_txn(
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
