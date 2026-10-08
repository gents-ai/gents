use super::*;
use crate::config_client::{
    apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::lifecycle::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};
use crate::Collection;
use serde_json::Value;
use std::sync::Arc;

fn document(collection: Collection, mut value: Value) -> DesiredStateApplyDocument {
    value["agent_did"] = json!(PIN_FIXED_DID);
    DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    }
}

fn source(collection: &str) -> DesiredStateApplyDocument {
    document(
        Collection::EventSource,
        json!({
            "event_source_id":"source", "source_collection":collection, "event_kind":"created"
        }),
    )
}

fn trigger(enabled: bool) -> DesiredStateApplyDocument {
    document(
        Collection::Trigger,
        json!({
            "trigger_id":"trigger", "task_id":"task", "enabled":enabled, "concurrency":"queued_serial",
            "source":{"kind":"event","event_source_id":"source"}
        }),
    )
}

async fn publish(access: &ConfigAccess, docs: Vec<DesiredStateApplyDocument>) -> Result<()> {
    let plan = DesiredStateApplyPlan::new(docs)?;
    access
        .transact("test.cursor.publish", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
}

async fn fixture(enabled: bool) -> Result<(Arc<defra_node::EmbeddedNode>, ConfigAccess)> {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await?);
    crate::ensure_runtime_schemas(&node).await?;
    let access = ConfigAccess::Local(node.clone());
    access
        .add_schema(
            "type WorkA { label: String @immutable } type WorkB { label: String @immutable }",
        )
        .await?;
    publish(&access, vec![
        document(Collection::InferenceBackend, json!({"backend_id":"backend", "name":"Test", "provider_kind":"OpenAiCompatible", "endpoint":"http://127.0.0.1:8000/v1", "auth":{"kind":"unauthenticated"}})),
        document(Collection::InferenceProfile, json!({"profile_id":"profile", "backend_id":"backend", "model_name":"model"})),
        document(Collection::AgentBehavior, json!({"behavior_id":"behavior", "inference_profile_id":"profile"})),
        document(Collection::Task, json!({"task_id":"task", "behavior_id":"behavior", "prompt_template":"work"})),
        source("WorkA"), trigger(enabled),
    ]).await?;
    Ok((node, access))
}

async fn create_source(access: &ConfigAccess, collection: &str, label: &str) -> Result<String> {
    crate::graphql::validate_collection_identifier(collection)?;
    access
        .transact("test.cursor.source", |txn| {
            Box::pin(async move {
                let field = format!("create_{collection}");
                let response = txn
                    .execute(&format!(
                        "mutation {{{field}(input: {{label: \"{}\"}}) {{_docID}}}}",
                        escape_graphql_string(label)
                    ))
                    .await?;
                crate::graphql::created_doc_id(&response, collection)
            })
        })
        .await
}

fn trigger_consumer() -> EventConsumer {
    EventConsumer::Trigger {
        trigger_id: "trigger".into(),
    }
}

async fn cursor(access: &ConfigAccess, collection: &str) -> Result<String> {
    access
        .transact("test.cursor.saved", |txn| {
            Box::pin(async move {
                Ok(
                    load_or_seed_for_source(txn, PIN_FIXED_DID, &trigger_consumer(), collection)
                        .await?
                        .cursor
                        .after,
                )
            })
        })
        .await
}

#[tokio::test]
async fn persisted_disable_blocks_stale_admission_and_exclusion_but_not_committed_ack() -> Result<()>
{
    let signing_home = tempfile::tempdir()?;
    let _identity = pin_fixed_signing_identity(signing_home.path());
    let (node, access) = fixture(true).await?;
    let doc_id = create_source(&access, "WorkA", "held").await?;
    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: PIN_FIXED_DID.into(),
        trigger_id: "trigger".into(),
        source_collection: "WorkA".into(),
        source_doc_id: doc_id.clone(),
    };
    let key = crate::lifecycle::task_fire_key(&identity);
    let fire = gents_protocol::trigger_delivery::TriggerFire {
        fire_key: key.clone(),
        identity,
        task_id: "task".into(),
        request_id: format!("trigger-request:{key}"),
        session_id: format!("trigger-session:{key}"),
        goal_id: None,
        goal_objective: None,
        goal_token_budget: None,
        goal_assignment_applied: false,
        emit_outcome: false,
        queued_serial: true,
        source_handoff_id: None,
        reply_session_id: None,
        shard_id: None,
        attempt: None,
        created_at: "2026-01-01T00:00:00Z".into(),
    };
    let trigger_record = access.execute("{Trigger{_docID}}").await?;
    let trigger_doc_id = trigger_record["data"]["Trigger"][0]["_docID"]
        .as_str()
        .unwrap();
    let create = crate::lifecycle::build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title(
        PIN_FIXED_DID, "behavior", "work", crate::lifecycle::ExecutionOrigin::Scheduled,
        crate::lifecycle::TriggerLineage {trigger_id:Some("trigger".into()), trigger_kind:Some("event".into()), source_doc_id:Some(doc_id.clone()), correlation:None, trigger_context:None},
        None, None, &fire.request_id, &fire.session_id, Some(&key), None, Some(trigger_doc_id),
    ).await?;
    publish(&access, vec![trigger(false)]).await?;
    assert!(
        crate::lifecycle::write_task_delivery(&access, &fire, false, &create)
            .await
            .is_err()
    );
    assert!(access
        .transact("test.cursor.exclude_disabled", |txn| Box::pin(async move {
            exclude_arrival(txn, PIN_FIXED_DID, &trigger_consumer(), "WorkA", "1").await
        }))
        .await
        .is_err());
    assert_eq!(cursor(&access, "WorkA").await?, "0");
    let stored = access
        .execute("{TriggerFire{fire_key} AgentRequest{request_id}}")
        .await?;
    assert!(stored["data"]["TriggerFire"].as_array().unwrap().is_empty());
    assert!(stored["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());
    publish(&access, vec![trigger(true)]).await?;
    let admitted = crate::lifecycle::write_task_delivery(&access, &fire, false, &create).await?;
    assert!(!admitted.duplicate);
    publish(&access, vec![trigger(false)]).await?;
    let duplicate = crate::lifecycle::write_task_delivery(&access, &fire, false, &create).await?;
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.request.doc_id, admitted.request.doc_id);
    access
        .transact("test.cursor.ack_disabled", |txn| {
            let doc_id = &doc_id;
            Box::pin(async move {
                acknowledge_fire(
                    txn,
                    PIN_FIXED_DID,
                    &trigger_consumer(),
                    "WorkA",
                    doc_id,
                    "1",
                )
                .await
            })
        })
        .await?;
    assert_eq!(cursor(&access, "WorkA").await?, "1");
    node.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn source_only_replacement_seeds_disabled_consumers_before_later_arrivals() -> Result<()> {
    let (node, access) = fixture(false).await?;
    create_source(&access, "WorkB", "before-replacement").await?;
    publish(&access, vec![source("WorkB")]).await?;
    let held = create_source(&access, "WorkB", "after-replacement").await?;
    assert_eq!(cursor(&access, "WorkB").await?, "1");
    publish(&access, vec![trigger(true)]).await?;
    assert_eq!(cursor(&access, "WorkB").await?, "1");
    let page = access.execute("{_documentArrivals(collection:\"WorkB\",after:\"1\",limit:128){entries{docID cursor}}}").await?;
    assert_eq!(
        page["data"]["_documentArrivals"]["entries"],
        json!([{"docID":held,"cursor":"2"}])
    );
    assert!(access
        .transact("test.cursor.stale_source", |txn| Box::pin(async move {
            exclude_arrival(txn, PIN_FIXED_DID, &trigger_consumer(), "WorkA", "1").await
        }))
        .await
        .is_err());
    assert_eq!(cursor(&access, "WorkA").await?, "0");
    publish(&access, vec![source("WorkA")]).await?;
    assert_eq!(cursor(&access, "WorkA").await?, "0");
    assert_eq!(cursor(&access, "WorkB").await?, "1");
    node.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn source_registered_before_collection_retains_its_first_arrival() -> Result<()> {
    let (node, access) = fixture(false).await?;
    publish(&access, vec![source("FutureWork")]).await?;
    assert_eq!(cursor(&access, "FutureWork").await?, "0");
    access
        .add_schema("type FutureWork { label: String @immutable }")
        .await?;
    let first = create_source(&access, "FutureWork", "first").await?;
    publish(&access, vec![trigger(true)]).await?;
    assert_eq!(cursor(&access, "FutureWork").await?, "0");
    let page = access.execute("{_documentArrivals(collection:\"FutureWork\",after:\"0\",limit:128){entries{docID cursor}}}").await?;
    assert_eq!(
        page["data"]["_documentArrivals"]["entries"],
        json!([{"docID":first,"cursor":"1"}])
    );
    node.shutdown().await;
    Ok(())
}
