use anyhow::{Context, Result};

use crate::collection::Collection;
use crate::AgentBehaviorDocument as AgentBehavior;

#[cfg(test)]
use super::ConfigApplyTxn;
use super::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};

/// Replace one complete canonical behavior. Omitted optional fields clear their
/// previous values; sparse edits belong to the explicit patch owner.
/// Desired-state application validates references in the mutation transaction.
pub async fn write_agent_behavior_document(
    access: &ConfigAccess,
    behavior: &AgentBehavior,
) -> Result<String> {
    access
        .transact("config.agent_behavior.write", |txn| {
            Box::pin(async move {
                let value = serde_json::to_value(behavior)?;
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: Collection::AgentBehavior,
                    add: value.clone(),
                    update: value,
                }])?;
                super::apply_desired_state_plan(txn, &plan).await?;
                super::desired_state::read_record(
                    txn,
                    Collection::AgentBehavior,
                    &behavior.agent_did,
                    &behavior.behavior_id,
                )
                .await?
                .map(|(doc_id, _)| doc_id)
                .context("replaced AgentBehavior missing")
            })
        })
        .await
}

#[cfg(test)]
async fn load_agent_behavior_in_txn(
    txn: &ConfigApplyTxn<'_>,
    agent_did: &str,
    behavior_id: &str,
) -> Result<Option<AgentBehavior>> {
    super::read_desired_state_document_in_txn(
        txn,
        Collection::AgentBehavior,
        agent_did,
        behavior_id,
    )
    .await?
    .map(serde_json::from_value)
    .transpose()
    .context("decoding canonical AgentBehavior")
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
            let backend = json!({"agent_did":owner,"backend_id":"local","name":"Local",
                "provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:8000/v1",
                "auth":{"kind":"unauthenticated"}});
            let context =
                json!({"agent_did":owner,"context_id":"context","system_prompt":"literal"});
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
                json!({"agent_did":owner,"profile_id":"inference","backend_id":"local","model_name":"exact-model"}),
            )?;
            write_inference_profile_document(&access, &profile).await?;
        }
        let mut behavior: AgentBehavior = serde_json::from_value(json!({
            "agent_did":"did:key:owner","behavior_id":"same","inference_profile_id":"inference",
            "context_id":"context","description":"old","tags":["old"]
        }))?;
        let original_id = write_agent_behavior_document(&access, &behavior).await?;
        let mut foreign = behavior.clone();
        foreign.agent_did = "did:key:other".to_owned();
        let foreign_id = write_agent_behavior_document(&access, &foreign).await?;
        assert_ne!(original_id, foreign_id);
        behavior.context_id = None;
        behavior.description = None;
        behavior.tags.clear();
        assert_eq!(
            write_agent_behavior_document(&access, &behavior).await?,
            original_id
        );
        access
            .transact("test.behavior.read", |txn| {
                let behavior = &behavior;
                let foreign = &foreign;
                Box::pin(async move {
                    let actual =
                        load_agent_behavior_in_txn(txn, &behavior.agent_did, &behavior.behavior_id)
                            .await?
                            .unwrap();
                    assert_eq!(actual, *behavior);
                    let actual_foreign =
                        load_agent_behavior_in_txn(txn, &foreign.agent_did, &foreign.behavior_id)
                            .await?
                            .unwrap();
                    assert_eq!(actual_foreign, *foreign);
                    Ok(())
                })
            })
            .await?;
        let mut invalid = behavior.clone();
        invalid.context_id = Some("missing".to_owned());
        assert!(write_agent_behavior_document(&access, &invalid)
            .await
            .is_err());
        access
            .transact("test.behavior.rollback", |txn| {
                let behavior = &behavior;
                Box::pin(async move {
                    assert_eq!(
                        load_agent_behavior_in_txn(txn, &behavior.agent_did, &behavior.behavior_id)
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
