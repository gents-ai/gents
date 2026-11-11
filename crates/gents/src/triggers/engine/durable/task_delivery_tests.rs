use super::*;
use crate::request::lifecycle::materialize::{
    created_session_provenance_in_txn, pending_agent_request_spec,
    write_pending_agent_request_with_lineage_workspace_and_conversation_title,
};
use crate::request::lifecycle::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};
use crate::request::lifecycle::{build_signed_request, ExecutionOrigin, TriggerLineage};
use crate::store::graphql::escape_graphql_string;
use std::sync::Arc;

#[test]
fn task_fire_requires_its_canonical_identity_and_exact_session() {
    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: "node".into(),
        trigger_id: "manual:task:invocation".into(),
        source_collection: "Task".into(),
        source_doc_id: "invocation".into(),
    };
    let request = identity.request_id();
    let mut fire = serde_json::json!({
        "fire_key": identity.fire_key(), "trigger_id": identity.trigger_id,
        "source_collection": identity.source_collection, "source_doc_id": identity.source_doc_id,
        "session_id": "session", "task_id": "task"
    });
    assert_eq!(
        gents_protocol::trigger_delivery::verified_task_fire(
            &fire,
            "node",
            "session",
            &request,
            "manual:task:invocation"
        )
        .map(|(_, task)| task),
        Some("task")
    );
    assert!(gents_protocol::trigger_delivery::verified_task_fire(
        &fire,
        "node",
        "other",
        &request,
        "manual:task:invocation"
    )
    .is_none());
    fire["fire_key"] = serde_json::json!("tampered");
    assert!(gents_protocol::trigger_delivery::verified_task_fire(
        &fire,
        "node",
        "session",
        &request,
        "manual:task:invocation"
    )
    .is_none());
}

#[tokio::test]
async fn task_admission_is_atomic_idempotent_and_defers_goal_assignment() {
    use crate::store::identity::NodeIdentity;
    let identity_dir = tempfile::tempdir().unwrap();
    let runtime = pin_fixed_signing_identity(identity_dir.path());
    let user =
        crate::KeyIdentity::load_or_create(identity_dir.path().join("user.key"), None).unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(PIN_FIXED_DID)
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let binding = crate::store::identity::provision_agent_identity(&runtime).unwrap();
    let agent = crate::store::identity::resolve_agent_identity(&runtime, &binding).unwrap();
    let user_access = crate::ConfigAccess::local_as(node.clone(), user.did().parse().unwrap());
    let setup_access = crate::ConfigAccess::local(node.clone());
    let (node_doc, agent_doc) = setup_access.transact_write("test.task.agent", |txn| {
        let binding = &binding;
        Box::pin(async move {
            let created = txn.execute_with_variables(
                "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                &serde_json::json!({"input": {"node_did": PIN_FIXED_DID}}),
            ).await?;
            let node_doc = crate::store::graphql::created_doc_id(&created, "Node")?;
            let created = txn.execute_with_variables(
                "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                &serde_json::json!({"input": {"node_did": PIN_FIXED_DID,
                    "agent_did": binding.agent_did, "creation_nonce": binding.creation_nonce,
                    "inference_profile_id": "test-profile"}}),
            ).await?;
            let agent_doc = crate::store::graphql::created_doc_id(&created, "Agent")?;
            Ok::<_, anyhow::Error>((node_doc, agent_doc))
        })
    }).await.unwrap();
    // Relationship writes need a signed actor; the node root owns the Agent it created.
    let node_actor =
        crate::ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
    node_actor
        .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
        .await
        .unwrap();
    node_actor
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "executor",
            &identity::Did::new(&binding.agent_did).unwrap(),
        )
        .await
        .unwrap();
    node_actor
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "administrator",
            &identity::Did::new(user.did()).unwrap(),
        )
        .await
        .unwrap();
    let access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: PIN_FIXED_DID.into(),
        trigger_id: "manual:task:invocation-key".into(),
        source_collection: "Task".into(),
        source_doc_id: "invocation-key".into(),
    };
    let key = task_fire_key(&identity);
    let session_id = format!("trigger-session:{key}");
    let fire = gents_protocol::trigger_delivery::TriggerFire {
        fire_key: key.clone(),
        identity,
        task_id: "task".into(),
        request_id: format!("trigger-request:{key}"),
        session_id: session_id.clone(),
        goal_id: Some(gents_protocol::goal::deterministic_goal_id(
            PIN_FIXED_DID,
            &session_id,
        )),
        goal_objective: Some("complete assignment".into()),
        goal_token_budget: Some(100),
        goal_assignment_applied: false,
        emit_outcome: true,
        queued_serial: false,
        source_handoff_id: Some("invocation-key".into()),
        reply_session_id: None,
        shard_id: None,
        attempt: None,
        created_at: "2026-01-01T00:00:00Z".into(),
    };
    let spec = pending_agent_request_spec(
        PIN_FIXED_DID,
        agent.did(),
        "complete assignment",
        ExecutionOrigin::Interactive,
        TriggerLineage {
            trigger_id: None,
            trigger_kind: Some("manual".into()),
            source_doc_id: None,
            correlation: None,
            trigger_context: None,
        },
        Some("task"),
        None,
        &fire.request_id,
        &fire.session_id,
        Some(&fire.fire_key),
        None,
        None,
    )
    .unwrap();
    let create = build_signed_request(spec, RequestSigner::Identity(agent.as_ref()))
        .await
        .unwrap();
    assert!(write_task_delivery(&access, &fire, true, &create)
        .await
        .is_err());
    let before = access
        .execute(
            "{ TriggerFire { fire_key } AgentRequest { request_id } AgentSession { session_id } }",
        )
        .await
        .unwrap();
    assert_eq!(before["data"]["TriggerFire"].as_array().unwrap().len(), 0);
    assert_eq!(before["data"]["AgentRequest"].as_array().unwrap().len(), 0);
    assert_eq!(before["data"]["AgentSession"].as_array().unwrap().len(), 0);
    let cases =
        &crate::lean_vocab_test::lean_contract_snapshot().identity_task_session_publication_cases;
    let modeled = |name: &str| cases.iter().find(|case| case["name"] == name).unwrap();
    let aborted: anyhow::Result<()> = access
        .transact_write("test.task.abort_after_session_relationships", |txn| {
            let fire = &fire;
            let create = &create;
            Box::pin(async move {
                stage_task_delivery(txn, fire, false, create).await?;
                anyhow::bail!("forced abort after staged Task session publication")
            })
        })
        .await;
    assert!(aborted
        .unwrap_err()
        .to_string()
        .contains("forced abort after staged Task session publication"));
    let before = access
        .execute(
            "{ TriggerFire { fire_key } AgentRequest { request_id } AgentSession { session_id } }",
        )
        .await
        .unwrap();
    assert_eq!(
        !before["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty(),
        modeled("aborted")["request"]
    );
    assert_eq!(
        !before["data"]["AgentSession"]
            .as_array()
            .unwrap()
            .is_empty(),
        modeled("aborted")["session"]
    );
    assert!(before["data"]["TriggerFire"].as_array().unwrap().is_empty());
    let first = write_task_delivery(&access, &fire, false, &create)
        .await
        .unwrap();
    let session_before_retry = access
        .execute("{ AgentSession { _docID relationships_pending } }")
        .await
        .unwrap();
    let session_doc = session_before_retry["data"]["AgentSession"][0]["_docID"]
        .as_str()
        .unwrap();
    assert_eq!(
        session_before_retry["data"]["AgentSession"][0]["relationships_pending"],
        false
    );
    let removed = node
        .delete_dac_actor_relationship(
            agent.did(),
            "AgentSession",
            session_doc,
            "working_agent",
            agent.did(),
        )
        .await
        .unwrap();
    assert_eq!(removed, modeled("created")["working_agent"]);
    let retry = write_task_delivery(&access, &fire, false, &create)
        .await
        .unwrap();
    let restored = node
        .delete_dac_actor_relationship(
            agent.did(),
            "AgentSession",
            session_doc,
            "working_agent",
            agent.did(),
        )
        .await
        .unwrap();
    assert_eq!(
        restored,
        modeled("duplicate-after-revocation")["working_agent"]
    );
    assert_eq!(first.request.doc_id, retry.request.doc_id);
    assert!(!first.duplicate);
    assert!(retry.duplicate);
    let after = access.execute("{ TriggerFire { fire_key goal_assignment_applied } AgentRequest { request_id lifecycle_state } Goal { goal_id } }").await.unwrap();
    assert_eq!(after["data"]["TriggerFire"].as_array().unwrap().len(), 1);
    assert_eq!(
        after["data"]["TriggerFire"][0]["goal_assignment_applied"],
        false
    );
    assert_eq!(after["data"]["AgentRequest"].as_array().unwrap().len(), 1);
    assert_eq!(
        after["data"]["AgentRequest"][0]["lifecycle_state"],
        "pending"
    );
    assert_eq!(after["data"]["Goal"].as_array().unwrap().len(), 0);
    let session = access
        .execute("{ AgentSession { _docID session_id title provenance } }")
        .await
        .unwrap();
    assert_eq!(session["data"]["AgentSession"].as_array().unwrap().len(), 1);
    let session = &session["data"]["AgentSession"][0];
    let modeled = crate::lean_vocab_test::lean_contract_snapshot()
        .session_document_cases
        .projection
        .iter()
        .find(|case| {
            case["operation"] == "atomic_admission" && case["name"] == "agent_unattended_creation"
        })
        .expect("modeled unattended admission");
    assert_eq!(session["session_id"], session_id);
    assert_eq!(session["title"], modeled["after"]["session"]["title"]);
    assert_eq!(session["provenance"]["task_id"], "task");
    assert_eq!(
        session["provenance"]["opening_request_doc_id"],
        first.request.doc_id
    );
    let owner = access
        .transact_readonly("test.task.session_owner", |txn| {
            Box::pin(async move {
                txn.document_owners(
                    "AgentSession",
                    &[session["_docID"].as_str().unwrap().to_owned()],
                )
                .await
            })
        })
        .await
        .unwrap();
    assert_eq!(
        owner[0].owner.as_ref().map(identity::Did::as_str),
        Some(agent.did())
    );
    let user_sessions = user_access
        .execute("{ AgentSession { _docID relationships_pending } }")
        .await
        .unwrap();
    assert_eq!(
        user_sessions["data"]["AgentSession"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        user_sessions["data"]["AgentSession"][0]["_docID"],
        session["_docID"]
    );
    assert_eq!(
        user_sessions["data"]["AgentSession"][0]["relationships_pending"],
        false
    );
    let household = crate::lean_vocab_test::lean_identity_household_cases()
        .first()
        .expect("modeled household");
    let modeled_user = &household
        .objects
        .iter()
        .find(|object| object.resource == "nodes" && object.id == "home")
        .unwrap()
        .owner;
    let modeled_agent = &household
        .objects
        .iter()
        .find(|object| object.resource == "delivered_requests" && object.id == "automated-request")
        .unwrap()
        .owner;
    let modeled_outsider = household.enrolled.last().expect("modeled outsider");
    let expected_read = |actor: &str| {
        household
            .actions
            .iter()
            .find_map(|action| match action {
                crate::lean_vocab_test::LeanIdentityAction::Check {
                    actor: checked,
                    resource,
                    id,
                    permission,
                    expected,
                } if checked == actor
                    && resource == "delivered_requests"
                    && id == "automated-request"
                    && permission == "read" =>
                {
                    Some(*expected)
                }
                _ => None,
            })
            .expect("modeled automated request read")
    };
    let outsider =
        crate::KeyIdentity::load_or_create(identity_dir.path().join("outsider.key"), None).unwrap();
    let outsider_access =
        crate::ConfigAccess::local_as(node.clone(), outsider.did().parse().unwrap());
    let request_query = format!(
        "{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{}\" }} }}) {{ _docID lifecycle_state }} }}",
        escape_graphql_string(&first.request.doc_id)
    );
    for (reader, modeled_actor) in [
        (&user_access, modeled_user.as_str()),
        (&access, modeled_agent.as_str()),
        (&outsider_access, modeled_outsider.as_str()),
    ] {
        let response = reader.execute(&request_query).await.unwrap();
        let visible = !response["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty();
        assert_eq!(
            visible,
            expected_read(modeled_actor),
            "Task request visibility for {modeled_actor}"
        );
    }
    access.write("test.task.finish", &format!(
        "mutation {{ update_AgentRequest(docID: \"{}\", input: {{ lifecycle_state: \"completed\" }}) {{ _docID }} }}",
        escape_graphql_string(&first.request.doc_id)
    )).await.unwrap();
    let observed = user_access.execute(&request_query).await.unwrap();
    assert_eq!(
        observed["data"]["AgentRequest"][0]["lifecycle_state"],
        "completed"
    );
    let session_doc = session["_docID"].as_str().unwrap();
    let trace = crate::lean_vocab_test::lean_contract_snapshot()
        .identity_publication_cases
        .iter()
        .find(|case| case["name"] == "interrupted_then_repaired")
        .unwrap();
    access
        .delete_resource_relationship("AgentSession", session_doc, "parent", "agents", &agent_doc)
        .await
        .unwrap();
    access.write("test.session.interrupted", &format!(
        r#"mutation {{ update_AgentSession(docID: "{}", input: {{ relationships_pending: true }}) {{ _docID }} }}"#,
        escape_graphql_string(session_doc),
    )).await.unwrap();
    let actor = identity::Did::new(agent.did()).unwrap();
    assert_eq!(
        crate::session::recover_agent_session_relationships_with_access(
            &access,
            PIN_FIXED_DID,
            &actor
        )
        .await
        .unwrap(),
        1
    );
    let recovered = user_access
        .execute("{ AgentSession { relationships_pending } }")
        .await
        .unwrap();
    assert_eq!(
        recovered["data"]["AgentSession"][0]["relationships_pending"],
        trace["steps"][1]["after"]["pending"]
    );
    let request_row = access
        .execute(&format!(
            "{{ AgentRequest {{ {} }} }}",
            gents_protocol::row::AGENT_REQUEST_FIELDS
        ))
        .await
        .unwrap();
    let request = AgentRequest::try_from(
        serde_json::from_value::<gents_protocol::row::AgentRequestRow>(
            request_row["data"]["AgentRequest"][0].clone(),
        )
        .unwrap(),
    )
    .unwrap();
    let provenance = access
        .transact_readonly("test.task_provenance", |txn| {
            let request = &request;
            Box::pin(async move { created_session_provenance_in_txn(txn, request).await })
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(provenance.task_id.as_deref(), Some("task"));
    assert_eq!(
        provenance.opening_request_doc_id.as_deref(),
        Some(first.request.doc_id.as_str())
    );
    let receipt = access.execute("{TriggerFire {_docID}}").await.unwrap();
    let receipt_doc = receipt["data"]["TriggerFire"][0]["_docID"]
        .as_str()
        .unwrap();
    let receipt_contract = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let creator = &receipt_contract
        .objects
        .iter()
        .find(|object| object.id == "agent-created")
        .unwrap()
        .owner;
    let user = &receipt_contract
        .objects
        .iter()
        .find(|object| object.id == "home")
        .unwrap()
        .owner;
    let sibling = receipt_contract
        .task_receipt_cases
        .iter()
        .find(|case| {
            case["creator"] == *creator
                && case["permission"] == "read"
                && case["producer_reader"] == false
                && case["expected"] == false
        })
        .unwrap()["actor"]
        .as_str()
        .unwrap();
    for (reader, actor) in [
        (&user_access, user.as_str()),
        (&access, creator.as_str()),
        (&outsider_access, sibling),
    ] {
        let expected = receipt_contract
            .task_receipt_cases
            .iter()
            .find(|case| {
                case["creator"] == *creator
                    && case["actor"] == actor
                    && case["permission"] == "read"
                    && case["producer_reader"] == false
            })
            .unwrap()["expected"]
            .as_bool()
            .unwrap();
        let response = reader.execute("{TriggerFire {_docID}}").await.unwrap();
        assert_eq!(
            !response["data"]["TriggerFire"]
                .as_array()
                .unwrap()
                .is_empty(),
            expected
        );
    }
    access
        .delete_resource_relationship(
            "TriggerFire",
            receipt_doc,
            "parent",
            "sessions",
            session_doc,
        )
        .await
        .unwrap();
    assert!(
        write_task_delivery(&access, &fire, false, &create)
            .await
            .unwrap()
            .duplicate
    );
    assert!(
        user_access.execute("{TriggerFire {_docID}}").await.unwrap()["data"]["TriggerFire"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn reserved_trigger_cause_is_refused() {
    let identity_dir = tempfile::tempdir().unwrap();
    let identity = pin_fixed_signing_identity(identity_dir.path());
    let binding = crate::store::identity::provision_agent_identity(&identity).unwrap();
    let agent = crate::store::identity::resolve_agent_identity(&identity, &binding).unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(PIN_FIXED_DID)
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let access = crate::store::config_client::ConfigAccess::local(node.clone());
    let message =
        "trigger graph-trigger-retired belongs to a retired graph revision; it no longer runs";
    let lineage = || TriggerLineage {
        trigger_id: Some("graph-trigger-retired".into()),
        trigger_kind: Some("schedule".into()),
        source_doc_id: None,
        correlation: None,
        trigger_context: None,
    };

    let actor = ::identity::Did::new(agent.did()).unwrap();
    let err = write_pending_agent_request_with_lineage_workspace_and_conversation_title(
        &node,
        actor,
        PIN_FIXED_DID,
        agent.did(),
        "work",
        ExecutionOrigin::Interactive,
        lineage(),
        None,
        None,
        None,
        None,
        Some("trigger-doc"),
        None,
        agent.as_ref(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.to_string(), message);

    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: PIN_FIXED_DID.into(),
        trigger_id: "manual:task".into(),
        source_collection: "Task".into(),
        source_doc_id: "invocation-key".into(),
    };
    let key = task_fire_key(&identity);
    let session_id = format!("trigger-session:{key}");
    let fire = gents_protocol::trigger_delivery::TriggerFire {
        fire_key: key.clone(),
        identity,
        task_id: "task".into(),
        request_id: format!("trigger-request:{key}"),
        session_id: session_id.clone(),
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
        created_at: "2026-01-01T00:00:00Z".into(),
    };
    let spec = pending_agent_request_spec(
        PIN_FIXED_DID,
        agent.did(),
        "work",
        ExecutionOrigin::Interactive,
        lineage(),
        None,
        None,
        &fire.request_id,
        &fire.session_id,
        Some(&fire.fire_key),
        None,
        Some("trigger-doc"),
    )
    .unwrap();
    let create = build_signed_request(spec, RequestSigner::Identity(agent.as_ref()))
        .await
        .unwrap();
    let err = write_task_delivery(&access, &fire, false, &create)
        .await
        .err()
        .expect("reserved cause must be refused");
    assert_eq!(err.to_string(), message);

    let after = access
        .execute("{ TriggerFire { fire_key } AgentRequest { request_id } }")
        .await
        .unwrap();
    assert_eq!(after["data"]["TriggerFire"].as_array().unwrap().len(), 0);
    assert_eq!(after["data"]["AgentRequest"].as_array().unwrap().len(), 0);
}
