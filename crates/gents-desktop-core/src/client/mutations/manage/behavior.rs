use anyhow::Result;
#[cfg(test)]
use defra_node::EmbeddedNode;
use gents::collection::Collection;
#[cfg(test)]
use gents::config_client::{apply_desired_state_plan, read_desired_state_record_in_txn};
use gents::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents::AgentDocument;
#[cfg(test)]
use std::sync::Arc;

pub async fn upsert_agent_on(access: &ConfigAccess, document: &AgentDocument) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Agent,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan(access, "desktop.agent.save", plan).await
}

#[cfg(test)]
pub async fn upsert_agent(node: &Arc<EmbeddedNode>, document: &AgentDocument) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Agent,
        add: value.clone(),
        update: value,
    }])?;
    ConfigAccess::transact_local_write(node, None, "desktop.agent.save", |txn| {
        let plan = &plan;
        Box::pin(async move {
            apply_desired_state_plan(txn, plan).await?;
            Ok(())
        })
    })
    .await
}

#[cfg(test)]
pub async fn delete_agent(node: &Arc<EmbeddedNode>, node_did: &str, id: &str) -> Result<usize> {
    super::delete_scoped_document_local(
        node,
        "desktop.agent.delete",
        Collection::Agent,
        node_did,
        id,
    )
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

#[cfg(test)]
pub async fn delete_agent_context(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    id: &str,
) -> Result<usize> {
    super::delete_scoped_document_local(
        node,
        "desktop.context.delete",
        Collection::AgentContext,
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
    use gents_protocol::error_chain::ErrorChain;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn context_delete_removes_the_scoped_document() -> Result<()> {
        let node = Arc::new(crate::test_support::node_builder().build().await?);
        gents::ensure_runtime_schemas(&node).await?;
        let owner = "did:test:context-delete";
        gents::ensure_node(&node, owner).await?;
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
        ConfigAccess::local(node.clone())
            .transact_write("test.context.create", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok(())
                })
            })
            .await?;

        assert_eq!(delete_agent_context(&node, owner, "temporary").await?, 1);
        assert_eq!(delete_agent_context(&node, owner, "temporary").await?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn agent_and_profile_deletion_obey_canonical_owned_references() -> Result<()> {
        let node = Arc::new(crate::test_support::node_builder().build().await?);
        gents::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::local(node.clone());
        for owner in ["did:test:alpha", "did:test:beta"] {
            gents::ensure_node(&node, owner).await?;
            let backend = serde_json::from_value(json!({
                "node_did":owner, "backend_id":"backend", "name":"Backend",
                "provider_kind":"OpenAiCompatible", "endpoint":"http://localhost:8000/v1", "auth":{"kind":"unauthenticated"}
            }))?;
            gents::config_client::write_inference_backend_document(&access, &backend).await?;
            let profile = serde_json::from_value(json!({
                "node_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model","reasoning_effort":"high"
            }))?;
            super::super::profile::upsert_inference_profile(&node, &profile).await?;
            let agent = serde_json::from_value(json!({
                "node_did":owner,"agent_id":"review","inference_profile_id":"profile"
            }))?;
            upsert_agent(&node, &agent).await?;
        }
        let invalid = serde_json::from_value(json!({
            "node_did":"did:test:alpha","agent_id":"review","inference_profile_id":"profile","context_id":"missing"
        }))?;
        assert!(upsert_agent(&node, &invalid).await.is_err());
        assert!(super::super::profile::delete_inference_profile(
            &node,
            "did:test:alpha",
            "profile"
        )
        .await
        .is_err());
        let target = json!({
            "node_did":"did:test:alpha","target_id":"target","target_node_did":"did:test:alpha","agent_id":"review","name":"reviewer"
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
                    Ok(())
                })
            })
            .await?;
        assert!(delete_agent(&node, "did:test:alpha", "review")
            .await
            .is_err());
        assert_eq!(delete_agent(&node, "did:test:beta", "review").await?, 1);
        assert_eq!(
            super::super::profile::delete_inference_profile(&node, "did:test:beta", "profile")
                .await?,
            1
        );
        assert!(delete_agent(&node, "did:test:alpha", "review")
            .await
            .is_err());
        let remove_target = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
            Collection::AgentTarget,
            "did:test:alpha".into(),
            "target".into(),
        )])?;
        access
            .transact_write("test.target.remove", |txn| {
                let plan = &remove_target;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok(())
                })
            })
            .await?;
        let fallback_profile = serde_json::from_value(json!({
            "node_did":"did:test:alpha","profile_id":"fallback","backend_id":"backend","model_name":"model"
        }))?;
        super::super::profile::upsert_inference_profile(&node, &fallback_profile).await?;
        let fallback = serde_json::from_value(json!({
            "node_did":"did:test:alpha","agent_id":"fallback","inference_profile_id":"fallback"
        }))?;
        upsert_agent(&node, &fallback).await?;
        for default in ["review", "fallback"] {
            access
                .transact_write("test.node.default", |txn| {
                    Box::pin(async move {
                        let (_, mut value) = read_desired_state_record_in_txn(
                            txn,
                            Collection::Node,
                            "did:test:alpha",
                            "did:test:alpha",
                        )
                        .await?
                        .unwrap();
                        value["default_agent_id"] = json!(default);
                        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                            collection: Collection::Node,
                            add: value.clone(),
                            update: value,
                        }])?;
                        apply_desired_state_plan(txn, &plan).await?;
                        Ok(())
                    })
                })
                .await?;
            if default == "review" {
                assert!(delete_agent(&node, "did:test:alpha", "review")
                    .await
                    .is_err());
                let disable = super::super::principal::patch_config_components(
                    &node,
                    "did:test:alpha",
                    &[(
                        gents::config_client::patch::SelfConfigTarget::Agent,
                        "review".into(),
                        vec![("enabled".into(), Some(json!(false)))],
                    )],
                )
                .await
                .unwrap_err();
                assert!(
                    ErrorChain(disable.as_ref())
                        .to_string()
                        .contains("must be enabled"),
                    "{}",
                    ErrorChain(disable.as_ref())
                );
            }
        }
        assert_eq!(delete_agent(&node, "did:test:alpha", "review").await?, 1);
        assert_eq!(
            super::super::profile::delete_inference_profile(&node, "did:test:alpha", "profile")
                .await?,
            1
        );
        Ok(())
    }
}
