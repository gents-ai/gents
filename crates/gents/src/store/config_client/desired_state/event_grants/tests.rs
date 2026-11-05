use super::*;
use crate::store::config_client::{
    apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::store::graphql::escape_graphql_string;
use crate::store::identity::NodeIdentity;
use serde_json::{json, Value};
use std::sync::Arc;

fn document(collection: Collection, value: Value) -> DesiredStateApplyDocument {
    DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    }
}

async fn apply(access: &ConfigAccess, documents: Vec<DesiredStateApplyDocument>) -> Result<()> {
    let plan = DesiredStateApplyPlan::new(documents)?;
    access
        .transact_write("test.event_grants.apply", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
}

async fn event_admitted(
    access: &ConfigAccess,
    fire: &gents_protocol::trigger_delivery::TriggerFire,
    generation: &Value,
) -> bool {
    access
        .transact_readonly("test.event_grants.admission", |txn| {
            Box::pin(async move {
                crate::store::config_client::event_source_cursor::validate_event_admission(
                    txn, fire, generation,
                )
                .await
            })
        })
        .await
        .is_ok()
}

async fn cursor_update(access: &ConfigAccess, doc: &str, value: &str) -> Result<bool> {
    match access
        .write(
            "test.event_grants.cursor_update",
            &format!(
        "mutation {{update_EventSourceCursor(docID:\"{}\",input:{{after:\"{}\"}}){{_docID}}}}",
        escape_graphql_string(doc), escape_graphql_string(value),
    ),
        )
        .await
    {
        Ok(response) => Ok(!response["data"]["update_EventSourceCursor"]
            .as_array()
            .context("cursor update omitted rows")?
            .is_empty()),
        Err(error) if error.is_graphql_rejection() => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn modeled_source(generation: &Value, owner: &str) -> Value {
    let expected = if generation["expected_count"].is_number() {
        generation["expected_count"].clone()
    } else if generation["expected_field"].is_string() {
        json!({"source_field":generation["expected_field"]})
    } else {
        Value::Null
    };
    let group = if generation["grouped"] == true {
        json!({"expected_count":expected,"timeout_secs":generation["timeout"],"min_count":generation["minimum"]})
    } else {
        Value::Null
    };
    json!({"node_did":owner,"event_source_id":"modeled-source", "source_collection":generation["collection"],
        "event_kind":generation["event_kind"],"filter":generation["filter"],"correlation_field":generation["correlation"],
        "group":group,"workspace_authority":generation["workspace_authority"]})
}

#[tokio::test]
async fn event_configuration_publication_rebinding_and_revocation_use_native_grants() -> Result<()>
{
    let contract = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let modeled_agent = &contract
        .objects
        .iter()
        .find(|object| object.id == "agent-created")
        .unwrap()
        .owner;
    let modeled_sibling = contract
        .event_configuration_cases
        .iter()
        .find(|case| {
            case["id"] == "source" && case["revoked"] == false && case["expected"] == false
        })
        .unwrap()["actor"]
        .as_str()
        .unwrap();
    let expected = |actor: &str, id: &str, revoked: bool, rebound: bool, post_revoke: bool| {
        contract
            .event_configuration_cases
            .iter()
            .find(|case| {
                case["actor"] == actor
                    && case["id"] == id
                    && case["permission"] == "read"
                    && case["revoked"] == revoked
                    && case["rebound"] == rebound
                    && (case["post_revoke"] == true) == post_revoke
            })
            .unwrap()["expected"]
            .as_bool()
            .unwrap()
    };
    let expected_claim = |actor: &str, revoked: bool, rebound: bool, post_revoke: bool| {
        contract
            .event_configuration_cases
            .iter()
            .find(|case| {
                case["actor"] == actor
                    && case["id"] == "source"
                    && case["permission"] == "read"
                    && case["revoked"] == revoked
                    && case["rebound"] == rebound
                    && (case["post_revoke"] == true) == post_revoke
            })
            .unwrap()["claim_allowed"]
            .as_bool()
            .unwrap()
    };
    let cursor_expected = |actor: &str, revoked: bool| {
        contract
            .event_cursor_rebind_cases
            .iter()
            .find(|case| case["actor"] == actor && case["revoked"] == revoked)
            .unwrap()["expected"]
            .as_bool()
            .unwrap()
    };
    let directory = tempfile::tempdir()?;
    let root = crate::KeyIdentity::load_or_create(directory.path().join("node.key"), None)?;
    let owner_binding = crate::store::identity::provision_agent_identity(&root)?;
    let first_binding = crate::store::identity::provision_agent_identity(&root)?;
    let second_binding = crate::store::identity::provision_agent_identity(&root)?;
    let owner = crate::store::identity::resolve_agent_identity(&root, &owner_binding)?;
    let first = crate::store::identity::resolve_agent_identity(&root, &first_binding)?;
    let second = crate::store::identity::resolve_agent_identity(&root, &second_binding)?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(root.did())
            .with_node_acp_enabled()
            .build()
            .await?,
    );
    let user = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(owner.did())?,
        owner.as_ref(),
    )?;
    let runtime = ConfigAccess::local_as(node.clone(), identity::Did::new(root.did())?);
    for did in [owner.did(), first.did(), second.did()] {
        for permission in [
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::DocumentUpdate,
            acp::nac::NodePermission::DacRelationAdd,
            acp::nac::NodePermission::DacRelationDelete,
        ] {
            runtime
                .grant_node_permission(&identity::Did::new(did)?, permission)
                .await?;
        }
    }
    for permission in [
        acp::nac::NodePermission::CollectionPatch,
        acp::nac::NodePermission::CollectionGet,
        acp::nac::NodePermission::DacPolicyAdd,
    ] {
        runtime
            .grant_node_permission(&identity::Did::new(owner.did())?, permission)
            .await?;
    }
    let first_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(first.did())?,
        first.as_ref(),
    )?;
    let second_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(second.did())?,
        second.as_ref(),
    )?;
    let context = defra_core::RequestContext::default().with_identity(Some(owner.did().to_owned()));
    node.add_dac_policy_with_counter_as(
        &context,
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await?;
    for (name, schema) in gents_protocol::schemas::RUNTIME_COLLECTION_NAMES
        .iter()
        .zip(gents_protocol::schemas::RUNTIME_ALL)
        .chain(
            gents_protocol::schemas::ALL_COLLECTION_NAMES
                .iter()
                .zip(gents_protocol::schemas::ALL),
        )
    {
        if Collection::ALL
            .iter()
            .any(|collection| collection.graphql_type() == *name)
        {
            user.add_collection(schema).await?;
        }
    }
    user.add_collection(gents_protocol::schemas::EVENT_SOURCE_CURSOR)
        .await?;
    user.add_collection(gents_protocol::schemas::AGENT_SESSION)
        .await?;
    user.add_collection(gents_protocol::schemas::AGENT_REQUEST)
        .await?;
    user.add_collection(gents_protocol::schemas::TRIGGER_FIRE)
        .await?;
    user.add_collection(gents_protocol::schemas::CALLBACK_INVOCATION)
        .await?;
    user.add_collection("type EventPolicyInput { value: String }")
        .await?;
    let runtime_did = root.did();
    let node_doc = user
        .transact_write("test.event_grants.node", |txn| {
            Box::pin(async move {
                let response = txn
                    .execute(&format!(
                        "mutation {{create_Node(input:{{node_did:\"{}\"}}){{_docID}}}}",
                        escape_graphql_string(runtime_did)
                    ))
                    .await?;
                let doc = crate::store::graphql::created_doc_id(&response, "Node")?;
                txn.add_actor_relationship(
                    "Node",
                    &doc,
                    "runtime_reader",
                    &identity::Did::new(runtime_did)?,
                )
                .await?;
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await?;
    for (agent, nonce) in [
        (first.as_ref(), &first_binding.creation_nonce),
        (second.as_ref(), &second_binding.creation_nonce),
    ] {
        user.transact_write("test.event_grants.agent", |txn| {
            let node_doc = &node_doc;
            Box::pin(async move {
                let response = txn.execute(&format!("mutation {{create_Agent(input:{{node_did:\"{}\",agent_did:\"{}\",creation_nonce:\"{}\",inference_profile_id:\"fixture-profile\"}}){{_docID}}}}", escape_graphql_string(runtime_did), escape_graphql_string(agent.did()), escape_graphql_string(nonce))).await?;
                let agent_doc = crate::store::graphql::created_doc_id(&response, "Agent")?;
                txn.add_resource_relationship("Agent", &agent_doc, "parent", "nodes", node_doc).await?;
                txn.add_actor_relationship("Agent", &agent_doc, "executor", &identity::Did::new(agent.did())?).await?;
                Ok::<_, anyhow::Error>(())
            })
        }).await?;
    }
    let task = |agent: &str| json!({"node_did":root.did(),"task_id":"task","agent_did":agent,"prompt_template":"hello"});
    apply(&user, vec![document(Collection::Task, task(first.did()))]).await?;
    let source = json!({"node_did":root.did(),"event_source_id":"source","source_collection":"EventPolicyInput","event_kind":"created"});
    let trigger = json!({"node_did":root.did(),"trigger_id":"trigger","task_id":"task","source":{"kind":"event","event_source_id":"source"}});
    let configs = vec![
        document(Collection::EventSource, source.clone()),
        document(Collection::Trigger, trigger.clone()),
    ];
    apply(&user, configs.clone()).await?;
    let rows = user
        .execute("{Trigger {_docID} EventSource {_docID} Task {_docID}}")
        .await?;
    let trigger_doc = rows["data"]["Trigger"][0]["_docID"].as_str().unwrap();
    let source_doc = rows["data"]["EventSource"][0]["_docID"].as_str().unwrap();
    let task_doc = rows["data"]["Task"][0]["_docID"].as_str().unwrap();
    let ids = crate::triggers::engine::event_delivery::EventConfigDocIds {
        trigger: trigger_doc,
        source: source_doc,
        task: task_doc,
    };
    let generation_for = |agent: &str| {
        crate::triggers::engine::event_delivery::event_admission_generation(
            &serde_json::from_value(source.clone())?,
            &serde_json::from_value(trigger.clone())?,
            &serde_json::from_value(task(agent))?,
            &ids,
        )
    };
    let generation = generation_for(first.did())?;
    let rebound_generation = generation_for(second.did())?;
    let cursor_rows = user.execute("{EventSourceCursor {_docID}}").await?;
    let cursor_doc = cursor_rows["data"]["EventSourceCursor"][0]["_docID"]
        .as_str()
        .unwrap();
    let query = "{Trigger {_docID} EventSource {_docID}}";
    let arrival = user
        .write(
            "test.event_grants.arrival",
            "mutation {create_EventPolicyInput(input:{value:\"one\"}){_docID}}",
        )
        .await?;
    let arrival_doc = crate::store::graphql::created_doc_id(&arrival, "EventPolicyInput")?;
    let fire_identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: root.did().into(),
        trigger_id: "trigger".into(),
        source_collection: "EventPolicyInput".into(),
        source_doc_id: arrival_doc,
    };
    let fire = gents_protocol::trigger_delivery::TriggerFire {
        fire_key: fire_identity.fire_key(),
        request_id: fire_identity.request_id(),
        session_id: fire_identity.session_id(),
        identity: fire_identity,
        task_id: "task".into(),
        goal_id: None,
        goal_objective: None,
        goal_token_budget: None,
        goal_assignment_applied: false,
        emit_outcome: false,
        queued_serial: false,
        source_handoff_id: None,
        reply_session_id: None,
        shard_id: None,
        attempt: None,
        created_at: "2030-01-01T00:00:00Z".into(),
    };
    let selected = first_access.execute(query).await?;
    assert_eq!(
        !selected["data"]["Trigger"].as_array().unwrap().is_empty(),
        expected(modeled_agent, "trigger", false, false, false)
    );
    assert_eq!(
        !selected["data"]["EventSource"]
            .as_array()
            .unwrap()
            .is_empty(),
        expected(modeled_agent, "source", false, false, false)
    );
    assert_eq!(
        event_admitted(&first_access, &fire, &generation).await,
        expected_claim(modeled_agent, false, false, false)
    );
    let agent_rows = user.execute("{Agent {_docID agent_did}}").await?;
    let first_doc = agent_rows["data"]["Agent"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["agent_did"] == first.did())
        .unwrap()["_docID"]
        .as_str()
        .unwrap();
    let agent_expected = |name: &str| {
        contract
            .unattended_agent_cases
            .iter()
            .find(|case| case["name"] == name)
            .unwrap()["allowed"]
            .as_bool()
            .unwrap()
    };
    user.write(
        "test.event.disable_agent",
        &format!(
            "mutation {{update_Agent(docID:\"{}\",input:{{enabled:false}}){{_docID}}}}",
            escape_graphql_string(first_doc)
        ),
    )
    .await?;
    assert_eq!(
        event_admitted(&first_access, &fire, &generation).await,
        agent_expected("disabled")
    );
    user.write(
        "test.event.enable_agent",
        &format!(
            "mutation {{update_Agent(docID:\"{}\",input:{{enabled:true}}){{_docID}}}}",
            escape_graphql_string(first_doc)
        ),
    )
    .await?;
    user.add_actor_relationship(
        "Agent",
        first_doc,
        "reader",
        &identity::Did::new(first.did())?,
    )
    .await?;
    user.delete_actor_relationship(
        "Agent",
        first_doc,
        "executor",
        &identity::Did::new(first.did())?,
    )
    .await?;
    assert_eq!(
        event_admitted(&first_access, &fire, &generation).await,
        agent_expected("reader_only")
    );
    user.add_actor_relationship(
        "Agent",
        first_doc,
        "executor",
        &identity::Did::new(first.did())?,
    )
    .await?;
    assert_eq!(
        event_admitted(&first_access, &fire, &generation).await,
        agent_expected("allowed")
    );
    let spec = crate::request::lifecycle::materialize::pending_agent_request_spec(
        root.did(),
        first.did(),
        "hello",
        crate::request::lifecycle::ExecutionOrigin::Scheduled,
        crate::request::lifecycle::TriggerLineage {
            trigger_id: Some("trigger".into()),
            trigger_kind: Some("event".into()),
            source_doc_id: Some(fire.identity.source_doc_id.clone()),
            correlation: None,
            trigger_context: None,
        },
        None,
        None,
        &fire.request_id,
        &fire.session_id,
        Some(&fire.fire_key),
        None,
        Some(trigger_doc),
    )?;
    let create = crate::request::lifecycle::build_signed_request(
        spec,
        crate::request::lifecycle::RequestSigner::Identity(first.as_ref()),
    )
    .await?;
    let prepared = crate::triggers::engine::durable::PreparedFire {
        receipt: fire.clone(),
        target_existing: false,
        replace_prior: false,
        event_source_generation: Some(generation.clone()),
    };
    let admitted = crate::request::lifecycle::materialize::write_trigger_delivery_with_identity(
        &node,
        identity::Did::new(first.did())?,
        &prepared,
        &create,
        Some(first.as_ref()),
    )
    .await?;
    assert!(!admitted.duplicate);
    let hidden = second_access.execute(query).await?;
    assert_eq!(
        !hidden["data"]["EventSource"].as_array().unwrap().is_empty(),
        expected(modeled_sibling, "source", false, false, false)
    );
    runtime.write("test.event_grants.observation", &format!("mutation {{update_Trigger(docID:\"{}\",input:{{last_status:\"seen\"}}){{_docID}}}}", escape_graphql_string(trigger_doc))).await?;
    user.delete_actor_relationship(
        "EventSource",
        source_doc,
        "reader",
        &identity::Did::new(first.did())?,
    )
    .await?;
    apply(&user, configs).await?;
    assert_eq!(
        !first_access.execute(query).await?["data"]["EventSource"]
            .as_array()
            .unwrap()
            .is_empty(),
        expected(modeled_agent, "source", true, false, false)
    );
    assert_eq!(
        event_admitted(&first_access, &fire, &generation).await,
        expected_claim(modeled_agent, true, false, false)
    );
    apply(&user, vec![document(Collection::Task, task(second.did()))]).await?;
    let rebound = second_access.execute(query).await?;
    assert_eq!(
        !first_access.execute(query).await?["data"]["Trigger"]
            .as_array()
            .unwrap()
            .is_empty(),
        expected(modeled_agent, "trigger", true, true, false)
    );
    assert_eq!(
        !rebound["data"]["Trigger"].as_array().unwrap().is_empty(),
        expected(modeled_sibling, "trigger", true, true, false)
    );
    assert_eq!(
        !rebound["data"]["EventSource"]
            .as_array()
            .unwrap()
            .is_empty(),
        expected(modeled_sibling, "source", true, true, false)
    );
    assert_eq!(
        event_admitted(&second_access, &fire, &rebound_generation).await,
        expected_claim(modeled_sibling, true, true, false)
    );
    let modeled_user = &contract
        .objects
        .iter()
        .find(|object| object.id == "home")
        .unwrap()
        .owner;
    let request_query = format!(
        "{{AgentRequest(filter:{{request_id:{{_eq:\"{}\"}}}}){{_docID}}}}",
        escape_graphql_string(&fire.request_id)
    );
    for (actor, modeled_actor) in [
        (&user, modeled_user.as_str()),
        (&first_access, modeled_agent.as_str()),
        (&second_access, modeled_sibling),
    ] {
        let visible = actor.execute(&request_query).await?;
        let expected = contract
            .event_rebound_checkpoint_cases
            .iter()
            .find(|case| case["actor"] == modeled_actor)
            .unwrap()["request_read"]
            .as_bool()
            .unwrap();
        assert_eq!(
            !visible["data"]["AgentRequest"]
                .as_array()
                .unwrap()
                .is_empty(),
            expected
        );
    }
    let old_request = user.execute(&request_query).await?;
    let request_doc = old_request["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap();
    let reader = identity::Did::new(second.did())?;
    let observations = second_access
        .transact_readonly("test.rebound.receipt_owner_observation", |txn| {
            let reader = &reader;
            Box::pin(async move {
                txn.document_owners_as(
                    reader,
                    "AgentRequest",
                    &[request_doc.to_owned(), first_doc.to_owned()],
                )
                .await
                .map_err(anyhow::Error::new)
            })
        })
        .await?;
    for (name, observation) in ["hidden", "absent"].into_iter().zip(observations) {
        let expected = contract
            .ordinary_receipt_visibility_cases
            .iter()
            .find(|case| case["name"] == name)
            .unwrap()["expected"]
            .as_bool()
            .unwrap();
        assert_eq!(observation.visible, expected, "{name}");
        assert!(observation.owner.is_none());
    }
    let owner_did = root.did();
    let advanced = second_access
        .transact_write("test.event_rebind_uncheckpointed", |txn| {
            Box::pin(async move {
                crate::store::config_client::event_source_cursor::checkpoint_prefix(
                    txn,
                    owner_did,
                    &gents_protocol::event_delivery::EventConsumer::Trigger {
                        trigger_id: "trigger".into(),
                    },
                    "EventPolicyInput",
                    "1",
                    false,
                )
                .await
            })
        })
        .await?;
    let modeled_cursor = contract
        .event_rebound_checkpoint_cases
        .iter()
        .find(|case| case["actor"] == modeled_sibling)
        .unwrap()["cursor"]
        .as_u64()
        .unwrap();
    assert_eq!(advanced, modeled_cursor == 1);
    user.add_actor_relationship(
        "EventSource",
        source_doc,
        "reader",
        &identity::Did::new(first.did())?,
    )
    .await?;
    let stale = &contract.event_stale_claim_cases[0];
    assert_eq!(
        !first_access.execute(query).await?["data"]["EventSource"]
            .as_array()
            .unwrap()
            .is_empty(),
        stale["source_read"].as_bool().unwrap()
    );
    assert_eq!(
        event_admitted(&first_access, &fire, &generation).await,
        stale["claim_allowed"].as_bool().unwrap()
    );
    assert_eq!(
        cursor_update(&first_access, cursor_doc, "1").await?,
        cursor_expected(modeled_agent, false)
    );
    assert_eq!(
        cursor_update(&second_access, cursor_doc, "2").await?,
        cursor_expected(modeled_sibling, false)
    );
    user.delete_actor_relationship(
        "EventSource",
        source_doc,
        "reader",
        &identity::Did::new(second.did())?,
    )
    .await?;
    user.delete_actor_relationship(
        "EventSourceCursor",
        cursor_doc,
        "writer",
        &identity::Did::new(second.did())?,
    )
    .await?;
    apply(&user, vec![document(Collection::Task, task(second.did()))]).await?;
    assert_eq!(
        !second_access.execute(query).await?["data"]["EventSource"]
            .as_array()
            .unwrap()
            .is_empty(),
        expected(modeled_sibling, "source", true, true, true)
    );
    assert_eq!(
        event_admitted(&second_access, &fire, &rebound_generation).await,
        expected_claim(modeled_sibling, true, true, true)
    );
    assert_eq!(
        cursor_update(&second_access, cursor_doc, "3").await?,
        cursor_expected(modeled_sibling, true)
    );
    user.add_collection("type EventPolicyInputNext { value: String }")
        .await?;
    let moved_source = json!({"node_did":root.did(),"event_source_id":"source","source_collection":"EventPolicyInputNext","event_kind":"created"});
    apply(&user, vec![document(Collection::EventSource, moved_source)]).await?;
    let cursors = user
        .execute("{EventSourceCursor {_docID source_collection}}")
        .await?;
    let new_doc = cursors["data"]["EventSourceCursor"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["source_collection"] == "EventPolicyInputNext")
        .unwrap()["_docID"]
        .as_str()
        .unwrap();
    for (name, doc, value) in [("old", cursor_doc, "4"), ("new", new_doc, "5")] {
        let expected = contract
            .event_cursor_move_cases
            .iter()
            .find(|case| case["cursor"] == name)
            .unwrap()["expected"]
            .as_bool()
            .unwrap();
        assert_eq!(cursor_update(&second_access, doc, value).await?, expected);
    }
    user.add_collection("type Records { eligible: Boolean batch: String other_batch: String }")
        .await?;
    user.add_collection("type Other { eligible: Boolean batch: String other_batch: String }")
        .await?;
    let initial = &contract.event_generation_cases[0]["observed"];
    let modeled_trigger = json!({"node_did":root.did(),"trigger_id":"modeled-trigger","task_id":"task",
        "source":{"kind":"event","event_source_id":"modeled-source"}});
    apply(
        &user,
        vec![
            document(Collection::EventSource, modeled_source(initial, root.did())),
            document(Collection::Trigger, modeled_trigger.clone()),
        ],
    )
    .await?;
    let mut modeled_fire = fire.clone();
    modeled_fire.identity.trigger_id = "modeled-trigger".into();
    modeled_fire.identity.source_collection = "EventGroupState".into();
    modeled_fire.fire_key = modeled_fire.identity.fire_key();
    modeled_fire.request_id = modeled_fire.identity.request_id();
    modeled_fire.session_id = modeled_fire.identity.session_id();
    let modeled_rows = user.execute("{Trigger(filter:{trigger_id:{_eq:\"modeled-trigger\"}}){_docID} EventSource(filter:{event_source_id:{_eq:\"modeled-source\"}}){_docID}}").await?;
    let modeled_trigger_doc = modeled_rows["data"]["Trigger"][0]["_docID"]
        .as_str()
        .unwrap();
    let modeled_source_doc = modeled_rows["data"]["EventSource"][0]["_docID"]
        .as_str()
        .unwrap();
    let modeled_ids = crate::triggers::engine::event_delivery::EventConfigDocIds {
        trigger: modeled_trigger_doc,
        source: modeled_source_doc,
        task: task_doc,
    };
    let modeled_generation = crate::triggers::engine::event_delivery::event_admission_generation(
        &serde_json::from_value(modeled_source(initial, root.did()))?,
        &serde_json::from_value(modeled_trigger.clone())?,
        &serde_json::from_value(task(second.did()))?,
        &modeled_ids,
    )?;
    for case in &contract.event_generation_cases {
        let current = &case["current"];
        apply(
            &user,
            vec![document(
                Collection::EventSource,
                modeled_source(current, root.did()),
            )],
        )
        .await?;
        assert_eq!(
            event_admitted(&second_access, &modeled_fire, &modeled_generation).await,
            case["allowed"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
    }
    let rejected: Result<()> = second_access
        .transact_write("test.event_generation.atomic_abort", |txn| {
            let modeled_fire = &modeled_fire;
            let modeled_generation = &modeled_generation;
            Box::pin(async move {
                crate::triggers::engine::durable::stage_fire_receipt(txn, modeled_fire).await?;
                crate::store::config_client::event_source_cursor::validate_event_admission(
                    txn,
                    modeled_fire,
                    modeled_generation,
                )
                .await?;
                Ok(())
            })
        })
        .await;
    assert!(rejected.is_err());
    let receipt = second_access
        .execute(&format!(
            "{{TriggerFire(filter:{{fire_key:{{_eq:\"{}\"}}}}){{_docID}}}}",
            escape_graphql_string(&modeled_fire.fire_key)
        ))
        .await?;
    let receipt_expected = contract.event_generation_cases.last().unwrap()["receipt"]
        .as_bool()
        .unwrap();
    assert_eq!(
        !receipt["data"]["TriggerFire"]
            .as_array()
            .unwrap()
            .is_empty(),
        receipt_expected
    );
    let original_execution = &contract.event_execution_cases[0]["observed"];
    let execution_task = |value: &Value| {
        json!({
            "node_did":root.did(), "task_id":"task",
            "agent_did":if value["agent"] == "agent" { second.did() } else { first.did() },
            "display_name":value["name"], "prompt_template":value["prompt"],
            "goal_objective_template":value["objective"], "goal_token_budget":value["budget"],
            "output_schema_ref":value["output"], "emit_outcome":value["emit"],
            "hooks":serde_json::from_str::<Value>(value["hooks"].as_str().unwrap()).unwrap(),
        })
    };
    let execution_trigger = |value: &Value| {
        json!({
            "node_did":root.did(), "trigger_id":"modeled-trigger", "task_id":"task",
            "source":{"kind":"event", "event_source_id":if value["source_id"] == "source" { "modeled-source" } else { "other" }},
            "session_id_template":value["session_template"], "concurrency":value["concurrency"],
        })
    };
    let mut other_source = modeled_source(initial, root.did());
    other_source["event_source_id"] = json!("other");
    apply(
        &user,
        vec![
            document(Collection::EventSource, modeled_source(initial, root.did())),
            document(Collection::EventSource, other_source),
            document(Collection::Task, execution_task(original_execution)),
            document(Collection::Trigger, execution_trigger(original_execution)),
        ],
    )
    .await?;
    let execution_generation = crate::triggers::engine::event_delivery::event_admission_generation(
        &serde_json::from_value(modeled_source(initial, root.did()))?,
        &serde_json::from_value(execution_trigger(original_execution))?,
        &serde_json::from_value(execution_task(original_execution))?,
        &modeled_ids,
    )?;
    for case in &contract.event_execution_cases {
        let current = &case["current"];
        apply(
            &user,
            vec![
                document(Collection::Task, execution_task(current)),
                document(Collection::Trigger, execution_trigger(current)),
            ],
        )
        .await?;
        let mut observed = execution_generation.clone();
        if let Some(field) = match case["name"].as_str().unwrap() {
            "trigger_recreated" => Some("trigger_doc"),
            "source_recreated" => Some("source_doc"),
            "task_recreated" => Some("task_doc"),
            _ => None,
        } {
            observed["execution"][field] = json!("former-physical-document");
        }
        assert_eq!(
            event_admitted(&second_access, &modeled_fire, &observed).await,
            case["allowed"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
    }
    let callback = json!({"node_did":root.did(),"callback_id":"callback",
        "handler":{"kind":"built_in","emitter":"create_workspace"}});
    let binding = |agent: &str| {
        json!({"node_did":root.did(),"binding_id":"binding",
        "callback_id":"callback","event_source_id":"source","agent_did":agent})
    };
    apply(
        &user,
        vec![
            document(Collection::Callback, callback),
            document(Collection::CallbackBinding, binding(first.did())),
        ],
    )
    .await?;
    let pending = crate::triggers::callback::CallbackInvocationDoc {
        invocation_id: "rebind-invocation".into(),
        owner_node_did: root.did().into(),
        agent_did: first.did().into(),
        callback_id: "callback".into(),
        configuration_generation: None,
        claimed_callback: None,
        input: json!({}),
        origin: crate::documents::CallbackInvocationOrigin::Event {
            binding_id: "binding".into(),
            source_collection: "EventPolicyInputNext".into(),
            source_doc_id: "rebind-source".into(),
            source_version: None,
        },
        idempotency_key: "rebind-key".into(),
        caused_by_correlation: None,
        lifecycle_state: "pending".into(),
        attempts: Some(0),
        action_plan: None,
        action_journal: None,
        error: None,
        claimed_at: None,
        created_at: None,
    };
    crate::triggers::callback::create_pending_invocation(&node, &first_access, &pending).await?;
    let module = json!({"node_did":root.did(),"module_id":"module"});
    let callback = json!({"node_did":root.did(),"callback_id":"callback",
        "handler":{"kind":"module","module_id":"module"}});
    apply(
        &user,
        vec![
            document(Collection::CallbackModule, module),
            document(Collection::Callback, callback.clone()),
        ],
    )
    .await?;
    let module_rows = user.execute("{CallbackModule {_docID}}").await?;
    let module_doc = module_rows["data"]["CallbackModule"][0]["_docID"]
        .as_str()
        .unwrap();
    let module_expected = |actor: &str, changed: bool, revoked: bool| {
        contract
            .callback_module_reader_cases
            .iter()
            .find(|case| {
                case["actor"] == actor && case["changed"] == changed && case["revoked"] == revoked
            })
            .unwrap()["expected"]
            .as_bool()
            .unwrap()
    };
    for (actor, modeled_actor) in [
        (&first_access, modeled_agent.as_str()),
        (&second_access, modeled_sibling),
    ] {
        let visible = actor.execute("{CallbackModule {_docID}}").await?;
        assert_eq!(
            !visible["data"]["CallbackModule"]
                .as_array()
                .unwrap()
                .is_empty(),
            module_expected(modeled_actor, false, false)
        );
    }
    apply(
        &user,
        vec![document(Collection::CallbackBinding, binding(second.did()))],
    )
    .await?;
    let invocation_query =
        "{CallbackInvocation(filter:{idempotency_key:{_eq:\"rebind-key\"}}){_docID}}";
    for (actor, modeled_actor) in [
        (&user, modeled_user.as_str()),
        (&first_access, modeled_agent.as_str()),
        (&second_access, modeled_sibling),
    ] {
        let visible = actor.execute(invocation_query).await?;
        let expected = contract
            .callback_rebound_visibility_cases
            .iter()
            .find(|case| case["actor"] == modeled_actor)
            .unwrap()["invocation_read"]
            .as_bool()
            .unwrap();
        assert_eq!(
            !visible["data"]["CallbackInvocation"]
                .as_array()
                .unwrap()
                .is_empty(),
            expected
        );
    }
    let mut replacement = pending.clone();
    replacement.invocation_id = "new-agent-invocation".into();
    replacement.agent_did = second.did().into();
    assert!(crate::triggers::callback::create_pending_invocation(
        &node,
        &second_access,
        &replacement
    )
    .await
    .is_err());
    for (actor, modeled_actor) in [
        (&first_access, modeled_agent.as_str()),
        (&second_access, modeled_sibling),
    ] {
        let visible = actor.execute("{CallbackBinding {_docID}}").await?;
        assert_eq!(
            !visible["data"]["CallbackBinding"]
                .as_array()
                .unwrap()
                .is_empty(),
            expected(modeled_actor, "trigger", true, true, false)
        );
        let visible = actor.execute("{CallbackModule {_docID}}").await?;
        assert_eq!(
            !visible["data"]["CallbackModule"]
                .as_array()
                .unwrap()
                .is_empty(),
            module_expected(modeled_actor, true, false)
        );
    }
    user.delete_actor_relationship(
        "CallbackModule",
        module_doc,
        "reader",
        &identity::Did::new(first.did())?,
    )
    .await?;
    apply(&user, vec![document(Collection::Callback, callback)]).await?;
    let visible = first_access.execute("{CallbackModule {_docID}}").await?;
    assert_eq!(
        !visible["data"]["CallbackModule"]
            .as_array()
            .unwrap()
            .is_empty(),
        module_expected(modeled_agent, false, true)
    );
    node.shutdown().await?;
    Ok(())
}
