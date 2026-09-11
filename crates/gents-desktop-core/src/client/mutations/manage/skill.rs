//! Canonical authored configuration through the shared retained-candidate owner.
use anyhow::Result;
use defra_node::EmbeddedNode;
use gents::collection::Collection;
use gents::config_client::{
    apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::document_config::SkillDocument;

pub async fn upsert_skill(node: &EmbeddedNode, document: &SkillDocument) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Skill,
        add: value.clone(),
        update: value,
    }])?;
    ConfigAccess::transact_local(node, None, "desktop.skill.save", |txn| {
        let plan = &plan;
        Box::pin(async move {
            apply_desired_state_plan(txn, plan).await?;
            Ok(())
        })
    })
    .await
}

pub async fn delete_skill(node: &EmbeddedNode, agent_did: &str, id: &str) -> Result<usize> {
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        Collection::Skill,
        agent_did.to_owned(),
        id.to_owned(),
    )])?;
    ConfigAccess::transact_local(node, None, "desktop.skill.delete", |txn| {
        let plan = &plan;
        Box::pin(async move {
            let existed = gents::config_client::read_desired_state_record_in_txn(
                txn,
                Collection::Skill,
                agent_did,
                id,
            )
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
    use gents::config_client::read_desired_state_record_in_txn;
    use serde_json::json;

    #[tokio::test]
    async fn skill_delete_rejects_context_references_without_detaching_or_cross_owner_effects(
    ) -> Result<()> {
        let node = EmbeddedNode::builder().build().await?;
        gents::ensure_runtime_schemas(&node).await?;
        for owner in ["did:test:skill-a", "did:test:skill-b"] {
            gents::ensure_agent_principal(&node, owner).await?;
            let document: SkillDocument = serde_json::from_value(
                json!({"agent_did":owner,"skill_id":"skill","instructions":"Literal {{ instruction }}","created_at":"2026-01-01T00:00:00Z","tags":["review"]}),
            )?;
            upsert_skill(&node, &document).await?;
        }
        let value =
            json!({"agent_did":"did:test:skill-a","context_id":"context","skill_ids":["skill"]});
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: Collection::AgentContext,
            add: value.clone(),
            update: value,
        }])?;
        ConfigAccess::transact_local(&node, None, "desktop.skill.context", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan).await?;
                Ok(())
            })
        })
        .await?;
        assert!(delete_skill(&node, "did:test:skill-a", "skill")
            .await
            .is_err());
        assert_eq!(delete_skill(&node, "did:test:skill-b", "skill").await?, 1);
        ConfigAccess::transact_local(&node, None, "desktop.skill.verify", |txn| {
            Box::pin(async move {
                let (_, context) = read_desired_state_record_in_txn(
                    txn,
                    Collection::AgentContext,
                    "did:test:skill-a",
                    "context",
                )
                .await?
                .unwrap();
                assert_eq!(context["skill_ids"], json!(["skill"]));
                let (_, skill) = read_desired_state_record_in_txn(
                    txn,
                    Collection::Skill,
                    "did:test:skill-a",
                    "skill",
                )
                .await?
                .unwrap();
                let skill: SkillDocument = serde_json::from_value(skill)?;
                assert_eq!(
                    skill.instructions.as_deref(),
                    Some("Literal {{ instruction }}")
                );
                assert_eq!(skill.created_at.as_deref(), Some("2026-01-01T00:00:00Z"));
                assert_eq!(skill.tags, vec!["review"]);
                let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
                    Collection::AgentContext,
                    "did:test:skill-a".into(),
                    "context".into(),
                )])?;
                apply_desired_state_plan(txn, &plan).await?;
                Ok(())
            })
        })
        .await?;
        assert_eq!(delete_skill(&node, "did:test:skill-a", "skill").await?, 1);
        assert_eq!(delete_skill(&node, "did:test:skill-a", "skill").await?, 0);
        Ok(())
    }
}
