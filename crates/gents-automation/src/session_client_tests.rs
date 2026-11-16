use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_session::request::lifecycle::client::*;
use gents_session::test_support::{assert_block_signer, seed_owned_agent};
use gents_store::config_client::ConfigAccess;
use gents_store::identity::{KeyIdentity, NodeIdentity};

#[tokio::test]
async fn agent_goal_submission_stages_the_session_before_its_goal_and_rolls_back_together() {
    for case in gents_lean_vocab::families::contracts_goal_backed_runtime::cases() {
        let name = case.input.name.as_str();
        let (db, user, client, binding) = gents_inference::test_support::signed_test_node(
            &format!("runtime-goal-opening-{name}"),
            crate::lifecycle_hooks::composition(),
        )
        .await;
        let node = db.node.clone();
        let agent =
            gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
                .unwrap();
        let access = ConfigAccess::local_managed_agent(
            node.clone(),
            identity::Did::new(agent.did()).unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        let admin = ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(db.node_identity.did()).unwrap(),
        );
        let mut request = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "task-opening",
            db.node_identity.did(),
            agent.did(),
            agent.did(),
            "task-session",
            "work",
            "scheduled",
            "2026-11-04T00:00:00Z",
            AgentRequestAdmissionRecord::runtime_automated_trigger(agent.did(), "trigger"),
        );
        request.retry_key = Some("task-retry".into());
        request.caused_by_trigger_id = Some("trigger".into());
        request.caused_by_trigger_kind = Some("schedule".into());
        request.caused_by_trigger_doc_id = Some("trigger-doc".into());
        gents_session::request::admission::sign_agent_request_create(agent.as_ref(), &mut request)
            .await
            .unwrap();
        let opened_doc_id = if case.input.fresh {
            None
        } else {
            let opened = crate::goal::submit_goal_backed_request_with_identity(
                &node,
                identity::Did::new(agent.did()).unwrap(),
                db.node_identity.did(),
                "task-session",
                "finish work",
                None,
                &request,
                Some(agent.as_ref()),
            )
            .await
            .unwrap();
            let rows = access.execute(&format!("{{ AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ {} relationships_pending }} }}", gents_store::graphql::escape_graphql_string(&opened.doc_id), gents_protocol::request_admission::SIGNED_REQUEST_FIELDS)).await.unwrap();
            let row: gents_protocol::row::AgentRequestRow =
                serde_json::from_value(rows["data"]["AgentRequest"][0].clone()).unwrap();
            admin
                .grant_node_permission(
                    &identity::Did::new(agent.did()).unwrap(),
                    gents_store::config_client::NodePermission::DacRelationDelete,
                )
                .await
                .unwrap();
            gents_store::test_support::relationships::resource_in_txn(
                &access,
                "AgentRequest",
                &opened.doc_id,
                "parent",
                "agents",
                row.agent_doc_id.as_deref().unwrap(),
                true,
            )
            .await
            .unwrap();
            gents_store::test_support::relationships::resource_in_txn(
                &access,
                "AgentRequest",
                &opened.doc_id,
                "session",
                "sessions",
                row.session_doc_id.as_deref().unwrap(),
                true,
            )
            .await
            .unwrap();
            Some(opened.doc_id)
        };
        let mut attempt = request.clone();
        let altered = case.input.target_session != case.input.observed_session
            || case.input.target_agent != case.input.observed_agent
            || !case.input.commit;
        if case.input.target_session != case.input.observed_session {
            attempt.session_doc_id = Some("foreign-session".into());
        }
        if case.input.target_agent != case.input.observed_agent {
            attempt.agent_doc_id = Some("foreign-agent".into());
        }
        if !case.input.commit {
            attempt.input.goal_assignment =
                Some(gents_protocol::request_input::GoalAssignmentInput {
                    goal_id: "wrong-goal".into(),
                    root_request_doc_id: None,
                });
        }
        if altered {
            gents_session::request::admission::sign_agent_request_create(
                agent.as_ref(),
                &mut attempt,
            )
            .await
            .unwrap();
        }
        let foreign_actor = case.input.actor != case.input.selected;
        let (actor, signer): (&str, &dyn NodeIdentity) = if foreign_actor {
            (db.node_identity.did(), db.node_identity.as_ref())
        } else {
            (agent.did(), agent.as_ref())
        };
        let submitted = crate::goal::submit_goal_backed_request_with_identity(
            &node,
            identity::Did::new(actor).unwrap(),
            db.node_identity.did(),
            "task-session",
            "finish work",
            None,
            &attempt,
            Some(signer),
        )
        .await;
        let agent_rows = access
            .execute("{ AgentSession {_docID} AgentRequest {_docID} Goal {_docID} GoalCreationClaim {_docID} }")
            .await
            .unwrap();
        let user_rows = client
            .execute("{ AgentSession {_docID} AgentRequest {_docID} }")
            .await
            .unwrap();
        let observed =
            gents_lean_vocab::families::contracts_goal_backed_runtime::RuntimeGoalPublication {
                session: !user_rows["data"]["AgentSession"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                goal: !agent_rows["data"]["Goal"].as_array().unwrap().is_empty(),
                request: !agent_rows["data"]["AgentRequest"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                grants: !user_rows["data"]["AgentRequest"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
            };
        if case.input.fresh && !observed.request {
            assert!(
                submitted.is_err(),
                "{name}: a refused opening reported success"
            );
            for collection in ["AgentSession", "GoalCreationClaim"] {
                assert!(
                    agent_rows["data"][collection]
                        .as_array()
                        .unwrap()
                        .is_empty(),
                    "{name}: a refused opening left a {collection} behind"
                );
            }
        }
        if let Some(opened_doc_id) = &opened_doc_id {
            assert_eq!(
                &submitted
                    .as_ref()
                    .expect("duplicate opening recovers")
                    .doc_id,
                opened_doc_id,
                "{name}: a duplicate opening returns the request it opened"
            );
        }
        if name == "created" {
            let submitted = submitted.unwrap();
            let rows = access.execute(&format!("{{ AgentSession {{_docID provenance}} AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ {} relationships_pending }} }}", gents_store::graphql::escape_graphql_string(&submitted.doc_id), gents_protocol::request_admission::SIGNED_REQUEST_FIELDS)).await.unwrap();
            let session = &rows["data"]["AgentSession"][0];
            let row: gents_protocol::row::AgentRequestRow =
                serde_json::from_value(rows["data"]["AgentRequest"][0].clone()).unwrap();
            assert_eq!(row.session_doc_id.as_deref(), session["_docID"].as_str());
            assert!(row
                .agent_doc_id
                .as_deref()
                .is_some_and(|doc| !doc.is_empty()));
            assert_eq!(
                session["provenance"]["opening_request_doc_id"],
                submitted.doc_id
            );
            gents_store::identity::request_receipt::verify_request_receipt_signature(&row).unwrap();
            let other_home = tempfile::tempdir().unwrap();
            let other =
                KeyIdentity::load_or_create(other_home.path().join("other.key"), None).unwrap();
            for permission in [
                gents_store::config_client::NodePermission::DocumentRead,
                gents_store::config_client::NodePermission::CollectionGet,
            ] {
                admin
                    .grant_node_permission(&identity::Did::new(other.did()).unwrap(), permission)
                    .await
                    .unwrap();
            }
            let other_access =
                ConfigAccess::local_as(node.clone(), identity::Did::new(other.did()).unwrap());
            let hidden = other_access
                .execute("{ AgentRequest {_docID} }")
                .await
                .unwrap();
            assert!(hidden["data"]["AgentRequest"]
                .as_array()
                .unwrap()
                .is_empty());
            let retry = crate::goal::submit_goal_backed_request_with_identity(
                &node,
                identity::Did::new(agent.did()).unwrap(),
                db.node_identity.did(),
                "task-session",
                "finish work",
                None,
                &request,
                Some(agent.as_ref()),
            )
            .await
            .unwrap();
            assert_eq!(retry.doc_id, submitted.doc_id);
        }
        case.check(observed);
        drop(user);
    }
}

#[tokio::test]
async fn http_client_creates_session_and_grants_only_its_selected_agent() {
    let home = tempfile::tempdir().unwrap();
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    let other_user = KeyIdentity::load_or_create(home.path().join("other.key"), None).unwrap();
    let db = gents_inference::test_support::test_db(
        "http-client-session-grants",
        crate::lifecycle_hooks::composition(),
    )
    .await;
    let node = db.node.clone();
    let runtime = gents_store::test_support::held_runtime_identity(db.node_identity.did());
    let agent = gents_store::identity::provision_agent_identity(&runtime).unwrap();
    let sibling = gents_store::identity::provision_agent_identity(&runtime).unwrap();
    let endpoint = db.client_endpoint().to_owned();
    let relationship_url = endpoint.replace("/graphql", "/acp/node/relationship");
    let endpoint_for = |actor: &KeyIdentity| {
        gents_store::config_client::GraphqlEndpoint::as_principal(&endpoint, actor.did())
            .with_client_signing_capability(actor.client_signing_capability().unwrap())
            .unwrap()
    };
    let access = db.client_access(&user);
    let node_endpoint = endpoint_for(&runtime);
    assert!(ConfigAccess::graphql_as(&endpoint, user.did())
        .execute("{ AgentSession { _docID } }")
        .await
        .is_err());
    gents_session::test_support::grant_operator_user(&node, &runtime, &user).await;
    let client = reqwest::Client::new();
    for did in [
        agent.agent_did.as_str(),
        sibling.agent_did.as_str(),
        other_user.did(),
    ] {
        for relation in ["read-document", "update-document", "add-dac-relation"] {
            let response = node_endpoint
                .authorize(
                    client
                        .post(relationship_url.as_str())
                        .json(&serde_json::json!({"Relation": relation, "TargetActor": did})),
                )
                .unwrap()
                .send()
                .await
                .unwrap();
            assert!(
                response.status().is_success(),
                "{}",
                response.text().await.unwrap()
            );
        }
    }
    let schema = "type IdentitySchemaProbe { name: String }";
    assert!(access.add_collection(schema).await.is_err());
    ConfigAccess::Graphql(node_endpoint.clone())
        .add_collection(schema)
        .await
        .unwrap();
    seed_owned_agent(&access, runtime.did(), &agent).await;
    let mut request = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "http-request",
        runtime.did(),
        user.did(),
        &agent.agent_did,
        "http-session",
        "hello",
        "interactive",
        "2026-11-04T00:00:00Z",
        AgentRequestAdmissionRecord::client(user.did()),
    );
    let agent_access = ConfigAccess::local_as(node.clone(), agent.agent_did.parse().unwrap());
    let agent_did = identity::Did::new(&agent.agent_did).unwrap();
    let (observed_access, observed_agent, working_agent) = (&access, &agent_access, &agent_did);
    let session_doc = move || async move {
        observed_access
            .execute("{ AgentSession { _docID } }")
            .await
            .unwrap()["data"]["AgentSession"]
            .as_array()
            .unwrap()
            .first()
            .map(|row| row["_docID"].as_str().unwrap().to_owned())
    };
    let observe_session = move || async move {
        let session = session_doc().await;
        let relationships = match &session {
            Some(doc) => observed_access
                .list_document_relationships("AgentSession", doc)
                .await
                .unwrap(),
            None => Vec::new(),
        };
        let visible = observed_agent
            .execute("{ AgentSession { _docID } }")
            .await
            .unwrap();
        gents_lean_vocab::families::session_publication_client_reservation::ReservationState {
            session: session.is_some(),
            parent: relationships.iter().any(|row| row.relation == "parent"),
            working_agent: visible["data"]["AgentSession"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| session.as_deref() == row["_docID"].as_str()),
        }
    };
    let mut reserved: Option<(String, String)> = None;
    for case in gents_lean_vocab::families::session_publication_client_reservation::cases() {
        let before = &case.input.before;
        if observe_session().await.working_agent && !before.working_agent {
            access
                .delete_actor_relationship(
                    "AgentSession",
                    &session_doc().await.unwrap(),
                    "working_agent",
                    working_agent,
                )
                .await
                .unwrap();
        }
        let start = observe_session().await;
        assert_eq!(
            (start.session, start.parent, start.working_agent),
            (before.session, before.parent, before.working_agent),
            "{} starts from its modeled state",
            case.input.name
        );
        if !case.input.commit {
            let aborted: anyhow::Result<()> = access
                .transact_write("test.client.abort_session_reservation", |txn| {
                    let request = &request;
                    let actor = user.did();
                    Box::pin(async move {
                        ensure_request_session_in_txn(txn, request, actor).await?;
                        anyhow::bail!("forced abort after client session relationships")
                    })
                })
                .await;
            assert!(aborted
                .unwrap_err()
                .to_string()
                .contains("forced abort after client session relationships"));
        } else {
            let targets = reserve_client_targets(&access, &request).await.unwrap();
            match &reserved {
                Some((session, _)) => assert_eq!(&targets.0, session),
                None => {
                    let created = access
                        .execute(
                            "{ AgentSession { _docID relationships_pending _version { cid height fieldName } } }",
                        )
                        .await
                        .unwrap();
                    let row = &created["data"]["AgentSession"][0];
                    assert_eq!(row["_docID"], targets.0);
                    assert_eq!(row["relationships_pending"], false);
                    let version =
                        gents_store::graphql::document_composite_version(row, "client session")
                            .unwrap()
                            .unwrap();
                    assert_block_signer(&node, &version.cid, user.did()).await;
                }
            }
            reserved = Some(targets);
        }
        case.check(observe_session().await);
    }
    let (session, agent_doc) = reserved.unwrap();
    access
        .add_actor_relationship("AgentSession", &session, "working_agent", working_agent)
        .await
        .unwrap();
    request.session_doc_id = Some(session);
    request.agent_doc_id = Some(agent_doc);
    gents_session::request::admission::sign_agent_request_create(&user, &mut request)
        .await
        .unwrap();
    let request_grantee = agent.agent_did.as_str();
    let request_doc = move || async move {
        observed_access
            .execute("{ AgentRequest { _docID relationships_pending } }")
            .await
            .unwrap()["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .first()
            .cloned()
    };
    let observe_request = move || async move {
        let row = request_doc().await;
        let grants = match &row {
            Some(row) => observed_access
                .list_document_relationships("AgentRequest", row["_docID"].as_str().unwrap())
                .await
                .unwrap()
                .iter()
                .any(|relationship| {
                    relationship.relation == "working_agent"
                        && relationship.subject == request_grantee
                }),
            None => false,
        };
        gents_lean_vocab::families::session_child_atomic_client_request::ClientRequestPublication {
            committed: row.is_some(),
            pending: row
                .as_ref()
                .is_some_and(|row| row["relationships_pending"].as_bool().unwrap()),
            grants,
        }
    };
    for case in gents_lean_vocab::families::session_child_atomic_client_request::cases() {
        let before = &case.input.before;
        if observe_request().await.grants && !before.grants {
            let doc = request_doc().await.unwrap();
            assert!(node
                .delete_dac_actor_relationship(
                    user.did(),
                    "AgentRequest",
                    doc["_docID"].as_str().unwrap(),
                    "working_agent",
                    request_grantee,
                )
                .await
                .unwrap());
        }
        let start = observe_request().await;
        assert_eq!(
            (start.committed, start.pending, start.grants),
            (before.committed, before.pending, before.grants),
            "{} starts from its modeled state",
            case.input.name
        );
        match (case.input.commit, case.input.fresh) {
            (false, _) => {
                let aborted: anyhow::Result<()> = access
                    .transact_write("test.client.abort_request", |txn| {
                        let request = &request;
                        Box::pin(async move {
                            stage_client_request_in_txn(
                                txn,
                                request,
                                request.session_doc_id.as_deref().unwrap(),
                                request.agent_doc_id.as_deref().unwrap(),
                            )
                            .await?;
                            anyhow::bail!("forced abort after client request relationships")
                        })
                    })
                    .await;
                assert!(aborted
                    .unwrap_err()
                    .to_string()
                    .contains("forced abort after client request relationships"));
            }
            (true, true) => {
                submit_client_request(&access, &request).await.unwrap();
                let published = access
                    .execute("{ AgentRequest { _docID _version { cid height fieldName } } }")
                    .await
                    .unwrap();
                let version = gents_store::graphql::document_composite_version(
                    &published["data"]["AgentRequest"][0],
                    "client request",
                )
                .unwrap()
                .unwrap();
                assert_block_signer(&node, &version.cid, user.did()).await;
            }
            (true, false) => assert!(reconcile_client_request(&access, &request).await.unwrap()),
        }
        case.check(observe_request().await);
    }
    let query = "{ AgentSession { session_id } }";
    let sibling_access = ConfigAccess::local_as(node.clone(), sibling.agent_did.parse().unwrap());
    let other_access = ConfigAccess::Graphql(endpoint_for(&other_user));
    let runtime_access = ConfigAccess::Graphql(node_endpoint.clone());
    for (reader, did, expected) in [
        (&access, user.did(), 1),
        (&agent_access, agent.agent_did.as_str(), 1),
        (&sibling_access, sibling.agent_did.as_str(), 0),
        (&other_access, other_user.did(), 0),
        (&runtime_access, runtime.did(), 0),
    ] {
        let result = reader.execute(query).await.unwrap();
        assert_eq!(
            result["data"]["AgentSession"].as_array().unwrap().len(),
            expected,
            "actor {did}"
        );
    }
    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: runtime.did().into(),
        trigger_id: "manual:task:http-invocation".into(),
        source_collection: "Task".into(),
        source_doc_id: "http-task".into(),
    };
    let task_request_id = identity.request_id();
    let fire = gents_protocol::trigger_delivery::TriggerFire {
        fire_key: crate::triggers::task_fire_key(&identity),
        identity,
        task_id: "task".into(),
        request_id: task_request_id,
        session_id: request.session_id.clone(),
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
        created_at: "2026-11-04T00:00:00Z".into(),
    };
    let mut manual = request.clone();
    manual.request_id = fire.request_id.clone();
    manual.caused_by_trigger_kind = Some("manual".into());
    manual.retry_key = Some(fire.fire_key.clone());
    gents_session::request::admission::sign_agent_request_create(&user, &mut manual)
        .await
        .unwrap();
    let task_request_query = format!(
        "{{ AgentRequest(filter: {{ request_id: {{ _eq: \"{}\" }} }}) {{ _docID relationships_pending _version {{ cid height fieldName }} }} }}",
        gents_store::graphql::escape_graphql_string(&fire.request_id)
    );
    let task_request_query = task_request_query.as_str();
    let task_request = move || async move {
        observed_access.execute(task_request_query).await.unwrap()["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .first()
            .cloned()
    };
    let observe_task_request = move || async move {
        let row = task_request().await;
        let grants = match &row {
            Some(row) => observed_access
                .list_document_relationships("AgentRequest", row["_docID"].as_str().unwrap())
                .await
                .unwrap()
                .iter()
                .any(|relationship| {
                    relationship.relation == "working_agent"
                        && relationship.subject == request_grantee
                }),
            None => false,
        };
        gents_lean_vocab::families::session_child_atomic_client_task_request::ClientRequestPublication {
            committed: row.is_some(),
            pending: row
                .as_ref()
                .is_some_and(|row| row["relationships_pending"].as_bool().unwrap()),
            grants,
        }
    };
    let mut task_deliveries = Vec::new();
    for case in gents_lean_vocab::families::session_child_atomic_client_task_request::cases() {
        let before = &case.input.before;
        if observe_task_request().await.grants && !before.grants {
            assert!(node
                .delete_dac_actor_relationship(
                    user.did(),
                    "AgentRequest",
                    task_request().await.unwrap()["_docID"].as_str().unwrap(),
                    "working_agent",
                    request_grantee,
                )
                .await
                .unwrap());
        }
        let start = observe_task_request().await;
        assert_eq!(
            (start.committed, start.pending, start.grants),
            (before.committed, before.pending, before.grants),
            "{} starts from its modeled state",
            case.input.name
        );
        assert!(case.input.commit, "{}", case.input.name);
        let delivery = crate::triggers::write_task_delivery(&access, &fire, true, &manual)
            .await
            .unwrap();
        assert_eq!(delivery.duplicate, !case.input.fresh);
        if case.input.fresh {
            let row = task_request().await.unwrap();
            assert_eq!(row["_docID"], delivery.request.doc_id);
            let version =
                gents_store::graphql::document_composite_version(&row, "client Task request")
                    .unwrap()
                    .unwrap();
            assert_block_signer(&node, &version.cid, user.did()).await;
        }
        task_deliveries.push(delivery);
        case.check(observe_task_request().await);
    }
    for delivery in &task_deliveries {
        assert_eq!(delivery.request.doc_id, task_deliveries[0].request.doc_id);
    }
    let legacy_session = access.transact_write("test.client.legacy_pending_session", |txn| {
            let agent = &agent.agent_did;
            let node_did = runtime.did();
            let requester = user.did();
            Box::pin(async move {
                let response = txn.execute_client_signed_create(
                    "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
                    &serde_json::json!({"input": {"node_did": node_did, "session_id": "legacy-http-session", "requester_did": requester, "agent_did": agent, "created_at": "2026-11-04T00:00:00Z", "relationships_pending": true}}),
                    gents_protocol::schemas::AGENT_SESSION,
                ).await?;
                Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(&response, "AgentSession")?)
            })
        }).await.unwrap();
    let mut legacy = request.clone();
    legacy.request_id = "legacy-http-request".into();
    legacy.session_id = "legacy-http-session".into();
    legacy.session_doc_id = Some(legacy_session.clone());
    gents_session::request::admission::sign_agent_request_create(&user, &mut legacy)
        .await
        .unwrap();
    assert!(matches!(
        submit_client_request(&access, &legacy).await,
        Err(ClientSubmissionError::Rejected(_))
    ));
    let repaired = reserve_client_targets(&access, &legacy).await.unwrap();
    assert_eq!(repaired.0, legacy_session);
    let repaired_session = access.execute("{ AgentSession(filter: { session_id: { _eq: \"legacy-http-session\" } }) { relationships_pending _version { cid height fieldName } } }").await.unwrap();
    let row = &repaired_session["data"]["AgentSession"][0];
    assert_eq!(row["relationships_pending"], false);
    let version = gents_store::graphql::document_composite_version(row, "repaired client session")
        .unwrap()
        .unwrap();
    assert_block_signer(&node, &version.cid, user.did()).await;
    submit_client_request(&access, &legacy).await.unwrap();
    node.shutdown().await.expect("shut down test node");
}
