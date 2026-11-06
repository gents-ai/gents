use super::*;
use crate::request::lifecycle::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};
use crate::store::config_client::{
    apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
    NodePermission,
};
use crate::store::identity::NodeIdentity;
use crate::Collection;
use serde_json::Value;
use std::sync::Arc;

#[test]
fn callback_checkpoint_claim_markers_match_lean() {
    let cases = &crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap()
        .callback_checkpoint_claim_cases;
    assert!(!cases.is_empty(), "Lean emitted no callback claim cases");
    for case in cases {
        let claim_time = case["claim_time"].as_bool().unwrap();
        let retained_configuration = case["retained_configuration"].as_bool().unwrap();
        let row = json!({
            "claimed_at": claim_time.then_some("2024-01-01T00:00:00Z"),
            "claimed_callback": retained_configuration.then_some("{}"),
        });
        assert_eq!(
            callback_claim_recorded(&row),
            case["expected"].as_bool().unwrap(),
            "{case}"
        );
    }
}

fn document(collection: Collection, mut value: Value) -> DesiredStateApplyDocument {
    value["node_did"] = json!(PIN_FIXED_DID);
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
        .transact_write("test.cursor.publish", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
}

struct Fixture {
    _home: tempfile::TempDir,
    node: Arc<defra_node::Node>,
    user: ConfigAccess,
    agent: ConfigAccess,
    agent_identity: Arc<dyn NodeIdentity>,
}

async fn fixture(enabled: bool) -> Result<Fixture> {
    let home = tempfile::tempdir()?;
    let root = pin_fixed_signing_identity(home.path());
    let user_binding = crate::store::identity::provision_agent_identity(&root)?;
    let agent_binding = crate::store::identity::provision_agent_identity(&root)?;
    let user_identity = crate::store::identity::resolve_agent_identity(&root, &user_binding)?;
    let agent_identity = crate::store::identity::resolve_agent_identity(&root, &agent_binding)?;
    let node = Arc::new(
        crate::test_support::node_builder()
            .with_node_identity_did(PIN_FIXED_DID)
            .with_node_acp_enabled()
            .build()
            .await?,
    );
    crate::ensure_runtime_schemas(&node).await?;
    let runtime = ConfigAccess::local_as(node.clone(), identity::Did::new(PIN_FIXED_DID)?);
    for did in [user_identity.did(), agent_identity.did()] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
        ] {
            runtime
                .grant_node_permission(&identity::Did::new(did)?, permission)
                .await?;
        }
    }
    for permission in [
        NodePermission::CollectionPatch,
        NodePermission::CollectionGet,
    ] {
        runtime
            .grant_node_permission(&identity::Did::new(user_identity.did())?, permission)
            .await?;
    }
    let user = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(user_identity.did())?,
        user_identity.as_ref(),
    )?;
    let agent = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent_identity.did())?,
        agent_identity.as_ref(),
    )?;
    user.transact_write("test.cursor.node", |txn| {
        Box::pin(async move {
            let response = txn
                .execute_client_signed_create(
                    "mutation($input: NodeMutationInputArg!) {create_Node(input:$input){_docID}}",
                    &json!({"input":{"node_did":PIN_FIXED_DID}}),
                    gents_protocol::schemas::NODE,
                )
                .await?;
            let doc = crate::store::graphql::created_doc_id(&response, "Node")?;
            txn.add_actor_relationship(
                "Node",
                &doc,
                "runtime_reader",
                &identity::Did::new(PIN_FIXED_DID)?,
            )
            .await?;
            Ok::<_, anyhow::Error>(())
        })
    })
    .await?;
    user.add_collection(
        "type WorkA { label: String @immutable } type WorkB { label: String @immutable }",
    )
    .await?;
    publish(&user, vec![
        document(Collection::InferenceBackend, json!({"backend_id":"backend", "name":"Test", "provider_kind":"OpenAiCompatible", "endpoint":"http://127.0.0.1:8000/v1", "auth":{"kind":"unauthenticated"}})),
        document(Collection::InferenceProfile, json!({"profile_id":"profile", "backend_id":"backend", "model_name":"model"})),
        document(Collection::Agent, json!({"agent_did":agent_identity.did(), "creation_nonce":agent_binding.creation_nonce, "inference_profile_id":"profile"})),
        document(Collection::Task, json!({"task_id":"task", "agent_did":agent_identity.did(), "prompt_template":"work"})),
        source("WorkA"), trigger(enabled),
    ]).await?;
    Ok(Fixture {
        _home: home,
        node,
        user,
        agent,
        agent_identity,
    })
}

async fn create_source(access: &ConfigAccess, collection: &str, label: &str) -> Result<String> {
    crate::store::graphql::validate_collection_identifier(collection)?;
    access
        .transact_write("test.cursor.source", |txn| {
            Box::pin(async move {
                let field = format!("create_{collection}");
                let response = txn
                    .execute(&format!(
                        "mutation {{{field}(input: {{label: \"{}\"}}) {{_docID}}}}",
                        escape_graphql_string(label)
                    ))
                    .await?;
                Ok::<_, anyhow::Error>(crate::store::graphql::created_doc_id(
                    &response, collection,
                )?)
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
        .transact_write("test.cursor.saved", |txn| {
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
    let key = crate::request::lifecycle::task_fire_key(&identity);
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
    let spec = crate::request::lifecycle::materialize::pending_agent_request_spec(
        PIN_FIXED_DID,
        agent_did,
        "work",
        crate::request::lifecycle::ExecutionOrigin::Scheduled,
        crate::request::lifecycle::TriggerLineage {
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
    let create = crate::request::lifecycle::build_signed_request(
        spec,
        crate::request::lifecycle::RequestSigner::Identity(fixture.agent_identity.as_ref()),
    )
    .await?;
    let generation = crate::triggers::engine::event_delivery::event_admission_generation(
        &serde_json::from_value(source("WorkA").add)?,
        &serde_json::from_value(trigger(true).add)?,
        &serde_json::from_value(
            document(
                Collection::Task,
                json!({"task_id":"task", "agent_did":agent_did, "prompt_template":"work"}),
            )
            .add,
        )?,
        &crate::triggers::engine::event_delivery::EventConfigDocIds {
            trigger: trigger_doc_id,
            source: trigger_record["data"]["EventSource"][0]["_docID"]
                .as_str()
                .unwrap(),
            task: trigger_record["data"]["Task"][0]["_docID"]
                .as_str()
                .unwrap(),
        },
    )?;
    let prepared = crate::triggers::engine::durable::PreparedFire {
        receipt: fire.clone(),
        target_existing: false,
        replace_prior: false,
        event_source_generation: Some(generation),
    };
    publish(access, vec![trigger(false)]).await?;
    assert!(
        crate::request::lifecycle::materialize::write_trigger_delivery_with_identity(
            node,
            identity::Did::new(agent_did)?,
            &prepared,
            &create,
            Some(fixture.agent_identity.as_ref())
        )
        .await
        .is_err()
    );
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
    let admitted = crate::request::lifecycle::materialize::write_trigger_delivery_with_identity(
        node,
        identity::Did::new(agent_did)?,
        &prepared,
        &create,
        Some(fixture.agent_identity.as_ref()),
    )
    .await?;
    assert!(!admitted.duplicate);
    publish(access, vec![trigger(false)]).await?;
    let duplicate = crate::request::lifecycle::materialize::write_trigger_delivery_with_identity(
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

#[tokio::test]
async fn held_agent_cursor_seed_uses_runtime_observed_node_binding() -> Result<()> {
    let fixture = fixture(false)
        .await
        .context("build the held-Agent cursor fixture")?;
    let agent = &fixture.agent;
    let hidden = agent.execute("{ Node { _docID } }").await?;
    assert!(hidden["data"]["Node"].as_array().unwrap().is_empty());
    let node = fixture.user.execute("{ Node { _docID node_did } }").await?;
    let node_doc = node["data"]["Node"][0]["_docID"]
        .as_str()
        .context("fixture Node has no physical ID")?
        .to_owned();
    let binding = super::super::ConfigurationNodeBinding {
        node_did: identity::Did::new(PIN_FIXED_DID)?,
        doc_id: node_doc,
        enabled: true,
        default_agent_did: None,
    };
    let without_binding = agent
        .transact_write("test.cursor.no_node_binding", |txn| {
            Box::pin(async move {
                load_or_seed_for_source(txn, PIN_FIXED_DID, &trigger_consumer(), "WorkB").await
            })
        })
        .await;
    assert!(
        without_binding.is_err(),
        "the Agent cannot substitute for the Node"
    );
    let agent_did = fixture.agent_identity.did().to_owned();
    let record = agent
        .transact_write("test.cursor.held_node_binding", |txn| {
            let binding = &binding;
            let agent_did = &agent_did;
            Box::pin(async move {
                assert_eq!(txn.actor_did(), Some(agent_did.as_str()));
                load_or_seed_for_source_with_node_binding(
                    txn,
                    PIN_FIXED_DID,
                    &trigger_consumer(),
                    "WorkB",
                    Some(binding),
                )
                .await
            })
        })
        .await
        .context("seed a cursor using the held Node binding")?;
    assert!(!record.doc_id.is_empty());
    assert_eq!(record.cursor.source_collection, "WorkB");
    assert_eq!(cursor(agent, "WorkB").await?, record.cursor.after);

    let mut new_source = source("WorkA");
    new_source.add["event_source_id"] = json!("source-agent-created");
    new_source.update["event_source_id"] = json!("source-agent-created");
    let mut new_trigger = trigger(false);
    new_trigger.add["trigger_id"] = json!("trigger-agent-created");
    new_trigger.update["trigger_id"] = json!("trigger-agent-created");
    new_trigger.add["source"]["event_source_id"] = json!("source-agent-created");
    new_trigger.update["source"]["event_source_id"] = json!("source-agent-created");
    let plan = DesiredStateApplyPlan::new(vec![new_source, new_trigger])?;
    agent
        .transact_write("test.cursor.agent_trigger", |txn| {
            let plan = &plan;
            let binding = &binding;
            Box::pin(async move {
                super::super::apply_desired_state_plan_with_node_binding(txn, plan, Some(binding))
                    .await
            })
        })
        .await
        .context("create an Agent-owned source and Trigger with the bound Node")?;
    let consumer = EventConsumer::Trigger {
        trigger_id: "trigger-agent-created".into(),
    };
    let seeded = agent
        .transact_readonly("test.cursor.agent_trigger_read", |txn| {
            let consumer = &consumer;
            Box::pin(async move { load_for_source(txn, PIN_FIXED_DID, consumer, "WorkA").await })
        })
        .await?;
    assert!(
        seeded.is_some(),
        "configuration committed its initial cursor"
    );
    fixture.node.shutdown().await.expect("node shutdown");
    Ok(())
}

#[tokio::test]
async fn source_only_replacement_seeds_disabled_consumers_before_later_arrivals() -> Result<()> {
    let fixture = fixture(false).await?;
    let node = &fixture.node;
    let access = &fixture.user;
    let agent = &fixture.agent;
    create_source(access, "WorkB", "before-replacement").await?;
    publish(access, vec![source("WorkB")]).await?;
    let held = create_source(access, "WorkB", "after-replacement").await?;
    assert_eq!(cursor(agent, "WorkB").await?, "1");
    publish(access, vec![trigger(true)]).await?;
    assert_eq!(cursor(agent, "WorkB").await?, "1");
    let page = access.execute("{_documentArrivals(collection:\"WorkB\",after:\"1\",limit:128){entries{docID cursor}}}").await?;
    assert_eq!(
        page["data"]["_documentArrivals"]["entries"],
        json!([{"docID":held,"cursor":"2"}])
    );
    assert!(agent
        .transact_write("test.cursor.stale_source", |txn| Box::pin(async move {
            exclude_arrival(txn, PIN_FIXED_DID, &trigger_consumer(), "WorkA", "1").await
        }))
        .await
        .is_err());
    assert_eq!(cursor(agent, "WorkA").await?, "0");
    publish(access, vec![source("WorkA")]).await?;
    assert_eq!(cursor(agent, "WorkA").await?, "0");
    assert_eq!(cursor(agent, "WorkB").await?, "1");
    node.shutdown().await.expect("node shutdown");
    Ok(())
}

#[tokio::test]
async fn source_registered_before_collection_retains_its_first_arrival() -> Result<()> {
    let fixture = fixture(false).await?;
    let node = &fixture.node;
    let access = &fixture.user;
    let agent = &fixture.agent;
    publish(access, vec![source("FutureWork")]).await?;
    assert_eq!(cursor(agent, "FutureWork").await?, "0");
    access
        .add_collection("type FutureWork { label: String @immutable }")
        .await?;
    let first = create_source(access, "FutureWork", "first").await?;
    publish(access, vec![trigger(true)]).await?;
    assert_eq!(cursor(agent, "FutureWork").await?, "0");
    let page = access.execute("{_documentArrivals(collection:\"FutureWork\",after:\"0\",limit:128){entries{docID cursor}}}").await?;
    assert_eq!(
        page["data"]["_documentArrivals"]["entries"],
        json!([{"docID":first,"cursor":"1"}])
    );
    node.shutdown().await.expect("node shutdown");
    Ok(())
}
