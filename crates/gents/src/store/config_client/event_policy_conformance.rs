use super::ConfigAccess;
use crate::store::graphql::escape_graphql_string;
use crate::store::identity::NodeIdentity;
use std::sync::Arc;

#[tokio::test]
async fn modeled_event_configuration_grants_use_native_permissions() -> anyhow::Result<()> {
    let contract = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("configuration contract");
    let modeled_user = &contract
        .objects
        .iter()
        .find(|row| row.id == "home")
        .unwrap()
        .owner;
    let modeled_agent = &contract
        .objects
        .iter()
        .find(|row| row.id == "agent-created")
        .unwrap()
        .owner;
    let modeled_runtime = contract
        .relations
        .iter()
        .find_map(|relation| match &relation.subject {
            crate::lean_vocab_test::LeanIdentitySubject::Actor { actor }
                if relation.resource == "nodes" && relation.relation == "runtime_reader" =>
            {
                Some(actor)
            }
            _ => None,
        })
        .unwrap();
    let directory = tempfile::tempdir()?;
    let root = crate::store::identity::KeyIdentity::load_or_create(
        directory.path().join("node.key"),
        None,
    )?;
    let runtime_binding = crate::store::identity::provision_agent_identity(&root)?;
    let agent_binding = crate::store::identity::provision_agent_identity(&root)?;
    let sibling_binding = crate::store::identity::provision_agent_identity(&root)?;
    let runtime = crate::store::identity::resolve_agent_identity(&root, &runtime_binding)?;
    let agent = crate::store::identity::resolve_agent_identity(&root, &agent_binding)?;
    let sibling = crate::store::identity::resolve_agent_identity(&root, &sibling_binding)?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(root.did())
            .build()
            .await?,
    );
    let owner = ConfigAccess::local_as(node.clone(), identity::Did::new(root.did())?);
    let runtime_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(runtime.did())?,
        runtime.as_ref(),
    )?;
    let agent_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did())?,
        agent.as_ref(),
    )?;
    let sibling_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(sibling.did())?,
        sibling.as_ref(),
    )?;
    let context = defra_core::RequestContext::default().with_identity(Some(root.did().to_owned()));
    let policy = node
        .add_dac_policy_with_counter_as(&context, &contract.policy_yaml, 0)
        .await?;
    assert_eq!(policy, gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID);
    owner.add_collection(gents_protocol::schemas::NODE).await?;
    owner
        .add_collection(gents_protocol::schemas::TRIGGER)
        .await?;
    owner
        .add_collection(gents_protocol::schemas::EVENT_SOURCE)
        .await?;
    let response = owner
        .write(
            "test.event_policy.node",
            "mutation {create_Node(input:{node_did:\"node\"}){_docID}}",
        )
        .await?;
    let parent = crate::store::graphql::created_doc_id(&response, "Node")?;
    owner
        .add_actor_relationship(
            "Node",
            &parent,
            "runtime_reader",
            &identity::Did::new(runtime.did())?,
        )
        .await?;
    let response = owner.write("test.event_policy.documents", "mutation {create_Trigger(input:{node_did:\"node\",trigger_id:\"trigger\",display_name:\"initial\"}){_docID} create_EventSource(input:{node_did:\"node\",event_source_id:\"source\",source_collection:\"Source\"}){_docID}}").await?;
    let trigger = crate::store::graphql::created_doc_id(&response, "Trigger")?;
    let source = crate::store::graphql::created_doc_id(&response, "EventSource")?;
    for (collection, doc) in [("Trigger", &trigger), ("EventSource", &source)] {
        owner
            .add_resource_relationship(collection, doc, "parent", "nodes", &parent)
            .await?;
        owner
            .add_actor_relationship(collection, doc, "reader", &identity::Did::new(agent.did())?)
            .await?;
    }
    owner
        .add_actor_relationship(
            "Trigger",
            &trigger,
            "writer",
            &identity::Did::new(runtime.did())?,
        )
        .await?;
    let mut revoked = false;
    let mut rebound = false;
    let mut rebound_revoked = false;
    for observation in &contract.event_configuration_cases {
        if observation["revoked"] == true && !revoked {
            owner
                .delete_actor_relationship(
                    "EventSource",
                    &source,
                    "reader",
                    &identity::Did::new(agent.did())?,
                )
                .await?;
            revoked = true;
        }
        if observation["rebound"] == true && !rebound {
            // Lean `rebindEventConfiguration`: the Trigger's exclusive reader moves
            // from the old Agent to the new one; the shared source only gains the
            // new reader and keeps the old one's grant unless revoked above.
            owner
                .delete_actor_relationship(
                    "Trigger",
                    &trigger,
                    "reader",
                    &identity::Did::new(agent.did())?,
                )
                .await?;
            for (collection, doc) in [("Trigger", &trigger), ("EventSource", &source)] {
                owner
                    .add_actor_relationship(
                        collection,
                        doc,
                        "reader",
                        &identity::Did::new(sibling.did())?,
                    )
                    .await?;
            }
            rebound = true;
        }
        if observation["post_revoke"] == true && !rebound_revoked {
            owner
                .delete_actor_relationship(
                    "EventSource",
                    &source,
                    "reader",
                    &identity::Did::new(sibling.did())?,
                )
                .await?;
            rebound_revoked = true;
        }
        let modeled_actor = observation["actor"].as_str().unwrap();
        let access = if modeled_actor == modeled_user {
            &owner
        } else if modeled_actor == modeled_runtime {
            &runtime_access
        } else if modeled_actor == modeled_agent {
            &agent_access
        } else {
            &sibling_access
        };
        let name = observation["id"].as_str().unwrap();
        let allowed = match (name, observation["permission"].as_str().unwrap()) {
            ("trigger", "read") => !access.execute("{Trigger {_docID}}").await?["data"]["Trigger"].as_array().unwrap().is_empty(),
            ("source", "read") => !access.execute("{EventSource {_docID}}").await?["data"]["EventSource"].as_array().unwrap().is_empty(),
            ("trigger", "update") => match access.write("test.event_policy.update", &format!("mutation {{update_Trigger(docID:\"{}\",input:{{display_name:\"edited\"}}){{_docID}}}}", escape_graphql_string(&trigger))).await {
                Ok(response) => !response["data"]["update_Trigger"].as_array().unwrap().is_empty(),
                Err(error) if error.is_graphql_rejection() => false,
                Err(error) => return Err(error.into()),
            },
            _ => unreachable!(),
        };
        assert_eq!(
            allowed,
            observation["expected"].as_bool().unwrap(),
            "{observation}"
        );
    }
    owner
        .add_collection(gents_protocol::schemas::EVENT_SOURCE_CURSOR)
        .await?;
    let created = owner.write("test.event_policy.cursor", "mutation {create_EventSourceCursor(input:{cursor_key:\"cursor\",owner_did:\"node\",source_collection:\"Source\",after:\"0\"}){_docID}}").await?;
    let cursor_doc = crate::store::graphql::created_doc_id(&created, "EventSourceCursor")?;
    owner
        .add_resource_relationship("EventSourceCursor", &cursor_doc, "parent", "nodes", &parent)
        .await?;
    owner
        .add_actor_relationship(
            "EventSourceCursor",
            &cursor_doc,
            "writer",
            &identity::Did::new(agent.did())?,
        )
        .await?;
    let mut cursor_revoked = false;
    for (step, observation) in contract.event_cursor_cases.iter().enumerate() {
        if observation["revoked"] == true && !cursor_revoked {
            owner
                .delete_actor_relationship(
                    "EventSourceCursor",
                    &cursor_doc,
                    "writer",
                    &identity::Did::new(agent.did())?,
                )
                .await?;
            cursor_revoked = true;
        }
        let modeled_actor = observation["actor"].as_str().unwrap();
        let access = if modeled_actor == modeled_user {
            &owner
        } else if modeled_actor == modeled_runtime {
            &runtime_access
        } else if modeled_actor == modeled_agent {
            &agent_access
        } else {
            &sibling_access
        };
        let allowed = match observation["permission"].as_str().unwrap() {
            "read" => !access.execute("{EventSourceCursor {_docID}}").await?["data"]["EventSourceCursor"].as_array().unwrap().is_empty(),
            "update" => match access.write("test.event_policy.update_cursor", &format!("mutation {{update_EventSourceCursor(docID:\"{}\",input:{{after:\"{}\"}}){{_docID}}}}", escape_graphql_string(&cursor_doc), step + 1)).await {
                Ok(response) => !response["data"]["update_EventSourceCursor"].as_array().unwrap().is_empty(),
                Err(error) if error.is_graphql_rejection() => false,
                Err(error) => return Err(error.into()),
            },
            _ => unreachable!(),
        };
        assert_eq!(
            allowed,
            observation["expected"].as_bool().unwrap(),
            "{observation}"
        );
    }
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn modeled_task_receipt_shares_native_session_permissions() -> anyhow::Result<()> {
    let contract = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let modeled_user = &contract
        .objects
        .iter()
        .find(|row| row.id == "home")
        .unwrap()
        .owner;
    let modeled_agent = &contract
        .objects
        .iter()
        .find(|row| row.id == "agent-created")
        .unwrap()
        .owner;
    let directory = tempfile::tempdir()?;
    let root = crate::store::identity::KeyIdentity::load_or_create(
        directory.path().join("node.key"),
        None,
    )?;
    let agent_binding = crate::store::identity::provision_agent_identity(&root)?;
    let sibling_binding = crate::store::identity::provision_agent_identity(&root)?;
    let agent = crate::store::identity::resolve_agent_identity(&root, &agent_binding)?;
    let sibling = crate::store::identity::resolve_agent_identity(&root, &sibling_binding)?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(root.did())
            .build()
            .await?,
    );
    let owner_did = escape_graphql_string(root.did());
    let user = ConfigAccess::local_as(node.clone(), identity::Did::new(root.did())?);
    let worker = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did())?,
        agent.as_ref(),
    )?;
    let other = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(sibling.did())?,
        sibling.as_ref(),
    )?;
    let ctx = defra_core::RequestContext::default().with_identity(Some(root.did().to_owned()));
    let policy = node
        .add_dac_policy_with_counter_as(&ctx, &contract.policy_yaml, 0)
        .await?;
    assert_eq!(policy, gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID);
    for schema in [
        gents_protocol::schemas::NODE,
        gents_protocol::schemas::AGENT,
        gents_protocol::schemas::AGENT_SESSION,
    ] {
        user.add_collection(schema).await?;
    }
    for schema in [
        gents_protocol::schemas::TRIGGER_FIRE,
        gents_protocol::schemas::FIRE_OUTCOME,
    ] {
        user.add_collection(schema).await?;
    }
    let node_row = user
        .write(
            "test.receipt.node",
            &format!(
                "mutation {{create_Node(input:{{node_did:\"{}\"}}){{_docID}}}}",
                escape_graphql_string(root.did())
            ),
        )
        .await?;
    let node_doc = crate::store::graphql::created_doc_id(&node_row, "Node")?;
    let agent_row = user.write("test.receipt.agent", &format!("mutation {{create_Agent(input:{{node_did:\"{}\",agent_did:\"{}\",creation_nonce:\"{}\"}}){{_docID}}}}", escape_graphql_string(root.did()), escape_graphql_string(agent.did()), escape_graphql_string(&agent_binding.creation_nonce))).await?;
    let agent_doc = crate::store::graphql::created_doc_id(&agent_row, "Agent")?;
    user.add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
        .await?;
    user.add_actor_relationship(
        "Agent",
        &agent_doc,
        "executor",
        &identity::Did::new(agent.did())?,
    )
    .await?;
    let session_row = user.write("test.receipt.session", &format!("mutation {{create_AgentSession(input:{{node_did:\"{}\",session_id:\"session\",agent_did:\"{}\"}}){{_docID}}}}", escape_graphql_string(root.did()), escape_graphql_string(agent.did()))).await?;
    let session_doc = crate::store::graphql::created_doc_id(&session_row, "AgentSession")?;
    user.add_resource_relationship("AgentSession", &session_doc, "parent", "agents", &agent_doc)
        .await?;
    user.add_actor_relationship(
        "AgentSession",
        &session_doc,
        "working_agent",
        &identity::Did::new(agent.did())?,
    )
    .await?;
    let mut receipts = std::collections::BTreeMap::new();
    for (key, access) in [("manual", &user), ("triggered", &worker)] {
        let doc = access.transact_write("test.receipt.publish", |txn| {
            let session_doc = &session_doc;
            let agent = &agent;
            let owner_did = &owner_did;
            Box::pin(async move {
                let response = txn.execute(&format!("mutation {{create_TriggerFire(input:{{fire_key:\"{}\",owner_did:\"{owner_did}\",session_id:\"session\"}}){{_docID}}}}", escape_graphql_string(key))).await?;
                let doc = crate::store::graphql::created_doc_id(&response, "TriggerFire")?;
                txn.add_resource_relationship("TriggerFire", &doc, "parent", "sessions", session_doc).await?;
                txn.add_actor_relationship("TriggerFire", &doc, "working_agent", &identity::Did::new(agent.did())?).await?;
                Ok::<_, anyhow::Error>(doc)
            })
        }).await?;
        receipts.insert(key, doc);
    }
    let outcome_doc = worker.transact_write("test.outcome.publish", |txn| {
        let session_doc = &session_doc;
        let agent = &agent;
        let owner_did = &owner_did;
        Box::pin(async move {
            let response = txn.execute(&format!("mutation {{create_FireOutcome(input:{{handoff_id:\"outcome\",owner_did:\"{owner_did}\",session_id:\"session\"}}){{_docID}}}}")).await?;
            let doc = crate::store::graphql::created_doc_id(&response, "FireOutcome")?;
            txn.add_resource_relationship("FireOutcome", &doc, "parent", "sessions", session_doc).await?;
            txn.add_actor_relationship("FireOutcome", &doc, "working_agent", &identity::Did::new(agent.did())?).await?;
            Ok::<_, anyhow::Error>(doc)
        })
    }).await?;
    assert!(other
        .execute(&format!(
            "{{FireOutcome(filter:{{_docID:{{_eq:\"{}\"}}}}){{_docID}}}}",
            escape_graphql_string(&outcome_doc)
        ))
        .await?["data"]["FireOutcome"]
        .as_array()
        .unwrap()
        .is_empty());
    let mut shared = false;
    for observation in &contract.task_receipt_cases {
        if observation["producer_reader"] == true && !shared {
            user.add_actor_relationship(
                "Agent",
                &agent_doc,
                "reader",
                &identity::Did::new(sibling.did())?,
            )
            .await?;
            shared = true;
        }
        let creator = observation["creator"].as_str().unwrap();
        let doc = &receipts[if creator == modeled_user {
            "manual"
        } else {
            "triggered"
        }];
        let actor = observation["actor"].as_str().unwrap();
        let access = if actor == modeled_user {
            &user
        } else if actor == modeled_agent {
            &worker
        } else {
            &other
        };
        let allowed = match observation["permission"].as_str().unwrap() {
            "read" => !access.execute(&format!("{{TriggerFire(filter:{{_docID:{{_eq:\"{}\"}}}}){{_docID}}}}", escape_graphql_string(doc))).await?["data"]["TriggerFire"].as_array().unwrap().is_empty(),
            "update" => match access.write("test.receipt.update", &format!("mutation {{update_TriggerFire(docID:\"{}\",input:{{goal_assignment_applied:true}}){{_docID}}}}", escape_graphql_string(doc))).await {
                Ok(response) => !response["data"]["update_TriggerFire"].as_array().unwrap().is_empty(),
                Err(error) if error.is_graphql_rejection() => false,
                Err(error) => return Err(error.into()),
            },
            _ => unreachable!(),
        };
        assert_eq!(
            allowed,
            observation["expected"].as_bool().unwrap(),
            "{observation}"
        );
    }
    assert_eq!(
        other
            .execute(&format!(
                "{{FireOutcome(filter:{{_docID:{{_eq:\"{}\"}}}}){{_docID}}}}",
                escape_graphql_string(&outcome_doc)
            ))
            .await?["data"]["FireOutcome"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    node.shutdown().await?;
    Ok(())
}
