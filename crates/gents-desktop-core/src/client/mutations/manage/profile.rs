use anyhow::{Context, Result};
#[cfg(test)]
use defra_node::EmbeddedNode;
use gents::store::collection::Collection;
use gents::store::config_client::{
    apply_desired_state_plan, list_inference_profiles_in_txn, read_desired_state_record_in_txn,
};
use gents::store::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents::InferenceProfile;
#[cfg(test)]
use std::sync::Arc;

/// Profiles can be shared by other agents and compaction. Selecting a model
/// replaces only this agent's binding, preserving those consumers and all
/// unrelated configuration in the same transaction.
pub async fn select_agent_model_on(
    access: &ConfigAccess,
    node_did: &str,
    agent_id: &str,
    backend_id: &str,
    model_name: &str,
    reasoning_effort: Option<gents::inference::resolved::ReasoningEffort>,
    service_tier: Option<gents::inference::resolved::ServiceTier>,
) -> Result<InferenceProfile> {
    anyhow::ensure!(!model_name.trim().is_empty(), "Choose a model.");
    access
        .transact_write("desktop.agent.model", |txn| {
            Box::pin(async move {
                let (_, mut agent) =
                    read_desired_state_record_in_txn(txn, Collection::Agent, node_did, agent_id)
                        .await?
                        .context("The selected agent no longer exists.")?;
                let profile_id = agent["inference_profile_id"]
                    .as_str()
                    .context("The selected agent has no inference profile.")?;
                let (_, value) = read_desired_state_record_in_txn(
                    txn,
                    Collection::InferenceProfile,
                    node_did,
                    profile_id,
                )
                .await?
                .context("The agent's inference profile is unavailable.")?;
                let current: InferenceProfile = serde_json::from_value(value)?;
                let mut selected = current.clone();
                selected.backend_id = backend_id.to_owned();
                selected.model_name = model_name.trim().to_owned();
                selected.reasoning_effort = reasoning_effort;
                selected.service_tier = service_tier;
                if selected == current {
                    return Ok(current);
                }
                let profiles = list_inference_profiles_in_txn(txn, node_did).await?;
                let reusable = profiles.into_iter().find(|candidate| {
                    let mut comparable = selected.clone();
                    comparable.profile_id = candidate.profile_id.clone();
                    comparable.display_name = candidate.display_name.clone();
                    comparable.description = candidate.description.clone();
                    comparable == *candidate
                });
                let mut documents = Vec::new();
                if let Some(existing) = reusable {
                    selected = existing;
                } else {
                    selected.profile_id = format!("model-{}", uuid::Uuid::new_v4());
                    selected.display_name = Some(selected.model_name.clone());
                    selected.description = None;
                    let value = serde_json::to_value(&selected)?;
                    documents.push(DesiredStateApplyDocument {
                        collection: Collection::InferenceProfile,
                        add: value.clone(),
                        update: value,
                    });
                }
                agent["inference_profile_id"] = selected.profile_id.clone().into();
                documents.push(DesiredStateApplyDocument {
                    collection: Collection::Agent,
                    add: agent.clone(),
                    update: agent,
                });
                apply_desired_state_plan(txn, &DesiredStateApplyPlan::new(documents)?).await?;
                Ok(selected)
            })
        })
        .await
}

pub async fn upsert_inference_profile_on(
    access: &ConfigAccess,
    document: &InferenceProfile,
) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::InferenceProfile,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan(access, "desktop.profile.save", plan).await
}

#[cfg(test)]
pub async fn upsert_inference_profile(
    node: &Arc<EmbeddedNode>,
    document: &InferenceProfile,
) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::InferenceProfile,
        add: value.clone(),
        update: value,
    }])?;
    ConfigAccess::transact_local_write(node, None, "desktop.profile.save", |txn| {
        let plan = &plan;
        Box::pin(async move {
            apply_desired_state_plan(txn, plan).await?;
            Ok(())
        })
    })
    .await
}

#[cfg(test)]
pub async fn delete_inference_profile(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    id: &str,
) -> Result<usize> {
    super::delete_scoped_document_local(
        node,
        "desktop.profile.delete",
        Collection::InferenceProfile,
        node_did,
        id,
    )
    .await
}

pub async fn delete_inference_profile_on(
    access: &ConfigAccess,
    node_did: &str,
    id: &str,
) -> Result<usize> {
    super::delete_scoped_document(
        access,
        "desktop.profile.delete",
        Collection::InferenceProfile,
        node_did,
        id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn model_selection_preserves_shared_consumers_and_reuses_profiles() -> Result<()> {
        let node = Arc::new(crate::test_support::node_builder().build().await?);
        gents::ensure_runtime_schemas(&node).await?;
        let owner = "did:test:model-selection";
        gents::ensure_node(&node, owner).await?;
        let access = ConfigAccess::local(node.clone());
        let backend = serde_json::from_value(json!({
            "node_did":owner,"backend_id":"backend","name":"Backend",
            "provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1",
            "auth":{"kind":"unauthenticated"}
        }))?;
        gents::store::config_client::write_inference_backend_document(&access, &backend).await?;
        let original: InferenceProfile = serde_json::from_value(json!({
            "node_did":owner,"profile_id":"shared","backend_id":"backend",
            "model_name":"original","reasoning_effort":"high",
            "context_window":64000,"max_output_tokens":100,"tags":["retained"]
        }))?;
        upsert_inference_profile_on(&access, &original).await?;
        for id in ["first", "second"] {
            let agent = serde_json::from_value(json!({
                "node_did":owner,"agent_id":id,"inference_profile_id":"shared",
                "display_name":id,"tags":["preserved"]
            }))?;
            super::super::upsert_agent_on(&access, &agent).await?;
        }
        let selected = select_agent_model_on(
            &access,
            owner,
            "first",
            "backend",
            "replacement",
            None,
            None,
        )
        .await?;
        assert_ne!(selected.profile_id, original.profile_id);
        assert_eq!(selected.context_window, original.context_window);
        assert_eq!(selected.max_output_tokens, original.max_output_tokens);
        assert_eq!(selected.tags, original.tags);
        assert_eq!(
            select_agent_model_on(
                &access,
                owner,
                "first",
                "backend",
                "replacement",
                None,
                None,
            )
            .await?,
            selected
        );
        assert!(select_agent_model_on(
            &access,
            owner,
            "first",
            "missing-backend",
            "rejected",
            None,
            None,
        )
        .await
        .is_err());
        assert!(select_agent_model_on(
            &access,
            "did:test:other",
            "first",
            "backend",
            "replacement",
            None,
            None,
        )
        .await
        .is_err());
        access
            .transact_write("test.model.inspect", |txn| {
                let selected = &selected;
                let original = &original;
                Box::pin(async move {
                    let profiles = list_inference_profiles_in_txn(txn, owner).await?;
                    assert_eq!(profiles.len(), 2);
                    assert!(profiles.contains(original));
                    for (id, expected) in [
                        ("first", selected.profile_id.as_str()),
                        ("second", "shared"),
                    ] {
                        let (_, agent) =
                            read_desired_state_record_in_txn(txn, Collection::Agent, owner, id)
                                .await?
                                .unwrap();
                        assert_eq!(agent["inference_profile_id"], expected);
                        assert_eq!(agent["display_name"], id);
                        assert_eq!(agent["tags"], json!(["preserved"]));
                    }
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        assert_eq!(
            select_agent_model_on(
                &access,
                owner,
                "first",
                "backend",
                "original",
                Some(gents::inference::resolved::ReasoningEffort::High),
                None,
            )
            .await?,
            original
        );
        let fast = select_agent_model_on(
            &access,
            owner,
            "first",
            "backend",
            "original",
            Some(gents::inference::resolved::ReasoningEffort::High),
            Some(gents::inference::resolved::ServiceTier::Fast),
        )
        .await?;
        assert_eq!(
            fast.service_tier,
            Some(gents::inference::resolved::ServiceTier::Fast)
        );
        assert_ne!(fast.profile_id, original.profile_id);
        assert_eq!(
            select_agent_model_on(
                &access,
                owner,
                "first",
                "backend",
                "original",
                Some(gents::inference::resolved::ReasoningEffort::High),
                Some(gents::inference::resolved::ServiceTier::Fast),
            )
            .await?,
            fast
        );
        let standard = select_agent_model_on(
            &access,
            owner,
            "first",
            "backend",
            "original",
            Some(gents::inference::resolved::ReasoningEffort::High),
            Some(gents::inference::resolved::ServiceTier::Default),
        )
        .await?;
        assert_eq!(
            standard.service_tier,
            Some(gents::inference::resolved::ServiceTier::Default)
        );
        let unsupported = serde_json::from_value(json!({
            "node_did":owner,"backend_id":"unsupported","name":"Unsupported",
            "provider_kind":"AnthropicApiKey","endpoint":"https://api.anthropic.com/v1",
            "auth":{"kind":"api_key","key":"fixture"}
        }))?;
        gents::store::config_client::write_inference_backend_document(&access, &unsupported)
            .await?;
        assert!(select_agent_model_on(
            &access,
            owner,
            "first",
            "unsupported",
            "original",
            None,
            Some(gents::inference::resolved::ServiceTier::Fast),
        )
        .await
        .is_err());
        assert_eq!(
            select_agent_model_on(
                &access,
                owner,
                "first",
                "backend",
                "original",
                Some(gents::inference::resolved::ReasoningEffort::High),
                Some(gents::inference::resolved::ServiceTier::Default),
            )
            .await?,
            standard
        );
        assert_eq!(
            select_agent_model_on(
                &access,
                owner,
                "second",
                "backend",
                "original",
                Some(gents::inference::resolved::ReasoningEffort::High),
                None,
            )
            .await?,
            original
        );
        Ok(())
    }

    #[tokio::test]
    async fn profile_save_validates_bounds_and_resolves_backend_in_exact_owner_scope() -> Result<()>
    {
        let node = Arc::new(crate::test_support::node_builder().build().await?);
        gents::ensure_runtime_schemas(&node).await?;
        for owner in ["did:test:owner", "did:test:other"] {
            gents::ensure_node(&node, owner).await?;
        }
        let backend = serde_json::from_value(
            json!({"node_did":"did:test:owner","backend_id":"backend","name":"Backend","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}),
        )?;
        gents::store::config_client::write_inference_backend_document(
            &ConfigAccess::local(node.clone()),
            &backend,
        )
        .await?;
        let mut profile: InferenceProfile = serde_json::from_value(
            json!({"node_did":"did:test:owner","profile_id":"profile","backend_id":"backend","model_name":"model","reasoning_effort":"high","max_output_tokens":100}),
        )?;
        upsert_inference_profile(&node, &profile).await?;
        profile.max_output_tokens = Some(0);
        assert!(upsert_inference_profile(&node, &profile).await.is_err());
        profile.max_output_tokens = None;
        profile.node_did = "did:test:other".into();
        assert!(upsert_inference_profile(&node, &profile).await.is_err());
        ConfigAccess::transact_local_write(&node, None, "test.profile.read", |txn| {
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
                    Some(gents::inference::resolved::ReasoningEffort::High)
                );
                Ok::<_, anyhow::Error>(())
            })
        })
        .await?;
        Ok(())
    }
}
