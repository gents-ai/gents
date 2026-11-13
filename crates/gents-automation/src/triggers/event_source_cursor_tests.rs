use crate::triggers::engine::durable::{write_trigger_delivery_with_identity, PreparedFire};
use anyhow::Result;
use gents_documents::event_consumer::exclude_arrival;
use gents_documents::test_support::event_cursor::{
    create_source, cursor, document, fixture, publish, source, trigger, trigger_consumer,
};
use gents_store::collection::Collection;
use gents_store::test_support::PIN_FIXED_DID;
use serde_json::json;

#[tokio::test]
async fn trigger_cursor_traces_match_the_durable_model() -> Result<()> {
    use gents_lean_vocab::families::trigger_cursor_trace as trace;
    for case in gents_lean_vocab::families::trigger_cursor_trace::cases::<String, String>() {
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
        let mut admitted_request = None;
        let mut observed = Vec::new();
        for step in &case.input.steps {
            let (admission, checkpointed) = match step.as_str() {
                "disable" => {
                    publish(access, vec![trigger(false)]).await?;
                    (None, None)
                }
                "enable" => {
                    publish(access, vec![trigger(true)]).await?;
                    (None, None)
                }
                "admit" => {
                    let result = write_trigger_delivery_with_identity(
                        node,
                        identity::Did::new(agent_did)?,
                        &prepared,
                        &create,
                        Some(fixture.agent_identity.as_ref()),
                    )
                    .await;
                    let admission = match result {
                        Err(_) => "refused",
                        Ok(delivery) => {
                            let first = admitted_request.get_or_insert(delivery.request.doc_id.clone());
                            assert_eq!(*first, delivery.request.doc_id, "{:?}", case.input);
                            if delivery.duplicate {
                                "duplicate"
                            } else {
                                "admitted"
                            }
                        }
                    };
                    (Some(admission.to_owned()), None)
                }
                "checkpoint" => {
                    let accepted = agent
                        .transact_write("test.cursor.checkpoint", |txn| {
                            Box::pin(async move {
                                exclude_arrival(txn, PIN_FIXED_DID, &trigger_consumer(), "WorkA", "1")
                                    .await
                            })
                        })
                        .await
                        .is_ok();
                    (None, Some(accepted))
                }
                other => panic!("unknown trigger step {other}"),
            };
            let stored = access
                .execute("{TriggerFire{fire_key} AgentRequest{request_id}}")
                .await?;
            observed.push(trace::TriggerObservation {
                admission,
                checkpointed,
                fires: stored["data"]["TriggerFire"].as_array().unwrap().len() as u64,
                requests: stored["data"]["AgentRequest"].as_array().unwrap().len() as u64,
                cursor: cursor(agent, "WorkA").await?.parse()?,
            });
        }
        case.check(observed);
        node.shutdown().await.expect("node shutdown");
    }
    Ok(())
}
