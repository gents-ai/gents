use super::*;
use crate::request::admission::AgentRequestAdmissionVerifier;
use crate::request::lifecycle::{ClaimOutcome, ExecutionOrigin, RequestLifecycle};
use crate::request::watcher::AgentRequest;
use crate::store::config_client::NodePermission;

#[tokio::test]
async fn watcher_routes_native_private_requests_as_their_selected_agents() {
    use crate::request::watcher::{DefraWatcher, Watcher};
    let snapshot = crate::lean_vocab_test::lean_contract_snapshot();
    let model = snapshot
        .watcher_scoped_scan_cases
        .iter()
        .find(|case| case["name"] == "different-agents-native-order")
        .unwrap();
    let expected: Vec<&str> = model["expected"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| match row["doc_id"].as_str().unwrap() {
            "earlier" => "z-first",
            "later" => "a-second",
            other => panic!("unexpected modeled row {other}"),
        })
        .collect();
    let home = tempfile::tempdir().unwrap();
    let runtime: Arc<dyn NodeIdentity> =
        Arc::new(KeyIdentity::load_or_create(home.path().join("runtime.key"), None).unwrap());
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    let (node, endpoint) = serve_client_node(runtime.as_ref()).await;
    let first = crate::store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
    let second = crate::store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
    let first_signer =
        crate::store::identity::resolve_agent_identity(runtime.as_ref(), &first).unwrap();
    let second_signer =
        crate::store::identity::resolve_agent_identity(runtime.as_ref(), &second).unwrap();
    let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
    for did in [
        user.did(),
        first.agent_did.as_str(),
        second.agent_did.as_str(),
    ] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
        ] {
            admin
                .grant_node_permission(&did.parse().unwrap(), permission)
                .await
                .unwrap();
        }
    }
    let owner = client_access(&endpoint, &user);
    seed_owned_agent(&owner, runtime.did(), &first).await;
    let parent = owner.execute("{ Node { _docID } }").await.unwrap()["data"]["Node"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    let agent_doc = create_client_agent(&owner, runtime.did(), &second).await;
    owner
        .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &parent)
        .await
        .unwrap();
    owner
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "executor",
            &second.agent_did.parse().unwrap(),
        )
        .await
        .unwrap();
    for (binding, request_id, session_id) in [
        (&second, "z-first", "second-session"),
        (&first, "a-second", "first-session"),
    ] {
        let request = crate::request::lifecycle::build_signed_client_request(
            &owner,
            crate::request::lifecycle::RequestSpec::new(
                RequestPurpose::Normal,
                crate::request::lifecycle::RequestIdentity {
                    request_id: request_id.into(),
                    node_did: runtime.did().into(),
                    requester_did: Some(user.did().into()),
                    agent_did: binding.agent_did.clone(),
                    session_id: session_id.into(),
                    content: "work".into(),
                    execution_origin: ExecutionOrigin::Interactive,
                    created_at: "2026-11-04T00:00:00Z".into(),
                },
                AgentRequestAdmissionRecord::client(user.did()),
            ),
            crate::request::lifecycle::RequestSigner::Identity(&user),
        )
        .await
        .unwrap();
        submit_client_request(&owner, &request).await.unwrap();
    }
    // The runtime DID cannot read user-owned sessions. Resuming completion must
    // use the selected agent for both session validation and sequence recovery.
    let agent_access = ConfigAccess::local_managed_agent(
        node.clone(),
        first.agent_did.parse().unwrap(),
        first_signer.as_ref(),
    )
    .unwrap();
    let sessions = agent_access
        .execute("{ AgentSession { _docID session_id } }")
        .await
        .unwrap();
    let session_doc = sessions["data"]["AgentSession"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["session_id"] == "first-session")
        .unwrap()["_docID"]
        .as_str()
        .unwrap();
    let created_message = agent_access.write("test.resume.message", &format!(
        r#"mutation {{ create_AgentMessage(input: {{node_did: "{}", requester_did: "{}", session_id: "first-session", sequence: 9}}) {{ _docID }} }}"#,
        runtime.did(), user.did())).await.unwrap();
    let message_doc =
        crate::store::graphql::created_doc_id(&created_message, "AgentMessage").unwrap();
    agent_access
        .add_resource_relationship(
            "AgentMessage",
            &message_doc,
            "parent",
            "sessions",
            session_doc,
        )
        .await
        .unwrap();
    assert_eq!(
        crate::session::max_sequence_as(
            &node,
            "first-session",
            runtime.did(),
            Some(user.did()),
            Some(first.agent_did.parse().unwrap())
        )
        .await
        .unwrap(),
        9
    );
    let hook = crate::tools::call::hook::DefraSessionHook::resume_with_actor_policy(
        node.clone(),
        "first-session",
        runtime.did(),
        Some(user.did()),
        first.agent_did.parse().unwrap(),
        crate::tools::call::hook::FailurePolicy::default(),
    )
    .await
    .unwrap();
    assert_eq!(hook.session_id().await.as_deref(), Some("first-session"));
    for actor in [runtime.did(), second.agent_did.as_str()] {
        assert!(
            crate::tools::call::hook::DefraSessionHook::resume_with_actor_policy(
                node.clone(),
                "first-session",
                runtime.did(),
                Some(user.did()),
                actor.parse().unwrap(),
                crate::tools::call::hook::FailurePolicy::default(),
            )
            .await
            .is_err(),
            "an unrelated actor cannot resume a private session"
        );
    }
    let root_rows = admin
        .execute("{ AgentRequest { request_id } }")
        .await
        .unwrap();
    assert_eq!(
        root_rows["data"]["AgentRequest"],
        serde_json::json!([]),
        "runtime identity does not inherit private requests"
    );
    let first_rows = ConfigAccess::local_as(node.clone(), first.agent_did.parse().unwrap())
        .execute("{ AgentRequest { request_id } }")
        .await
        .unwrap();
    assert_eq!(
        first_rows["data"]["AgentRequest"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        first_rows["data"]["AgentRequest"][0]["request_id"],
        "a-second"
    );
    let mut watcher = DefraWatcher::for_agents(
        node.clone(),
        runtime.did(),
        vec![first_signer.clone(), second_signer.clone()],
    )
    .unwrap();
    for request_id in expected {
        assert_eq!(
            watcher.next_request().await.unwrap().unwrap().request_id,
            request_id
        );
    }
    let household = crate::lean_vocab_test::lean_identity_household_cases()
        .first()
        .expect("modeled household");
    let model_user = &household
        .objects
        .iter()
        .find(|object| object.resource == "nodes" && object.id == "home")
        .expect("modeled Node")
        .owner;
    let model_agent = &household
        .objects
        .iter()
        .find(|object| object.resource == "sessions" && object.id == "automated-session")
        .expect("modeled autonomous session")
        .owner;
    let model_sibling = household
        .relations
        .iter()
        .find_map(|relation| {
            if relation.resource != "agents"
                || relation.id != "sibling"
                || relation.relation != "executor"
            {
                return None;
            }
            match &relation.subject {
                crate::lean_vocab_test::LeanIdentitySubject::Actor { actor } => Some(actor),
                _ => None,
            }
        })
        .expect("modeled sibling executor");
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
                    && resource == "sessions"
                    && id == "automated-session"
                    && permission == "read" =>
                {
                    Some(*expected)
                }
                _ => None,
            })
            .expect("modeled autonomous session read")
    };
    let reservation = snapshot
        .goal_client_start_cases
        .iter()
        .find(|case| case["name"] == "reserved_session")
        .expect("modeled autonomous reservation");
    for (binding, signer, other, session_id) in [
        (&first, &first_signer, &second, "first-autonomous-session"),
        (&second, &second_signer, &first, "second-autonomous-session"),
    ] {
        let access = ConfigAccess::local_managed_agent(
            node.clone(),
            binding.agent_did.parse().unwrap(),
            signer.as_ref(),
        )
        .unwrap();
        let mut request = AgentRequestCreate::base(
            RequestPurpose::Normal,
            format!("{session_id}-request"),
            runtime.did(),
            &binding.agent_did,
            &binding.agent_did,
            session_id,
            "autonomous work",
            "interactive",
            "2026-11-04T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(&binding.agent_did),
        );
        crate::sign_agent_request_create(signer.as_ref(), &mut request)
            .await
            .unwrap();
        let doc = reserve_goal_session(&access, &request).await.unwrap();
        let owners = access
            .transact_readonly("test.autonomous.session_owner", |txn| {
                let doc = doc.clone();
                Box::pin(async move { txn.document_owners("AgentSession", &[doc]).await })
            })
            .await
            .unwrap();
        assert_eq!(
            owners[0]
                .owner
                .as_ref()
                .is_some_and(|owner| owner.as_str() == binding.agent_did),
            reservation["reservation"]["agent_is_owner"]
                .as_bool()
                .unwrap(),
            "{session_id} native owner"
        );
        let query = format!(
            r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ session_id }} }}"#,
            crate::store::graphql::escape_graphql_string(session_id),
        );
        for (observer, modeled_actor) in [
            (&access, model_agent.as_str()),
            (&owner, model_user.as_str()),
        ] {
            let rows = observer.execute(&query).await.unwrap();
            let visible = !rows["data"]["AgentSession"].as_array().unwrap().is_empty();
            assert_eq!(
                visible,
                expected_read(modeled_actor),
                "{session_id} observer {modeled_actor}"
            );
        }
        let sibling = ConfigAccess::local_as(node.clone(), other.agent_did.parse().unwrap());
        let rows = sibling.execute(&query).await.unwrap();
        let visible = !rows["data"]["AgentSession"].as_array().unwrap().is_empty();
        assert_eq!(
            visible,
            expected_read(model_sibling),
            "{session_id} sibling observer"
        );
    }
    watcher
        .set_agent_identities(vec![first_signer.clone()])
        .unwrap();
    let remaining = watcher.pending_requests().await.unwrap();
    assert!(remaining
        .iter()
        .all(|request| request.agent_did == first.agent_did));
    watcher
        .set_agent_identities(vec![first_signer, second_signer.clone()])
        .unwrap();
    admin
        .revoke_node_permission(
            &second.agent_did.parse().unwrap(),
            NodePermission::DocumentRead,
        )
        .await
        .unwrap();
    let missing = snapshot
        .watcher_scoped_scan_cases
        .iter()
        .find(|case| case["name"] == "missing-root-is-not-empty")
        .unwrap();
    assert!(!missing["expected"]["failures"]
        .as_array()
        .unwrap()
        .is_empty());
    let healthy = watcher.pending_requests().await.unwrap();
    assert_eq!(healthy.len(), 1);
    assert_eq!(healthy[0].agent_did, first.agent_did);
    watcher.set_agent_identities(vec![second_signer]).unwrap();
    let error = watcher
        .pending_requests()
        .await
        .expect_err("missing NAC root is not an empty queue");
    assert!(
        error.to_string().contains(&second.agent_did),
        "the failed actor is reported: {error:#}"
    );
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn signed_client_claim_uses_transaction_bound_intent_and_native_permissions() {
    let modeled = crate::lean_vocab_test::lean_contract_snapshot();
    for (name, mutate, scheduled, steer) in [
        ("committed_claim", false, false, false),
        ("committed_claim", false, true, false),
        ("invalid_signature", true, false, false),
        ("invoke_denied_at_check", false, false, false),
        ("committed_claim", false, false, true),
        ("invalid_signature", true, false, true),
        ("invoke_denied_at_check", false, false, true),
    ] {
        let case = modeled
            .cross_actor_authorization_cases
            .iter()
            .find(|case| case.name == name)
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let runtime: Arc<dyn NodeIdentity> =
            Arc::new(KeyIdentity::load_or_create(home.path().join("runtime.key"), None).unwrap());
        let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
        let owner = KeyIdentity::load_or_create(home.path().join("owner.key"), None).unwrap();
        let (node, endpoint) = serve_client_node(runtime.as_ref()).await;
        let binding = crate::store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
        let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
        for did in [user.did(), owner.did(), binding.agent_did.as_str()] {
            let subject = ::identity::Did::new(did).unwrap();
            for permission in [
                NodePermission::DocumentRead,
                NodePermission::DocumentUpdate,
                NodePermission::DacRelationAdd,
            ] {
                admin
                    .grant_node_permission(&subject, permission)
                    .await
                    .unwrap();
            }
        }
        let user_access = client_access(&endpoint, &user);
        if name == "invoke_denied_at_check" {
            let owner_access = client_access(&endpoint, &owner);
            seed_owned_agent(&owner_access, runtime.did(), &binding).await;
            let agent = owner_access.execute("{ Agent { _docID } }").await.unwrap();
            let doc = agent["data"]["Agent"][0]["_docID"].as_str().unwrap();
            owner_access
                .add_actor_relationship("Agent", doc, "reader", &user.did().parse().unwrap())
                .await
                .unwrap();
        } else {
            seed_owned_agent(&user_access, runtime.did(), &binding).await;
        }
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            format!("model-{name}"),
            runtime.did(),
            user.did(),
            &binding.agent_did,
            format!("session-{name}"),
            "signed text",
            if scheduled {
                "scheduled"
            } else {
                "interactive"
            },
            "2026-11-04T00:00:00Z",
            AgentRequestAdmissionRecord::client(user.did()),
        );
        if scheduled {
            create.max_retries = 1;
        }
        if steer {
            create.input.queue = Some(gents_protocol::request_input::RequestQueue {
                delivery: gents_protocol::request_input::QueueDelivery::Steer,
                source: gents_protocol::request_input::QueueSource::User,
                policy: gents_protocol::request_input::QueuePolicy::Append,
                position: None,
                key: None,
                queued_after_request_id: None,
                interrupted_request_id: None,
                background_completion_wake_version: None,
            });
        }
        create.session_doc_id = Some(reserve_goal_session(&user_access, &create).await.unwrap());
        let agent = user_access
            .execute("{ Agent { _docID agent_did } }")
            .await
            .unwrap();
        create.agent_doc_id = Some(
            agent["data"]["Agent"][0]["_docID"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        crate::sign_agent_request_create(&user, &mut create)
            .await
            .unwrap();
        submit_client_request(&user_access, &create).await.unwrap();
        let agent_identity =
            crate::store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
        let agent_access = ConfigAccess::local_managed_agent(
            node.clone(),
            binding.agent_did.parse().unwrap(),
            agent_identity.as_ref(),
        )
        .unwrap();
        let query = format!("{{ AgentRequest {{ {} }} }}", crate::SIGNED_REQUEST_FIELDS);
        let response = agent_access.execute(&query).await.unwrap();
        let row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
        let request = AgentRequest::try_from(row).unwrap();
        let (_owner, authority) =
            crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
        let verifier = AgentRequestAdmissionVerifier::new(node.clone(), runtime.clone(), authority);
        let (request, mut admission) = verifier
            .verify_fresh_for_claim(&request, &binding.agent_did)
            .await
            .unwrap();
        assert!(admission.is_some());
        if mutate {
            admission.as_mut().unwrap().alter_signed_content_for_test();
        }
        let mut lifecycle = RequestLifecycle::new_with_execution_binding(
            node.clone(),
            &binding.agent_did,
            runtime.did(),
            request,
            60,
            if scheduled {
                ExecutionOrigin::Scheduled
            } else {
                ExecutionOrigin::Interactive
            },
            String::new(),
        );
        lifecycle
            .bind_managed_agent(agent_identity.as_ref())
            .unwrap();
        lifecycle.set_client_claim_admission(admission);
        let result = lifecycle.claim_with_identity().await;
        let claimed = matches!(result.as_ref(), Ok(ClaimOutcome::Claimed));
        assert_eq!(
            result.is_err(),
            case.decision == crate::lean_vocab_test::LeanCrossActorDecision::Denied,
            "{name}: {}",
            result
                .as_ref()
                .err()
                .map(|error| gents_protocol::error_chain::ErrorChain(error.as_ref()).to_string())
                .unwrap_or_default()
        );
        assert_eq!(
            claimed,
            case.decision == crate::lean_vocab_test::LeanCrossActorDecision::Claimed,
            "{name}"
        );
        if claimed {
            assert!(lifecycle.owns_execution().await.unwrap());
        }
        let state = user_access
            .execute("{ AgentRequest { lifecycle_state failure_reason terminal_output } }")
            .await
            .unwrap();
        assert_eq!(
            state["data"]["AgentRequest"][0]["lifecycle_state"].as_str(),
            Some(case.lifecycle.as_str()),
            "{name}"
        );
        if case.decision == crate::lean_vocab_test::LeanCrossActorDecision::Denied {
            assert_eq!(
                state["data"]["AgentRequest"][0]["failure_reason"].as_str(),
                case.failure_reason.as_deref()
            );
            let output: gents_protocol::output::TerminalOutput =
                serde_json::from_value(state["data"]["AgentRequest"][0]["terminal_output"].clone())
                    .unwrap();
            assert_eq!(output, gents_protocol::output::TerminalOutput::NoMessage);
            assert_eq!(case.terminal_output.as_deref(), Some("no_message"));
        }
        if claimed {
            if scheduled {
                let writer = crate::request::streaming::DefraStreamWriter::new_managed(
                    node.clone(),
                    agent_identity.as_ref(),
                    std::time::Duration::ZERO,
                )
                .unwrap();
                lifecycle.begin_owned_execution(&writer).await.unwrap();
                lifecycle.validate_owned_execution().await.unwrap();
            }
            // The runtime can see the Agent configuration but does not own this request.
            let node_view = admin.execute("{ AgentRequest { _docID } }").await.unwrap();
            assert!(node_view["data"]["AgentRequest"]
                .as_array()
                .unwrap()
                .is_empty());
            let doc_id = crate::store::graphql::escape_graphql_string(&lifecycle.request().doc_id);
            agent_access
                .write(
                    "test.expire_claim",
                    &format!(
                        r#"mutation {{ update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{ execution_lease_expires_at: "2000-01-01T00:00:00Z" }}
            ) {{ _docID }} }}"#
                    ),
                )
                .await
                .unwrap();
            let recovery = RequestLifecycle::repair_terminal_requests_with_identities(
                &node,
                runtime.did(),
                std::slice::from_ref(&agent_identity),
            )
            .await
            .unwrap();
            assert_eq!(recovery.scanned, 1);
            assert_eq!(recovery.repaired, 1);
            assert_eq!(recovery.failed, 0);

            let terminal = RequestLifecycle::redrive_terminal_convergence_with_identities(
                &node,
                runtime.did(),
                std::slice::from_ref(&agent_identity),
            )
            .await
            .unwrap();
            assert_eq!(terminal.scanned, 1);
            assert_eq!(terminal.reasserted, 1);
            assert_eq!(terminal.failed, 0);

            if scheduled {
                let wake = RequestLifecycle::redrive_failed_background_wakeups_with_identities(
                    &node,
                    runtime.did(),
                    std::slice::from_ref(&agent_identity),
                )
                .await
                .unwrap();
                assert_eq!(wake.scanned, 1);
                assert_eq!(wake.ineligible, 1);
            }
        }
        drop(lifecycle);
        node.shutdown().await.expect("shut down test node");
    }
}

#[tokio::test]
async fn client_tool_control_claim_rechecks_native_authority_and_retains_committed_claim() {
    use crate::tools::call::control::client::{
        claim, claim_paused, claim_unavailable, execute_claimed, prepare, read_receipt,
        recover_claim, submit, ClaimPause,
    };
    use crate::tools::call::lifecycle::admission_fixture::publish_accepted_on_claimed_request;
    use crate::tools::call::lifecycle::AwaitMode;

    let model = &crate::lean_vocab_test::lean_contract_snapshot().client_process_claim_cases;
    let grant_model = &crate::lean_vocab_test::lean_contract_snapshot().client_control_grant_cases;
    let may_publish = |authority: bool, fenced: bool| {
        grant_model
            .iter()
            .find(|case| case["authority"] == authority && case["fenced"] == fenced)
            .unwrap()["expected"]
            .as_bool()
            .unwrap()
    };
    let conflict = model
        .iter()
        .find(|case| {
            case["claimant"] == "agent"
                && case["owner"] == "client"
                && case["requested_id"] == "call"
                && case["requested_doc"] == "doc"
                && case["read"] == true
                && case["update"] == true
                && case["invoke"] == true
                && case["current_revision"] == 1
        })
        .unwrap()["expected"]
        .as_bool()
        .unwrap();
    assert!(
        !conflict,
        "the executable model rejects the stale authorization observation"
    );
    let modeled = |read: bool, update: bool, invoke: bool| {
        model
            .iter()
            .find(|case| {
                case["claimant"] == "agent"
                    && case["owner"] == "client"
                    && case["requested_id"] == "call"
                    && case["requested_doc"] == "doc"
                    && case["read"] == read
                    && case["update"] == update
                    && case["invoke"] == invoke
                    && case["current_revision"] == 0
            })
            .unwrap()["expected"]
            .as_bool()
            .unwrap()
    };
    let home = tempfile::tempdir().unwrap();
    let runtime: Arc<dyn NodeIdentity> =
        Arc::new(KeyIdentity::load_or_create(home.path().join("runtime.key"), None).unwrap());
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    let foreign =
        KeyIdentity::load_or_create(home.path().join("foreign-control.key"), None).unwrap();
    let collaborator =
        KeyIdentity::load_or_create(home.path().join("collaborator.key"), None).unwrap();
    let (node, endpoint) = serve_client_node(runtime.as_ref()).await;
    let binding = crate::store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
    let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
    for did in [
        user.did(),
        foreign.did(),
        collaborator.did(),
        binding.agent_did.as_str(),
    ] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
        ] {
            admin
                .grant_node_permission(&did.parse().unwrap(), permission)
                .await
                .unwrap();
        }
    }
    let user_access = client_access(&endpoint, &user);
    seed_owned_agent(&user_access, runtime.did(), &binding).await;
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "tool-control-request",
        runtime.did(),
        user.did(),
        &binding.agent_did,
        "tool-control-session",
        "run",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::client(user.did()),
    );
    create.session_doc_id = Some(reserve_goal_session(&user_access, &create).await.unwrap());
    let rows = user_access.execute("{ Agent { _docID } }").await.unwrap();
    create.agent_doc_id = Some(rows["data"]["Agent"][0]["_docID"].as_str().unwrap().into());
    let agent_doc = create.agent_doc_id.clone().unwrap();
    user_access
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "session_writer",
            &collaborator.did().parse().unwrap(),
        )
        .await
        .unwrap();
    crate::sign_agent_request_create(&user, &mut create)
        .await
        .unwrap();
    submit_client_request(&user_access, &create).await.unwrap();
    let agent: identity::Did = binding.agent_did.parse().unwrap();
    let agent_identity =
        crate::store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
    let agent_access =
        ConfigAccess::local_managed_agent(node.clone(), agent.clone(), agent_identity.as_ref())
            .unwrap();
    let query = format!("{{ AgentRequest {{ {} }} }}", crate::SIGNED_REQUEST_FIELDS);
    let response = agent_access.execute(&query).await.unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let request = AgentRequest::try_from(row).unwrap();
    let (_owner, authority) = crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = AgentRequestAdmissionVerifier::new(node.clone(), runtime.clone(), authority);
    let (request, admission) = verifier
        .verify_fresh_for_claim(&request, &binding.agent_did)
        .await
        .unwrap();
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        &binding.agent_did,
        runtime.did(),
        request,
        60,
        ExecutionOrigin::Interactive,
        String::new(),
    );
    lifecycle
        .bind_managed_agent(agent_identity.as_ref())
        .unwrap();
    lifecycle.set_client_claim_admission(admission);
    assert!(matches!(
        lifecycle.claim_with_identity().await,
        Ok(ClaimOutcome::Claimed)
    ));
    let tool = publish_accepted_on_claimed_request(
        node.clone(),
        &mut lifecycle,
        runtime.did(),
        0,
        "bash",
        "control-call",
        serde_json::json!({"command":"work"}),
        AwaitMode::Background,
        false,
    )
    .await
    .unwrap();
    let tool_doc = tool.doc_id().unwrap();
    agent_access.write("test.control.background_mode", &format!(r#"mutation {{ update_AgentToolCall(docID: "{}", input: {{ await_mode: "background" }}) {{ _docID }} }}"#, tool_doc)).await.unwrap();
    let collaborator_access = client_access(&endpoint, &collaborator);
    let collaborator_intent = prepare(
        &collaborator_access,
        &collaborator,
        runtime.did(),
        tool_doc,
        Some(("tool-control-session", "control-call")),
        "process_stop",
    )
    .await
    .unwrap();
    let document_race = submit(&node, collaborator.did(), &collaborator_intent)
        .await
        .unwrap();
    let document_pause = Arc::new(ClaimPause {
        observed: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
        armed: std::sync::atomic::AtomicBool::new(true),
    });
    let document_claim = {
        let node = node.clone();
        let access = agent_access.clone();
        let agent = agent.clone();
        let doc = document_race.clone();
        let pause = document_pause.clone();
        tokio::spawn(async move { claim_paused(&node, &access, &agent, &doc, &pause).await })
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        document_pause.observed.notified(),
    )
    .await
    .expect("claim did not observe document ACP");
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        user_access.delete_actor_relationship(
            "Agent",
            &agent_doc,
            "session_writer",
            &collaborator.did().parse().unwrap(),
        ),
    )
    .await
    .expect("document ACP revocation did not finish")
    .unwrap();
    document_pause.resume.notify_one();
    assert!(
        document_claim.await.unwrap().unwrap().is_none(),
        "revoked document ACP cannot commit a claim"
    );
    let session_doc = create.session_doc_id.as_ref().unwrap();
    user_access
        .add_actor_relationship(
            "AgentSession",
            session_doc,
            "reader",
            &collaborator.did().parse().unwrap(),
        )
        .await
        .unwrap();
    user_access
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "executor",
            &collaborator.did().parse().unwrap(),
        )
        .await
        .unwrap();
    let denied_update = prepare(
        &collaborator_access,
        &collaborator,
        runtime.did(),
        tool_doc,
        Some(("tool-control-session", "control-call")),
        "process_stop",
    )
    .await
    .unwrap();
    let denied_update = submit(&node, collaborator.did(), &denied_update)
        .await
        .unwrap();
    assert_eq!(
        claim(&node, &agent_access, &agent, &denied_update)
            .await
            .unwrap()
            .is_some(),
        modeled(true, false, true),
        "session read and agent invocation do not authorize a session update"
    );
    let denied_update_receipt = read_receipt(
        &collaborator_access,
        runtime.did(),
        &denied_update,
        tool_doc,
        "process_stop",
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        denied_update_receipt.outcome,
        gents_protocol::process_control::ToolControlReceiptOutcome::Rejected
    ));
    user_access
        .delete_actor_relationship(
            "Agent",
            &agent_doc,
            "executor",
            &collaborator.did().parse().unwrap(),
        )
        .await
        .unwrap();
    user_access
        .add_actor_relationship(
            "AgentSession",
            session_doc,
            "working_agent",
            &collaborator.did().parse().unwrap(),
        )
        .await
        .unwrap();
    let denied_invoke = prepare(
        &collaborator_access,
        &collaborator,
        runtime.did(),
        tool_doc,
        Some(("tool-control-session", "control-call")),
        "process_stop",
    )
    .await
    .unwrap();
    let denied_invoke = submit(&node, collaborator.did(), &denied_invoke)
        .await
        .unwrap();
    assert_eq!(
        claim(&node, &agent_access, &agent, &denied_invoke)
            .await
            .unwrap()
            .is_some(),
        modeled(true, true, false),
        "session update does not authorize invoking the agent"
    );
    let denied_invoke_receipt = read_receipt(
        &collaborator_access,
        runtime.did(),
        &denied_invoke,
        tool_doc,
        "process_stop",
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        denied_invoke_receipt.outcome,
        gents_protocol::process_control::ToolControlReceiptOutcome::Rejected
    ));
    user_access
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "session_writer",
            &collaborator.did().parse().unwrap(),
        )
        .await
        .unwrap();
    let denied_read = prepare(
        &collaborator_access,
        &collaborator,
        runtime.did(),
        tool_doc,
        Some(("tool-control-session", "control-call")),
        "process_stop",
    )
    .await
    .unwrap();
    let denied_read = submit(&node, collaborator.did(), &denied_read)
        .await
        .unwrap();
    admin
        .revoke_node_permission(
            &collaborator.did().parse().unwrap(),
            NodePermission::DocumentRead,
        )
        .await
        .unwrap();
    assert_eq!(
        claim(&node, &agent_access, &agent, &denied_read)
            .await
            .unwrap()
            .is_some(),
        modeled(false, true, true),
        "session update and agent invoke do not bypass the native node read gate"
    );
    assert!(matches!(
        prepare(
            &user_access,
            &user,
            runtime.did(),
            tool_doc,
            Some(("tool-control-session", "different-call")),
            "process_stop"
        )
        .await,
        Err(crate::tools::call::control::client::ClientControlError::NotFound)
    ));
    let signed = prepare(
        &user_access,
        &user,
        runtime.did(),
        tool_doc,
        Some(("tool-control-session", "control-call")),
        "process_stop",
    )
    .await
    .unwrap();
    let mut tampered = signed.clone();
    tampered.action = "job_stop".into();
    assert!(matches!(
        submit(&node, user.did(), &tampered).await,
        Err(crate::tools::call::control::client::ClientControlError::NotAuthorized)
    ));
    assert!(matches!(
        submit(&node, runtime.did(), &signed).await,
        Err(crate::tools::call::control::client::ClientControlError::NotAuthorized)
    ));
    let persisted_tampered = user_access.transact_write("test.control.invalid_signed_intent", |txn| {
        let tampered = &tampered;
        Box::pin(async move {
            let response = txn.execute_with_variables(
                "mutation($input: AgentToolControlMutationInputArg!) { create_AgentToolControl(input: $input) { _docID } }",
                &serde_json::json!({"input": {
                    "intent_id": tampered.intent_id, "intent_json": serde_json::to_string(tampered)?,
                    "client_did": tampered.client_did, "agent_did": tampered.agent_did,
                    "node_did": tampered.node_did, "session_id": tampered.session_id,
                    "session_doc_id": tampered.session_doc_id, "request_doc_id": tampered.request_doc_id,
                    "requester_did": tampered.requester_did, "tool_call_id": tampered.tool_call_id,
                    "tool_call_doc_id": tampered.tool_call_doc_id, "action": tampered.action, "state": "pending"
                }}),
            ).await?;
            Ok::<_, anyhow::Error>(crate::store::graphql::created_doc_id(&response, "AgentToolControl")?)
        })
    }).await.unwrap();
    user_access
        .add_actor_relationship(
            "AgentToolControl",
            &persisted_tampered,
            "working_agent",
            &agent,
        )
        .await
        .unwrap();
    assert!(
        claim(&node, &agent_access, &agent, &persisted_tampered)
            .await
            .unwrap()
            .is_none(),
        "the agent rejects a stored intent whose signed operation was changed"
    );
    async fn publish(
        node: &Arc<defra_node::Node>,
        access: &ConfigAccess,
        user: &KeyIdentity,
        node_did: &str,
        tool_doc: &str,
    ) -> String {
        let intent = prepare(
            access,
            user,
            node_did,
            tool_doc,
            Some(("tool-control-session", "control-call")),
            "process_stop",
        )
        .await
        .unwrap();
        submit(node, user.did(), &intent).await.unwrap()
    }
    let first = publish(&node, &user_access, &user, runtime.did(), tool_doc).await;
    let second = publish(&node, &user_access, &user, runtime.did(), tool_doc).await;
    let third = publish(&node, &user_access, &user, runtime.did(), tool_doc).await;
    let fourth = publish(&node, &user_access, &user, runtime.did(), tool_doc).await;
    let fifth = publish(&node, &user_access, &user, runtime.did(), tool_doc).await;
    let foreign_access = client_access(&endpoint, &foreign);
    let fake = foreign_access.transact_write("test.control.foreign_claim", |txn| {
        let first = &first; let binding = &binding;
        Box::pin(async move {
            let response = txn.execute_with_variables(
                "mutation($input: AgentToolControlClaimMutationInputArg!) { create_AgentToolControlClaim(input: $input) { _docID } }",
                &serde_json::json!({"input": {"intent_doc_id": first, "intent_json": "{}", "agent_did": binding.agent_did, "receipt_json": "{}"}}),
            ).await?;
            Ok::<_, anyhow::Error>(crate::store::graphql::created_doc_id(&response, "AgentToolControlClaim")?)
        })
    }).await.unwrap();
    foreign_access
        .add_actor_relationship("AgentToolControlClaim", &fake, "working_agent", &agent)
        .await
        .unwrap();
    foreign_access
        .add_actor_relationship(
            "AgentToolControlClaim",
            &fake,
            "reader",
            &user.did().parse().unwrap(),
        )
        .await
        .unwrap();
    let other = identity::Did::new(user.did()).unwrap();
    assert!(claim(&node, &agent_access, &other, &first).await.is_err());
    assert!(
        claim(&node, &ConfigAccess::local(node.clone()), &agent, &first)
            .await
            .is_err()
    );
    assert_eq!(
        claim(&node, &agent_access, &agent, &first)
            .await
            .unwrap()
            .is_some(),
        modeled(true, true, true),
        "a foreign-owned claim cannot poison the agent's claim"
    );
    let claim_query = |intent: &str| {
        format!(
            r#"{{ AgentToolControlClaim(filter: {{ intent_doc_id: {{ _eq: "{}" }} }}) {{ _docID receipt_json }} }}"#,
            intent
        )
    };
    let first_rows = agent_access.execute(&claim_query(&first)).await.unwrap();
    let first_claim = first_rows["data"]["AgentToolControlClaim"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["_docID"].as_str())
        .find(|id| *id != fake)
        .unwrap()
        .to_owned();
    assert!(claim_unavailable(&node, &agent_access, &agent, &fourth)
        .await
        .unwrap()
        .is_none());
    let unavailable = read_receipt(
        &user_access,
        runtime.did(),
        &fourth,
        tool_doc,
        "process_stop",
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        unavailable.outcome,
        gents_protocol::process_control::ToolControlReceiptOutcome::Rejected
    ));
    let failed_claim = claim(&node, &agent_access, &agent, &third)
        .await
        .unwrap()
        .unwrap();
    let (result, mutation_count) =
        crate::store::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
            Some(1),
            execute_claimed(
                &node,
                &agent_access,
                &crate::tools::call::hook::BackgroundExecutionRegistry::default(),
                &agent,
                failed_claim,
            ),
        )
        .await;
    result.unwrap();
    assert!(
        mutation_count > 1,
        "the agent stores the signed result after a failed control mutation"
    );
    let unconfirmed = read_receipt(
        &user_access,
        runtime.did(),
        &third,
        tool_doc,
        "process_stop",
    )
    .await
    .unwrap();
    assert_eq!(unconfirmed.is_some(), may_publish(true, true));
    let unconfirmed = unconfirmed.unwrap();
    assert!(matches!(
        &unconfirmed.outcome,
        gents_protocol::process_control::ToolControlReceiptOutcome::Unconfirmed
    ));
    let mut forged = unconfirmed.clone();
    forged.outcome = gents_protocol::process_control::ToolControlReceiptOutcome::Process(
        gents_protocol::process_control::ProcessControlOutcome::Lost,
    );
    agent_access.transact_write("test.control.forged_mirror", |txn| {
        let forged = &forged; let third = &third;
        Box::pin(async move {
            txn.execute_with_variables(&format!(r#"mutation($input: AgentToolControlMutationInputArg!) {{ update_AgentToolControl(docID: "{}", input: $input) {{ _docID }} }}"#, third), &serde_json::json!({"input": {"outcome_json": serde_json::to_string(forged)?}})).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
    assert!(
        read_receipt(
            &user_access,
            runtime.did(),
            &third,
            tool_doc,
            "process_stop"
        )
        .await
        .is_err(),
        "a modified outcome cannot reuse an agent signature"
    );
    let _claimed_before_restart = claim(&node, &agent_access, &agent, &fifth)
        .await
        .unwrap()
        .unwrap();
    let fifth_rows = agent_access.execute(&claim_query(&fifth)).await.unwrap();
    let fifth_claim = fifth_rows["data"]["AgentToolControlClaim"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    user_access
        .delete_actor_relationship("AgentToolControl", &fifth, "working_agent", &agent)
        .await
        .unwrap();
    drop(_claimed_before_restart);
    let recovered = recover_claim(&agent_access, &agent, runtime.did(), &fifth_claim)
        .await
        .unwrap()
        .expect("agent-owned claim retains the signed intent");
    assert!(
        execute_claimed(
            &node,
            &agent_access,
            &crate::tools::call::hook::BackgroundExecutionRegistry::default(),
            &agent,
            recovered
        )
        .await
        .is_err(),
        "revoked intent access prevents mirroring"
    );
    let state = agent_access
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
            tool_doc
        ))
        .await
        .unwrap();
    assert_eq!(
        state["data"]["AgentToolCall"][0]["lifecycle_state"], "cancelled",
        "the retained claim executes despite the revoked intent grant"
    );
    assert!(
        read_receipt(
            &user_access,
            runtime.did(),
            &fifth,
            tool_doc,
            "process_stop"
        )
        .await
        .unwrap()
        .is_none(),
        "the client does not inherit claim access"
    );
    let pause = Arc::new(ClaimPause {
        observed: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
        armed: std::sync::atomic::AtomicBool::new(true),
    });
    let paused = {
        let node = node.clone();
        let access = agent_access.clone();
        let agent = agent.clone();
        let doc = second.clone();
        let pause = pause.clone();
        tokio::spawn(async move { claim_paused(&node, &access, &agent, &doc, &pause).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), pause.observed.notified())
        .await
        .expect("claim did not reach its native authorization boundary");
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        admin.revoke_node_permission(&user.did().parse().unwrap(), NodePermission::DocumentUpdate),
    )
    .await
    .expect("native revocation did not finish while the claim was open")
    .unwrap();
    pause.resume.notify_one();
    assert_eq!(
        paused.await.unwrap().unwrap().is_some(),
        modeled(true, false, false)
    );
    let retained = recover_claim(&agent_access, &agent, runtime.did(), &first_claim)
        .await
        .unwrap()
        .expect("the committed agent-owned claim survives later revocation");
    assert!(
        execute_claimed(
            &node,
            &agent_access,
            &crate::tools::call::hook::BackgroundExecutionRegistry::default(),
            &agent,
            retained
        )
        .await
        .is_err(),
        "receipt publication rechecks revoked client authority"
    );
    let observed = read_receipt(
        &user_access,
        runtime.did(),
        &first,
        tool_doc,
        "process_stop",
    )
    .await
    .unwrap();
    assert_eq!(observed.is_some(), may_publish(false, true));
    assert!(!may_publish(true, false));
    let rows = agent_access.execute(&claim_query(&first)).await.unwrap();
    let signed = rows["data"]["AgentToolControlClaim"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|row| row["receipt_json"].as_str().filter(|raw| *raw != "{}"))
        .expect("the agent retained its signed result");
    let receipt: gents_protocol::process_control::ToolControlReceipt =
        serde_json::from_str(signed).unwrap();
    assert!(runtime
        .verify(
            &binding.agent_did,
            &receipt.signing_payload().unwrap(),
            &receipt.signature
        )
        .await
        .unwrap());
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn generated_tool_control_receipts_require_selected_agent_and_exact_intent() {
    use gents_protocol::process_control::{
        ProcessControlOutcome, ToolControlReceipt, ToolControlReceiptOutcome,
    };
    let home = tempfile::tempdir().unwrap();
    let agent = KeyIdentity::load_or_create(home.path().join("agent.key"), None).unwrap();
    let other = KeyIdentity::load_or_create(home.path().join("other.key"), None).unwrap();
    let expected = ToolControlReceipt {
        version: 1,
        intent_doc_id: "intent".into(),
        client_did: "client".into(),
        agent_did: agent.did().into(),
        node_did: "node".into(),
        session_id: "session".into(),
        session_doc_id: "session-doc".into(),
        request_doc_id: "request-doc".into(),
        tool_call_id: "call".into(),
        tool_call_doc_id: "doc".into(),
        action: "process_stop".into(),
        outcome: ToolControlReceiptOutcome::Process(ProcessControlOutcome::Cancelled {
            live_execution_cancelled: false,
        }),
        signature: Vec::new(),
    };
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().client_process_receipt_cases;
    assert!(!cases.is_empty());
    for case in cases {
        let mut receipt = expected.clone();
        receipt.intent_doc_id = case["intent_doc"].as_str().unwrap().into();
        receipt.client_did = case["author"].as_str().unwrap().into();
        receipt.agent_did = if case["agent"] == "agent" {
            agent.did()
        } else {
            other.did()
        }
        .into();
        receipt.tool_call_id = case["call"].as_str().unwrap().into();
        receipt.tool_call_doc_id = case["document"].as_str().unwrap().into();
        let signer = if case["agent"] == "agent" {
            &agent
        } else {
            &other
        };
        let outcome = |result: &serde_json::Value| {
            if result == "applied" {
                ToolControlReceiptOutcome::Process(ProcessControlOutcome::Cancelled {
                    live_execution_cancelled: false,
                })
            } else {
                ToolControlReceiptOutcome::Unconfirmed
            }
        };
        receipt.outcome = outcome(&case["signed_result"]);
        receipt.signature = signer
            .sign(&receipt.signing_payload().unwrap())
            .await
            .unwrap();
        receipt.outcome = outcome(&case["observed_result"]);
        if case["signature_valid"] == false {
            receipt.signature[0] ^= 1;
        }
        let accepted = receipt.validate_for(&expected).is_ok()
            && agent
                .verify(
                    &expected.agent_did,
                    &receipt.signing_payload().unwrap(),
                    &receipt.signature,
                )
                .await
                .unwrap_or(false);
        assert_eq!(accepted, case["expected"].as_bool().unwrap(), "{case}");
    }
    let mut signed = expected.clone();
    signed.signature = agent
        .sign(&signed.signing_payload().unwrap())
        .await
        .unwrap();
    signed.outcome = ToolControlReceiptOutcome::Process(ProcessControlOutcome::Lost);
    assert!(
        !agent
            .verify(
                &expected.agent_did,
                &signed.signing_payload().unwrap(),
                &signed.signature
            )
            .await
            .unwrap_or(false),
        "a result cannot be changed after signing"
    );
}

#[tokio::test]
async fn active_client_steer_rechecks_its_own_author_at_atomic_consumption() {
    let modeled = crate::lean_vocab_test::lean_contract_snapshot();
    let allowed = modeled
        .cross_actor_authorization_cases
        .iter()
        .find(|case| case.name == "committed_claim")
        .unwrap();
    let denied = modeled
        .cross_actor_authorization_cases
        .iter()
        .find(|case| case.name == "node_denied_at_check")
        .unwrap();
    assert_eq!(
        allowed.decision,
        crate::lean_vocab_test::LeanCrossActorDecision::Claimed
    );
    assert_eq!(
        denied.decision,
        crate::lean_vocab_test::LeanCrossActorDecision::Denied
    );
    let home = tempfile::tempdir().unwrap();
    let runtime: Arc<dyn NodeIdentity> =
        Arc::new(KeyIdentity::load_or_create(home.path().join("runtime.key"), None).unwrap());
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    let collaborator =
        KeyIdentity::load_or_create(home.path().join("collaborator.key"), None).unwrap();
    let (node, endpoint) = serve_client_node(runtime.as_ref()).await;
    let binding = crate::store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
    let agent = crate::store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
    let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
    for did in [user.did(), collaborator.did(), agent.did()] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
        ] {
            admin
                .grant_node_permission(&did.parse().unwrap(), permission)
                .await
                .unwrap();
        }
    }
    let client = |identity: &KeyIdentity| client_access(&endpoint, identity);
    let owner = client(&user);
    seed_owned_agent(&owner, runtime.did(), &binding).await;
    let agent_doc = owner.execute("{ Agent { _docID } }").await.unwrap()["data"]["Agent"][0]
        ["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    owner
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "session_writer",
            &collaborator.did().parse().unwrap(),
        )
        .await
        .unwrap();
    let user_access = client(&user);
    let collaborator_access = client(&collaborator);
    let mut head = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "steer-head",
        runtime.did(),
        user.did(),
        agent.did(),
        "steer-session",
        "head",
        "interactive",
        "2026-11-04T00:00:00Z",
        AgentRequestAdmissionRecord::client(user.did()),
    );
    head.session_doc_id = Some(reserve_goal_session(&user_access, &head).await.unwrap());
    head.agent_doc_id = Some(agent_doc);
    crate::sign_agent_request_create(&user, &mut head)
        .await
        .unwrap();
    submit_client_request(&user_access, &head).await.unwrap();
    let agent_access = ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let query = format!(
        "{{ AgentRequest(filter: {{ request_id: {{ _eq: \"steer-head\" }} }}) {{ {} }} }}",
        crate::SIGNED_REQUEST_FIELDS
    );
    let response = agent_access.execute(&query).await.unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let request = AgentRequest::try_from(row).unwrap();
    let (_owner, authority) = crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = AgentRequestAdmissionVerifier::new(node.clone(), runtime.clone(), authority);
    let (request, admission) = verifier
        .verify_fresh_for_claim_with_identity(&request, agent.as_ref())
        .await
        .unwrap();
    assert!(admission.is_some());
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        agent.did(),
        runtime.did(),
        request,
        60,
        ExecutionOrigin::Interactive,
        String::new(),
    );
    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
    lifecycle.set_client_claim_admission(admission);
    lifecycle.set_steering_admission(verifier.clone(), agent.clone());
    assert!(matches!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    ));
    let writer = crate::request::streaming::DefraStreamWriter::new_managed(
        node.clone(),
        agent.as_ref(),
        std::time::Duration::ZERO,
    )
    .unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let mut steer = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "steer-input",
        runtime.did(),
        user.did(),
        agent.did(),
        "steer-session",
        "new signed input",
        "interactive",
        "2026-11-04T00:00:01Z",
        AgentRequestAdmissionRecord::client(collaborator.did()),
    );
    steer.session_doc_id = head.session_doc_id.clone();
    steer.agent_doc_id = head.agent_doc_id.clone();
    steer.input.queue = Some(gents_protocol::request_input::RequestQueue {
        source: gents_protocol::request_input::QueueSource::User,
        delivery: gents_protocol::request_input::QueueDelivery::Steer,
        policy: gents_protocol::request_input::QueuePolicy::Append,
        position: None,
        key: None,
        queued_after_request_id: None,
        interrupted_request_id: None,
        background_completion_wake_version: None,
    });
    crate::sign_agent_request_create(&collaborator, &mut steer)
        .await
        .unwrap();
    submit_client_request(&collaborator_access, &steer)
        .await
        .unwrap();
    let snapshot = crate::request::lifecycle::queue::steering_snapshot(
        &node,
        lifecycle.request(),
        &verifier,
        Some(agent.as_ref()),
    )
    .await
    .unwrap();
    assert_eq!(snapshot.inputs.len(), 1);
    let input = &snapshot.inputs[0];
    let before = agent_access
        .execute("{ AgentMessage { _docID } }")
        .await
        .unwrap();
    admin
        .revoke_node_permission(
            &collaborator.did().parse().unwrap(),
            NodePermission::DocumentUpdate,
        )
        .await
        .unwrap();
    let blocked = crate::request::lifecycle::queue::steering_snapshot(
        &node,
        lifecycle.request(),
        &verifier,
        Some(agent.as_ref()),
    )
    .await
    .unwrap();
    assert!(
        blocked.inputs.is_empty(),
        "a revoked author is not repeatedly selected"
    );
    assert!(writer
        .publish_authored_message(&lifecycle, &input.key, &input.message)
        .await
        .is_err());
    let after = agent_access
        .execute("{ AgentMessage { _docID } }")
        .await
        .unwrap();
    assert_eq!(
        before["data"]["AgentMessage"], after["data"]["AgentMessage"],
        "denied consumption rolls back publication"
    );
    let pending = agent_access.execute("{ AgentRequest(filter: { request_id: { _eq: \"steer-input\" } }) { lifecycle_state } }").await.unwrap();
    assert_eq!(
        pending["data"]["AgentRequest"][0]["lifecycle_state"],
        "pending"
    );
    admin
        .grant_node_permission(
            &collaborator.did().parse().unwrap(),
            NodePermission::DocumentUpdate,
        )
        .await
        .unwrap();
    admin
        .revoke_node_permission(&user.did().parse().unwrap(), NodePermission::DocumentUpdate)
        .await
        .unwrap();
    writer
        .publish_authored_message(&lifecycle, &input.key, &input.message)
        .await
        .unwrap();
    let result = agent_access.execute("{ AgentRequest(filter: { request_id: { _eq: \"steer-input\" } }) { lifecycle_state superseded_by_request_doc_id } }").await.unwrap();
    assert_eq!(
        result["data"]["AgentRequest"][0]["lifecycle_state"],
        "superseded"
    );
    assert_eq!(
        result["data"]["AgentRequest"][0]["superseded_by_request_doc_id"],
        lifecycle.request().doc_id
    );
    admin
        .revoke_node_permission(
            &collaborator.did().parse().unwrap(),
            NodePermission::DocumentUpdate,
        )
        .await
        .unwrap();
    writer
        .publish_authored_message(&lifecycle, &input.key, &input.message)
        .await
        .unwrap();
    drop(lifecycle);
    node.shutdown().await.unwrap();
}
