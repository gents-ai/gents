//! Recovery conformance after real native tool and session-message
//! admissions. Faults alter only recovery observations; assistant headers and
//! tool rows come from the production stream owner.

use crate::identity::NodeIdentity;
use crate::tool_call_lifecycle::admission_fixture::{
    complete_child, published_admission, published_session_message,
    published_session_message_with_owner, PublishedAdmission, PublishedAdmissionOptions,
};
use crate::tool_call_lifecycle::{AwaitMode, ToolCallLifecycle};
use std::sync::Arc;

/// Fixture writes to a `@branchable` collection also advance its collection
/// head, so two back-to-back writes can conflict; retry those, bounded.
async fn execute_fixture_write(node: &crate::defra_node::EmbeddedNode, mutation: &str) {
    let mut backoff = std::time::Duration::from_millis(5);
    for _ in 0..8 {
        let result = node.execute(mutation).await;
        let conflicted = result.errors.iter().any(|error| {
            error
                .extensions
                .as_ref()
                .is_some_and(|extensions| extensions.code == "TXN_CONFLICT")
        });
        if !conflicted {
            assert!(!result.has_errors(), "{:?}", result.errors);
            return;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(std::time::Duration::from_millis(200));
    }
    panic!("fixture write kept conflicting: {mutation}");
}

async fn update(node: &crate::defra_node::EmbeddedNode, doc_id: &str, fields: &str) {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    execute_fixture_write(node, &format!(
        r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#
    ))
    .await;
}

async fn update_request(node: &crate::defra_node::EmbeddedNode, doc_id: &str, fields: &str) {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    execute_fixture_write(node, &format!(
        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#
    ))
    .await;
}

async fn remove_parent(node: &crate::defra_node::EmbeddedNode, doc_id: &str) {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    let result = node
        .execute(&format!(
            r#"mutation {{ delete_AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(!result.has_errors(), "{:?}", result.errors);
}

async fn completion_obligations(
    node: &crate::defra_node::EmbeddedNode,
    session_id: &str,
    node_did: &str,
) -> (Vec<String>, Vec<serde_json::Value>) {
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ _docID }} AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }}, execution_origin: {{ _eq: "scheduled" }} }}) {{ input }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let data = response.data.unwrap();
    let mut notifications = Vec::new();
    for row in data["AgentMessage"].as_array().unwrap() {
        let (_, message) = crate::session::load_canonical_message_from_node(
            node,
            row["_docID"].as_str().unwrap(),
            node_did,
            Some(node_did),
        )
        .await
        .expect("reconstruct canonical recovery notification");
        if let crate::llm::message::Message::User { content } = message {
            for item in content {
                if let crate::llm::message::UserContent::Text(text) = item {
                    if text.text.contains("<tool-completion") {
                        notifications.push(text.text);
                    }
                }
            }
        }
    }
    let wakes = data["AgentRequest"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["input"].clone())
        .collect();
    (notifications, wakes)
}

/// A missing parent replica is never evidence for terminalizing a row: native
/// and session-message rows alike stay running until the parent is observed.
#[tokio::test]
async fn generated_native_missing_parent_restart_cases_defer() {
    let cases = crate::lean_vocab_test::lean_restart_disposition_cases()
        .iter()
        .filter(|case| case.parent_observation == "missing")
        .collect::<Vec<_>>();
    assert!(cases.iter().any(|case| case.session_message));
    assert!(cases.iter().any(|case| !case.session_message));
    for case in cases {
        let name = case.name.as_str();
        assert_eq!(case.disposition, "leave_running", "{name}");
        assert_eq!(case.await_mode, "background", "{name}");
        let admission = published_admission(PublishedAdmissionOptions {
            name: format!("restart-closeout-{name}"),
            real_identity: true,
            await_mode: AwaitMode::Background,
            tool_name: case
                .session_message
                .then(|| crate::toolset::AGENT_NEW_TOOL_NAME.to_owned()),
            ..Default::default()
        })
        .await
        .expect("publish accepted restart call");
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        remove_parent(&admission.node, admission.tool.request_doc_id().unwrap()).await;
        if case.deadline_expired {
            update(
                &admission.node,
                &tool_doc_id,
                r#"deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
        let report = ToolCallLifecycle::recover_all(&admission.node, &admission.node_did)
            .await
            .unwrap();
        assert_eq!(report.tool_calls_recovered, 0, "{name}");
        let escaped = crate::graphql::escape_graphql_string(&tool_doc_id);
        let response = admission.node.execute(&format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ lifecycle_state }} }}"#)).await;
        assert!(!response.has_errors(), "{name}: {:?}", response.errors);
        assert_eq!(
            response.data.unwrap()["AgentToolCall"][0]["lifecycle_state"],
            "running",
            "{name}"
        );
        admission.node.shutdown().await;
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
    }
}

async fn settle(admission: &PublishedAdmission) -> usize {
    crate::background_completion::settle_running_session_message_rows(
        &admission.node,
        &admission.node_did,
    )
    .await
    .unwrap()
}

fn assert_notification_reason(
    notification: &str,
    case: &crate::lean_vocab_test::LeanRecoverySweepCase,
) {
    match case.notification_reason.as_deref() {
        Some(reason) => assert!(
            notification.contains(&format!("<reason>{reason}</reason>")),
            "{}: {notification}",
            case.name
        ),
        None => assert!(!notification.contains("<reason>"), "{}", case.name),
    }
}

/// An `agent_new`/`agent_message` row ends only on the terminal of the
/// request it caused. The Lean row carries the observed
/// cause; the fixture builds exactly that premise on an accepted call whose
/// caused request was materialized by the session-message owner.
#[tokio::test]
async fn generated_session_message_recovery_cases_use_accepted_call() {
    let cases = crate::lean_vocab_test::lean_recovery_sweep_cases()
        .iter()
        .filter(|case| case.sweep_id == "tool_call_lifecycle_recover_session_message_rows")
        .collect::<Vec<_>>();
    assert!(
        !cases.is_empty(),
        "Lean emitted no session-message recovery rows"
    );
    for case in cases {
        let name = case.name.as_str();
        assert_eq!(case.pre_state, "running", "{name}");
        let cause = case
            .recovery_cause
            .as_deref()
            .unwrap_or_else(|| panic!("{name}: session-message row carries its observed cause"));
        if cause == "causedRequestUnbound" {
            // A running row that names no caused request: the state a crash
            // left before start, receipt and request shared a transaction.
            let admission = published_admission(PublishedAdmissionOptions {
                name: format!("recovery-closeout-{name}"),
                real_identity: true,
                await_mode: AwaitMode::Background,
                tool_name: Some(crate::toolset::AGENT_NEW_TOOL_NAME.to_owned()),
                start_running: true,
                ..Default::default()
            })
            .await
            .expect("publish a running session-message row without a receipt");
            let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
            let session_id = admission.tool.session_id().to_owned();
            let settled = settle(&admission).await;
            assert_eq!(settled, case.measure_before - case.measure_after, "{name}");
            let escaped = crate::graphql::escape_graphql_string(&tool_doc_id);
            let response = admission
                .node
                .execute(&format!(
                    r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ lifecycle_state tool_failure_class }} }}"#
                ))
                .await;
            assert!(!response.has_errors(), "{name}: {:?}", response.errors);
            let row = &response.data.unwrap()["AgentToolCall"][0];
            assert_eq!(
                row["lifecycle_state"],
                case.terminal_state.as_str(),
                "{name}"
            );
            assert_eq!(row["tool_failure_class"], "external", "{name}");
            let (notifications, _) =
                completion_obligations(&admission.node, &session_id, &admission.node_did).await;
            assert_eq!(
                notifications.len(),
                1,
                "{name}: fails closed with a notification"
            );
            assert_notification_reason(&notifications[0], case);
            assert_eq!(settle(&admission).await, 0, "{name}: fails closed once");
            admission.node.shutdown().await;
            std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
            continue;
        }
        let message = published_session_message(PublishedAdmissionOptions {
            name: format!("recovery-closeout-{name}"),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .expect("publish accepted session message and materialize its request");
        let admission = &message.admission;
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        let caused_state = match cause {
            "requestCompleted" => {
                complete_child(
                    &admission.node,
                    &message.caused_request_id,
                    &admission.node_did,
                    "done",
                )
                .await;
                None
            }
            "requestFailed" => Some("failed"),
            "requestDead" => Some("dead"),
            "requestInterrupted" => Some("interrupted"),
            "requestSuperseded" => Some("superseded"),
            other => panic!("{name}: unknown session-message recovery cause {other}"),
        };
        if let Some(state) = caused_state {
            update_request(
                &admission.node,
                &message.caused_request_doc_id,
                &format!(r#"lifecycle_state: "{state}""#),
            )
            .await;
        }
        let settled = settle(admission).await;
        assert_eq!(settled, case.measure_before - case.measure_after, "{name}");
        let escaped = crate::graphql::escape_graphql_string(&tool_doc_id);
        let response = admission
            .node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ status lifecycle_state cancel_cause tool_failure_class }} }}"#
            ))
            .await;
        assert!(!response.has_errors(), "{name}: {:?}", response.errors);
        let row = &response.data.unwrap()["AgentToolCall"][0];
        assert_eq!(
            row["lifecycle_state"],
            case.terminal_state.as_str(),
            "{name}"
        );
        assert_eq!(row["status"], "completed", "{name}");
        if case.terminal_state == "cancelled" {
            assert_eq!(row["cancel_cause"], "interrupted", "{name}");
        }
        let session_id = admission.tool.session_id().to_owned();
        let (notifications, _) =
            completion_obligations(&admission.node, &session_id, &admission.node_did).await;
        assert_eq!(notifications.len(), 1, "{name}");
        assert_notification_reason(&notifications[0], case);
        assert_eq!(settle(admission).await, 0, "{name}");
        message.admission.node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).expect("remove exact recovery fixture");
    }
}

/// A session-message row has no deadline: a restart and every settlement sweep
/// leave it running past its stored `deadline_at`, and when the caused request
/// ends later its result reaches the calling session exactly once.
#[tokio::test]
async fn caused_result_is_delivered_after_the_row_outlives_its_stored_deadline() {
    let message = published_session_message(PublishedAdmissionOptions {
        name: "session-message-outlives-deadline".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .expect("publish accepted session message and materialize its request");
    let admission = &message.admission;
    let node = &admission.node;
    let did = admission.node_did.clone();
    let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
    let session_id = admission.tool.session_id().to_owned();
    update(node, &tool_doc_id, r#"deadline_at: "2020-01-01T00:00:00Z""#).await;

    let restart = ToolCallLifecycle::recover_all(node, &did).await.unwrap();
    assert_eq!(restart.tool_calls_recovered, 0);
    assert_eq!(
        crate::background_completion::settle_running_session_message_rows(node, &did)
            .await
            .unwrap(),
        0
    );
    let row = |node: Arc<crate::defra_node::EmbeddedNode>| {
        let tool_doc_id = tool_doc_id.clone();
        async move {
            let response = node
                .execute(&format!(
                    r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ lifecycle_state cancel_cause }} }}"#,
                    crate::graphql::escape_graphql_string(&tool_doc_id)
                ))
                .await;
            assert!(!response.has_errors(), "{:?}", response.errors);
            response.data.unwrap()["AgentToolCall"][0].clone()
        }
    };
    assert_eq!(row(node.clone()).await["lifecycle_state"], "running");
    let (notifications, _) = completion_obligations(node, &session_id, &did).await;
    assert!(notifications.is_empty(), "{notifications:?}");

    complete_child(node, &message.caused_request_id, &did, "late caused result").await;
    assert_eq!(
        crate::background_completion::settle_running_session_message_rows(node, &did)
            .await
            .unwrap(),
        1
    );
    let settled = row(node.clone()).await;
    assert_eq!(settled["lifecycle_state"], "completed");
    assert!(settled["cancel_cause"].is_null());
    let (notifications, wakes) = completion_obligations(node, &session_id, &did).await;
    assert_eq!(notifications.len(), 1, "{notifications:?}");
    assert!(notifications[0].contains("late caused result"));
    assert_eq!(wakes.len(), 1);

    assert_eq!(
        ToolCallLifecycle::recover_all(node, &did)
            .await
            .unwrap()
            .tool_calls_recovered,
        0
    );
    assert_eq!(
        crate::background_completion::settle_running_session_message_rows(node, &did)
            .await
            .unwrap(),
        0
    );
    let (notifications, _) = completion_obligations(node, &session_id, &did).await;
    assert_eq!(notifications.len(), 1, "delivered exactly once");
    message.admission.node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).expect("remove exact recovery fixture");
}

/// A running session-message row with the caused request a kill observes
/// (Lean `Recovery.KillObservation`).
async fn kill_fixture(observation: &str) -> (PublishedAdmission, Option<String>) {
    let options = |start_running| PublishedAdmissionOptions {
        name: format!("kill-session-message-{observation}"),
        real_identity: true,
        await_mode: AwaitMode::Background,
        tool_name: Some(crate::toolset::AGENT_NEW_TOOL_NAME.to_owned()),
        start_running,
        ..Default::default()
    };
    match observation {
        "unresolved" => (published_admission(options(true)).await.unwrap(), None),
        "causedLiveRemote" => {
            let (mut admission, request) =
                crate::tool_call_lifecycle::admission_fixture::published_admission_with_owner(
                    options(false),
                )
                .await
                .unwrap();
            crate::tool_call_lifecycle::admission_fixture::materialize_session_message(
                &admission.node,
                &request,
                &mut admission.tool,
                "did:key:z6MkpeerRemoteSessionMessageTarget",
                "work on the peer",
            )
            .await
            .unwrap();
            (admission, None)
        }
        "causedLiveLocal" | "causedTerminal" => {
            let message = published_session_message(options(false)).await.unwrap();
            if observation == "causedTerminal" {
                complete_child(
                    &message.admission.node,
                    &message.caused_request_id,
                    &message.admission.node_did,
                    "finished before the kill",
                )
                .await;
            }
            (message.admission, Some(message.caused_request_id))
        }
        other => panic!("unknown Lean kill observation {other}"),
    }
}

/// Lean `Recovery.killAction` over every kill observation: a kill settles from
/// an observed terminal, waits only on a local caused request it interrupted,
/// and otherwise cancels the row now with a cancelled notification.
#[tokio::test]
async fn generated_kill_cases_drive_the_session_message_kill() {
    use crate::session_message::KillOutcome;
    use crate::tool_call_lifecycle::ToolCallState;
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().session_message_kill_cases;
    assert_eq!(cases.len(), 4);
    for case in cases {
        let (admission, caused_request_id) = kill_fixture(&case.observation).await;
        let node = admission.node.clone();
        let did = admission.node_did.clone();
        let session_id = admission.tool.session_id().to_owned();
        let mut tool = admission.tool;
        let outcome = crate::session_message::kill(&node, &mut tool)
            .await
            .unwrap();
        let (notifications, _) = completion_obligations(&node, &session_id, &did).await;
        match case.action.as_str() {
            "settle" => {
                assert_eq!(outcome, KillOutcome::Settled, "{case:?}");
                assert_eq!(tool.state(), ToolCallState::Completed, "{case:?}");
                assert_eq!(notifications.len(), 1, "{notifications:?}");
                assert!(notifications[0].contains("finished before the kill"));
            }
            "interruptCaused" => {
                assert_eq!(
                    outcome,
                    KillOutcome::Interrupting {
                        request_id: caused_request_id.unwrap()
                    },
                    "{case:?}"
                );
                assert!(tool.is_running(), "{case:?}");
                assert!(notifications.is_empty(), "{notifications:?}");
            }
            "interruptAndCancelRow" | "cancelRow" => {
                assert_eq!(outcome, KillOutcome::Cancelled, "{case:?}");
                assert_eq!(tool.state(), ToolCallState::Cancelled, "{case:?}");
                assert_eq!(notifications.len(), 1, "{notifications:?}");
                assert!(notifications[0].contains("explicit_cancel"));
            }
            other => panic!("unknown Lean kill action {other}"),
        }
        node.shutdown().await;
        std::fs::remove_dir_all(admission.path).unwrap();
    }
}

/// Lean `CausalHop.return_keeps_caller_hop` through real settlement and
/// admission (#2065): a callee that ran at the bound returns its result to
/// the caller at the caller's own hop, so the wake is admitted, and a native
/// completion of the same turn rides that wake.
#[tokio::test]
async fn a_completion_at_the_bound_returns_at_the_callers_hop() {
    use crate::identity::NodeIdentity;
    let message = published_session_message(PublishedAdmissionOptions {
        name: "session-message-wake-at-bound".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .expect("publish accepted session message and materialize its request");
    let node = message.admission.node.clone();
    let did = message.admission.node_did.clone();
    let session_id = message.admission.tool.session_id().to_owned();
    let caller_doc_id = message.admission.tool.request_doc_id().unwrap().to_owned();
    // The caused request runs at hop 1, the bound; its result returns at hop 0.
    crate::document_config::ensure_node(&node, &did)
        .await
        .unwrap();
    let response = node
        .execute(&format!(
            r#"mutation {{ update_Node(filter: {{ node_did: {{ _eq: "{}" }} }}, input: {{ max_request_hop: 1 }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(&did)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let identity: Arc<dyn NodeIdentity> = Arc::new(
        crate::KeyIdentity::load_or_create(message.admission.path.join("test-agent.key"), None)
            .unwrap(),
    );
    let verifier = crate::request_admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        identity,
        crate::agent::p2p_reconcile::enrollment_authority_channel().1,
    );
    let pending_wakes = |node: Arc<crate::defra_node::EmbeddedNode>, session: String| async move {
        let response = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }}, execution_origin: {{ _eq: "scheduled" }}, lifecycle_state: {{ _eq: "pending" }} }}) {{ _docID }} }}"#,
                crate::graphql::escape_graphql_string(&session)
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        response.data.unwrap()["AgentRequest"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["_docID"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let admit = |doc_id: String| {
        let node = node.clone();
        let verifier = &verifier;
        async move {
            let request = crate::request_binding::load_agent_request_by_doc_id(&node, &doc_id)
                .await
                .unwrap()
                .unwrap();
            let behavior = request.agent_id.clone();
            let admitted = crate::agent::daemon::verify_request_at_claim_boundary(
                verifier,
                node.clone(),
                &behavior,
                request,
            )
            .await;
            let row = node
                .execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ request_hop lifecycle_state failure_reason }} }}"#,
                    crate::graphql::escape_graphql_string(&doc_id)
                ))
                .await
                .data
                .unwrap()["AgentRequest"][0]
                .clone();
            (admitted.is_some(), row)
        }
    };

    complete_child(
        &node,
        &message.caused_request_id,
        &did,
        "result at the bound",
    )
    .await;
    assert_eq!(
        crate::background_completion::settle_running_session_message_rows(&node, &did)
            .await
            .unwrap(),
        1
    );
    let (notifications, _) = completion_obligations(&node, &session_id, &did).await;
    assert_eq!(notifications.len(), 1, "{notifications:?}");
    assert!(notifications[0].contains("result at the bound"));
    let wakes = pending_wakes(node.clone(), session_id.clone()).await;
    assert_eq!(wakes.len(), 1);

    // A native process of the same turn completes afterwards and joins it.
    let caller = crate::request_binding::load_agent_request_by_doc_id(&node, &caller_doc_id)
        .await
        .unwrap()
        .unwrap();
    let native = crate::lifecycle::queue::persist_background_completion_with_message_waking(
        &node,
        &caller,
        "process done",
        "background-completion-notification:process:tool",
        crate::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT,
        crate::lifecycle::queue::RequestQueue {
            source: crate::lifecycle::queue::QueueSource::BackgroundCompletion,
            policy: crate::lifecycle::queue::QueuePolicy::Coalesce,
            key: Some(format!("background_completion:{session_id}")),
            queued_after_request_id: Some(caller.request_id.clone()),
            interrupted_request_id: None,
            background_completion_wake_version: None,
        },
        None,
        crate::lifecycle::RequestHopCause::Continuation,
    )
    .await
    .unwrap();
    assert!(!native.created_request);
    assert_eq!(pending_wakes(node.clone(), session_id.clone()).await, wakes);
    crate::test_support::install_test_agent(node.as_ref(), &did, &caller.agent_id).await;
    let (admitted, row) = admit(wakes[0].clone()).await;
    assert!(admitted, "{row}");
    assert_eq!(row["request_hop"], caller.request_hop);
    assert_eq!(row["lifecycle_state"], "pending");
    node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

/// #2064 through real settlement: an `agent_new` made from a paired client's
/// session returns its result into that session under the client's
/// requester, at the caller's hop; a redrive is inert, and the wake is
/// admitted, claimed and resumed in that same session.
#[tokio::test]
async fn a_paired_client_session_receives_its_agent_new_result() {
    let (message, mut calling) = published_session_message_with_owner(PublishedAdmissionOptions {
        name: "paired-client-agent-new".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        paired_client: true,
        ..Default::default()
    })
    .await
    .expect("publish a paired client's agent_new and materialize its request");
    let node = message.admission.node.clone();
    let did = message.admission.node_did.clone();
    let session_id = message.admission.tool.session_id().to_owned();
    let caller_doc_id = message.admission.tool.request_doc_id().unwrap().to_owned();
    let caller = crate::request_binding::load_agent_request_by_doc_id(&node, &caller_doc_id)
        .await
        .unwrap()
        .unwrap();
    let client = caller
        .requester_did
        .clone()
        .expect("paired client requester");
    assert_ne!(client, did);

    complete_child(&node, &message.caused_request_id, &did, "gatekeeper answer").await;
    assert_eq!(
        crate::background_completion::settle_running_session_message_rows(&node, &did)
            .await
            .unwrap(),
        1
    );
    ToolCallLifecycle::reconcile_background_completion_side_effects(&node, &did)
        .await
        .unwrap();
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }}, execution_origin: {{ _eq: "scheduled" }} }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(&session_id)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let wakes = response.data.unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(wakes.len(), 1, "{wakes:?}");
    let wake = crate::request_binding::load_agent_request_by_doc_id(
        &node,
        wakes[0]["_docID"].as_str().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(wake.requester_did.as_deref(), Some(client.as_str()));
    assert_eq!(wake.request_hop, caller.request_hop);
    // The calling turn ends; its session's queued result is next.
    calling
        .terminalize_owned(
            crate::lifecycle::RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::Message {
                message_doc_id: message
                    .admission
                    .tool
                    .accepted_header_doc_id()
                    .expect("accepted caller assistant header")
                    .to_owned(),
            },
            None,
        )
        .await
        .unwrap();

    crate::test_support::install_test_agent(node.as_ref(), &did, &caller.agent_id).await;
    let identity: Arc<dyn NodeIdentity> = Arc::new(
        crate::KeyIdentity::load_or_create(message.admission.path.join("test-agent.key"), None)
            .unwrap(),
    );
    let verifier = crate::request_admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        identity,
        crate::agent::p2p_reconcile::enrollment_authority_channel().1,
    );
    let verified = crate::agent::daemon::verify_request_at_claim_boundary(
        &verifier,
        node.clone(),
        &wake.agent_id,
        wake.clone(),
    )
    .await
    .expect("the returned result is admitted into the client's session");
    let mut lifecycle = crate::RequestLifecycle::new_with_node_did(
        node.clone(),
        &wake.agent_id,
        &did,
        verified.clone(),
        60,
    );
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        crate::lifecycle::ClaimOutcome::Claimed
    );
    crate::hook::DefraSessionHook::resume_with_identity_policy(
        node.clone(),
        &session_id,
        &did,
        verified.requester_did.as_deref(),
        crate::hook::FailurePolicy::FailClosed,
    )
    .await
    .expect("the result resumes the client's session");
    node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn generated_orphan_background_recovery_cases_use_accepted_native_call() {
    let cases = crate::lean_vocab_test::lean_recovery_sweep_cases();
    for name in [
        "orphaned_background_tool_without_execution_to_cancelled",
        "orphaned_background_tool_expired_terminal_parent_to_timed_out",
        "orphaned_background_tool_terminal_parent_to_cancelled",
        "orphaned_background_tool_interrupted_parent_to_cancelled",
        "orphaned_background_tool_unowned_process_to_failed",
        "orphaned_background_tool_exited_process_to_failed",
    ] {
        let case = cases.iter().find(|case| case.name == name).unwrap();
        assert_eq!(case.execution_registered, Some(false), "{name}");
        assert_eq!(case.owner_task_deleted, Some(false), "{name}");
        let fixture_name = format!("recovery-closeout-{name}");
        let admission = published_admission(PublishedAdmissionOptions {
            name: fixture_name.clone(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .expect("publish accepted orphan background call");
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        let parent_doc_id = admission.tool.request_doc_id().unwrap().to_owned();
        if case.parent_terminal == Some(true) {
            update_request(
                &admission.node,
                &parent_doc_id,
                r#"lifecycle_state: "completed""#,
            )
            .await;
        }
        if case.parent_interrupted == Some(true) {
            update_request(
                &admission.node,
                &parent_doc_id,
                r#"lifecycle_state: "interrupted""#,
            )
            .await;
        }
        if case.deadline_expired == Some(true) {
            update(
                &admission.node,
                &tool_doc_id,
                r#"deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
        // The restarted runtime's owner reads the record a crashed runtime left
        // for a surviving test-owned process group.
        let records = tempfile::tempdir().unwrap();
        let registry = crate::BackgroundExecutionRegistry::default()
            .with_process_records(records.path().to_path_buf());
        let process = crate::managed_exec::ownership::test_support::process_for_generated_outcome(
            &registry,
            admission.tool.tool_call_id(),
            &tool_doc_id,
            case.process_outcome.as_deref().unwrap(),
        )
        .await;
        let report = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &admission.node,
            &admission.node_did,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(report.tool_calls_terminalized, 1, "{name}");
        if let Some(process) = process {
            assert_ne!(
                process.identity.observe(),
                crate::managed_exec::ownership::ProcessObservation::Running,
                "{name}: settled row left its process running"
            );
            process.finish().await;
        }
        assert!(registry.process_record_list().is_empty(), "{name}");
        let escaped = crate::graphql::escape_graphql_string(&tool_doc_id);
        let response = admission
            .node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ status lifecycle_state cancel_cause tool_failure_class }} }}"#
            ))
            .await;
        assert!(!response.has_errors(), "{name}: {:?}", response.errors);
        let row = &response.data.unwrap()["AgentToolCall"][0];
        assert_eq!(row["status"], "completed", "{name}");
        assert_eq!(
            row["lifecycle_state"],
            case.terminal_state.as_str(),
            "{name}"
        );
        match case.recovery_cause.as_deref() {
            Some("deadlineExceeded" | "parentTerminal" | "processLost") => {
                assert_eq!(row["tool_failure_class"], "external", "{name}");
            }
            Some("TerminalizeBackgroundedAsInterrupted" | "parentInterrupted") => {
                assert_eq!(row["cancel_cause"], "interrupted", "{name}");
                assert!(row["tool_failure_class"].is_null(), "{name}");
            }
            other => panic!("{name}: unsupported Lean recovery cause {other:?}"),
        }
        let session_id = format!("session-{fixture_name}");
        let (notifications, wakes) =
            completion_obligations(&admission.node, &session_id, &admission.node_did).await;
        if let Some(reason) = case.notification_reason.as_deref() {
            assert_eq!(notifications.len(), 1, "{name}");
            assert!(
                notifications[0].contains(&format!("<reason>{reason}</reason>")),
                "{name}"
            );
            assert_eq!(wakes.len(), 1, "{name}");
        } else {
            assert!(notifications.is_empty(), "{name}");
            assert!(wakes.is_empty(), "{name}");
        }
        let second = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &admission.node,
            &admission.node_did,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(second.tool_calls_terminalized, 0, "{name}");
        admission.node.shutdown().await;
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
    }
}

/// A live worker keeps its row until the task that started its request is
/// deleted; then the cancellation is persisted and the worker's process
/// stopped through the same owner.
#[cfg(unix)]
#[tokio::test]
async fn generated_registered_background_task_deletion_cases_use_live_worker() {
    let cases = crate::lean_vocab_test::lean_recovery_sweep_cases();
    let left = cases
        .iter()
        .find(|case| case.name == "registered_background_tool_left_to_worker_deferred")
        .unwrap();
    let deleted = cases
        .iter()
        .find(|case| case.name == "registered_background_tool_task_deleted_to_cancelled")
        .unwrap();
    assert_eq!(left.execution_registered, Some(true));
    assert_eq!(left.owner_task_deleted, Some(false));
    assert_eq!(deleted.owner_task_deleted, Some(true));

    let name = "task-deleted";
    let path =
        std::env::temp_dir().join(format!("recovery-closeout-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    let identity = crate::KeyIdentity::load_or_create(path.join("agent.key"), None).unwrap();
    let node_did = identity.did().to_owned();
    let node = Arc::new(
        crate::defra_node::EmbeddedNode::builder()
            .data_path(&path)
            .with_node_identity_did(&node_did)
            .build()
            .await
            .unwrap(),
    );
    crate::schema::ensure_runtime_schemas(&node).await.unwrap();
    crate::test_support::install_test_agent(&node, &node_did, "general").await;
    let session_id = format!("session-{name}");
    let mut parent =
        crate::tool_call_lifecycle::admission_fixture::claimed_signed_request_with_trigger(
            &node,
            &format!("request-{name}"),
            &session_id,
            &identity,
            None,
            Some("trigger-1858"),
        )
        .await;
    let tool = crate::tool_call_lifecycle::admission_fixture::publish_accepted_on_claimed_request(
        node.clone(),
        &mut parent,
        &node_did,
        0,
        crate::toolset::SPAWN_PROCESS_TOOL_NAME,
        "task-native-tool",
        serde_json::json!({"tool_name": "bash", "args": {}}),
        AwaitMode::Background,
        true,
    )
    .await
    .unwrap();
    let tool_doc_id = tool.doc_id().unwrap().to_owned();
    let tool_call_id = tool.tool_call_id().to_owned();

    // The live worker: it owns a test process group until its token fires,
    // then stops it, releases its record and its execution.
    let registry = crate::BackgroundExecutionRegistry::default();
    let token = tokio_util::sync::CancellationToken::new();
    let reservation = registry.reserve(tool_call_id.clone(), token.clone());
    let (identity_tx, identity_rx) = tokio::sync::oneshot::channel();
    let worker = {
        let registry = registry.clone();
        let recorder = registry.process_recorder(&tool_call_id, &tool_doc_id);
        let tool_call_id = tool_call_id.clone();
        let tool_doc_id = tool_doc_id.clone();
        tokio::spawn(async move {
            let process = crate::managed_exec::ownership::test_support::OwnedTestProcess::spawn(
                Some(recorder),
            )
            .await;
            let _ = identity_tx.send(process.identity.clone());
            token.cancelled().await;
            process.finish().await;
            registry
                .release_process_record(&tool_call_id, &tool_doc_id)
                .await;
            drop(reservation);
        })
    };
    let process = identity_rx.await.unwrap();

    let escaped_agent = crate::graphql::escape_graphql_string(&node_did);
    for mutation in [
        format!(
            r#"mutation {{ create_Task(input: {{ task_id: "task-1858", node_did: "{escaped_agent}", agent_id: "general", prompt_template: "tick" }}) {{ _docID }} }}"#
        ),
        format!(
            r#"mutation {{ create_Trigger(input: {{ trigger_id: "trigger-1858", node_did: "{escaped_agent}", task_id: "task-1858" }}) {{ _docID }} }}"#
        ),
    ] {
        let response = node.execute(&mutation).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }
    let report =
        ToolCallLifecycle::reconcile_orphaned_background_tools(&node, &node_did, &registry)
            .await
            .unwrap();
    assert_eq!(report.tool_calls_terminalized, 0, "{}", left.name);
    assert_eq!(
        process.observe(),
        crate::managed_exec::ownership::ProcessObservation::Running,
        "{}",
        left.name
    );

    let response = node
        .execute(&format!(
            r#"mutation {{ delete_Task(filter: {{ task_id: {{ _eq: "task-1858" }}, node_did: {{ _eq: "{escaped_agent}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let report =
        ToolCallLifecycle::reconcile_orphaned_background_tools(&node, &node_did, &registry)
            .await
            .unwrap();
    assert_eq!(report.tool_calls_terminalized, 1, "{}", deleted.name);
    worker.await.unwrap();
    assert_ne!(
        process.observe(),
        crate::managed_exec::ownership::ProcessObservation::Running,
        "{}",
        deleted.name
    );
    let escaped = crate::graphql::escape_graphql_string(&tool_doc_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ lifecycle_state cancel_cause }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row = &response.data.unwrap()["AgentToolCall"][0];
    assert_eq!(row["lifecycle_state"], deleted.terminal_state.as_str());
    assert_eq!(row["cancel_cause"], "interrupted");
    let (notifications, _) = completion_obligations(&node, &session_id, &node_did).await;
    let reason = deleted.notification_reason.as_deref().unwrap();
    assert_eq!(notifications.len(), 1);
    assert!(notifications[0].contains(&format!("<reason>{reason}</reason>")));
    node.shutdown().await;
    std::fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn generated_missing_parent_deferred_cases_keep_accepted_row_running() {
    let cases = crate::lean_vocab_test::lean_recovery_sweep_cases();
    for name in ["orphaned_background_tool_expired_missing_parent_deferred"] {
        let case = cases.iter().find(|case| case.name == name).unwrap();
        assert_eq!(case.terminal_state, "running", "{name}");
        assert_eq!((case.measure_before, case.measure_after), (0, 0), "{name}");
        let admission = published_admission(PublishedAdmissionOptions {
            name: format!("recovery-closeout-{name}"),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .expect("publish accepted native background call");
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        remove_parent(&admission.node, admission.tool.request_doc_id().unwrap()).await;
        assert_eq!(case.deadline_expired, Some(true), "{name}");
        update(
            &admission.node,
            &tool_doc_id,
            r#"deadline_at: "2020-01-01T00:00:00Z""#,
        )
        .await;
        let error = match ToolCallLifecycle::load_by_doc_id(
            admission.node.clone(),
            &tool_doc_id,
            &admission.node_did,
            admission.tool.session_id(),
            admission.tool.requester_did(),
        )
        .await
        {
            Ok(_) => panic!("{name}: missing accepted parent must not authorize rehydration"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("tool owner request is missing or ambiguous"),
            "{name}: unexpected missing-owner guard: {error:#}"
        );
        let registry = crate::BackgroundExecutionRegistry::default();
        let report = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &admission.node,
            &admission.node_did,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(report.tool_calls_terminalized, 0, "{name}");
        let escaped = crate::graphql::escape_graphql_string(&tool_doc_id);
        let response = admission.node.execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ lifecycle_state }} }}"#
        )).await;
        assert!(!response.has_errors(), "{name}: {:?}", response.errors);
        assert_eq!(
            response.data.unwrap()["AgentToolCall"][0]["lifecycle_state"],
            "running",
            "{name}"
        );
        admission.node.shutdown().await;
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
    }
}
#[tokio::test]
async fn generated_background_completion_recovery_uses_accepted_native_call() {
    let cases = crate::lean_vocab_test::lean_recovery_sweep_cases();
    let case = cases
        .iter()
        .find(|case| {
            case.name == "terminal_background_tool_missing_completion_side_effects_to_converged"
        })
        .unwrap();
    let fixture_name = "recovery-closeout-background-completion";
    let mut admission = published_admission(PublishedAdmissionOptions {
        name: fixture_name.into(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .expect("publish accepted background call");
    admission
        .tool
        .fail_owned(
            "seed terminal background failure",
            crate::tool_call_lifecycle::FailureClass::External,
            None,
        )
        .await
        .unwrap();
    let report = ToolCallLifecycle::reconcile_background_completion_side_effects(
        &admission.node,
        &admission.node_did,
    )
    .await
    .unwrap();
    assert_eq!(report.side_effects_converged, 1, "{}", case.name);
    let second = ToolCallLifecycle::reconcile_background_completion_side_effects(
        &admission.node,
        &admission.node_did,
    )
    .await
    .unwrap();
    assert!(second.is_noop(), "{}", case.name);
    let session_id = format!("session-{fixture_name}");
    let (notifications, wakes) =
        completion_obligations(&admission.node, &session_id, &admission.node_did).await;
    assert_eq!(notifications.len(), 1, "{}", case.name);
    assert_eq!(wakes.len(), 1, "{}", case.name);
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
}
