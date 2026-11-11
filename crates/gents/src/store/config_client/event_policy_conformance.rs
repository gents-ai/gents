use super::ConfigAccess;
use crate::store::graphql::escape_graphql_string;
use crate::store::identity::NodeIdentity;
use std::sync::Arc;

#[tokio::test]
async fn modeled_event_configuration_grants_use_native_permissions() -> anyhow::Result<()> {
    let contract = gents_lean_vocab::lean_identity_household_cases()
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
            gents_lean_vocab::LeanIdentitySubject::Actor { actor }
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
