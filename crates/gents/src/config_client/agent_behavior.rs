use anyhow::{Context, Result};

use crate::collection::Collection;
use crate::AgentDocument as Agent;

#[cfg(test)]
use super::ConfigApplyTxn;
use super::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};

/// Replace one complete canonical behavior. Omitted optional fields clear their
/// previous values; sparse edits belong to the explicit patch owner.
/// Desired-state application validates references in the mutation transaction.
pub async fn write_agent_document(access: &ConfigAccess, behavior: &Agent) -> Result<String> {
    access
        .transact("config.agent.write", |txn| {
            Box::pin(async move {
                let value = serde_json::to_value(behavior)?;
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: Collection::Agent,
                    add: value.clone(),
                    update: value,
                }])?;
                super::apply_desired_state_plan(txn, &plan).await?;
                super::desired_state::read_record(
                    txn,
                    Collection::Agent,
                    &behavior.node_did,
                    &behavior.agent_id,
                )
                .await?
                .map(|(doc_id, _)| doc_id)
                .context("replaced Agent missing")
            })
        })
        .await
}

#[cfg(test)]
async fn load_agent_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    agent_id: &str,
) -> Result<Option<Agent>> {
    super::read_desired_state_document_in_txn(txn, Collection::Agent, node_did, agent_id)
        .await?
        .map(serde_json::from_value)
        .transpose()
        .context("decoding canonical Agent")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_client::write_inference_profile_document;
    use crate::document_config::InferenceProfile;
    use defra_node::EmbeddedNode;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn replacement_is_scoped_clears_context_and_rolls_back_dangling_links() -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::Local(node.clone());
        for owner in ["did:key:owner", "did:key:other"] {
            let backend = json!({"node_did":owner,"backend_id":"local","name":"Local",
                "provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:8000/v1",
                "auth":{"kind":"unauthenticated"}});
            let context =
                json!({"node_did":owner,"context_id":"context","system_prompt":"literal"});
            let plan = DesiredStateApplyPlan::new(vec![
                DesiredStateApplyDocument {
                    collection: Collection::InferenceBackend,
                    add: backend.clone(),
                    update: backend,
                },
                DesiredStateApplyDocument {
                    collection: Collection::AgentContext,
                    add: context.clone(),
                    update: context,
                },
            ])?;
            access
                .transact("test.behavior.seed", |txn| {
                    let plan = &plan;
                    Box::pin(async move {
                        super::super::apply_desired_state_plan(txn, plan)
                            .await
                            .map(|_| ())
                    })
                })
                .await?;
            let profile: InferenceProfile = serde_json::from_value(
                json!({"node_did":owner,"profile_id":"inference","backend_id":"local","model_name":"exact-model"}),
            )?;
            write_inference_profile_document(&access, &profile).await?;
        }
        let mut behavior: Agent = serde_json::from_value(json!({
            "node_did":"did:key:owner","agent_id":"same","inference_profile_id":"inference",
            "context_id":"context","description":"old","tags":["old"]
        }))?;
        let original_id = write_agent_document(&access, &behavior).await?;
        let mut foreign = behavior.clone();
        foreign.node_did = "did:key:other".to_owned();
        let foreign_id = write_agent_document(&access, &foreign).await?;
        assert_ne!(original_id, foreign_id);
        behavior.context_id = None;
        behavior.description = None;
        behavior.tags.clear();
        assert_eq!(write_agent_document(&access, &behavior).await?, original_id);
        access
            .transact("test.behavior.read", |txn| {
                let behavior = &behavior;
                let foreign = &foreign;
                Box::pin(async move {
                    let actual = load_agent_in_txn(txn, &behavior.node_did, &behavior.agent_id)
                        .await?
                        .unwrap();
                    assert_eq!(actual, *behavior);
                    let actual_foreign =
                        load_agent_in_txn(txn, &foreign.node_did, &foreign.agent_id)
                            .await?
                            .unwrap();
                    assert_eq!(actual_foreign, *foreign);
                    Ok(())
                })
            })
            .await?;
        let mut invalid = behavior.clone();
        invalid.context_id = Some("missing".to_owned());
        assert!(write_agent_document(&access, &invalid).await.is_err());
        access
            .transact("test.behavior.rollback", |txn| {
                let behavior = &behavior;
                Box::pin(async move {
                    assert_eq!(
                        load_agent_in_txn(txn, &behavior.node_did, &behavior.agent_id)
                            .await?
                            .as_ref(),
                        Some(behavior)
                    );
                    Ok(())
                })
            })
            .await?;
        Ok(())
    }
}
