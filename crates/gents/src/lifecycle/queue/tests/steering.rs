#[test]
fn agent_steering_scope_matches_generated_delivery_contract() {
    let json = gents_lean_contract::run_prepared_generator(
        &gents_lean_contract::proofs_dir().unwrap(),
        "Proofs.Conformance.AgentMessageDelivery:olean",
        "Proofs/Conformance/AgentMessageDelivery.lean",
    )
    .unwrap();
    let cases: serde_json::Value = serde_json::from_str(&json).unwrap();
    let mut head = parent_request("did:test:node", "target");
    head.requester_did = Some("did:test:human".into());
    head.input.cwd = Some("/head".into());
    for case in cases["requesters"].as_array().unwrap() {
        let agent = case["agent"].as_bool().unwrap();
        let same = case["same_requester"].as_bool().unwrap();
        let edge = case["complete_parent_edge"].as_bool().unwrap();
        let parent = if edge { Some("parent") } else { None };
        let partial = if agent { Some("parent") } else { parent };
        let row: AgentRequestRow = serde_json::from_value(serde_json::json!({
            "request_id": "steer", "requester_did": if same { "did:test:human" } else { "did:test:agent" },
            "input": {"queue": {"source": if agent { "steering" } else { "user" }, "policy": "append", "delivery": "steer"}},
            "caused_by_parent_request_id": parent, "caused_by_parent_request_doc_id": parent,
            "caused_by_parent_tool_call_id": partial, "caused_by_parent_tool_call_doc_id": parent,
        })).unwrap();
        assert_eq!(
            super::super::intake::steering_requester_allowed(&head, &row),
            case["expected"].as_bool().unwrap(),
            "{case}"
        );
    }
    let user_continuation: AgentRequestRow = serde_json::from_value(serde_json::json!({
        "request_id": "user-steer", "requester_did": "did:test:human",
        "input": {"queue": {"source": "steering", "policy": "append", "delivery": "steer"}},
        "caused_by_parent_request_id": "request", "caused_by_parent_request_doc_id": "request-doc"
    }))
    .unwrap();
    assert!(super::super::intake::steering_requester_allowed(
        &head,
        &user_continuation
    ));
    for case in cases["contexts"].as_array().unwrap() {
        let agent = case["agent"].as_bool().unwrap();
        let exact = case["exact_context"].as_bool().unwrap();
        let empty = case["empty_agent_context"].as_bool().unwrap();
        let cwd = if agent && empty {
            None
        } else if !agent && exact {
            Some("/head")
        } else {
            Some("/other")
        };
        let row: AgentRequestRow = serde_json::from_value(serde_json::json!({
            "request_id": "steer", "input": {"cwd": cwd, "queue": {"source": if agent { "steering" } else { "user" }, "policy": "append", "delivery": "steer"}},
            "caused_by_parent_tool_call_id": if agent { Some("parent") } else { None },
        })).unwrap();
        assert_eq!(
            super::super::intake::steering_context_allowed(&head, &row),
            case["expected"].as_bool().unwrap(),
            "{case}"
        );
    }
    for case in cases["targets"].as_array().unwrap() {
        head.doc_id = case["head"].as_str().unwrap().to_owned();
        let row: AgentRequestRow = serde_json::from_value(serde_json::json!({
            "request_id": "steer", "node_did": head.node_did, "session_id": head.session_id,
            "agent_id": head.agent_id, "requester_did": "did:test:agent", "purpose": "normal",
            "execution_origin": "interactive",
            "input": {"steer_target_request_doc_id": case["requested"], "queue": {"source": "steering", "policy": "append", "delivery": "steer"}},
            "caused_by_parent_request_id": "caller", "caused_by_parent_request_doc_id": "caller-doc",
            "caused_by_parent_tool_call_id": "call", "caused_by_parent_tool_call_doc_id": "call-doc",
        })).unwrap();
        assert_eq!(
            super::super::intake::steering_compatible(&head, &row, chrono::Utc::now()),
            case["expected"].as_bool().unwrap(),
            "{case}"
        );
    }
}

use super::*;
use crate::lean_vocab_test::LeanQueuedSteeringAction as Action;

#[tokio::test]
async fn signed_agent_steer_enters_active_turn_with_different_requester() {
    use gents_protocol::request_input::QueueDelivery;
    let db = test_db("signed-agent-steer").await;
    let mut head = parent_request(db.node_did(), "agent-steer-session");
    head.requester_did = Some("did:test:human".into());
    head.workspace_id = Some("workspace".into());
    head.workspace_owner_node_did = Some(db.node_did().into());
    head.workspace_authority = Some("readOnly".into());
    head.workspace_seal_hash = Some("sealed".into());
    head.input.cwd = Some("/workspace/project".into());
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "agent-steer-request",
        db.node_did(),
        db.node_did(),
        TEST_AGENT_ID,
        "agent-steer-session",
        "change course",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(db.node_did()),
    );
    create.input.steer_target_request_doc_id = Some(head.doc_id.clone());
    create.input.queue = Some(RequestQueue {
        delivery: QueueDelivery::Steer,
        position: None,
        source: QueueSource::Steering,
        policy: QueuePolicy::Append,
        key: None,
        queued_after_request_id: Some(head.request_id.clone()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    });
    create.request_hop = 1;
    create.caused_by_parent_request_id = Some("caller-request".into());
    create.caused_by_parent_request_doc_id = Some("caller-request-doc".into());
    create.caused_by_parent_tool_call_id = Some("caller-tool".into());
    create.caused_by_parent_tool_call_doc_id = Some("caller-tool-doc".into());
    crate::sign_agent_request_create(db.identity.as_ref(), &mut create)
        .await
        .unwrap();
    let response = crate::config_client::ConfigAccess::write_local_response(
        &db.node,
        "test.signed_agent_steer",
        &create.graphql_mutation().unwrap(),
    )
    .await
    .unwrap();
    let doc = extract_single_doc_id(&response, "create_AgentRequest").unwrap();
    let (_owner, authority) = crate::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request_admission::AgentRequestAdmissionVerifier::new(
        db.node.clone(),
        db.identity.clone(),
        authority,
    );
    let snapshot = super::super::intake::steering_snapshot(&db.node, &head, &verifier)
        .await
        .unwrap();
    assert_eq!(snapshot.inputs.len(), 1);
    assert_eq!(snapshot.inputs[0].key, folded_input_key(&doc));
    assert_eq!(
        snapshot.inputs[0].message.rag_text().as_deref(),
        Some("change course")
    );
    head.doc_id = "later-physical-turn".into();
    let later = super::super::intake::steering_snapshot(&db.node, &head, &verifier)
        .await
        .unwrap();
    assert!(
        later.inputs.is_empty(),
        "a later turn cannot consume the steer"
    );
    db.node.shutdown().await;
}

#[tokio::test]
async fn steering_admission_keeps_signed_content_queued_without_transcript_publication() {
    let db = test_db("steering-admission-only").await;
    let mut parent = parent_request(db.node_did(), "steering-session");
    parent.doc_id = insert_raw_queue_request(
        &db.node,
        db.node_did(),
        &parent.request_id,
        &parent.session_id,
        &RequestInput::default(),
    )
    .await;
    let content = "preserve this exact queued steering input: <>&\nsecond line";
    let enqueued = enqueue_steering_request(
        &db.node,
        &parent,
        content,
        RequestInput {
            queue: Some(RequestQueue {
                delivery: Default::default(),
                position: None,
                source: QueueSource::Steering,
                policy: QueuePolicy::Append,
                key: None,
                queued_after_request_id: Some(parent.request_id.clone()),
                interrupted_request_id: Some("interrupted-request".to_string()),
                background_completion_wake_version: None,
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let request = crate::request_binding::load_agent_request_by_doc_id(&db.node, &enqueued.doc_id)
        .await
        .unwrap()
        .expect("queued steering request");
    assert_eq!(request.content, content);
    assert_eq!(request.request_id, enqueued.request_id);
    let queue = request.input.queue.expect("steering queue input");
    assert_eq!(queue.source, QueueSource::Steering);
    assert_eq!(queue.policy, QueuePolicy::Append);
    assert_eq!(
        queue.queued_after_request_id.as_deref(),
        Some("parent-request")
    );
    assert_eq!(
        queue.interrupted_request_id.as_deref(),
        Some("interrupted-request")
    );

    let messages = db.node.execute("{ AgentMessage { _docID } }").await;
    assert!(!messages.has_errors(), "{:?}", messages.errors);
    assert_eq!(
        messages.data.unwrap()["AgentMessage"],
        serde_json::json!([])
    );
}

#[tokio::test]
async fn generated_pending_steering_terminals_retain_signed_admission_without_output() {
    let cases = crate::lean_vocab_test::lean_queued_steering_trace_cases();
    let mut covered = 0;
    for case in cases {
        let terminal = match case.actions.as_slice() {
            [Action::Enqueue, Action::InterruptBeforeClaim] => Action::InterruptBeforeClaim,
            [Action::Enqueue, Action::AdmissionReject] => Action::AdmissionReject,
            _ => continue,
        };
        covered += 1;
        assert_eq!(case.entry.request_id, case.request_id, "{}", case.name);
        assert_eq!(case.entry.source, "steering", "{}", case.name);
        assert_eq!(case.entry.policy, "append", "{}", case.name);
        assert!(case.entry.queue_key.is_none(), "{}", case.name);
        assert!(case.entry.queued_after.is_some(), "{}", case.name);

        let db = test_db("generated-steering-terminal").await;
        let mut parent = parent_request(db.node_did(), "generated-steering-session");
        parent.doc_id = insert_raw_queue_request(
            &db.node,
            db.node_did(),
            &parent.request_id,
            &parent.session_id,
            &RequestInput::default(),
        )
        .await;
        let content = format!("steering input token {}", case.content_token);
        let enqueued = enqueue_steering_request(
            &db.node,
            &parent,
            &content,
            RequestInput {
                queue: Some(RequestQueue {
                    delivery: Default::default(),
                    position: None,
                    source: QueueSource::Steering,
                    policy: QueuePolicy::Append,
                    key: None,
                    queued_after_request_id: Some(parent.request_id.clone()),
                    interrupted_request_id: None,
                    background_completion_wake_version: None,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let doc_id = crate::graphql::escape_graphql_string(&enqueued.doc_id);
        let query = format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ _docID lifecycle_state content input admission_signature }} }}"#
        );
        let before = db.node.execute(&query).await;
        assert!(!before.has_errors(), "{}: {:?}", case.name, before.errors);
        let before = before.data.unwrap()["AgentRequest"][0].clone();
        assert_eq!(before["content"], content, "{}", case.name);
        assert!(
            before["admission_signature"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "{}",
            case.name
        );
        let request =
            crate::request_binding::load_agent_request_by_doc_id(&db.node, &enqueued.doc_id)
                .await
                .unwrap()
                .unwrap();
        let mut lifecycle = RequestLifecycle::new_with_node_did(
            db.node.clone(),
            TEST_AGENT_ID,
            db.node_did(),
            request,
            60,
        );
        match terminal {
            Action::InterruptBeforeClaim => {
                assert!(case.interrupt_at.is_some(), "{}", case.name);
                crate::interrupt::interrupt_request_by_doc_id(
                    &db.node,
                    &enqueued.doc_id,
                    db.node_did(),
                    Some(db.node_did()),
                )
                .await
                .unwrap();
                assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Interrupted);
            }
            Action::AdmissionReject => {
                lifecycle
                    .reject_admission("generated admission rejection")
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let after = db.node.execute(&query).await;
        assert!(!after.has_errors(), "{}: {:?}", case.name, after.errors);
        let after = &after.data.unwrap()["AgentRequest"][0];
        assert_eq!(
            after["lifecycle_state"], case.lifecycle_state,
            "{}",
            case.name
        );
        assert_eq!(after["content"], before["content"], "{}", case.name);
        assert_eq!(after["input"], before["input"], "{}", case.name);
        assert_eq!(
            after["admission_signature"], before["admission_signature"],
            "{}",
            case.name
        );
        let output = db.node.execute(&format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{doc_id}" }} }}) {{ _docID }} AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{doc_id}" }} }}) {{ _docID }} }}"#
        )).await;
        assert!(!output.has_errors(), "{}: {:?}", case.name, output.errors);
        let output = output.data.unwrap();
        assert_eq!(
            output["AgentMessage"].as_array().unwrap().len() as u64,
            case.canonical_authored_count,
            "{}",
            case.name
        );
        assert!(
            output["AgentOutputSegment"].as_array().unwrap().is_empty(),
            "{}",
            case.name
        );
    }
    assert_eq!(
        covered, 2,
        "only the two modeled pending terminal paths are bound here"
    );
}

#[tokio::test]
async fn generated_owned_prepublication_terminals_retain_signed_admission_without_output() {
    let cases = crate::lean_vocab_test::lean_queued_steering_trace_cases();
    let mut covered = 0;
    for case in cases {
        let outcome = match case.actions.as_slice() {
            [Action::Enqueue, Action::ClaimAndBegin, Action::PrepareFails, Action::Fail, Action::Capture, Action::Send] => {
                crate::lifecycle::RequestTerminalOutcome::Failed
            }
            [Action::Enqueue, Action::ClaimAndBegin, Action::PrepareFails, Action::LatchInterrupt, Action::InterruptProcessing, Action::Capture, Action::Send] => {
                crate::lifecycle::RequestTerminalOutcome::Interrupted
            }
            _ => continue,
        };
        covered += 1;
        assert!(case.accepted_input, "{}", case.name);
        assert!(!case.provider_send_permitted, "{}", case.name);
        assert_eq!(case.canonical_authored_count, 0, "{}", case.name);
        assert_eq!(case.queue_active, None, "{}", case.name);

        let db = test_db("generated-owned-steering-terminal").await;
        crate::session::ensure_session_with_agent_id_and_requester_did(
            &db.node,
            "generated-owned-steering-session",
            db.node_did(),
            TEST_AGENT_ID,
            Some(db.node_did()),
        )
        .await
        .unwrap();
        let mut parent_create = gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "parent-request",
            db.node_did(),
            db.node_did(),
            TEST_AGENT_ID,
            "generated-owned-steering-session",
            "parent input",
            "interactive",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                db.node_did(),
            ),
        );
        crate::sign_agent_request_create(db.identity.as_ref(), &mut parent_create)
            .await
            .unwrap();
        let created_parent = db
            .node
            .execute(&parent_create.graphql_mutation().unwrap())
            .await;
        assert!(
            !created_parent.has_errors(),
            "{}: {:?}",
            case.name,
            created_parent.errors
        );
        let parent = crate::request_binding::load_agent_request(&db.node, "parent-request")
            .await
            .unwrap()
            .expect("signed parent fixture");
        let content = format!("steering input token {}", case.content_token);
        let enqueued = enqueue_steering_request(
            &db.node,
            &parent,
            &content,
            RequestInput {
                queue: Some(RequestQueue {
                    delivery: Default::default(),
                    position: None,
                    source: QueueSource::Steering,
                    policy: QueuePolicy::Append,
                    key: None,
                    queued_after_request_id: Some(parent.request_id.clone()),
                    interrupted_request_id: None,
                    background_completion_wake_version: None,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let physical = crate::graphql::escape_graphql_string(&enqueued.doc_id);
        let query = format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{physical}" }} }}, limit: 1) {{ lifecycle_state content input admission_signature terminal_output }} AgentMessage(filter: {{ request_doc_id: {{ _eq: "{physical}" }} }}) {{ _docID }} AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{physical}" }} }}) {{ _docID }} }}"#
        );
        let before = db.node.execute(&query).await;
        assert!(!before.has_errors(), "{}: {:?}", case.name, before.errors);
        let before = before.data.unwrap();
        let original = &before["AgentRequest"][0];
        assert_eq!(original["content"], content, "{}", case.name);
        assert!(
            original["admission_signature"]
                .as_str()
                .is_some_and(|signature| !signature.is_empty()),
            "{}",
            case.name
        );

        let parent_row =
            crate::request_binding::load_agent_request_by_doc_id(&db.node, &parent.doc_id)
                .await
                .unwrap()
                .expect("parent queue fixture");
        let mut parent_lifecycle = RequestLifecycle::new_with_node_did(
            db.node.clone(),
            TEST_AGENT_ID,
            db.node_did(),
            parent_row,
            60,
        );
        parent_lifecycle
            .reject_admission("finished parent fixture")
            .await
            .unwrap();

        let request =
            crate::request_binding::load_agent_request_by_doc_id(&db.node, &enqueued.doc_id)
                .await
                .unwrap()
                .expect("signed queued steering request");
        let mut lifecycle = RequestLifecycle::new_with_node_did(
            db.node.clone(),
            TEST_AGENT_ID,
            db.node_did(),
            request,
            60,
        );
        assert_eq!(
            lifecycle.claim().await.unwrap(),
            ClaimOutcome::Claimed,
            "{}",
            case.name
        );
        let writer = DefraStreamWriter::new(db.node.clone(), db.node_did(), Duration::ZERO);
        lifecycle.begin_owned_execution(&writer).await.unwrap();

        if outcome == crate::lifecycle::RequestTerminalOutcome::Interrupted {
            assert!(case.interrupt_at.is_none(), "{}", case.name);
            crate::interrupt::interrupt_request_by_doc_id(
                &db.node,
                &enqueued.doc_id,
                db.node_did(),
                Some(db.node_did()),
            )
            .await
            .unwrap();
        }
        assert_eq!(
            lifecycle
                .terminalize_owned(
                    outcome,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    Some("injected before authored publication")
                )
                .await
                .unwrap(),
            crate::lifecycle::TerminalizeResult::Won,
            "{}",
            case.name
        );
        let after = db.node.execute(&query).await;
        assert!(!after.has_errors(), "{}: {:?}", case.name, after.errors);
        let after = after.data.unwrap();
        let terminal = &after["AgentRequest"][0];
        assert_eq!(
            terminal["lifecycle_state"], case.lifecycle_state,
            "{}",
            case.name
        );
        assert_eq!(terminal["content"], original["content"], "{}", case.name);
        assert_eq!(terminal["input"], original["input"], "{}", case.name);
        assert_eq!(
            terminal["admission_signature"], original["admission_signature"],
            "{}",
            case.name
        );
        assert_eq!(
            serde_json::from_value::<gents_protocol::output::TerminalOutput>(
                terminal["terminal_output"].clone(),
            )
            .unwrap(),
            gents_protocol::output::TerminalOutput::NoMessage,
            "{}",
            case.name
        );
        assert_eq!(
            after["AgentMessage"].as_array().unwrap().len() as u64,
            case.canonical_authored_count,
            "{}",
            case.name
        );
        assert!(
            after["AgentOutputSegment"].as_array().unwrap().is_empty(),
            "{}",
            case.name
        );
        assert!(
            crate::interrupt::active_session_request(
                &db.node,
                &parent.session_id,
                db.node_did(),
                Some(db.node_did()),
            )
            .await
            .unwrap()
            .is_none(),
            "{} left an active request",
            case.name
        );
    }
    assert_eq!(
        covered, 2,
        "the two generated owned pre-publication terminals must be bound"
    );
}
