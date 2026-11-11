use anyhow::{Context, Result};
use gents_documents::list_inference_profiles_in_txn;
use gents_documents::read_desired_state_record_in_txn;
use gents_documents::InferenceProfile;
use gents_inference::operations;
use gents_protocol::operations::inference::{InferenceDocument, WriteMode};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;

/// Profiles can be shared by other agents and compaction. Selecting a model
/// replaces only this agent's binding, preserving those consumers. The
/// binding is an Agent edit admitted by the operations layer
/// (`edit_in_txn`) in the same transaction as the profile it may create.
pub async fn select_agent_model_on(
    access: &ConfigAccess,
    node_did: &str,
    agent_did: &str,
    backend_id: &str,
    model_name: &str,
    reasoning_effort: Option<gents_documents::inference_defaults::ReasoningEffort>,
    service_tier: Option<gents_inference::resolved::ServiceTier>,
) -> Result<InferenceProfile> {
    use gents::operations::agent::{edit_in_txn, observe};
    use gents_protocol::operations::agent::AgentEdit;
    use gents_protocol::operations::StringUpdate;

    use super::inference::{operation_failure, put_write};
    use super::principal::agent_failure;

    anyhow::ensure!(!model_name.trim().is_empty(), "Choose a model.");
    let admission = observe(access, node_did, &[agent_did.to_owned()])
        .await
        .map_err(agent_failure)?;
    access
        .transact_write("desktop.agent.model", |txn| {
            let admission = &admission;
            Box::pin(async move {
                let (_, agent) =
                    read_desired_state_record_in_txn(txn, Collection::Agent, node_did, agent_did)
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
                if let Some(existing) = reusable {
                    selected = existing;
                } else {
                    selected.profile_id = format!("model-{}", uuid::Uuid::new_v4());
                    selected.display_name = Some(selected.model_name.clone());
                    selected.description = None;
                    let (profile_node, write) = put_write(
                        InferenceDocument::Profile,
                        &serde_json::to_value(&selected)?,
                        WriteMode::Create,
                    )?;
                    operations::write_in_txn(txn, &profile_node, &write)
                        .await
                        .map_err(|failure| {
                            operation_failure(failure, InferenceDocument::Profile, "create")
                        })?;
                }
                let binding = AgentEdit {
                    inference_profile_id: StringUpdate::Set(selected.profile_id.clone()),
                    ..Default::default()
                };
                edit_in_txn(txn, node_did, admission, agent_did, &binding)
                    .await
                    .map_err(agent_failure)?;
                Ok(selected)
            })
        })
        .await
}

pub async fn upsert_inference_profile_on(
    access: &ConfigAccess,
    document: &InferenceProfile,
) -> Result<()> {
    super::inference::put_document(
        access,
        InferenceDocument::Profile,
        &serde_json::to_value(document)?,
    )
    .await
}

pub async fn delete_inference_profile_on(
    access: &ConfigAccess,
    node_did: &str,
    id: &str,
) -> Result<usize> {
    super::inference::remove_document(access, InferenceDocument::Profile, node_did, id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::ServedRuntimeHome;
    use serde_json::json;

    #[tokio::test]
    async fn model_selection_preserves_shared_consumers_and_reuses_profiles() -> Result<()> {
        let runtime = ServedRuntimeHome::start().await;
        let owner = runtime.node_did();
        let access = runtime.user_access();
        let first = runtime.provision_agent().await;
        let second = runtime.provision_agent().await;
        let (first_did, second_did) = (first.agent_did.as_str(), second.agent_did.as_str());
        let backend = serde_json::from_value(json!({
            "node_did":owner,"backend_id":"backend","name":"Backend",
            "provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1",
            "auth":{"kind":"unauthenticated"}
        }))?;
        gents_documents::write_inference_backend_document(
            &access,
            &backend,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await?;
        let original: InferenceProfile = serde_json::from_value(json!({
            "node_did":owner,"profile_id":"shared","backend_id":"backend",
            "model_name":"original","reasoning_effort":"high",
            "context_window":64000,"max_output_tokens":100,"tags":["retained"]
        }))?;
        upsert_inference_profile_on(&access, &original).await?;
        for (id, binding) in [("first", &first), ("second", &second)] {
            let agent = serde_json::from_value(json!({
                "node_did":owner,"agent_did":binding.agent_did,"creation_nonce":binding.creation_nonce,
                "inference_profile_id":"shared","display_name":id,"tags":["preserved"]
            }))?;
            super::super::upsert_agent_on(&access, &agent).await?;
        }
        let selected = select_agent_model_on(
            &access,
            owner,
            first_did,
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
                first_did,
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
            first_did,
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
            first_did,
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
                    for (id, agent_did, expected) in [
                        ("first", first_did, selected.profile_id.as_str()),
                        ("second", second_did, "shared"),
                    ] {
                        let (_, agent) = read_desired_state_record_in_txn(
                            txn,
                            Collection::Agent,
                            owner,
                            agent_did,
                        )
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
                first_did,
                "backend",
                "original",
                Some(gents_documents::inference_defaults::ReasoningEffort::High),
                None,
            )
            .await?,
            original
        );
        let fast = select_agent_model_on(
            &access,
            owner,
            first_did,
            "backend",
            "original",
            Some(gents_documents::inference_defaults::ReasoningEffort::High),
            Some(gents_inference::resolved::ServiceTier::Fast),
        )
        .await?;
        assert_eq!(
            fast.service_tier,
            Some(gents_inference::resolved::ServiceTier::Fast)
        );
        assert_ne!(fast.profile_id, original.profile_id);
        assert_eq!(
            select_agent_model_on(
                &access,
                owner,
                first_did,
                "backend",
                "original",
                Some(gents_documents::inference_defaults::ReasoningEffort::High),
                Some(gents_inference::resolved::ServiceTier::Fast),
            )
            .await?,
            fast
        );
        let standard = select_agent_model_on(
            &access,
            owner,
            first_did,
            "backend",
            "original",
            Some(gents_documents::inference_defaults::ReasoningEffort::High),
            Some(gents_inference::resolved::ServiceTier::Default),
        )
        .await?;
        assert_eq!(
            standard.service_tier,
            Some(gents_inference::resolved::ServiceTier::Default)
        );
        let unsupported = serde_json::from_value(json!({
            "node_did":owner,"backend_id":"unsupported","name":"Unsupported",
            "provider_kind":"AnthropicApiKey","endpoint":"https://api.anthropic.com/v1",
            "auth":{"kind":"api_key","key":"fixture"}
        }))?;
        gents_documents::write_inference_backend_document(
            &access,
            &unsupported,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await?;
        assert!(select_agent_model_on(
            &access,
            owner,
            first_did,
            "unsupported",
            "original",
            None,
            Some(gents_inference::resolved::ServiceTier::Fast),
        )
        .await
        .is_err());
        assert_eq!(
            select_agent_model_on(
                &access,
                owner,
                first_did,
                "backend",
                "original",
                Some(gents_documents::inference_defaults::ReasoningEffort::High),
                Some(gents_inference::resolved::ServiceTier::Default),
            )
            .await?,
            standard
        );
        assert_eq!(
            select_agent_model_on(
                &access,
                owner,
                second_did,
                "backend",
                "original",
                Some(gents_documents::inference_defaults::ReasoningEffort::High),
                None,
            )
            .await?,
            original
        );
        runtime.shutdown().await;
        Ok(())
    }
}
