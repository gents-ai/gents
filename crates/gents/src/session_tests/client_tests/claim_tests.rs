use super::*;
use gents_protocol::request_admission::AgentRequestCreate;
use gents_protocol::row::AgentRequest;
use gents_session::request::admission::AgentRequestAdmissionVerifier;
use gents_session::request::lifecycle::{ClaimOutcome, ExecutionOrigin, RequestLifecycle};
use gents_session::test_support::{grant_operator_user, seed_owned_agent, serve_client_node};
use gents_store::config_client::{ConfigAccess, NodePermission};

#[tokio::test]
async fn watcher_routes_native_private_requests_as_their_selected_agents() {
    use gents_session::request::watcher::{DefraWatcher, Watcher};
    let snapshot = gents_lean_vocab::lean_contract_snapshot();
    let home = tempfile::tempdir().unwrap();
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    let (db, runtime) = serve_client_node(
        "watcher-routes-native-private-requests-a",
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
    let runtime: Arc<dyn NodeIdentity> = Arc::new(runtime);
    let node = db.node.clone();
    grant_operator_user(&node, runtime.as_ref(), &user).await;
    let first = gents_store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
    let second = gents_store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
    let first_signer =
        gents_store::identity::resolve_agent_identity(runtime.as_ref(), &first).unwrap();
    let second_signer =
        gents_store::identity::resolve_agent_identity(runtime.as_ref(), &second).unwrap();
    let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
    let owner = db.client_access(&user);
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
        let request = gents_session::request::lifecycle::build_signed_client_request(
            &owner,
            gents_session::request::lifecycle::RequestSpec::new(
                RequestPurpose::Normal,
                gents_session::request::lifecycle::RequestIdentity {
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
            gents_session::request::lifecycle::RequestSigner::Identity(&user),
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
        gents_store::graphql::created_doc_id(&created_message, "AgentMessage").unwrap();
    gents_store::test_support::relationships::resource_in_txn(
        &agent_access,
        "AgentMessage",
        &message_doc,
        "parent",
        "sessions",
        session_doc,
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        gents_session::session::max_sequence_as(
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
    let hook = gents_toolcalls::call::hook::DefraSessionHook::resume_with_actor_policy(
        node.clone(),
        "first-session",
        runtime.did(),
        Some(user.did()),
        first.agent_did.parse().unwrap(),
        gents_toolcalls::call::hook::FailurePolicy::default(),
    )
    .await
    .unwrap();
    assert_eq!(hook.session_id().await.as_deref(), Some("first-session"));
    for actor in [runtime.did(), second.agent_did.as_str()] {
        assert!(
            gents_toolcalls::call::hook::DefraSessionHook::resume_with_actor_policy(
                node.clone(),
                "first-session",
                runtime.did(),
                Some(user.did()),
                actor.parse().unwrap(),
                gents_toolcalls::call::hook::FailurePolicy::default(),
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
    for request_id in ["z-first", "a-second"] {
        assert_eq!(
            watcher.next_request().await.unwrap().unwrap().request_id,
            request_id
        );
    }
    let household = gents_lean_vocab::lean_identity_household_cases()
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
                gents_lean_vocab::LeanIdentitySubject::Actor { actor } => Some(actor),
                _ => None,
            }
        })
        .expect("modeled sibling executor");
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
        gents_session::request::admission::sign_agent_request_create(signer.as_ref(), &mut request)
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
            gents_store::graphql::escape_graphql_string(session_id),
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
        .set_agent_identities(vec![first_signer.clone(), second_signer.clone()])
        .unwrap();
    async fn set_agent_enabled(owner: &ConfigAccess, agent_doc: &str, enabled: bool) {
        owner
            .transact_write("test.watcher.set_agent_enabled", |txn| {
                Box::pin(async move {
                    txn.execute_client_signed_update(
                        &format!(
                            r#"mutation($input: AgentMutationInputArg!) {{ update_Agent(docID: "{}", input: $input) {{ _docID }} }}"#,
                            gents_store::graphql::escape_graphql_string(agent_doc)
                        ),
                        &serde_json::json!({"input": {"enabled": enabled}}),
                        gents_protocol::schemas::AGENT,
                    )
                    .await
                })
            })
            .await
            .unwrap();
    }
    set_agent_enabled(&owner, &agent_doc, false).await;
    let disabled = [second.agent_did.clone()]
        .into_iter()
        .collect::<rapidhash::RapidHashSet<_>>();
    watcher.set_disabled_agents(disabled.clone());
    let skipped = watcher.scan_pending_requests().await.unwrap();
    assert_eq!(
        skipped
            .requests
            .iter()
            .map(|request| request.agent_did.as_str())
            .collect::<Vec<_>>(),
        [first.agent_did.as_str()]
    );
    assert!(
        skipped.failures.is_empty(),
        "a disabled Agent is not a scan failure: {:?}",
        skipped.failures
    );
    assert!(!skipped.needs_rescan());
    watcher
        .set_agent_identities(vec![second_signer.clone()])
        .unwrap();
    assert!(
        watcher.pending_requests().await.unwrap().is_empty(),
        "a node whose only Agent is disabled has an empty queue, not an error"
    );
    watcher.set_disabled_agents(Default::default());
    watcher
        .set_agent_identities(vec![first_signer.clone(), second_signer.clone()])
        .unwrap();
    let partial = watcher.scan_pending_requests().await.unwrap();
    assert_eq!(
        partial
            .requests
            .iter()
            .map(|request| request.agent_did.as_str())
            .collect::<Vec<_>>(),
        [first.agent_did.as_str()]
    );
    assert_eq!(
        partial
            .failures
            .iter()
            .map(|failure| failure.actor())
            .collect::<Vec<_>>(),
        [second.agent_did.as_str()],
        "an Agent the watcher was not told is disabled still fails its scan"
    );
    assert!(partial.needs_rescan());
    watcher
        .set_agent_identities(vec![second_signer.clone()])
        .unwrap();
    let error = watcher
        .pending_requests()
        .await
        .expect_err("missing NAC root is not an empty queue");
    assert!(
        error.to_string().contains(&second.agent_did),
        "the failed actor is reported: {error:#}"
    );
    watcher.set_disabled_agents(disabled);
    set_agent_enabled(&owner, &agent_doc, true).await;
    watcher.set_disabled_agents(Default::default());
    watcher
        .set_agent_identities(vec![first_signer.clone(), second_signer])
        .unwrap();
    let restored = watcher.scan_pending_requests().await.unwrap();
    assert!(restored.failures.is_empty(), "{:?}", restored.failures);
    assert!(!restored.needs_rescan());
    for agent in [&first.agent_did, &second.agent_did] {
        assert!(
            restored
                .requests
                .iter()
                .any(|request| &request.agent_did == agent),
            "{agent} work is scanned once its access returns"
        );
    }
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn client_tool_control_claim_rechecks_native_authority_and_retains_committed_claim() {
    use gents_protocol::tool_call::AwaitMode;
    use gents_toolcalls::call::control::client::{
        claim, claim_paused, claim_unavailable, execute_claimed, prepare, read_receipt,
        recover_claim, submit, ClaimPause,
    };
    use gents_toolcalls::call::lifecycle::admission_fixture::publish_accepted_on_claimed_request;

    let grant_cases = gents_lean_vocab::families::native_refinement_grant_scenario::cases();
    let mut publications = Vec::new();
    let claim_cases = gents_lean_vocab::families::native_refinement_claim_scenario::cases();
    let mut claims = Vec::new();
    let home = tempfile::tempdir().unwrap();
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    let foreign =
        KeyIdentity::load_or_create(home.path().join("foreign-control.key"), None).unwrap();
    let collaborator =
        KeyIdentity::load_or_create(home.path().join("collaborator.key"), None).unwrap();
    let (db, runtime) = serve_client_node(
        "client-tool-control-claim-rechecks-nativ",
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
    let runtime: Arc<dyn NodeIdentity> = Arc::new(runtime);
    let node = db.node.clone();
    grant_operator_user(&node, runtime.as_ref(), &user).await;
    let binding = gents_store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
    let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
    for did in [
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
    let user_access = db.client_access(&user);
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
    // The collaborator reads the Agent's sessions and their tool calls; the
    // phases below vary only its update and invoke authority.
    for relation in ["reader", "session_writer"] {
        user_access
            .add_actor_relationship(
                "Agent",
                &agent_doc,
                relation,
                &collaborator.did().parse().unwrap(),
            )
            .await
            .unwrap();
    }
    // `session_writer` confers invoke but not session update, which comes
    // from the session's `working_agent` or the Agent's `session_admin`
    // (`Identity.Household.policy`).
    user_access
        .add_actor_relationship(
            "AgentSession",
            create.session_doc_id.as_ref().unwrap(),
            "working_agent",
            &collaborator.did().parse().unwrap(),
        )
        .await
        .unwrap();
    gents_session::request::admission::sign_agent_request_create(&user, &mut create)
        .await
        .unwrap();
    submit_client_request(&user_access, &create).await.unwrap();
    let agent: identity::Did = binding.agent_did.parse().unwrap();
    let agent_identity =
        gents_store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
    let agent_access =
        ConfigAccess::local_managed_agent(node.clone(), agent.clone(), agent_identity.as_ref())
            .unwrap();
    let query = format!(
        "{{ AgentRequest {{ {} }} }}",
        gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
    );
    let response = agent_access.execute(&query).await.unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let request = AgentRequest::try_from(row).unwrap();
    let (_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
    let verifier = AgentRequestAdmissionVerifier::new(node.clone(), runtime.clone(), authority);
    let (request, admission) = verifier
        .verify_fresh_for_claim(&request, &binding.agent_did)
        .await
        .unwrap();
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
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
    let collaborator_access = db.client_access(&collaborator);
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
    let document_race = submit(&collaborator_access, &collaborator_intent)
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
        .delete_actor_relationship(
            "AgentSession",
            session_doc,
            "working_agent",
            &collaborator.did().parse().unwrap(),
        )
        .await
        .unwrap();
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
    let denied_update = submit(&collaborator_access, &denied_update).await.unwrap();
    claims.push((
        (true, false, true),
        claim(&node, &agent_access, &agent, &denied_update)
            .await
            .unwrap()
            .is_some(),
    ));
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
    let denied_invoke = submit(&collaborator_access, &denied_invoke).await.unwrap();
    claims.push((
        (true, true, false),
        claim(&node, &agent_access, &agent, &denied_invoke)
            .await
            .unwrap()
            .is_some(),
    ));
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
    let denied_read = submit(&collaborator_access, &denied_read).await.unwrap();
    admin
        .revoke_node_permission(
            &collaborator.did().parse().unwrap(),
            NodePermission::DocumentRead,
        )
        .await
        .unwrap();
    claims.push((
        (false, true, true),
        claim(&node, &agent_access, &agent, &denied_read)
            .await
            .unwrap()
            .is_some(),
    ));
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
        Err(gents_toolcalls::call::control::client::ClientControlError::NotFound)
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
        submit(&user_access, &tampered).await,
        Err(gents_toolcalls::call::control::client::ClientControlError::NotAuthorized)
    ));
    assert!(matches!(
        submit(&collaborator_access, &signed).await,
        Err(gents_toolcalls::call::control::client::ClientControlError::NotAuthorized)
    ));
    let persisted_tampered = user_access.transact_write("test.control.invalid_signed_intent", |txn| {
        let tampered = &tampered;
        Box::pin(async move {
            let response = txn.execute_client_signed_create(
                "mutation($input: AgentToolControlMutationInputArg!) { create_AgentToolControl(input: $input) { _docID } }",
                &serde_json::json!({"input": {
                    "intent_id": tampered.intent_id, "intent_json": serde_json::to_string(tampered)?,
                    "client_did": tampered.client_did, "agent_did": tampered.agent_did,
                    "node_did": tampered.node_did, "session_id": tampered.session_id,
                    "session_doc_id": tampered.session_doc_id, "request_doc_id": tampered.request_doc_id,
                    "requester_did": tampered.requester_did, "tool_call_id": tampered.tool_call_id,
                    "tool_call_doc_id": tampered.tool_call_doc_id, "action": tampered.action, "state": "pending"
                }}),
                gents_protocol::schemas::AGENT_TOOL_CONTROL,
            ).await?;
            Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(&response, "AgentToolControl")?)
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
        submit(access, &intent).await.unwrap()
    }
    let first = publish(&user_access, &user, runtime.did(), tool_doc).await;
    let second = publish(&user_access, &user, runtime.did(), tool_doc).await;
    let third = publish(&user_access, &user, runtime.did(), tool_doc).await;
    let fourth = publish(&user_access, &user, runtime.did(), tool_doc).await;
    let fifth = publish(&user_access, &user, runtime.did(), tool_doc).await;
    let foreign_access = db.client_access(&foreign);
    let fake = foreign_access.transact_write("test.control.foreign_claim", |txn| {
        let first = &first; let binding = &binding;
        Box::pin(async move {
            let response = txn.execute_client_signed_create(
                "mutation($input: AgentToolControlClaimMutationInputArg!) { create_AgentToolControlClaim(input: $input) { _docID } }",
                &serde_json::json!({"input": {"intent_doc_id": first, "intent_json": "{}", "agent_did": binding.agent_did, "receipt_json": "{}"}}),
                gents_protocol::schemas::AGENT_TOOL_CONTROL_CLAIM,
            ).await?;
            Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(&response, "AgentToolControlClaim")?)
        })
    }).await.unwrap();
    gents_store::test_support::relationships::actor_in_txn(
        &foreign_access,
        "AgentToolControlClaim",
        &fake,
        "working_agent",
        &agent,
        false,
    )
    .await
    .unwrap();
    gents_store::test_support::relationships::actor_in_txn(
        &foreign_access,
        "AgentToolControlClaim",
        &fake,
        "reader",
        &user.did().parse().unwrap(),
        false,
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
    claims.push((
        (true, true, true),
        claim(&node, &agent_access, &agent, &first)
            .await
            .unwrap()
            .is_some(),
    ));
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
        gents_store::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
            Some(1),
            execute_claimed(
                &node,
                &agent_access,
                &gents_toolcalls::call::hook::BackgroundExecutionRegistry::default(),
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
    publications.push(((true, true), unconfirmed.is_some()));
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
            &gents_toolcalls::call::hook::BackgroundExecutionRegistry::default(),
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
    claims.push((
        (true, false, false),
        paused.await.unwrap().unwrap().is_some(),
    ));
    let retained = recover_claim(&agent_access, &agent, runtime.did(), &first_claim)
        .await
        .unwrap()
        .expect("the committed agent-owned claim survives later revocation");
    assert!(
        execute_claimed(
            &node,
            &agent_access,
            &gents_toolcalls::call::hook::BackgroundExecutionRegistry::default(),
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
    publications.push(((false, true), observed.is_some()));
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
    assert_eq!(claim_cases.len(), claims.len());
    for (case, (permissions, claimed)) in claim_cases.iter().zip(claims) {
        assert_eq!(
            (
                case.input.claimant.as_str(),
                case.input.owner.as_deref(),
                case.input.requested_id.as_str(),
                case.input.requested_doc.as_str(),
                case.input.current_revision,
                (case.input.read, case.input.update, case.input.invoke),
            ),
            ("agent", Some("client"), "call", "doc", 0, permissions),
            "the served scenario builds {:?}",
            case.input
        );
        case.check(claimed);
    }
    assert_eq!(grant_cases.len(), publications.len());
    for (case, (grant, published)) in grant_cases.iter().zip(publications) {
        assert_eq!(
            (case.input.authority, case.input.fenced),
            grant,
            "the served scenario publishes {:?}",
            case.input
        );
        case.check(published);
    }
    node.shutdown().await.unwrap();
}
