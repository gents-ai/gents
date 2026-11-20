use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use std::sync::Arc;

#[tokio::test]
async fn modeled_task_receipt_shares_native_session_permissions() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let root = gents_store::identity::KeyIdentity::load_or_create(
        directory.path().join("node.key"),
        None,
    )?;
    let agent_binding = gents_store::identity::provision_agent_identity(&root)?;
    let sibling_binding = gents_store::identity::provision_agent_identity(&root)?;
    let agent = gents_store::identity::resolve_agent_identity(&root, &agent_binding)?;
    let sibling = gents_store::identity::resolve_agent_identity(&root, &sibling_binding)?;
    let supervisor_binding = gents_store::identity::provision_agent_identity(&root)?;
    let supervisor_identity =
        gents_store::identity::resolve_agent_identity(&root, &supervisor_binding)?;
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
        .add_dac_policy_with_counter_as(&ctx, gents_protocol::schemas::AGENT_OWNERSHIP_POLICY, 0)
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
    let node_doc = gents_store::graphql::created_doc_id(&node_row, "Node")?;
    let agent_row = user.write("test.receipt.agent", &format!("mutation {{create_Agent(input:{{node_did:\"{}\",agent_did:\"{}\",creation_nonce:\"{}\"}}){{_docID}}}}", escape_graphql_string(root.did()), escape_graphql_string(agent.did()), escape_graphql_string(&agent_binding.creation_nonce))).await?;
    let agent_doc = gents_store::graphql::created_doc_id(&agent_row, "Agent")?;
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
    let session_doc = gents_store::graphql::created_doc_id(&session_row, "AgentSession")?;
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
                let doc = gents_store::graphql::created_doc_id(&response, "TriggerFire")?;
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
            let doc = gents_store::graphql::created_doc_id(&response, "FireOutcome")?;
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
    // Lean `Identity.Automation.publishOutcome`: the worker publishes an
    // outcome whose Task declares the supervisor as its outcome reader. The
    // checks cover every actor's read and the declared reader's update.
    let supervisor = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(supervisor_identity.did())?,
        supervisor_identity.as_ref(),
    )?;
    let requested_doc = worker.transact_write("test.outcome.requested", |txn| {
        let session_doc = &session_doc;
        let agent = &agent;
        let owner_did = &owner_did;
        let reader = supervisor_identity.did();
        Box::pin(async move {
            let response = txn.execute(&format!("mutation {{create_FireOutcome(input:{{handoff_id:\"requested\",owner_did:\"{owner_did}\",session_id:\"session\"}}){{_docID}}}}")).await?;
            let doc = gents_store::graphql::created_doc_id(&response, "FireOutcome")?;
            crate::triggers::engine::durable::stage_session_child_relationships(txn, "FireOutcome", &doc, session_doc, agent.did()).await?;
            crate::triggers::engine::durable::stage_outcome_reader(txn, &doc, reader).await?;
            Ok::<_, anyhow::Error>(doc)
        })
    }).await?;
    for case in gents_lean_vocab::families::household_outcome_reader_access::cases() {
        let (access, did) = match case.input.actor.as_str() {
            "user" => (&user, root.did()),
            "agent" => (&worker, agent.did()),
            "supervisor" => (&supervisor, supervisor_identity.did()),
            "sibling" => (&other, sibling.did()),
            actor => panic!("unmodeled outcome actor {actor}"),
        };
        let (collection, doc) = match (case.input.resource.as_str(), case.input.id.as_str()) {
            ("children", "outcome") => ("FireOutcome", &requested_doc),
            ("children", "other-outcome") => ("FireOutcome", &outcome_doc),
            ("sessions", "session") => ("AgentSession", &session_doc),
            ("agents", "agent-doc") => ("Agent", &agent_doc),
            ("nodes", "node") => ("Node", &node_doc),
            document => panic!("unmodeled outcome document {document:?}"),
        };
        let permission = match case.input.permission.as_str() {
            "read" => defra_node::DocumentPermission::Read,
            "update" => defra_node::DocumentPermission::Update,
            "delete" => defra_node::DocumentPermission::Delete,
            permission => panic!("unmodeled outcome permission {permission}"),
        };
        let allowed = node
            .document_acp()
            .check_doc_access(
                &defra_node::AcpIdentity::Authenticated(did.parse()?),
                permission,
                gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
                &case.input.resource,
                doc,
            )
            .await?;
        if case.input.permission == "read" {
            let visible = !access
                .execute(&format!(
                    "{{{collection}(filter:{{_docID:{{_eq:\"{}\"}}}}){{_docID}}}}",
                    escape_graphql_string(doc)
                ))
                .await?["data"][collection]
                .as_array()
                .unwrap()
                .is_empty();
            assert_eq!(visible, allowed, "{:?}", case.input);
        }
        case.check(allowed);
    }
    let mut shared = false;
    for observation in gents_lean_vocab::families::household_task_receipt_session::cases() {
        if observation.input.producer_reader && !shared {
            user.add_actor_relationship(
                "Agent",
                &agent_doc,
                "reader",
                &identity::Did::new(sibling.did())?,
            )
            .await?;
            shared = true;
        }
        let doc = &receipts[if observation.input.creator_role == "user" {
            "manual"
        } else {
            "triggered"
        }];
        let access = match observation.input.role.as_str() {
            "user" => &user,
            "agent" => &worker,
            _ => &other,
        };
        let allowed = match observation.input.permission.as_str() {
            "read" => !access.execute(&format!("{{TriggerFire(filter:{{_docID:{{_eq:\"{}\"}}}}){{_docID}}}}", escape_graphql_string(doc))).await?["data"]["TriggerFire"].as_array().unwrap().is_empty(),
            "update" => match access.write("test.receipt.update", &format!("mutation {{update_TriggerFire(docID:\"{}\",input:{{goal_assignment_applied:true}}){{_docID}}}}", escape_graphql_string(doc))).await {
                Ok(response) => !response["data"]["update_TriggerFire"].as_array().unwrap().is_empty(),
                Err(error) if error.is_graphql_rejection() => false,
                Err(error) => return Err(error.into()),
            },
            _ => unreachable!(),
        };
        observation.check(allowed);
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
