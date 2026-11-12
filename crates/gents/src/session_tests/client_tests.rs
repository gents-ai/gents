use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_session::request::lifecycle::client::*;
use gents_session::test_support::{assert_block_signer, seed_owned_agent};
use gents_store::config_client::ConfigAccess;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use std::sync::Arc;

async fn create_client_agent(
    access: &ConfigAccess,
    runtime_did: &str,
    agent: &gents_protocol::agent_identity::AgentIdentityBinding,
) -> String {
    access
            .transact_write("test.client.created_agent", |txn| {
                Box::pin(async move {
                    let created = txn
                        .execute_client_signed_create(
                            "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                            &serde_json::json!({"input":{"node_did":runtime_did,"agent_did":agent.agent_did,"creation_nonce":agent.creation_nonce,"inference_profile_id":"fixture","enabled":true}}),
                            gents_protocol::schemas::AGENT,
                        )
                        .await?;
                    Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(&created, "Agent")?)
                })
            })
            .await
            .unwrap()
}

#[tokio::test]
async fn agent_goal_submission_stages_the_session_before_its_goal_and_rolls_back_together() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().goal_backed_runtime_cases;
    let modeled = |name: &str| cases.iter().find(|case| case.name == name).unwrap();
    let (db, user, client, binding) =
        crate::test_support::root::signed_test_node("runtime-goal-opening").await;
    let node = db.node.clone();
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
    let access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
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
    let submitted = gents_automation::goal::submit_goal_backed_request_with_identity(
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
    let rows = access.execute(&format!("{{ AgentSession {{_docID provenance}} AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ {} relationships_pending }} Goal {{_docID}} GoalCreationClaim {{_docID}} }}", gents_store::graphql::escape_graphql_string(&submitted.doc_id), gents_protocol::request_admission::SIGNED_REQUEST_FIELDS)).await.unwrap();
    let session = &rows["data"]["AgentSession"][0];
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(rows["data"]["AgentRequest"][0].clone()).unwrap();
    assert_eq!(
        !rows["data"]["Goal"].as_array().unwrap().is_empty(),
        modeled("created").goal
    );
    assert_eq!(
        !rows["data"]["AgentRequest"].as_array().unwrap().is_empty(),
        modeled("created").request
    );
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
    let user_rows = client
        .execute("{ AgentSession {_docID} AgentRequest {_docID} }")
        .await
        .unwrap();
    assert_eq!(
        !user_rows["data"]["AgentSession"]
            .as_array()
            .unwrap()
            .is_empty(),
        modeled("created").session
    );
    assert_eq!(
        !user_rows["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty(),
        modeled("created").grants
    );
    let other_home = tempfile::tempdir().unwrap();
    let other = KeyIdentity::load_or_create(other_home.path().join("other.key"), None).unwrap();
    let admin = ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    );
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
    let retry = gents_automation::goal::submit_goal_backed_request_with_identity(
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
    for (field, name) in [("session", "wrong-session"), ("agent", "wrong-agent")] {
        let mut wrong = request.clone();
        if field == "session" {
            wrong.session_doc_id = Some("foreign-session".into());
        } else {
            wrong.agent_doc_id = Some("foreign-agent".into());
        }
        gents_session::request::admission::sign_agent_request_create(agent.as_ref(), &mut wrong)
            .await
            .unwrap();
        assert!(
            gents_automation::goal::submit_goal_backed_request_with_identity(
                &node,
                identity::Did::new(agent.did()).unwrap(),
                db.node_identity.did(),
                "task-session",
                "finish work",
                None,
                &wrong,
                Some(agent.as_ref()),
            )
            .await
            .is_err(),
            "{name}"
        );
        assert!(!modeled(name).request);
    }
    assert!(
        gents_automation::goal::submit_goal_backed_request_with_identity(
            &node,
            identity::Did::new(db.node_identity.did()).unwrap(),
            db.node_identity.did(),
            "task-session",
            "finish work",
            None,
            &request,
            Some(db.node_identity.as_ref()),
        )
        .await
        .is_err()
    );
    assert!(!modeled("foreign-actor").request);
    admin
        .grant_node_permission(
            &identity::Did::new(agent.did()).unwrap(),
            gents_store::config_client::NodePermission::DacRelationDelete,
        )
        .await
        .unwrap();
    access
        .delete_resource_relationship(
            "AgentRequest",
            &submitted.doc_id,
            "parent",
            "agents",
            row.agent_doc_id.as_deref().unwrap(),
        )
        .await
        .unwrap();
    access
        .delete_resource_relationship(
            "AgentRequest",
            &submitted.doc_id,
            "session",
            "sessions",
            row.session_doc_id.as_deref().unwrap(),
        )
        .await
        .unwrap();
    let retry = gents_automation::goal::submit_goal_backed_request_with_identity(
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
    let observed = client.execute("{ AgentRequest {_docID} }").await.unwrap();
    assert_eq!(
        !observed["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty(),
        modeled("duplicate-after-revocation").grants
    );
    let mut rejected = request.clone();
    rejected.request_id = "rejected-opening".into();
    rejected.session_id = "rejected-session".into();
    rejected.retry_key = Some("rejected-retry".into());
    rejected.input.goal_assignment = Some(gents_protocol::request_input::GoalAssignmentInput {
        goal_id: "wrong-goal".into(),
        root_request_doc_id: None,
    });
    gents_session::request::admission::sign_agent_request_create(agent.as_ref(), &mut rejected)
        .await
        .unwrap();
    assert!(
        gents_automation::goal::submit_goal_backed_request_with_identity(
            &node,
            identity::Did::new(agent.did()).unwrap(),
            db.node_identity.did(),
            "rejected-session",
            "finish work",
            None,
            &rejected,
            Some(agent.as_ref()),
        )
        .await
        .is_err()
    );
    let after = access.execute("{ AgentSession {_docID} AgentRequest {_docID} Goal {_docID} GoalCreationClaim {_docID} }").await.unwrap();
    for name in ["AgentSession", "AgentRequest", "Goal", "GoalCreationClaim"] {
        assert_eq!(after["data"][name].as_array().unwrap().len(), 1, "{name}");
    }
    assert!(!modeled("aborted").session);
    assert!(!modeled("aborted").goal);
    assert!(!modeled("aborted").request);
    drop(user);
}

#[tokio::test]
async fn http_client_creates_session_and_grants_only_its_selected_agent() {
    let home = tempfile::tempdir().unwrap();
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    let other_user = KeyIdentity::load_or_create(home.path().join("other.key"), None).unwrap();
    let db = gents_inference::test_support::test_db(
        "http-client-session-grants",
        gents_automation::lifecycle_hooks::composition(),
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
    let cases =
        &gents_lean_vocab::lean_contract_snapshot().identity_client_session_reservation_cases;
    let modeled = |name: &str| cases.iter().find(|case| case["name"] == name).unwrap();
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
    let before = access.execute("{ AgentSession { _docID } }").await.unwrap();
    assert_eq!(
        !before["data"]["AgentSession"]
            .as_array()
            .unwrap()
            .is_empty(),
        modeled("aborted")["session"]
    );
    let (session, agent_doc) = reserve_client_targets(&access, &request).await.unwrap();
    let created = access
        .execute(
            "{ AgentSession { _docID relationships_pending _version { cid height fieldName } } }",
        )
        .await
        .unwrap();
    let row = &created["data"]["AgentSession"][0];
    assert_eq!(row["_docID"], session);
    assert_eq!(row["relationships_pending"], false);
    let version = gents_store::graphql::document_composite_version(row, "client session")
        .unwrap()
        .unwrap();
    assert_block_signer(&node, &version.cid, user.did()).await;
    let agent_access = ConfigAccess::local_as(node.clone(), agent.agent_did.parse().unwrap());
    let visible = agent_access
        .execute("{ AgentSession { _docID } }")
        .await
        .unwrap();
    assert_eq!(
        !visible["data"]["AgentSession"]
            .as_array()
            .unwrap()
            .is_empty(),
        modeled("created")["working_agent"]
    );
    access
        .delete_actor_relationship(
            "AgentSession",
            &session,
            "working_agent",
            &identity::Did::new(&agent.agent_did).unwrap(),
        )
        .await
        .unwrap();
    let duplicate = reserve_client_targets(&access, &request).await.unwrap();
    assert_eq!(duplicate.0, session);
    let visible = agent_access
        .execute("{ AgentSession { _docID } }")
        .await
        .unwrap();
    assert_eq!(
        !visible["data"]["AgentSession"]
            .as_array()
            .unwrap()
            .is_empty(),
        modeled("duplicate-after-revocation")["working_agent"]
    );
    access
        .add_actor_relationship(
            "AgentSession",
            &session,
            "working_agent",
            &identity::Did::new(&agent.agent_did).unwrap(),
        )
        .await
        .unwrap();
    request.session_doc_id = Some(session);
    request.agent_doc_id = Some(agent_doc);
    gents_session::request::admission::sign_agent_request_create(&user, &mut request)
        .await
        .unwrap();
    let request_cases =
        &gents_lean_vocab::lean_contract_snapshot().identity_atomic_client_request_cases;
    let request_model = |name: &str| {
        request_cases
            .iter()
            .find(|case| case["name"] == name)
            .unwrap()
    };
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
    let before = access.execute("{ AgentRequest { _docID } }").await.unwrap();
    assert_eq!(
        !before["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty(),
        request_model("aborted")["committed"]
    );
    submit_client_request(&access, &request).await.unwrap();
    let published = access
        .execute(
            "{ AgentRequest { _docID relationships_pending _version { cid height fieldName } } }",
        )
        .await
        .unwrap();
    let row = &published["data"]["AgentRequest"][0];
    let request_doc = row["_docID"].as_str().unwrap();
    assert_eq!(
        row["relationships_pending"],
        request_model("created")["pending"]
    );
    let version = gents_store::graphql::document_composite_version(row, "client request")
        .unwrap()
        .unwrap();
    assert_block_signer(&node, &version.cid, user.did()).await;
    let removed = node
        .delete_dac_actor_relationship(
            user.did(),
            "AgentRequest",
            request_doc,
            "working_agent",
            &agent.agent_did,
        )
        .await
        .unwrap();
    assert_eq!(removed, request_model("created")["grants"]);
    assert!(reconcile_client_request(&access, &request).await.unwrap());
    let restored = node
        .delete_dac_actor_relationship(
            user.did(),
            "AgentRequest",
            request_doc,
            "working_agent",
            &agent.agent_did,
        )
        .await
        .unwrap();
    assert_eq!(
        restored,
        request_model("duplicate-after-revocation")["grants"]
    );
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
        fire_key: gents_automation::triggers::task_fire_key(&identity),
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
    let first = gents_automation::triggers::write_task_delivery(&access, &fire, true, &manual)
        .await
        .unwrap();
    assert!(!first.duplicate);
    let query = format!("{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{}\" }} }}) {{ _docID relationships_pending _version {{ cid height fieldName }} }} }}", gents_store::graphql::escape_graphql_string(&first.request.doc_id));
    let response = access.execute(&query).await.unwrap();
    let row = &response["data"]["AgentRequest"][0];
    assert_eq!(
        row["relationships_pending"],
        request_model("created")["pending"]
    );
    let version = gents_store::graphql::document_composite_version(row, "client Task request")
        .unwrap()
        .unwrap();
    assert_block_signer(&node, &version.cid, user.did()).await;
    let removed = node
        .delete_dac_actor_relationship(
            user.did(),
            "AgentRequest",
            &first.request.doc_id,
            "working_agent",
            &agent.agent_did,
        )
        .await
        .unwrap();
    assert_eq!(removed, request_model("created")["grants"]);
    let duplicate = gents_automation::triggers::write_task_delivery(&access, &fire, true, &manual)
        .await
        .unwrap();
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.request.doc_id, first.request.doc_id);
    let restored = node
        .delete_dac_actor_relationship(
            user.did(),
            "AgentRequest",
            &first.request.doc_id,
            "working_agent",
            &agent.agent_did,
        )
        .await
        .unwrap();
    assert_eq!(
        restored,
        request_model("duplicate-after-revocation")["grants"]
    );
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

mod claim_tests;
