use super::*;
use gents_session::request::lifecycle::materialize::{
    apply_request_session_projection, build_request, created_session_provenance_in_txn,
    pending_agent_request_spec,
    write_pending_agent_request_with_lineage_workspace_and_conversation_title,
};
use gents_session::request::lifecycle::{build_signed_request, ExecutionOrigin, TriggerLineage};
use gents_store::graphql::escape_graphql_string;

async fn keyed_delivery(
    node: &std::sync::Arc<EmbeddedNode>,
    owner: &str,
    agent: &dyn gents_store::identity::NodeIdentity,
    document: &str,
    session: &str,
    create_session: bool,
) -> Result<TaskDeliveryAdmission> {
    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: owner.to_owned(),
        trigger_id: "player-red".to_owned(),
        source_collection: "Schedule".to_owned(),
        source_doc_id: document.to_owned(),
    };
    let fire = gents_protocol::trigger_delivery::TriggerFire {
        fire_key: identity.fire_key(),
        request_id: identity.request_id(),
        session_id: session.to_owned(),
        identity,
        task_id: "play".to_owned(),
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
        created_at: "2030-06-01T00:00:00Z".to_owned(),
    };
    let prepared = crate::triggers::engine::durable::PreparedFire {
        receipt: fire.clone(),
        target_existing: true,
        create_session,
        replace_prior: false,
        event_source_generation: None,
        max_fires_per_hour: gents_documents::DEFAULT_MAX_FIRES_PER_HOUR,
    };
    let spec = pending_agent_request_spec(
        owner,
        agent.did(),
        "play",
        gents_session::request::lifecycle::ExecutionOrigin::Scheduled,
        gents_session::request::lifecycle::TriggerLineage {
            trigger_id: Some("player-red".to_owned()),
            trigger_kind: Some("schedule".to_owned()),
            ..Default::default()
        },
        None,
        None,
        &fire.request_id,
        session,
        Some(&fire.fire_key),
        None,
        Some("player-red-document"),
    )?;
    let create = build_request(spec)?;
    write_trigger_delivery_with_identity(
        node,
        ::identity::Did::new(agent.did()).unwrap(),
        &prepared,
        &create,
        Some(agent),
    )
    .await
}

/// The claim owner runs for each request as its own committed transaction;
/// the unique `(node_did, session_id)` index resolves the race between them.
#[tokio::test]
async fn fires_for_one_key_admit_before_it_exists_and_share_one_session() {
    let (db, _user, _user_access, binding) =
        crate::test_support::signed_test_node("keyed-session-fires").await;
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
    let owner = db.node_identity.did().to_owned();
    let node = &db.node;
    let key = "game:g1:red";
    let (first, second, third) = tokio::join!(
        keyed_delivery(node, &owner, agent.as_ref(), "turn-1", key, true),
        keyed_delivery(node, &owner, agent.as_ref(), "turn-2", key, true),
        keyed_delivery(node, &owner, agent.as_ref(), "turn-3", key, true),
    );
    let admissions = [first.unwrap(), second.unwrap(), third.unwrap()];
    assert!(admissions
        .iter()
        .all(|admission| admission.request.session_id == key));

    let refused = keyed_delivery(node, &owner, agent.as_ref(), "turn-4", "game:g2:red", false)
        .await
        .err()
        .expect("a literal target without create_session still requires an owned session");
    assert!(
        format!("{refused:#}").contains("Task target session is missing"),
        "{refused:#}"
    );

    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let now = "2030-06-01T00:00:01Z";
    let requests = admissions.iter().map(|admission| {
        let doc = admission.request.doc_id.clone();
        let access = &access;
        async move {
            access
                .transact_write("test.project_keyed_claim", |txn| {
                    let doc = doc.clone();
                    Box::pin(async move {
                        let rows = txn
                            .execute(&format!(
                                "{{AgentRequest(filter: {{_docID: {{_eq: \"{doc}\"}}}}) {{{}}}}}",
                                gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
                            ))
                            .await?;
                        let row: gents_protocol::row::AgentRequestRow =
                            serde_json::from_value(rows["data"]["AgentRequest"][0].clone())?;
                        let request = gents_protocol::row::AgentRequest::try_from(row)
                            .map_err(anyhow::Error::msg)?;
                        apply_request_session_projection(txn, &request, now).await
                    })
                })
                .await
        }
    });
    for result in futures::future::join_all(requests).await {
        result.unwrap();
    }

    let sessions = access
        .execute(&format!(
            r#"{{ AgentSession(filter: {{session_id: {{_eq: "{key}"}}}}) {{ agent_did provenance }} }}"#
        ))
        .await
        .unwrap();
    let rows = sessions["data"]["AgentSession"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "one session per key: {rows:?}");
    assert_eq!(rows[0]["agent_did"], agent.did());
    assert_eq!(
        rows[0]["provenance"]["task_id"], "play",
        "AgentSession provenance names the Task from its fire receipt"
    );
}

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
    use gents_store::identity::NodeIdentity;
    let identity_dir = tempfile::tempdir().unwrap();
    let db = gents_inference::test_support::test_db(
        "task-admission-atomic",
        crate::lifecycle_hooks::composition(),
    )
    .await;
    let node = db.node.clone();
    let node_did = db.node_identity.did();
    let runtime = gents_store::test_support::held_runtime_identity(node_did);
    let user = gents_store::identity::KeyIdentity::load_or_create(
        identity_dir.path().join("user.key"),
        None,
    )
    .unwrap();
    let binding = gents_store::identity::provision_agent_identity(&runtime).unwrap();
    let agent = gents_store::identity::resolve_agent_identity(&runtime, &binding).unwrap();
    let user_access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        user.did().parse().unwrap(),
    );
    let setup_access = gents_store::config_client::ConfigAccess::local(node.clone());
    let (node_doc, agent_doc) = setup_access.transact_write("test.task.agent", |txn| {
        let binding = &binding;
        Box::pin(async move {
            let created = txn.execute_with_variables(
                "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                &serde_json::json!({"input": {"node_did": node_did}}),
            ).await?;
            let node_doc = gents_store::graphql::created_doc_id(&created, "Node")?;
            let created = txn.execute_with_variables(
                "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                &serde_json::json!({"input": {"node_did": node_did,
                    "agent_did": binding.agent_did, "creation_nonce": binding.creation_nonce,
                    "inference_profile_id": "test-profile"}}),
            ).await?;
            let agent_doc = gents_store::graphql::created_doc_id(&created, "Agent")?;
            Ok::<_, anyhow::Error>((node_doc, agent_doc))
        })
    }).await.unwrap();
    // Relationship writes need a signed actor; the node root owns the Agent it created.
    let node_actor = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(runtime.did()).unwrap(),
    );
    gents_store::identity::GrantSet::OperatorUser
        .grant(&node_actor, &identity::Did::new(user.did()).unwrap())
        .await
        .unwrap();
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
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: node_did.into(),
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
            node_did,
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
        node_did,
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
    let agent_did = identity::Did::new(agent.did()).unwrap();
    let (observed_access, working_agent) = (&access, &agent_did);
    let session_doc = move || async move {
        observed_access
            .execute("{ AgentSession { _docID relationships_pending } }")
            .await
            .unwrap()["data"]["AgentSession"]
            .as_array()
            .unwrap()
            .first()
            .map(|row| {
                assert_eq!(row["relationships_pending"], false);
                row["_docID"].as_str().unwrap().to_owned()
            })
    };
    let observe = move || async move {
        let requests = observed_access
            .execute("{ AgentRequest { request_id } }")
            .await
            .unwrap();
        let session = session_doc().await;
        let relationships = match &session {
            Some(doc) => observed_access
                .list_document_relationships("AgentSession", doc)
                .await
                .unwrap(),
            None => Vec::new(),
        };
        gents_lean_vocab::families::session_publication_task::TaskSessionPublication {
            request: !requests["data"]["AgentRequest"]
                .as_array()
                .unwrap()
                .is_empty(),
            session: session.is_some(),
            parent: relationships.iter().any(|row| row.relation == "parent"),
            working_agent: relationships.iter().any(|row| {
                row.relation == "working_agent" && row.subject == working_agent.as_str()
            }),
        }
    };
    let mut deliveries = Vec::new();
    for case in gents_lean_vocab::families::session_publication_task::cases() {
        let before = &case.input.before;
        if observe().await.working_agent && !before.working_agent {
            gents_store::test_support::relationships::actor_in_txn(
                observed_access,
                "AgentSession",
                &session_doc().await.unwrap(),
                "working_agent",
                working_agent,
                true,
            )
            .await
            .unwrap();
        }
        let start = observe().await;
        assert_eq!(
            (
                start.request,
                start.session,
                start.parent,
                start.working_agent
            ),
            (
                before.request,
                before.session,
                before.parent,
                before.working_agent
            ),
            "{} starts from its modeled state",
            case.input.name
        );
        if case.input.committed {
            let delivery = write_task_delivery(&access, &fire, false, &create)
                .await
                .unwrap();
            assert_eq!(delivery.duplicate, !case.input.fresh);
            deliveries.push(delivery);
        } else {
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
            let fires = access
                .execute("{ TriggerFire { fire_key } }")
                .await
                .unwrap();
            assert!(fires["data"]["TriggerFire"].as_array().unwrap().is_empty());
        }
        case.check(observe().await);
    }
    let first = &deliveries[0];
    for retry in &deliveries[1..] {
        assert_eq!(first.request.doc_id, retry.request.doc_id);
    }
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
    assert_eq!(session["session_id"], session_id);
    for case in gents_lean_vocab::families::contracts_atomic_admission_unattended::cases::<String>()
    {
        case.check(
            gents_lean_vocab::families::contracts_atomic_admission_unattended::AdmissionTitle {
                title: serde_json::from_value(session["title"].clone()).unwrap(),
            },
        );
    }
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
    let household = gents_lean_vocab::lean_identity_household_cases()
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
                gents_lean_vocab::LeanIdentityAction::Check {
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
    let outsider = gents_store::identity::KeyIdentity::load_or_create(
        identity_dir.path().join("outsider.key"),
        None,
    )
    .unwrap();
    gents_store::identity::GrantSet::OperatorUser
        .grant(&node_actor, &identity::Did::new(outsider.did()).unwrap())
        .await
        .unwrap();
    let outsider_access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        outsider.did().parse().unwrap(),
    );
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
    for trace in gents_lean_vocab::families::identity_publication_session_recovery::cases() {
        gents_store::test_support::relationships::resource_in_txn(
            &access,
            "AgentSession",
            session_doc,
            "parent",
            "agents",
            &agent_doc,
            true,
        )
        .await
        .unwrap();
        access.write("test.session.interrupted", &format!(
        r#"mutation {{ update_AgentSession(docID: "{}", input: {{ relationships_pending: true }}) {{ _docID }} }}"#,
        escape_graphql_string(session_doc),
    )).await.unwrap();
        let actor = identity::Did::new(agent.did()).unwrap();
        assert_eq!(
            gents_session::session::recover_agent_session_relationships_with_access(
                &access, node_did, &actor
            )
            .await
            .unwrap(),
            1
        );
        let recovered = user_access
            .execute("{ AgentSession { relationships_pending } }")
            .await
            .unwrap();
        trace.check(
            recovered["data"]["AgentSession"][0]["relationships_pending"]
                .as_bool()
                .unwrap(),
        );
    }
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
    let receipt_contract = gents_lean_vocab::lean_identity_household_cases()
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
    gents_store::test_support::relationships::resource_in_txn(
        &access,
        "TriggerFire",
        receipt_doc,
        "parent",
        "sessions",
        session_doc,
        true,
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
    let db = gents_inference::test_support::test_db(
        "reserved-trigger-cause-refused",
        crate::lifecycle_hooks::composition(),
    )
    .await;
    let node = db.node.clone();
    let node_did = db.node_identity.did();
    let identity = gents_store::test_support::held_runtime_identity(node_did);
    let binding = gents_store::identity::provision_agent_identity(&identity).unwrap();
    let agent = gents_store::identity::resolve_agent_identity(&identity, &binding).unwrap();
    let access = gents_store::config_client::ConfigAccess::local(node.clone());
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
        node_did,
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
        owner_did: node_did.into(),
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
        node_did,
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
