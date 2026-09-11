use anyhow::Result;
use defra_node::EmbeddedNode;
use gents::collection::Collection;
use gents::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, ConfigAccess,
    DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::InferenceProfile;

pub async fn upsert_inference_profile(
    node: &EmbeddedNode,
    document: &InferenceProfile,
) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::InferenceProfile,
        add: value.clone(),
        update: value,
    }])?;
    ConfigAccess::transact_local(node, None, "desktop.profile.save", |txn| {
        let plan = &plan;
        Box::pin(async move {
            apply_desired_state_plan(txn, plan).await?;
            Ok(())
        })
    })
    .await
}

pub async fn delete_inference_profile(
    node: &EmbeddedNode,
    agent_did: &str,
    id: &str,
) -> Result<usize> {
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        Collection::InferenceProfile,
        agent_did.to_owned(),
        id.to_owned(),
    )])?;
    ConfigAccess::transact_local(node, None, "desktop.profile.delete", |txn| {
        let plan = &plan;
        Box::pin(async move {
            let existed =
                read_desired_state_record_in_txn(txn, Collection::InferenceProfile, agent_did, id)
                    .await?
                    .is_some();
            apply_desired_state_plan(txn, plan).await?;
            Ok(usize::from(existed))
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
    async fn profile_save_validates_bounds_and_resolves_backend_in_exact_owner_scope() -> Result<()>
    {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        gents::ensure_runtime_schemas(&node).await?;
        for owner in ["did:test:owner", "did:test:other"] {
            gents::ensure_agent_principal(&node, owner).await?;
        }
        let backend = serde_json::from_value(
            json!({"agent_did":"did:test:owner","backend_id":"backend","name":"Backend","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}),
        )?;
        gents::config_client::write_inference_backend_document(
            &ConfigAccess::Local(node.clone()),
            &backend,
        )
        .await?;
        let mut profile: InferenceProfile = serde_json::from_value(
            json!({"agent_did":"did:test:owner","profile_id":"profile","backend_id":"backend","model_name":"model","reasoning_effort":"high","max_output_tokens":100}),
        )?;
        upsert_inference_profile(&node, &profile).await?;
        profile.max_output_tokens = Some(0);
        assert!(upsert_inference_profile(&node, &profile).await.is_err());
        profile.max_output_tokens = None;
        profile.agent_did = "did:test:other".into();
        assert!(upsert_inference_profile(&node, &profile).await.is_err());
        ConfigAccess::transact_local(&node, None, "test.profile.read", |txn| {
            Box::pin(async move {
                let (_, value) = read_desired_state_record_in_txn(
                    txn,
                    Collection::InferenceProfile,
                    "did:test:owner",
                    "profile",
                )
                .await?
                .unwrap();
                let retained: InferenceProfile = serde_json::from_value(value)?;
                assert_eq!(retained.max_output_tokens, Some(100));
                assert_eq!(
                    retained.reasoning_effort,
                    Some(gents::config::ReasoningEffort::High)
                );
                Ok(())
            })
        })
        .await?;
        Ok(())
    }
}
