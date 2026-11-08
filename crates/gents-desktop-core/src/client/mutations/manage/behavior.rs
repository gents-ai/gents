use anyhow::Result;
#[cfg(test)]
use gents::documents::apply_desired_state_plan;
use gents::store::collection::Collection;
#[cfg(test)]
use gents::store::config_client::read_desired_state_record_in_txn;
use gents::store::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents::AgentDocument;

#[cfg(test)]
#[path = "behavior/client_signed_tests.rs"]
mod client_signed_tests;

#[cfg(test)]
pub async fn upsert_agent_on(access: &ConfigAccess, document: &AgentDocument) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Agent,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan(access, "desktop.agent.save", plan).await
}

pub(crate) async fn upsert_agent_with_publication_on(
    access: &ConfigAccess,
    document: &AgentDocument,
    observer: std::sync::Arc<dyn gents::store::config_client::AgentPublicationObserver>,
) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Agent,
        add: value.clone(),
        update: value,
    }])?;
    access
        .transact_write_with_agent_publication("desktop.agent.save", observer, |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents::documents::apply_desired_state_plan(txn, plan).await?;
                Ok(())
            })
        })
        .await
}

pub async fn delete_agent_on(access: &ConfigAccess, node_did: &str, id: &str) -> Result<usize> {
    super::delete_scoped_document(
        access,
        "desktop.agent.delete",
        Collection::Agent,
        node_did,
        id,
    )
    .await
}

pub async fn delete_agent_context_on(
    access: &ConfigAccess,
    node_did: &str,
    id: &str,
) -> Result<usize> {
    super::delete_scoped_document(
        access,
        "desktop.context.delete",
        Collection::AgentContext,
        node_did,
        id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::ServedRuntimeHome;
    use serde_json::json;

    #[tokio::test]
    async fn context_delete_removes_the_scoped_document() -> Result<()> {
        let runtime = ServedRuntimeHome::start().await;
        let owner = runtime.node_did();
        let access = runtime.user_access();
        let context = json!({
            "node_did": owner,
            "context_id": "temporary",
            "display_name": "Temporary"
        });
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: Collection::AgentContext,
            add: context.clone(),
            update: context,
        }])?;
        access
            .transact_write("test.context.create", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;

        assert_eq!(
            delete_agent_context_on(&access, owner, "temporary").await?,
            1
        );
        assert_eq!(
            delete_agent_context_on(&access, owner, "temporary").await?,
            0
        );
        runtime.shutdown().await;
        Ok(())
    }

    /// The runtime's own Node is `alpha`; `beta` is a second Node scope the
    /// User publishes on the same store, holding same-named backend and
    /// profile documents, so every delete is checked against its own scope.
    #[tokio::test]
    async fn agent_and_profile_deletion_obey_canonical_owned_references() -> Result<()> {
        let runtime = ServedRuntimeHome::start().await;
        let access = runtime.user_access();
        let alpha = runtime.node_did().to_owned();
        let beta = runtime.publish_foreign_node().await;
        let review_alpha = runtime.provision_agent().await;
        let review_beta = runtime.provision_agent().await;
        for (owner, review) in [(&alpha, &review_alpha), (&beta, &review_beta)] {
            let backend = serde_json::from_value(json!({
                "node_did":owner, "backend_id":"backend", "name":"Backend",
                "provider_kind":"OpenAiCompatible", "endpoint":"http://localhost:8000/v1", "auth":{"kind":"unauthenticated"}
            }))?;
            gents::documents::write_inference_backend_document(&access, &backend).await?;
            let profile = serde_json::from_value(json!({
                "node_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model","reasoning_effort":"high"
            }))?;
            super::super::profile::upsert_inference_profile_on(&access, &profile).await?;
            let agent = serde_json::from_value(json!({
                "node_did":owner,"agent_did":review.agent_did,"creation_nonce":review.creation_nonce,
                "inference_profile_id":"profile"
            }))?;
            upsert_agent_on(&access, &agent).await?;
        }
        let review = review_alpha.agent_did.as_str();
        let invalid = serde_json::from_value(json!({
            "node_did":alpha,"agent_did":review,"creation_nonce":review_alpha.creation_nonce,
            "inference_profile_id":"profile","context_id":"missing"
        }))?;
        assert!(upsert_agent_on(&access, &invalid).await.is_err());
        assert!(
            super::super::profile::delete_inference_profile_on(&access, &alpha, "profile")
                .await
                .is_err()
        );
        let target = json!({
            "node_did":alpha,"target_id":"target","target_node_did":alpha,"agent_did":review,"name":"reviewer"
        });
        let target_plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: Collection::AgentTarget,
            add: target.clone(),
            update: target,
        }])?;
        access
            .transact_write("test.target", |txn| {
                let plan = &target_plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        assert!(delete_agent_on(&access, &alpha, review).await.is_err());
        assert_eq!(
            delete_agent_on(&access, &beta, &review_beta.agent_did).await?,
            1
        );
        assert_eq!(
            super::super::profile::delete_inference_profile_on(&access, &beta, "profile").await?,
            1
        );
        assert!(delete_agent_on(&access, &alpha, review).await.is_err());
        let remove_target = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
            Collection::AgentTarget,
            alpha.clone(),
            "target".into(),
        )])?;
        access
            .transact_write("test.target.remove", |txn| {
                let plan = &remove_target;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        let fallback_profile = serde_json::from_value(json!({
            "node_did":alpha,"profile_id":"fallback","backend_id":"backend","model_name":"model"
        }))?;
        super::super::profile::upsert_inference_profile_on(&access, &fallback_profile).await?;
        let fallback = runtime.provision_agent().await;
        let fallback_agent = serde_json::from_value(json!({
            "node_did":alpha,"agent_did":fallback.agent_did,"creation_nonce":fallback.creation_nonce,
            "inference_profile_id":"fallback"
        }))?;
        upsert_agent_on(&access, &fallback_agent).await?;
        for default in [review, fallback.agent_did.as_str()] {
            let alpha = alpha.as_str();
            access
                .transact_write("test.node.default", |txn| {
                    Box::pin(async move {
                        let (_, mut value) =
                            read_desired_state_record_in_txn(txn, Collection::Node, alpha, alpha)
                                .await?
                                .unwrap();
                        value["default_agent_did"] = json!(default);
                        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                            collection: Collection::Node,
                            add: value.clone(),
                            update: value,
                        }])?;
                        apply_desired_state_plan(txn, &plan).await?;
                        Ok::<_, anyhow::Error>(())
                    })
                })
                .await?;
            if default == review {
                assert!(delete_agent_on(&access, alpha, review).await.is_err());
                let disable = super::super::principal::patch_config_components_on(
                    &access,
                    alpha,
                    &[(
                        gents::store::config_client::patch::SelfConfigTarget::Agent,
                        review.into(),
                        vec![("enabled".into(), Some(json!(false)))],
                    )],
                )
                .await
                .unwrap_err();
                assert!(
                    format!("{disable:#}").contains("must be enabled"),
                    "{disable:#}"
                );
            }
        }
        assert_eq!(delete_agent_on(&access, &alpha, review).await?, 1);
        assert_eq!(
            super::super::profile::delete_inference_profile_on(&access, &alpha, "profile").await?,
            1
        );
        runtime.shutdown().await;
        Ok(())
    }
}
