use anyhow::Result;
use gents_documents::apply_desired_state_plan;
use gents_documents::read_desired_state_record_in_txn;
use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_documents::{EventSource, Schedule, Task, Trigger};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;

pub async fn upsert_task_on(access: &ConfigAccess, document: &Task) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Task,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan(access, "desktop.task.save", plan).await
}

pub async fn prepare_access_review_task_on(
    access: &ConfigAccess,
    node: &str,
    agent: &str,
    preferred: Option<&str>,
) -> Result<gents_tools::plugin::approval::workspace::ReviewTaskSelection> {
    use gents_tools::plugin::approval::workspace::{default_review_task, review_task_policy};

    let default = default_review_task(node, agent);
    let task_id = preferred.unwrap_or(&default.task_id);
    access.transact_write("desktop.access_review.prepare", |txn| {
        let default = &default;
        Box::pin(async move {
            anyhow::ensure!(
                read_desired_state_record_in_txn(txn, Collection::Agent, node, agent).await?.is_some(),
                "Choose an existing agent before setting workspace permissions."
            );
            let existing = read_desired_state_record_in_txn(txn, Collection::Task, node, task_id).await?;
            let task = match existing {
                Some((_, value)) => serde_json::from_value::<Task>(value)?,
                None if preferred.is_none() => {
                    let value = serde_json::to_value(default)?;
                    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                        collection: Collection::Task,
                        add: value.clone(),
                        update: value,
                    }])?;
                    apply_desired_state_plan(txn, &plan, &gents_inference::resolved::ProviderProfilePolicy).await?;
                    default.clone()
                }
                None => anyhow::bail!("The configured access reviewer is missing. Restore its Task in Runtime Settings."),
            };
            review_task_policy(&task, node, task_id, agent, "access-review")?;
            Ok(gents_tools::plugin::approval::workspace::ReviewTaskSelection::for_task(&task))
        })
    }).await
}

pub async fn delete_task_on(access: &ConfigAccess, node_did: &str, id: &str) -> Result<usize> {
    super::delete_scoped_document(
        access,
        "desktop.task.delete",
        Collection::Task,
        node_did,
        id,
    )
    .await
}

pub async fn upsert_schedule_on(access: &ConfigAccess, document: &Schedule) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Schedule,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan(access, "desktop.schedule.save", plan).await
}

pub async fn delete_schedule_on(access: &ConfigAccess, node_did: &str, id: &str) -> Result<usize> {
    super::delete_scoped_document(
        access,
        "desktop.schedule.delete",
        Collection::Schedule,
        node_did,
        id,
    )
    .await
}

pub async fn upsert_trigger_on(access: &ConfigAccess, document: &Trigger) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Trigger,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan(access, "desktop.trigger.save", plan).await
}

pub async fn delete_trigger_on(access: &ConfigAccess, node_did: &str, id: &str) -> Result<usize> {
    super::delete_scoped_document(
        access,
        "desktop.trigger.delete",
        Collection::Trigger,
        node_did,
        id,
    )
    .await
}

pub async fn upsert_event_source_on(access: &ConfigAccess, document: &EventSource) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::EventSource,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan(access, "desktop.event_source.save", plan).await
}

pub async fn delete_event_source_on(
    access: &ConfigAccess,
    node_did: &str,
    id: &str,
) -> Result<usize> {
    super::delete_scoped_document(
        access,
        "desktop.event_source.delete",
        Collection::EventSource,
        node_did,
        id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::ServedRuntimeHome;
    use serde_json::{json, Value};

    #[tokio::test]
    async fn access_reviewer_setup_preserves_policy_and_rejects_task_side_effects() -> Result<()> {
        let runtime = ServedRuntimeHome::start().await;
        let owner = runtime.node_did();
        let binding = runtime.provision_agent().await;
        let agent = binding.agent_did.as_str();
        let config: gents_documents::PackConfig = serde_json::from_value(json!({
            "node":{"node_did":owner},
            "inference_backends":[{"node_did":owner,"backend_id":"backend","name":"Fixture","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}],
            "inference_profiles":[{"node_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model"}],
            "agents":[{"node_did":owner,"agent_did":agent,"creation_nonce":binding.creation_nonce,"inference_profile_id":"profile"}]
        }))?;
        let access = runtime.user_access();
        super::super::apply_plan(
            &access,
            "test.access_review.seed",
            DesiredStateApplyPlan::from_pack_config(&config)?,
        )
        .await?;
        let id = prepare_access_review_task_on(&access, owner, agent, None).await?;
        assert_eq!(
            prepare_access_review_task_on(&access, owner, agent, None).await?,
            id
        );
        let mut task = gents_tools::plugin::approval::workspace::default_review_task(owner, agent);
        assert_eq!(task.task_id, id.task_id);
        task.prompt_template = "Ask about changes outside the requested project.".into();
        upsert_task_on(&access, &task).await?;
        let updated = prepare_access_review_task_on(&access, owner, agent, None).await?;
        assert_eq!(updated.task_id, id.task_id);
        assert_ne!(updated.policy_digest, id.policy_digest);
        task.display_name = Some("Workspace access review".into());
        upsert_task_on(&access, &task).await?;
        assert_eq!(
            prepare_access_review_task_on(&access, owner, agent, None).await?,
            updated
        );
        let saved: Option<(String, Value)> = access
            .transact_readonly("test.access_review.read", |txn| {
                let id = &id.task_id;
                Box::pin(async move {
                    read_desired_state_record_in_txn(txn, Collection::Task, owner, id).await
                })
            })
            .await?;
        let saved = saved.unwrap().1;
        assert_eq!(
            serde_json::from_value::<Task>(saved)?.prompt_template,
            task.prompt_template
        );
        task.emit_outcome = true;
        upsert_task_on(&access, &task).await?;
        assert!(prepare_access_review_task_on(&access, owner, agent, None)
            .await
            .is_err());
        assert!(
            prepare_access_review_task_on(&access, owner, agent, Some("missing"))
                .await
                .is_err()
        );
        assert!(
            prepare_access_review_task_on(&access, owner, "missing-agent", None)
                .await
                .is_err()
        );
        runtime.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn automation_replacements_preserve_scoped_references_and_goal_hook_rules() -> Result<()>
    {
        let runtime = ServedRuntimeHome::start().await;
        let access = runtime.user_access();
        let automation_a = runtime.node_did().to_owned();
        let automation_b = runtime.publish_foreign_node().await;
        let agent_a = runtime.provision_agent().await;
        let agent_b = runtime.provision_agent().await;
        for (owner, binding) in [(&automation_a, &agent_a), (&automation_b, &agent_b)] {
            let config: gents_documents::PackConfig = serde_json::from_value(json!({
                "node":{"node_did":owner},
                "inference_backends":[{"node_did":owner,"backend_id":"backend","name":"Backend","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}],
                "inference_profiles":[{"node_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model"}],
                "agents":[{"node_did":owner,"agent_did":binding.agent_did,"creation_nonce":binding.creation_nonce,"inference_profile_id":"profile"}]
            }))?;
            let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
            access
                .transact_write("desktop.automation.seed", |txn| {
                    let plan = &plan;
                    Box::pin(async move {
                        apply_desired_state_plan(
                            txn,
                            plan,
                            &gents_inference::resolved::ProviderProfilePolicy,
                        )
                        .await?;
                        Ok::<_, anyhow::Error>(())
                    })
                })
                .await?;
        }
        let owner = automation_a.as_str();
        let agent = agent_a.agent_did.as_str();
        let mut task: Task = serde_json::from_value(
            json!({"node_did":owner,"task_id":"task","agent_did":agent,"prompt_template":"Do work","goal_objective_template":"Finish work","goal_token_budget":10000,"hooks":[{"hook_id":"prepare","phase":"before","command":["true"]}]}),
        )?;
        upsert_task_on(&access, &task).await?;
        task.goal_objective_template = None;
        assert!(upsert_task_on(&access, &task).await.is_err());
        task.goal_token_budget = None;
        upsert_task_on(&access, &task).await?;
        task.hooks[0].timeout_secs = Some(0);
        assert!(upsert_task_on(&access, &task).await.is_err());
        task.hooks[0].timeout_secs = None;
        let source: EventSource = serde_json::from_value(
            json!({"node_did":owner,"event_source_id":"source","source_collection":"AgentRequest"}),
        )?;
        upsert_event_source_on(&access, &source).await?;
        let mut trigger: Trigger = serde_json::from_value(
            json!({"node_did":owner,"trigger_id":"trigger","task_id":"task","source":{"kind":"event","event_source_id":"source"}}),
        )?;
        upsert_trigger_on(&access, &trigger).await?;
        assert!(delete_task_on(&access, owner, "task").await.is_err());
        assert!(delete_event_source_on(&access, owner, "source")
            .await
            .is_err());
        trigger.node_did = automation_b.as_str().to_owned();
        assert!(upsert_trigger_on(&access, &trigger).await.is_err());
        assert_eq!(
            delete_trigger_on(&access, automation_b.as_str(), "trigger").await?,
            0
        );
        trigger.node_did = owner.into();
        let schedule: Schedule = serde_json::from_value(
            json!({"node_did":owner,"schedule_id":"schedule","cadence":{"kind":"interval","interval_secs":60}}),
        )?;
        upsert_schedule_on(&access, &schedule).await?;
        trigger.source = gents_documents::TriggerSource::Schedule {
            schedule_id: "schedule".into(),
        };
        upsert_trigger_on(&access, &trigger).await?;
        assert_eq!(delete_event_source_on(&access, owner, "source").await?, 1);
        assert!(delete_schedule_on(&access, owner, "schedule")
            .await
            .is_err());
        access
            .transact_write("desktop.automation.verify", |txn| {
                Box::pin(async move {
                    let (_, value) =
                        read_desired_state_record_in_txn(txn, Collection::Task, owner, "task")
                            .await?
                            .unwrap();
                    let saved: Task = serde_json::from_value(value)?;
                    assert!(saved.goal_objective_template.is_none());
                    assert!(saved.goal_token_budget.is_none());
                    assert_eq!(saved.hooks.len(), 1);
                    assert_eq!(saved.hooks[0].command, vec!["true"]);
                    assert!(saved.hooks[0].timeout_secs.is_none());
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        assert_eq!(delete_trigger_on(&access, owner, "trigger").await?, 1);
        assert_eq!(delete_task_on(&access, owner, "task").await?, 1);
        assert_eq!(delete_schedule_on(&access, owner, "schedule").await?, 1);
        runtime.shutdown().await;
        Ok(())
    }
}
