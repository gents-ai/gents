use anyhow::Result;
use defra_node::EmbeddedNode;
use gents::AgentBehaviorDocument;
use gents::collection::Collection;
use gents::config_client::{
    ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan, apply_desired_state_plan,
    read_desired_state_record_in_txn,
};

pub async fn upsert_agent_behavior(
    node: &EmbeddedNode,
    document: &AgentBehaviorDocument,
) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::AgentBehavior,
        add: value.clone(),
        update: value,
    }])?;
    ConfigAccess::transact_local(node, None, "desktop.behavior.save", |txn| {
        let plan = &plan;
        Box::pin(async move {
            apply_desired_state_plan(txn, plan).await?;
            Ok(())
        })
    })
    .await
}

pub async fn delete_agent_behavior(
    node: &EmbeddedNode,
    agent_did: &str,
    id: &str,
) -> Result<usize> {
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        Collection::AgentBehavior,
        agent_did.to_owned(),
        id.to_owned(),
    )])?;
    ConfigAccess::transact_local(node, None, "desktop.behavior.delete", |txn| {
        let plan = &plan;
        Box::pin(async move {
            let existed =
                read_desired_state_record_in_txn(txn, Collection::AgentBehavior, agent_did, id)
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
    async fn behavior_and_profile_deletion_obey_canonical_owned_references() -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        gents::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::Local(node.clone());
        for owner in ["did:test:alpha", "did:test:beta"] {
            gents::ensure_agent_principal(&node, owner).await?;
            let backend = serde_json::from_value(json!({
                "agent_did":owner, "backend_id":"backend", "name":"Backend",
                "provider_kind":"OpenAiCompatible", "endpoint":"http://localhost:8000/v1", "auth":{"kind":"unauthenticated"}
            }))?;
            gents::config_client::write_inference_backend_document(&access, &backend).await?;
            let profile = serde_json::from_value(json!({
                "agent_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model","reasoning_effort":"high"
            }))?;
            super::super::profile::upsert_inference_profile(&node, &profile).await?;
            let behavior = serde_json::from_value(json!({
                "agent_did":owner,"behavior_id":"review","inference_profile_id":"profile"
            }))?;
            upsert_agent_behavior(&node, &behavior).await?;
        }
        let invalid = serde_json::from_value(json!({
            "agent_did":"did:test:alpha","behavior_id":"review","inference_profile_id":"profile","context_id":"missing"
        }))?;
        assert!(upsert_agent_behavior(&node, &invalid).await.is_err());
        assert!(
            super::super::profile::delete_inference_profile(&node, "did:test:alpha", "profile")
                .await
                .is_err()
        );
        // The shared closure owns local target references; foreign destinations
        // remain governed by delegation admission, not global label lookup.
        let target = json!({
            "agent_did":"did:test:alpha","target_id":"target","target_agent_did":"did:test:alpha","behavior_id":"review","name":"reviewer"
        });
        let target_plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: Collection::SubagentTarget,
            add: target.clone(),
            update: target,
        }])?;
        access
            .transact("test.target", |txn| {
                let plan = &target_plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok(())
                })
            })
            .await?;
        assert!(
            delete_agent_behavior(&node, "did:test:alpha", "review")
                .await
                .is_err()
        );
        assert_eq!(
            delete_agent_behavior(&node, "did:test:beta", "review").await?,
            1
        );
        assert_eq!(
            super::super::profile::delete_inference_profile(&node, "did:test:beta", "profile")
                .await?,
            1
        );
        assert!(
            delete_agent_behavior(&node, "did:test:alpha", "review")
                .await
                .is_err()
        );
        let remove_target = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
            Collection::SubagentTarget,
            "did:test:alpha".into(),
            "target".into(),
        )])?;
        access
            .transact("test.target.remove", |txn| {
                let plan = &remove_target;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok(())
                })
            })
            .await?;
        for default in [Some("review"), None] {
            access
                .transact("test.principal.default", |txn| {
                    Box::pin(async move {
                        let (_, mut value) = read_desired_state_record_in_txn(
                            txn,
                            Collection::AgentPrincipal,
                            "did:test:alpha",
                            "did:test:alpha",
                        )
                        .await?
                        .unwrap();
                        value["default_behavior_id"] = serde_json::to_value(default)?;
                        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                            collection: Collection::AgentPrincipal,
                            add: value.clone(),
                            update: value,
                        }])?;
                        apply_desired_state_plan(txn, &plan).await?;
                        Ok(())
                    })
                })
                .await?;
            if default.is_some() {
                assert!(
                    delete_agent_behavior(&node, "did:test:alpha", "review")
                        .await
                        .is_err()
                );
            }
        }
        assert_eq!(
            delete_agent_behavior(&node, "did:test:alpha", "review").await?,
            1
        );
        assert_eq!(
            super::super::profile::delete_inference_profile(&node, "did:test:alpha", "profile")
                .await?,
            1
        );
        Ok(())
    }
}
