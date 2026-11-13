use super::*;
use crate::request::admission::AgentRequestAdmissionVerifier;
use crate::request::lifecycle::{ClaimOutcome, ExecutionOrigin, RequestLifecycle};
use gents_protocol::row::AgentRequest;
use gents_store::config_client::NodePermission;

#[tokio::test]
async fn signed_client_claim_uses_transaction_bound_intent_and_native_permissions() {
    for (index, case) in
        gents_lean_vocab::families::cross_actor_authorization_signed_client_claim::cases()
            .into_iter()
            .enumerate()
    {
        let input = &case.input;
        let name = format!("{}-{index}", input.name);
        let (mutate, scheduled, steer) = (!input.signature_valid, input.scheduled, input.steer);
        let home = tempfile::tempdir().unwrap();
        let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
        let owner = KeyIdentity::load_or_create(home.path().join("owner.key"), None).unwrap();
        let (db, runtime) = serve_client_node(
            "signed-client-claim-uses-transaction-bou",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
        let runtime: Arc<dyn NodeIdentity> = Arc::new(runtime);
        let node = db.node.clone();
        grant_operator_user(&node, runtime.as_ref(), &user).await;
        grant_operator_user(&node, runtime.as_ref(), &owner).await;
        let binding = gents_store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
        let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
        let subject = ::identity::Did::new(binding.agent_did.as_str()).unwrap();
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
        let user_access = db.client_access(&user);
        if !input.agent_invoke {
            let owner_access = db.client_access(&owner);
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
        crate::request::admission::sign_agent_request_create(&user, &mut create)
            .await
            .unwrap();
        submit_client_request(&user_access, &create).await.unwrap();
        if !input.requester_node_permission {
            admin
                .revoke_node_permission(&user.did().parse().unwrap(), NodePermission::DocumentUpdate)
                .await
                .unwrap();
        }
        let agent_identity =
            gents_store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
        let agent_access = ConfigAccess::local_managed_agent(
            node.clone(),
            binding.agent_did.parse().unwrap(),
            agent_identity.as_ref(),
        )
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
        let decision = match result.as_ref() {
            Ok(ClaimOutcome::Claimed) => "claimed",
            Err(_) => "denied",
            Ok(other) => panic!("{name}: unmodeled claim outcome {other:?}"),
        };
        if claimed {
            assert!(lifecycle.owns_execution().await.unwrap());
        }
        let state = user_access
            .execute("{ AgentRequest { lifecycle_state failure_reason terminal_output } }")
            .await
            .unwrap();
        let row = &state["data"]["AgentRequest"][0];
        let terminal_output = (!row["terminal_output"].is_null()).then(|| {
            let output: gents_protocol::output::TerminalOutput =
                serde_json::from_value(row["terminal_output"].clone()).unwrap();
            match output {
                gents_protocol::output::TerminalOutput::NoMessage => "no_message".to_owned(),
                _ => "message".to_owned(),
            }
        });
        case.check(
            gents_lean_vocab::families::cross_actor_authorization_signed_client_claim::ClaimOutput {
                decision: decision.to_owned(),
                lifecycle: row["lifecycle_state"].as_str().unwrap().to_owned(),
                terminal_output,
                failure_reason: row["failure_reason"]
                    .as_str()
                    .filter(|reason| !reason.is_empty())
                    .map(str::to_owned),
            },
        );
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
            let doc_id = gents_store::graphql::escape_graphql_string(&lifecycle.request().doc_id);
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
    for case in gents_lean_vocab::families::native_refinement_client_process_receipt::cases() {
        let input = &case.input;
        let mut receipt = expected.clone();
        receipt.intent_doc_id = input.intent_doc.clone();
        receipt.client_did = input.author.clone();
        receipt.agent_did = if input.agent == "agent" {
            agent.did()
        } else {
            other.did()
        }
        .into();
        receipt.tool_call_id = input.call.clone();
        receipt.tool_call_doc_id = input.document.clone();
        let signer = if input.agent == "agent" {
            &agent
        } else {
            &other
        };
        let outcome = |result: &str| {
            if result == "applied" {
                ToolControlReceiptOutcome::Process(ProcessControlOutcome::Cancelled {
                    live_execution_cancelled: false,
                })
            } else {
                ToolControlReceiptOutcome::Unconfirmed
            }
        };
        receipt.outcome = outcome(&input.signed_result);
        receipt.signature = signer
            .sign(&receipt.signing_payload().unwrap())
            .await
            .unwrap();
        receipt.outcome = outcome(&input.observed_result);
        if !input.signature_valid {
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
        case.check(accepted);
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
    let home = tempfile::tempdir().unwrap();
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
    let collaborator =
        KeyIdentity::load_or_create(home.path().join("collaborator.key"), None).unwrap();
    let (db, runtime) = serve_client_node(
        "active-client-steer-rechecks-its-own-aut",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let runtime: Arc<dyn NodeIdentity> = Arc::new(runtime);
    let node = db.node.clone();
    grant_operator_user(&node, runtime.as_ref(), &user).await;
    let binding = gents_store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
    let agent = gents_store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
    let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
    for did in [collaborator.did(), agent.did()] {
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
    let client = |identity: &KeyIdentity| db.client_access(identity);
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
    // A steering author holds update on the session it steers, as the modeled
    // committed claim requires of the immediate signer.
    owner
        .add_actor_relationship(
            "AgentSession",
            head.session_doc_id.as_deref().unwrap(),
            "working_agent",
            &collaborator.did().parse().unwrap(),
        )
        .await
        .unwrap();
    head.agent_doc_id = Some(agent_doc);
    crate::request::admission::sign_agent_request_create(&user, &mut head)
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
        gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
    );
    let response = agent_access.execute(&query).await.unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let request = AgentRequest::try_from(row).unwrap();
    let (_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
    let verifier = AgentRequestAdmissionVerifier::new(node.clone(), runtime.clone(), authority);
    let (request, admission) = verifier
        .verify_fresh_for_claim_with_identity(&request, agent.as_ref())
        .await
        .unwrap();
    assert!(admission.is_some());
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
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
    crate::request::admission::sign_agent_request_create(&collaborator, &mut steer)
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
