use crate::triggers::engine::durable::{write_trigger_delivery_with_identity, PreparedFire};
use anyhow::Result;
use gents_documents::event_consumer::{acknowledge_fire, exclude_arrival};
use gents_documents::test_support::event_cursor::{
    create_source, cursor, document, fixture, publish, source, trigger, trigger_consumer,
};
use gents_store::collection::Collection;
use gents_store::test_support::PIN_FIXED_DID;
use serde_json::json;

#[tokio::test]
async fn persisted_disable_blocks_stale_admission_and_exclusion_but_not_committed_ack() -> Result<()>
{
    let fixture = fixture(true).await?;
    let node = &fixture.node;
    let access = &fixture.user;
    let agent = &fixture.agent;
    let agent_did = fixture.agent_identity.did();
    let doc_id = create_source(access, "WorkA", "held").await?;
    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: PIN_FIXED_DID.into(),
        trigger_id: "trigger".into(),
        source_collection: "WorkA".into(),
        source_doc_id: doc_id.clone(),
    };
    let key = crate::triggers::task_fire_key(&identity);
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
    let trigger_record = access
        .execute("{Trigger{_docID} EventSource{_docID} Task{_docID}}")
        .await?;
    let trigger_doc_id = trigger_record["data"]["Trigger"][0]["_docID"]
        .as_str()
        .unwrap();
    let spec = gents_session::request::lifecycle::materialize::pending_agent_request_spec(
        PIN_FIXED_DID,
        agent_did,
        "work",
        gents_loop::execution_origin::ExecutionOrigin::Scheduled,
        gents_session::request::lifecycle::TriggerLineage {
            trigger_id: Some("trigger".into()),
            trigger_kind: Some("event".into()),
            source_doc_id: Some(doc_id.clone()),
            correlation: None,
            trigger_context: None,
        },
        None,
        None,
        &fire.request_id,
        &fire.session_id,
        Some(&key),
        None,
        Some(trigger_doc_id),
    )?;
    let create = gents_session::request::lifecycle::build_signed_request(
        spec,
        gents_session::request::lifecycle::RequestSigner::Identity(fixture.agent_identity.as_ref()),
    )
    .await?;
    let generation = gents_documents::automation::event_admission_generation(
        &serde_json::from_value(source("WorkA").add)?,
        &serde_json::from_value(trigger(true).add)?,
        &serde_json::from_value(
            document(
                Collection::Task,
                json!({"task_id":"task", "agent_did":agent_did, "prompt_template":"work"}),
            )
            .add,
        )?,
        &gents_documents::automation::EventConfigDocIds {
            trigger: trigger_doc_id,
            source: trigger_record["data"]["EventSource"][0]["_docID"]
                .as_str()
                .unwrap(),
            task: trigger_record["data"]["Task"][0]["_docID"]
                .as_str()
                .unwrap(),
        },
    )?;
    let prepared = PreparedFire {
        receipt: fire.clone(),
        target_existing: false,
        create_session: false,
        max_fires_per_hour: gents_documents::DEFAULT_MAX_FIRES_PER_HOUR,
        replace_prior: false,
        event_source_generation: Some(generation),
    };
    publish(access, vec![trigger(false)]).await?;
    assert!(write_trigger_delivery_with_identity(
        node,
        identity::Did::new(agent_did)?,
        &prepared,
        &create,
        Some(fixture.agent_identity.as_ref())
    )
    .await
    .is_err());
    assert!(agent
        .transact_write("test.cursor.exclude_disabled", |txn| Box::pin(async move {
            exclude_arrival(txn, PIN_FIXED_DID, &trigger_consumer(), "WorkA", "1").await
        }))
        .await
        .is_err());
    assert_eq!(cursor(agent, "WorkA").await?, "0");
    let stored = access
        .execute("{TriggerFire{fire_key} AgentRequest{request_id}}")
        .await?;
    assert!(stored["data"]["TriggerFire"].as_array().unwrap().is_empty());
    assert!(stored["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());
    publish(access, vec![trigger(true)]).await?;
    let admitted = write_trigger_delivery_with_identity(
        node,
        identity::Did::new(agent_did)?,
        &prepared,
        &create,
        Some(fixture.agent_identity.as_ref()),
    )
    .await?;
    assert!(!admitted.duplicate);
    publish(access, vec![trigger(false)]).await?;
    let duplicate = write_trigger_delivery_with_identity(
        node,
        identity::Did::new(agent_did)?,
        &prepared,
        &create,
        Some(fixture.agent_identity.as_ref()),
    )
    .await?;
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.request.doc_id, admitted.request.doc_id);
    agent
        .transact_write("test.cursor.ack_disabled", |txn| {
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
    assert_eq!(cursor(agent, "WorkA").await?, "1");
    node.shutdown().await.expect("node shutdown");
    Ok(())
}
