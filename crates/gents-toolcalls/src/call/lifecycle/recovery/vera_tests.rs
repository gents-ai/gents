use gents_documents::{apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use gents_tools::test_support::vera_test_home::{Household, PROVIDERS};
use serde_json::json;

fn document(collection: Collection, value: serde_json::Value) -> DesiredStateApplyDocument {
    DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    }
}

/// The event automation the Engineer publishes: a Task and the Trigger that
/// starts it.
fn automation(node_did: &str, agent: &str) -> Vec<DesiredStateApplyDocument> {
    vec![
        document(
            Collection::Task,
            json!({"node_did": node_did, "task_id": "task", "agent_did": agent, "prompt_template": "hello"}),
        ),
        document(
            Collection::EventSource,
            json!({"node_did": node_did, "event_source_id": "source", "source_collection": "Task", "event_kind": "created"}),
        ),
        document(
            Collection::Trigger,
            json!({"node_did": node_did, "trigger_id": "trigger", "task_id": "task", "source": {"kind": "event", "event_source_id": "source"}}),
        ),
    ]
}

async fn publish(engineer: &ConfigAccess, node_did: &str, agent: &str) -> anyhow::Result<()> {
    let plan = DesiredStateApplyPlan::new(automation(node_did, agent))?;
    engineer
        .transact_write("test.recovery.publish_automation", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_documents::test_support::AdmitAllProfiles,
                )
                .await
                .map(|_| ())
            })
        })
        .await
}

/// Delete the one `collection` row whose `field` is `id` as the init User's
/// client does: only a User holds the node's document delete.
async fn delete(
    user: &ConfigAccess,
    collection: &str,
    field: &str,
    id: &str,
) -> anyhow::Result<()> {
    let deleted = user
        .write_with_variables(
            "test.recovery.delete_automation",
            &format!(
                r#"mutation($id: String) {{ delete_{collection}(filter: {{ {field}: {{ _eq: $id }} }}) {{ _docID }} }}"#
            ),
            &json!({ "id": id }),
        )
        .await?;
    anyhow::ensure!(
        deleted["data"][format!("delete_{collection}")]
            .as_array()
            .is_some_and(|rows| rows.len() == 1),
        "{collection}: {deleted}"
    );
    Ok(())
}

/// The request a Trigger started, as recovery reads its parent.
fn started_by_trigger(node_did: &str) -> gents_protocol::row::AgentRequestRow {
    gents_protocol::row::AgentRequestRow {
        request_id: "started-by-trigger".into(),
        node_did: Some(node_did.to_owned()),
        caused_by_trigger_id: Some("trigger".into()),
        ..Default::default()
    }
}

/// Recovery reads a background owner's Trigger, and while it is live its
/// Task, as the Node, with deleted documents shown. The Engineer creates them
/// and the init User deletes them. The Node reads them through their `parent`
/// Node, of which it is the runtime reader, and on a Vera home a deleted
/// document is decided as a live one, so recovery sees the Task, then the
/// Trigger, as deleted. Were either grant missing, the reads would come back
/// empty and recovery would decide the owner's task was not deleted.
#[tokio::test]
async fn recovery_sees_a_deleted_trigger_and_task_as_deleted() -> anyhow::Result<()> {
    for provider in PROVIDERS {
        let household = Household::open(provider).await?;
        let (engineer, _) = household.engineer().await?;
        let worker = household.derive_agent()?;
        household.create_agent(&worker).await?;
        let engineer_did = identity::Did::new(&engineer.agent_did)?;
        let node_did = household.node_did.to_string();
        let access = household.agent_access(&engineer)?.with_runtime_agent_proof(
            gents_store::identity::RuntimeAgentProof::new(household.home.node.clone()),
        );
        publish(&access, &node_did, &worker.agent_did).await?;
        household
            .settled(&engineer_did, "the Engineer's automation")
            .await?;
        let parent = started_by_trigger(&node_did);
        let live = super::owner_task_deleted(&household.node, &node_did, &parent).await?;

        let user = household.as_user()?;
        delete(&user, "Task", "task_id", "task").await?;
        household
            .settled(&household.user_did, "the Task's deletion")
            .await?;
        let task_deleted = super::owner_task_deleted(&household.node, &node_did, &parent).await?;
        delete(&user, "Trigger", "trigger_id", "trigger").await?;
        household
            .settled(&household.user_did, "the Trigger's deletion")
            .await?;
        let trigger_deleted =
            super::owner_task_deleted(&household.node, &node_did, &parent).await?;
        household.shutdown().await?;
        assert_eq!(
            (live, task_deleted, trigger_deleted),
            (false, true, true),
            "{provider}"
        );
    }
    Ok(())
}
