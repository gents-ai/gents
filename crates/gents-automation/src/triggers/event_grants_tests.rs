use anyhow::{Context, Result};
use gents_documents::{apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use rapidhash::{HashMapExt, RapidHashMap};
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

async fn event_admitted(
    access: &ConfigAccess,
    fire: &gents_protocol::trigger_delivery::TriggerFire,
    generation: &Value,
) -> bool {
    access
        .transact_readonly("test.event_grants.admission", |txn| {
            Box::pin(async move {
                gents_documents::event_consumer::validate_event_admission(txn, fire, generation)
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
    let directory = tempfile::tempdir()?;
    let root = Arc::new(gents_store::identity::KeyIdentity::load_or_create(
        directory.path().join("node.key"),
        None,
    )?);
    let owner_binding = gents_store::identity::provision_agent_identity(root.as_ref())?;
    let first_binding = gents_store::identity::provision_agent_identity(root.as_ref())?;
    let second_binding = gents_store::identity::provision_agent_identity(root.as_ref())?;
    let owner = gents_store::identity::resolve_agent_identity(root.as_ref(), &owner_binding)?;
    let first = gents_store::identity::resolve_agent_identity(root.as_ref(), &first_binding)?;
    let second = gents_store::identity::resolve_agent_identity(root.as_ref(), &second_binding)?;
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
    )?
    .with_runtime_agent_proof(gents_store::identity::RuntimeAgentProof::new(root.clone()));
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
                let doc = gents_store::graphql::created_doc_id(&response, "Node")?;
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
                let agent_doc = gents_store::graphql::created_doc_id(&response, "Agent")?;
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
    let ids = gents_documents::automation::EventConfigDocIds {
        trigger: trigger_doc,
        source: source_doc,
        task: task_doc,
    };
    let generation_for = |agent: &str| {
        gents_documents::automation::event_admission_generation(
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
    let arrival_doc = gents_store::graphql::created_doc_id(&arrival, "EventPolicyInput")?;
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
    let visible = |response: &Value, collection: &str| {
        !response["data"][collection].as_array().unwrap().is_empty()
    };
    let mut reads: RapidHashMap<(&str, &str, bool, bool, bool), bool> = RapidHashMap::new();
    let mut claims: RapidHashMap<(&str, bool, bool, bool), bool> = RapidHashMap::new();
    let selected = first_access.execute(query).await?;
    reads.insert(
        ("agent", "trigger", false, false, false),
        visible(&selected, "Trigger"),
    );
    reads.insert(
        ("agent", "source", false, false, false),
        visible(&selected, "EventSource"),
    );
    claims.insert(
        ("agent", false, false, false),
        event_admitted(&first_access, &fire, &generation).await,
    );
    claims.insert(
        ("sibling", false, false, false),
        event_admitted(&second_access, &fire, &generation).await,
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
    let first_did = identity::Did::new(first.did())?;
    let mut enabled = true;
    let mut executor = true;
    let mut reader = false;
    for case in gents_lean_vocab::families::household_unattended_event_admission::cases() {
        if case.input.enabled != enabled {
            user.write(
                "test.event.agent_enabled",
                &format!(
                    "mutation {{update_Agent(docID:\"{}\",input:{{enabled:{}}}){{_docID}}}}",
                    escape_graphql_string(first_doc),
                    case.input.enabled
                ),
            )
            .await?;
            enabled = case.input.enabled;
        }
        let invoke = case.input.invoke == "allowed";
        if !invoke && !reader {
            user.add_actor_relationship("Agent", first_doc, "reader", &first_did)
                .await?;
            reader = true;
        }
        if invoke != executor {
            if invoke {
                user.add_actor_relationship("Agent", first_doc, "executor", &first_did)
                    .await?;
            } else {
                user.delete_actor_relationship("Agent", first_doc, "executor", &first_did)
                    .await?;
            }
            executor = invoke;
        }
        let claimant = if case.input.actor_matches {
            &first_access
        } else {
            &second_access
        };
        case.check(event_admitted(claimant, &fire, &generation).await);
    }
    if !enabled {
        user.write(
            "test.event.enable_agent",
            &format!(
                "mutation {{update_Agent(docID:\"{}\",input:{{enabled:true}}){{_docID}}}}",
                escape_graphql_string(first_doc)
            ),
        )
        .await?;
    }
    if !executor {
        user.add_actor_relationship("Agent", first_doc, "executor", &first_did)
            .await?;
    }
    let spec = gents_session::request::lifecycle::materialize::pending_agent_request_spec(
        root.did(),
        first.did(),
        "hello",
        gents_loop::execution_origin::ExecutionOrigin::Scheduled,
        gents_session::request::lifecycle::TriggerLineage {
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
    let create = gents_session::request::lifecycle::build_signed_request(
        spec,
        gents_session::request::lifecycle::RequestSigner::Identity(first.as_ref()),
    )
    .await?;
    let prepared = crate::triggers::engine::durable::PreparedFire {
        receipt: fire.clone(),
        target_existing: false,
        create_session: false,
        max_fires_per_hour: gents_documents::DEFAULT_MAX_FIRES_PER_HOUR,
        replace_prior: false,
        event_source_generation: Some(generation.clone()),
    };
    let admitted = crate::triggers::engine::durable::write_trigger_delivery_with_identity(
        &node,
        identity::Did::new(first.did())?,
        &prepared,
        &create,
        Some(first.as_ref()),
    )
    .await?;
    assert!(!admitted.duplicate);
    let hidden = second_access.execute(query).await?;
    reads.insert(
        ("sibling", "source", false, false, false),
        visible(&hidden, "EventSource"),
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
    reads.insert(
        ("agent", "source", true, false, false),
        visible(&first_access.execute(query).await?, "EventSource"),
    );
    claims.insert(
        ("agent", true, false, false),
        event_admitted(&first_access, &fire, &generation).await,
    );
    apply(&user, vec![document(Collection::Task, task(second.did()))]).await?;
    let rebound = second_access.execute(query).await?;
    let former = first_access.execute(query).await?;
    reads.insert(
        ("agent", "trigger", true, true, false),
        visible(&former, "Trigger"),
    );
    reads.insert(
        ("agent", "source", true, true, false),
        visible(&former, "EventSource"),
    );
    reads.insert(
        ("sibling", "trigger", true, true, false),
        visible(&rebound, "Trigger"),
    );
    reads.insert(
        ("sibling", "source", true, true, false),
        visible(&rebound, "EventSource"),
    );
    claims.insert(
        ("sibling", true, true, false),
        event_admitted(&second_access, &fire, &rebound_generation).await,
    );
    claims.insert(
        ("agent", true, true, false),
        event_admitted(&first_access, &fire, &rebound_generation).await,
    );
    let request_query = format!(
        "{{AgentRequest(filter:{{request_id:{{_eq:\"{}\"}}}}){{_docID}}}}",
        escape_graphql_string(&fire.request_id)
    );
    for case in gents_lean_vocab::families::household_event_rebound_checkpoint::cases() {
        let reader = match case.input.role.as_str() {
            "user" => &user,
            "agent" => &first_access,
            _ => &second_access,
        };
        case.check(visible(
            &reader.execute(&request_query).await?,
            "AgentRequest",
        ));
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
    for case in gents_lean_vocab::families::household_ordinary_receipt_visibility::cases() {
        let observation = &observations[usize::from(!case.input.present)];
        assert!(observation.owner.is_none());
        case.check(observation.visible);
    }
    let owner_did = root.did();
    for case in gents_lean_vocab::families::household_event_rebound_cursor::cases() {
        let checkpointer = match case.input.role.as_str() {
            "user" => &user,
            "agent" => &first_access,
            _ => &second_access,
        };
        let advanced = checkpointer
            .transact_write("test.event_rebind_uncheckpointed", |txn| {
                Box::pin(async move {
                    gents_documents::event_consumer::checkpoint_prefix(
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
        case.check(u64::from(advanced));
    }
    user.add_actor_relationship(
        "EventSource",
        source_doc,
        "reader",
        &identity::Did::new(first.did())?,
    )
    .await?;
    for case in gents_lean_vocab::families::household_event_stale_claim::cases() {
        case.check(
            gents_lean_vocab::families::household_event_stale_claim::StaleClaim {
                source_read: visible(&first_access.execute(query).await?, "EventSource"),
                claim_allowed: event_admitted(&first_access, &fire, &generation).await,
            },
        );
    }
    let mut cursor_value = 0;
    for case in gents_lean_vocab::families::household_event_cursor_rebind::cases() {
        if case.input.revoked {
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
            reads.insert(
                ("sibling", "source", true, true, true),
                visible(&second_access.execute(query).await?, "EventSource"),
            );
            claims.insert(
                ("sibling", true, true, true),
                event_admitted(&second_access, &fire, &rebound_generation).await,
            );
        }
        let writer = if case.input.role == "agent" {
            &first_access
        } else {
            &second_access
        };
        cursor_value += 1;
        case.check(cursor_update(writer, cursor_doc, &cursor_value.to_string()).await?);
    }
    for case in gents_lean_vocab::families::household_event_grants_configuration::cases() {
        let input = &case.input;
        let read = reads[&(
            input.role.as_str(),
            input.id.as_str(),
            input.revoked,
            input.rebound,
            input.post_revoke,
        )];
        let claim = claims[&(
            input.role.as_str(),
            input.revoked,
            input.rebound,
            input.post_revoke,
        )];
        case.check(
            gents_lean_vocab::families::household_event_grants_configuration::EventGrant {
                read,
                claim,
            },
        );
    }
    // The source feeds a Task that emits outcomes, so its handoff_id must be a String.
    user.add_collection("type EventPolicyInputNext { value: String handoff_id: String }")
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
    for case in gents_lean_vocab::families::household_event_cursor_move::cases() {
        let doc = if case.input.cursor == "old" {
            cursor_doc
        } else {
            new_doc
        };
        cursor_value += 1;
        case.check(cursor_update(&second_access, doc, &cursor_value.to_string()).await?);
    }
    let generation_cases =
        gents_lean_vocab::families::household_event_generation_admission::cases();
    let initial = serde_json::to_value(&generation_cases[0].input.observed)?;
    user.add_collection(
        "type Records { eligible: Boolean batch: String other_batch: String handoff_id: String }",
    )
    .await?;
    user.add_collection("type Other { eligible: Boolean batch: String other_batch: String }")
        .await?;
    let modeled_trigger = json!({"node_did":root.did(),"trigger_id":"modeled-trigger","task_id":"task",
        "source":{"kind":"event","event_source_id":"modeled-source"}});
    apply(
        &user,
        vec![
            document(
                Collection::EventSource,
                modeled_source(&initial, root.did()),
            ),
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
    let modeled_ids = gents_documents::automation::EventConfigDocIds {
        trigger: modeled_trigger_doc,
        source: modeled_source_doc,
        task: task_doc,
    };
    let modeled_generation = gents_documents::automation::event_admission_generation(
        &serde_json::from_value(modeled_source(&initial, root.did()))?,
        &serde_json::from_value(modeled_trigger.clone())?,
        &serde_json::from_value(task(second.did()))?,
        &modeled_ids,
    )?;
    for case in generation_cases {
        let current = serde_json::to_value(&case.input.current)?;
        apply(
            &user,
            vec![document(
                Collection::EventSource,
                modeled_source(&current, root.did()),
            )],
        )
        .await?;
        let allowed = event_admitted(&second_access, &modeled_fire, &modeled_generation).await;
        let mut staged_fire = modeled_fire.clone();
        staged_fire.identity.source_doc_id = format!("generation-{}", case.input.name);
        staged_fire.fire_key = staged_fire.identity.fire_key();
        staged_fire.request_id = staged_fire.identity.request_id();
        staged_fire.session_id = staged_fire.identity.session_id();
        let staged: Result<()> = second_access
            .transact_write("test.event_generation.receipt", |txn| {
                let staged_fire = &staged_fire;
                let modeled_generation = &modeled_generation;
                Box::pin(async move {
                    crate::triggers::engine::durable::stage_fire_receipt(txn, staged_fire).await?;
                    gents_documents::event_consumer::validate_event_admission(
                        txn,
                        staged_fire,
                        modeled_generation,
                    )
                    .await?;
                    Ok(())
                })
            })
            .await;
        let receipt = visible(
            &second_access
                .execute(&format!(
                    "{{TriggerFire(filter:{{fire_key:{{_eq:\"{}\"}}}}){{_docID}}}}",
                    escape_graphql_string(&staged_fire.fire_key)
                ))
                .await?,
            "TriggerFire",
        );
        assert_eq!(staged.is_ok(), receipt, "{}", case.input.name);
        case.check(
            gents_lean_vocab::families::household_event_generation_admission::GenerationAdmission {
                allowed,
                receipt,
            },
        );
    }
    let execution_cases = gents_lean_vocab::families::household_event_execution_admission::cases();
    let original_execution = &serde_json::to_value(&execution_cases[0].input.observed)?;
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
    let mut other_source = modeled_source(&initial, root.did());
    other_source["event_source_id"] = json!("other");
    apply(
        &user,
        vec![
            document(
                Collection::EventSource,
                modeled_source(&initial, root.did()),
            ),
            document(Collection::EventSource, other_source),
            document(Collection::Task, execution_task(original_execution)),
            document(Collection::Trigger, execution_trigger(original_execution)),
        ],
    )
    .await?;
    let execution_generation = gents_documents::automation::event_admission_generation(
        &serde_json::from_value(modeled_source(&initial, root.did()))?,
        &serde_json::from_value(execution_trigger(original_execution))?,
        &serde_json::from_value(execution_task(original_execution))?,
        &modeled_ids,
    )?;
    for case in execution_cases {
        let current = &serde_json::to_value(&case.input.current)?;
        apply(
            &user,
            vec![
                document(Collection::Task, execution_task(current)),
                document(Collection::Trigger, execution_trigger(current)),
            ],
        )
        .await?;
        let mut observed = execution_generation.clone();
        if let Some(field) = match case.input.name.as_str() {
            "trigger_recreated" => Some("trigger_doc"),
            "source_recreated" => Some("source_doc"),
            "task_recreated" => Some("task_doc"),
            _ => None,
        } {
            observed["execution"][field] = json!("former-physical-document");
        }
        case.check(event_admitted(&second_access, &modeled_fire, &observed).await);
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
        origin: gents_documents::CallbackInvocationOrigin::Event {
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
    let mut module_reads: RapidHashMap<(&str, bool, bool), bool> = RapidHashMap::new();
    for (role, actor) in [("agent", &first_access), ("sibling", &second_access)] {
        module_reads.insert(
            (role, false, false),
            visible(
                &actor.execute("{CallbackModule {_docID}}").await?,
                "CallbackModule",
            ),
        );
    }
    apply(
        &user,
        vec![document(Collection::CallbackBinding, binding(second.did()))],
    )
    .await?;
    let invocation_query =
        "{CallbackInvocation(filter:{idempotency_key:{_eq:\"rebind-key\"}}){_docID}}";
    for case in gents_lean_vocab::families::household_callback_rebound_visibility::cases() {
        let actor = match case.input.role.as_str() {
            "user" => &user,
            "agent" => &first_access,
            _ => &second_access,
        };
        case.check(visible(
            &actor.execute(invocation_query).await?,
            "CallbackInvocation",
        ));
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
    for case in gents_lean_vocab::families::household_callback_binding_rebound::cases() {
        let actor = if case.input.role == "agent" {
            &first_access
        } else {
            &second_access
        };
        case.check(visible(
            &actor.execute("{CallbackBinding {_docID}}").await?,
            "CallbackBinding",
        ));
    }
    for (role, actor) in [("agent", &first_access), ("sibling", &second_access)] {
        module_reads.insert(
            (role, true, false),
            visible(
                &actor.execute("{CallbackModule {_docID}}").await?,
                "CallbackModule",
            ),
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
    module_reads.insert(
        ("agent", false, true),
        visible(
            &first_access.execute("{CallbackModule {_docID}}").await?,
            "CallbackModule",
        ),
    );
    for case in gents_lean_vocab::families::household_callback_module_reader::cases() {
        let input = &case.input;
        case.check(module_reads[&(input.role.as_str(), input.changed, input.revoked)]);
    }
    node.shutdown().await?;
    Ok(())
}
