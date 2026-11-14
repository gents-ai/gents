use anyhow::{Context, Result};

use super::Tools;
use crate::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_store::collection::Collection;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};

/// Replace a complete Tools document; omitted groups revoke their capabilities.
pub async fn write_tools_document(
    access: &ConfigAccess,
    tools: &Tools,
    policy: &dyn crate::inference_backend::AdvertisedProfilePolicy,
) -> Result<String> {
    access
        .transact_write("config.tools.write", |txn| {
            Box::pin(async move {
                write_tools_document_in_txn(txn, tools, policy).await?;
                crate::read_desired_state_record_in_txn(
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

#[derive(Debug, thiserror::Error)]
pub enum ToolsWriteError {
    #[error(transparent)]
    Invalid(anyhow::Error),
    #[error(transparent)]
    Publication(anyhow::Error),
}

pub async fn write_tools_document_in_txn(
    txn: &ConfigApplyTxn<'_>,
    tools: &Tools,
    policy: &dyn crate::inference_backend::AdvertisedProfilePolicy,
) -> std::result::Result<(), ToolsWriteError> {
    tools.validate().map_err(ToolsWriteError::Invalid)?;
    let value =
        serde_json::to_value(tools).map_err(|error| ToolsWriteError::Invalid(error.into()))?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Tools,
        add: value.clone(),
        update: value,
    }])
    .map_err(ToolsWriteError::Invalid)?;
    crate::apply_desired_state_plan(txn, &plan, policy)
        .await
        .map_err(ToolsWriteError::Publication)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn replacement_revokes_omitted_groups_without_changing_other_owner() -> Result<()> {
        let node = Arc::new(gents_store::test_support::node_builder().build().await?);
        gents_store::schema::ensure_runtime_schemas(&node).await?;
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
        let first_id =
            write_tools_document(&access, &tools, &crate::test_support::AdmitAllProfiles).await?;
        let mut foreign = tools.clone();
        foreign.node_did = "did:key:other".into();
        assert_ne!(
            write_tools_document(&access, &foreign, &crate::test_support::AdmitAllProfiles).await?,
            first_id
        );
        tools.host = None;
        tools.built_ins = None;
        tools.tags.clear();
        assert_eq!(
            write_tools_document(&access, &tools, &crate::test_support::AdmitAllProfiles).await?,
            first_id
        );
        access
            .transact_write("test.tools.read", |txn| {
                let tools = &tools;
                let foreign = &foreign;
                Box::pin(async move {
                    for (owner, expected) in [("did:key:owner", tools), ("did:key:other", foreign)]
                    {
                        let (_, value) = crate::read_desired_state_record_in_txn(
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
