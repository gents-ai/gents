use anyhow::{Context, Result};

use super::InferenceProfile;
use crate::store::config_client::{
    ConfigAccess, ConfigApplyTxn, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::Collection;

pub async fn list_inference_profiles_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
) -> Result<Vec<InferenceProfile>> {
    let fields =
        crate::store::config_client::config_projection(Collection::InferenceProfile, None)?
            .0
            .join(" ");
    let response = txn
        .execute(&format!(
            r#"{{ InferenceProfile(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ {fields} }} }}"#,
            crate::store::graphql::escape_graphql_string(node_did)
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
                crate::documents::apply_desired_state_plan(txn, &plan).await?;
                crate::store::config_client::read_desired_state_record_in_txn(
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
                        crate::documents::apply_desired_state_plan(txn, plan)
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
        let first_id = write_inference_profile_document(&access, &initial).await?;
        let mut foreign = initial.clone();
        foreign.node_did = "did:key:other".to_owned();
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
            .transact_write("test.profile.read", |txn| {
                let initial = &initial;
                let foreign = &foreign;
                Box::pin(async move {
                    for expected in [initial, foreign] {
                        let value =
                            crate::store::config_client::read_desired_state_document_in_txn(
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
