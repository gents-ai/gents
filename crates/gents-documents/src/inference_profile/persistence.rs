use anyhow::{Context, Result};

use super::InferenceProfile;
use crate::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_store::collection::Collection;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};

pub async fn list_inference_profiles_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
) -> Result<Vec<InferenceProfile>> {
    let fields = crate::config_projection(Collection::InferenceProfile, None)?
        .0
        .join(" ");
    let response = txn
        .execute(&format!(
            r#"{{ InferenceProfile(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ {fields} }} }}"#,
            gents_store::graphql::escape_graphql_string(node_did)
        ))
        .await?;
    gents_protocol::graphql::graphql_rows_from_response(&response, "InferenceProfile")
        .into_iter()
        .map(|row| serde_json::from_value(row).context("decoding scoped InferenceProfile"))
        .collect()
}

pub async fn write_inference_profile_document(
    access: &ConfigAccess,
    profile: &InferenceProfile,
    policy: &dyn crate::inference_backend::AdvertisedProfilePolicy,
) -> Result<String> {
    profile.validate()?;
    access
        .transact_write("config.inference_profile.write", |txn| {
            Box::pin(async move {
                let value = serde_json::to_value(profile)?;
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: Collection::InferenceProfile,
                    add: value.clone(),
                    update: value,
                }])?;
                crate::apply_desired_state_plan(txn, &plan, policy).await?;
                crate::read_desired_state_record_in_txn(
                    txn,
                    Collection::InferenceProfile,
                    &profile.node_did,
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

    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn replacement_preserves_other_owner_and_clears_optional_selection() -> Result<()> {
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
        for owner in ["did:key:owner", "did:key:other"] {
            crate::test_support::bootstrap_signed_test_node(&access, owner).await;
            let backend = json!({"node_did":owner,"backend_id":"local","name":"Local",
                "provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:8000/v1",
                "auth":{"kind":"unauthenticated"}});
            let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                collection: Collection::InferenceBackend,
                add: backend.clone(),
                update: backend,
            }])?;
            access
                .transact_write("test.profile.seed", |txn| {
                    let plan = &plan;
                    Box::pin(async move {
                        crate::apply_desired_state_plan(
                            txn,
                            plan,
                            &crate::inference::resolved::ProviderProfilePolicy,
                        )
                        .await
                        .map(|_| ())
                    })
                })
                .await?;
        }
        let mut initial: InferenceProfile = serde_json::from_value(json!({
            "node_did":"did:key:owner","profile_id":"same","backend_id":"local",
            "model_name":"exact-model","reasoning_effort":"high","max_output_tokens":1234,"tags":["old"]
        }))?;
        let first_id = write_inference_profile_document(
            &access,
            &initial,
            &crate::inference::resolved::ProviderProfilePolicy,
        )
        .await?;
        let mut foreign = initial.clone();
        foreign.node_did = "did:key:other".to_owned();
        assert_ne!(
            write_inference_profile_document(
                &access,
                &foreign,
                &crate::inference::resolved::ProviderProfilePolicy
            )
            .await?,
            first_id
        );
        initial.reasoning_effort = None;
        initial.max_output_tokens = None;
        initial.tags.clear();
        assert_eq!(
            write_inference_profile_document(
                &access,
                &initial,
                &crate::inference::resolved::ProviderProfilePolicy
            )
            .await?,
            first_id
        );
        access
            .transact_write("test.profile.read", |txn| {
                let initial = &initial;
                let foreign = &foreign;
                Box::pin(async move {
                    for expected in [initial, foreign] {
                        let value = crate::read_desired_state_document_in_txn(
                            txn,
                            Collection::InferenceProfile,
                            &expected.node_did,
                            &expected.profile_id,
                        )
                        .await?
                        .unwrap();
                        let actual: InferenceProfile = serde_json::from_value(value)?;
                        assert_eq!(&actual, expected);
                    }
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        Ok(())
    }
}
