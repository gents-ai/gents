use anyhow::{Context, Result};

use crate::collection::Collection;
use crate::document_config::InferenceProfile;

use super::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};

/// Replace a complete inference profile through the common configuration writer.
/// Sampling and execution settings are references, never a second set of profile
/// fields. Explicit sparse patches remain owned by the patch API.
pub async fn write_inference_profile_document(
    access: &ConfigAccess,
    profile: &InferenceProfile,
) -> Result<String> {
    profile.validate()?;
    access
        .transact("config.inference_profile.write", |txn| {
            Box::pin(async move {
                let value = serde_json::to_value(profile)?;
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: Collection::InferenceProfile,
                    add: value.clone(),
                    update: value,
                }])?;
                super::apply_desired_state_plan(txn, &plan).await?;
                super::desired_state::read_record(
                    txn,
                    Collection::InferenceProfile,
                    &profile.agent_did,
                    &profile.profile_id,
                )
                .await?
                .map(|(doc_id, _)| doc_id)
                .context("replaced InferenceProfile missing")
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
    async fn replacement_preserves_other_owner_and_clears_optional_selection() -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::Local(node);
        for owner in ["did:key:owner", "did:key:other"] {
            let backend = json!({"agent_did":owner,"backend_id":"local","name":"Local",
                "provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:8000/v1",
                "auth":{"kind":"unauthenticated"}});
            let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                collection: Collection::InferenceBackend,
                add: backend.clone(),
                update: backend,
            }])?;
            access
                .transact("test.profile.seed", |txn| {
                    let plan = &plan;
                    Box::pin(async move {
                        super::super::apply_desired_state_plan(txn, plan)
                            .await
                            .map(|_| ())
                    })
                })
                .await?;
        }
        let mut initial: InferenceProfile = serde_json::from_value(json!({
            "agent_did":"did:key:owner","profile_id":"same","backend_id":"local",
            "model_name":"exact-model","reasoning_effort":"high","max_output_tokens":1234,"tags":["old"]
        }))?;
        let first_id = write_inference_profile_document(&access, &initial).await?;
        let mut foreign = initial.clone();
        foreign.agent_did = "did:key:other".to_owned();
        assert_ne!(
            write_inference_profile_document(&access, &foreign).await?,
            first_id
        );
        initial.reasoning_effort = None;
        initial.max_output_tokens = None;
        initial.tags.clear();
        assert_eq!(
            write_inference_profile_document(&access, &initial).await?,
            first_id
        );
        access
            .transact("test.profile.read", |txn| {
                let initial = &initial;
                let foreign = &foreign;
                Box::pin(async move {
                    for expected in [initial, foreign] {
                        let value = super::super::read_desired_state_document_in_txn(
                            txn,
                            Collection::InferenceProfile,
                            &expected.agent_did,
                            &expected.profile_id,
                        )
                        .await?
                        .unwrap();
                        let actual: InferenceProfile = serde_json::from_value(value)?;
                        assert_eq!(&actual, expected);
                    }
                    Ok(())
                })
            })
            .await?;
        Ok(())
    }
}
