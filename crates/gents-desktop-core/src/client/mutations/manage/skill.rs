use anyhow::Result;
#[cfg(test)]
use gents::documents::apply_desired_state_plan;
use gents::documents::SkillDocument;
use gents::store::collection::Collection;
use gents::store::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};

pub async fn upsert_skill_on(access: &ConfigAccess, document: &SkillDocument) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Skill,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan(access, "desktop.skill.save", plan).await
}

pub async fn delete_skill_on(access: &ConfigAccess, node_did: &str, id: &str) -> Result<usize> {
    super::delete_scoped_document(
        access,
        "desktop.skill.delete",
        Collection::Skill,
        node_did,
        id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::ServedRuntimeHome;
    use gents::store::config_client::read_desired_state_record_in_txn;
    use serde_json::json;

    #[tokio::test]
    async fn skill_delete_rejects_context_references_without_detaching_or_cross_owner_effects(
    ) -> Result<()> {
        let runtime = ServedRuntimeHome::start().await;
        let access = runtime.user_access();
        let skill_a = runtime.node_did();
        let skill_b = runtime.publish_foreign_node().await;
        for owner in [skill_a, skill_b.as_str()] {
            let document: SkillDocument = serde_json::from_value(
                json!({"node_did":owner,"skill_id":"skill","instructions":"Literal {{ instruction }}","created_at":"2026-01-01T00:00:00Z","tags":["review"]}),
            )?;
            upsert_skill_on(&access, &document).await?;
        }
        let value = json!({"node_did":skill_a,"context_id":"context","skill_ids":["skill"]});
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: Collection::AgentContext,
            add: value.clone(),
            update: value,
        }])?;
        access
            .transact_write("desktop.skill.context", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        assert!(delete_skill_on(&access, skill_a, "skill").await.is_err());
        assert_eq!(delete_skill_on(&access, &skill_b, "skill").await?, 1);
        access
            .transact_write("desktop.skill.verify", |txn| {
                Box::pin(async move {
                    let (_, context) = read_desired_state_record_in_txn(
                        txn,
                        Collection::AgentContext,
                        skill_a,
                        "context",
                    )
                    .await?
                    .unwrap();
                    assert_eq!(context["skill_ids"], json!(["skill"]));
                    let (_, skill) =
                        read_desired_state_record_in_txn(txn, Collection::Skill, skill_a, "skill")
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
                        skill_a.into(),
                        "context".into(),
                    )])?;
                    apply_desired_state_plan(txn, &plan).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        assert_eq!(delete_skill_on(&access, skill_a, "skill").await?, 1);
        assert_eq!(delete_skill_on(&access, skill_a, "skill").await?, 0);
        runtime.shutdown().await;
        Ok(())
    }
}
