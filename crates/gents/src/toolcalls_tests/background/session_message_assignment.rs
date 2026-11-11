use gents_toolcalls::background::completion::session_message::*;
use std::sync::Arc;
use gents_toolcalls::call::lifecycle::admission_fixture::PublishedSessionMessage;
use gents_protocol::background_completion::background_completion_notification_message_key;
use gents_protocol::tool_call::AwaitMode;

fn recovery_identities(
    admission: &gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmission,
) -> Vec<Arc<dyn gents_store::identity::NodeIdentity>> {
    vec![Arc::new(
        admission
            .tool
            .access()
            .unwrap()
            .managed_agent_signer()
            .unwrap(),
    )]
}

fn handoff(
    turn_completed: bool,
    assignment: AssignmentObservation,
    terminal_reason: Option<&'static str>,
) -> TurnAssignmentHandoff {
    TurnAssignmentHandoff {
        turn_completed,
        assignment,
        terminal_reason,
    }
}

#[test]
fn a_completed_turn_with_a_live_assignment_does_not_report_assignment_success() {
    for status in [
        gents_protocol::goal_status::GoalStatus::Active,
        gents_protocol::goal_status::GoalStatus::Blocked,
        gents_protocol::goal_status::GoalStatus::Paused,
        gents_protocol::goal_status::GoalStatus::UsageLimited,
        gents_protocol::goal_status::GoalStatus::BudgetLimited,
    ] {
        let handoff = handoff(true, AssignmentObservation::Assigned(status), None);
        assert_eq!(
            handoff.reason(),
            Some(assignment_status_reason(status)),
            "{status:?}: a finished turn must name its non-complete assignment"
        );
        assert_ne!(handoff.reason(), Some("assignment_complete"));
    }
}

#[test]
fn a_completed_turn_with_a_complete_assignment_is_a_clean_success() {
    let handoff = handoff(
        true,
        AssignmentObservation::Assigned(gents_protocol::goal_status::GoalStatus::Complete),
        None,
    );
    assert_eq!(handoff.reason(), None);
}

#[test]
fn a_completed_turn_without_a_goal_is_distinguished_from_assignment_success() {
    let handoff = handoff(true, AssignmentObservation::None, None);
    assert_eq!(handoff.reason(), Some("assignment_absent"));
}

#[test]
fn an_unreadable_assignment_never_reads_as_success() {
    let handoff = handoff(true, AssignmentObservation::Unknown, None);
    assert_eq!(handoff.reason(), Some("assignment_unknown"));
}

#[test]
fn a_failed_turn_keeps_its_terminal_reason_not_an_assignment_reason() {
    for (terminal, reason) in [
        (
            gents_toolcalls::call::lifecycle::CausedRequestTerminal::Failed {
                reason: "backend failed".into(),
            },
            Some("request_failed"),
        ),
        (
            gents_toolcalls::call::lifecycle::CausedRequestTerminal::Dead,
            Some("request_dead"),
        ),
        (
            gents_toolcalls::call::lifecycle::CausedRequestTerminal::Interrupted,
            Some("request_interrupted"),
        ),
        (
            gents_toolcalls::call::lifecycle::CausedRequestTerminal::Superseded,
            Some("request_superseded"),
        ),
    ] {
        // The assignment read is skipped for failed turns even when the
        // session carries a live Goal: the terminal reason decides.
        let handoff = handoff(
            false,
            AssignmentObservation::Assigned(gents_protocol::goal_status::GoalStatus::Active),
            reason,
        );
        assert_eq!(handoff.reason(), reason, "{terminal:?}");
        assert_eq!(reason, terminal.completion_reason());
    }
}

#[test]
fn the_live_reason_vocabulary_is_distinct_from_the_terminal_vocabulary() {
    let reasons: std::collections::BTreeSet<&str> = [
        "assignment_absent",
        "assignment_active",
        "assignment_paused",
        "assignment_blocked",
        "assignment_usage_limited",
        "assignment_budget_limited",
        "assignment_unknown",
    ]
    .into_iter()
    .collect();
    assert_eq!(reasons.len(), 7);
    for terminal in [
        "request_failed",
        "request_dead",
        "request_interrupted",
        "request_superseded",
    ] {
        assert!(!reasons.contains(terminal));
    }
}

/// A completed turn must not read as assignment evidence: only a
/// complete assignment is the reasonless success form.
#[tokio::test]
async fn folded_session_message_waits_for_exact_head_and_replays_once() {
    let PublishedSessionMessage {
        admission,
        caused_request_doc_id,
        ..
    } = gents_toolcalls::call::lifecycle::admission_fixture::published_session_message(
        gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmissionOptions {
            name: "folded-session-message".into(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let node = &admission.node;
    let did = &admission.node_did;
    let session =
        caused_session_id(&admission.tool.access().unwrap(), &caused_request_doc_id).await;
    // The caused request and every head below are written and read as the
    // Agent that owns them; the node has no fallback read or write.
    let owner = admission.tool.access().unwrap();
    let caused_row = owner
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ requester_did agent_did }} }}"#,
            escape_graphql_string(&caused_request_doc_id)
        ))
        .await
        .unwrap()["data"]["AgentRequest"][0]
        .clone();
    let caused_requester = caused_row["requester_did"].as_str().unwrap().to_owned();
    let caused_agent = caused_row["agent_did"].as_str().unwrap().to_owned();
    let head_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let create = format!(
        r#"mutation {{ create_AgentRequest(input: {{
        request_id: "{}", purpose: "normal", node_did: "{}", requester_did: "{}",
        agent_did: "{}", session_id: "{}", content: "head", lifecycle_state: "pending",
        execution_origin: "interactive", created_at: "{}", retry_count: 0, max_retries: 3, request_hop: 0
    }}) {{ _docID }} }}"#,
        escape_graphql_string(&head_id),
        escape_graphql_string(did),
        escape_graphql_string(&caused_requester),
        escape_graphql_string(&caused_agent),
        escape_graphql_string(&session),
        escape_graphql_string(&now)
    );
    // The head is verified and claimed below, so its Agent signs it as a
    // local-self request; the foreign heads only name fold targets.
    assert_eq!(
        caused_requester, caused_agent,
        "a caused request is local-self"
    );
    let signer = owner.managed_agent_signer().unwrap();
    let mut head = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        &head_id,
        did,
        &caused_requester,
        &caused_agent,
        &session,
        "head",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
            &caused_agent,
        ),
    );
    gents_session::request::admission::sign_agent_request_create(&signer, &mut head)
        .await
        .unwrap();
    let created = owner
        .write("test.folded_head", &head.graphql_mutation().unwrap())
        .await
        .unwrap();
    let head_doc = gents_store::graphql::created_doc_id(&created, "AgentRequest").unwrap();
    let foreign_create = create
        .replace(
            &format!("request_id: \"{}\"", head_id),
            "request_id: \"foreign-head\"",
        )
        .replace(
            &format!(
                "requester_did: \"{}\"",
                escape_graphql_string(&caused_requester)
            ),
            "requester_did: \"did:test:foreign\"",
        );
    let foreign = owner
        .write("test.foreign_head", &foreign_create)
        .await
        .unwrap();
    let foreign_doc = gents_store::graphql::created_doc_id(&foreign, "AgentRequest").unwrap();
    let foreign_fold = format!(
        r#"mutation {{ update_AgentRequest(docID: "{}", input: {{
        lifecycle_state: "superseded", superseded_by_request: "foreign-head", superseded_by_request_doc_id: "{}",
        failure_reason: "{}"
    }}) {{ _docID }} }}"#,
        escape_graphql_string(&caused_request_doc_id),
        escape_graphql_string(&foreign_doc),
        escape_graphql_string(gents_session::request::lifecycle::FOLDED_REASON)
    );
    owner
        .write("test.foreign_fold", &foreign_fold)
        .await
        .unwrap();
    let binding_cases = &gents_lean_vocab::lean_contract_snapshot().agent_folded_binding_cases;
    let foreign_model = binding_cases
        .iter()
        .find(|case| {
            case.caused_requester != case.head_requester && case.named_head == case.actual_head
        })
        .unwrap();
    let foreign_accepted = gents_toolcalls::background::status::load_session_message_terminal(
        &owner,
        &caused_request_doc_id,
    )
    .await
    .is_ok();
    assert_eq!(foreign_accepted, foreign_model.expected);
    assert_eq!(
        settle_running_session_message_rows_with_access(
            node,
            &admission.tool.access().unwrap(),
            did
        )
        .await
        .unwrap(),
        0
    );
    assert!(session_message_notifications(
        &admission.tool.access().unwrap(),
        did,
        admission.tool.session_id()
    )
    .await
    .is_empty());
    let foreign_agent_create = create
        .replace(
            &format!("request_id: \"{}\"", head_id),
            "request_id: \"foreign-agent-head\"",
        )
        .replace(
            &format!("agent_did: \"{}\"", escape_graphql_string(&caused_agent)),
            "agent_did: \"foreign-agent\"",
        );
    let foreign_agent = owner
        .write("test.foreign_agent_head", &foreign_agent_create)
        .await
        .unwrap();
    let foreign_agent_doc =
        gents_store::graphql::created_doc_id(&foreign_agent, "AgentRequest").unwrap();
    let foreign_agent_fold = format!(
        r#"mutation {{ update_AgentRequest(docID: "{}", input: {{
        lifecycle_state: "superseded", superseded_by_request: "foreign-agent-head", superseded_by_request_doc_id: "{}",
        failure_reason: "{}"
    }}) {{ _docID }} }}"#,
        escape_graphql_string(&caused_request_doc_id),
        escape_graphql_string(&foreign_agent_doc),
        escape_graphql_string(gents_session::request::lifecycle::FOLDED_REASON)
    );
    owner
        .write("test.foreign_agent_fold", &foreign_agent_fold)
        .await
        .unwrap();
    let foreign_agent_model = binding_cases
        .iter()
        .find(|case| {
            case.caused_requester == case.head_requester
                && case.caused_agent != case.head_agent
                && case.named_head == case.actual_head
        })
        .unwrap();
    let foreign_agent_accepted = gents_toolcalls::background::status::load_session_message_terminal(
        &owner,
        &caused_request_doc_id,
    )
    .await
    .is_ok();
    assert_eq!(foreign_agent_accepted, foreign_agent_model.expected);
    assert_eq!(
        settle_running_session_message_rows_with_access(
            node,
            &admission.tool.access().unwrap(),
            did
        )
        .await
        .unwrap(),
        0
    );
    let mismatched_fold = format!(
        r#"mutation {{ update_AgentRequest(docID: "{}", input: {{
        lifecycle_state: "superseded", superseded_by_request: "wrong-head", superseded_by_request_doc_id: "{}",
        failure_reason: "{}"
    }}) {{ _docID }} }}"#,
        escape_graphql_string(&caused_request_doc_id),
        escape_graphql_string(&head_doc),
        escape_graphql_string(gents_session::request::lifecycle::FOLDED_REASON)
    );
    owner
        .write("test.mismatched_fold", &mismatched_fold)
        .await
        .unwrap();
    let mismatch_model = binding_cases
        .iter()
        .find(|case| {
            case.caused_requester == case.head_requester && case.named_head != case.actual_head
        })
        .unwrap();
    let mismatch_accepted = gents_toolcalls::background::status::load_session_message_terminal(
        &owner,
        &caused_request_doc_id,
    )
    .await
    .is_ok();
    assert_eq!(mismatch_accepted, mismatch_model.expected);
    let fold = format!(
        r#"mutation {{ update_AgentRequest(docID: "{}", input: {{
        lifecycle_state: "superseded", superseded_by_request: "{}", superseded_by_request_doc_id: "{}",
        failure_reason: "{}"
    }}) {{ _docID }} }}"#,
        escape_graphql_string(&caused_request_doc_id),
        escape_graphql_string(&head_id),
        escape_graphql_string(&head_doc),
        escape_graphql_string(gents_session::request::lifecycle::FOLDED_REASON)
    );
    owner.write("test.folded_cause", &fold).await.unwrap();
    let matching_model = binding_cases
        .iter()
        .find(|case| {
            case.caused_requester == case.head_requester
                && case.caused_agent == case.head_agent
                && case.named_head == case.actual_head
        })
        .unwrap();
    let matching_accepted = gents_toolcalls::background::status::load_session_message_terminal(
        &owner,
        &caused_request_doc_id,
    )
    .await
    .is_ok();
    assert_eq!(matching_accepted, matching_model.expected);
    assert_eq!(
        settle_running_session_message_rows_with_access(
            node,
            &admission.tool.access().unwrap(),
            did
        )
        .await
        .unwrap(),
        0
    );
    assert!(session_message_notifications(
        &admission.tool.access().unwrap(),
        did,
        admission.tool.session_id()
    )
    .await
    .is_empty());
    gents_toolcalls::call::lifecycle::admission_fixture::complete_child_managed_verified(
        node,
        &head_id,
        "head answer",
        &admission
            .tool
            .access()
            .unwrap()
            .managed_agent_signer()
            .unwrap(),
        admission.runtime.clone(),
    )
    .await;
    assert_eq!(
        settle_running_session_message_rows_with_access(
            node,
            &admission.tool.access().unwrap(),
            did
        )
        .await
        .unwrap(),
        1
    );
    let notifications = session_message_notifications(
        &admission.tool.access().unwrap(),
        did,
        admission.tool.session_id(),
    )
    .await;
    assert_eq!(notifications.len(), 1);
    assert!(notifications[0].contains("head answer"));
    assert!(!notifications[0].contains("request_superseded"));
    settle_running_session_message_rows_with_access(
        node,
        &admission.tool.access().unwrap(),
        did,
    )
    .await
    .unwrap();
    gents_toolcalls::call::lifecycle::ToolCallLifecycle::reconcile_background_completion_side_effects(
        node,
        did,
        &recovery_identities(&admission),
    )
    .await
    .unwrap();
    assert_eq!(
        session_message_notifications(
            &admission.tool.access().unwrap(),
            did,
            admission.tool.session_id()
        )
        .await,
        notifications
    );
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&admission.path).unwrap();
}

#[tokio::test]
async fn managed_parent_receives_child_completion_when_node_cannot_read_request() {
    let PublishedSessionMessage {
        admission,
        caused_request_id,
        ..
    } = gents_toolcalls::call::lifecycle::admission_fixture::published_session_message(
        gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmissionOptions {
            name: "held-parent-child-completion".into(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        },
    )
    .await
    .expect("publish managed parent and child");
    let node = &admission.node;
    let access = admission.tool.access().unwrap();
    let parent_id = admission.tool.calling_request_id().await.unwrap();
    let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
    let parent =
        gents_session::request::binding::load_agent_request_with_access(&access, &parent_id)
            .await
            .unwrap()
            .expect("the selected Agent can read its parent request");
    assert_eq!(parent.agent_did, access.actor_did().unwrap());
    assert!(
        gents_session::request::binding::load_agent_request(node, &parent_id)
            .await
            .unwrap()
            .is_none(),
        "the Node cannot substitute for the held Agent"
    );
    let runtime = Arc::new(
        gents_store::identity::KeyIdentity::load_existing(
            admission.path.join("node.key"),
            None,
        )
        .expect("open the retained fixture runtime identity"),
    );
    gents_toolcalls::call::lifecycle::admission_fixture::complete_child_managed_verified(
        node,
        &caused_request_id,
        "ECHO-2468",
        &access.managed_agent_signer().unwrap(),
        runtime,
    )
    .await;
    assert_eq!(
        settle_running_session_message_rows_with_access(node, &access, &admission.node_did)
            .await
            .unwrap(),
        1
    );
    let key = background_completion_notification_message_key(&tool_doc_id, "tool");
    let query = format!(
        r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{}" }} }}) {{ _docID }} AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ status completion_notification_delivered_at }} }}"#,
        escape_graphql_string(&key),
        escape_graphql_string(&tool_doc_id),
    );
    let state = access.execute(&query).await.unwrap();
    assert_eq!(state["data"]["AgentMessage"].as_array().unwrap().len(), 1);
    let doc_id = state["data"]["AgentMessage"][0]["_docID"].as_str().unwrap();
    let (_, notification) = gents_session::session::load_canonical_message(
        &access,
        doc_id,
        &parent.node_did,
        parent.requester_did.as_deref(),
    )
    .await
    .unwrap();
    assert!(notification.rag_text().unwrap().contains("ECHO-2468"));
    let tool = &state["data"]["AgentToolCall"][0];
    assert_eq!(tool["status"], "completed");
    assert!(tool["completion_notification_delivered_at"]
        .as_str()
        .is_some());
    settle_running_session_message_rows_with_access(node, &access, &admission.node_did)
        .await
        .unwrap();
    let replay = access.execute(&query).await.unwrap();
    assert_eq!(replay["data"]["AgentMessage"].as_array().unwrap().len(), 1);
    assert_eq!(replay["data"]["AgentToolCall"][0], *tool);
    node.shutdown().await.unwrap();
    std::fs::remove_dir_all(&admission.path).unwrap();
}

#[tokio::test]
async fn completed_turn_with_live_goal_hands_off_a_non_complete_reason() {
    let PublishedSessionMessage {
        admission,
        caused_request_id,
        caused_request_doc_id,
        ..
    } = gents_toolcalls::call::lifecycle::admission_fixture::published_session_message(
        gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmissionOptions {
            name: "handoff-live-goal".into(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        },
    )
    .await
    .expect("publish accepted session message");
    let node = &admission.node;
    let did = &admission.node_did;
    let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
    gents_inference::test_support::install_test_agent(node, did, "general").await;
    // The caused session carries an active Goal: a finished turn with a
    // live assignment is not assignment success.
    gents_automation::goal::set_goal_from_access(
        &admission.tool.access().unwrap(),
        did,
        &caused_session_id(&admission.tool.access().unwrap(), &caused_request_doc_id).await,
        Some("hold"),
        Some(gents_protocol::goal_status::GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("persist an active Goal in the caused session");
    bind_caused_assignment(
        &admission.tool.access().unwrap(),
        did,
        &caused_request_doc_id,
    )
    .await;
    gents_toolcalls::call::lifecycle::admission_fixture::complete_child_managed_verified(
        node,
        &caused_request_id,
        "child done",
        &admission
            .tool
            .access()
            .unwrap()
            .managed_agent_signer()
            .unwrap(),
        admission.runtime.clone(),
    )
    .await;

    let settled = settle_running_session_message_rows_with_access(
        node,
        &admission.tool.access().unwrap(),
        did,
    )
    .await
    .unwrap();
    assert_eq!(settled, 1, "the caused terminal settles the row");
    // One settle call persists the suffix, publishes the notice and
    // advances the row: the converged form is `completed` with the
    // reason rendered. The durable pre-delivery suffix is pinned in the
    // crash-before-notice regression, where the side effect is skipped.
    let row = tool_call_row(&admission.tool.access().unwrap(), &tool_doc_id).await;
    assert_eq!(row["lifecycle_state"], "completed");
    assert_eq!(row["status"], "completed");

    let notifications = session_message_notifications(
        &admission.tool.access().unwrap(),
        did,
        admission.tool.session_id(),
    )
    .await;
    assert_eq!(notifications.len(), 1);
    assert!(
        notifications[0].contains("<reason>assignment_active</reason>"),
        "first notice names the live assignment: {}",
        notifications[0]
    );

    // Replay: the side effects already converged; the redrive republishes
    // nothing and the notice stays byte-identical.
    let redrive =
        gents_toolcalls::call::lifecycle::ToolCallLifecycle::reconcile_background_completion_side_effects(
            node, did, &recovery_identities(&admission),
        )
        .await
        .unwrap();
    assert_eq!(
        redrive.side_effects_converged, 0,
        "side effects already converged; the replay republishes nothing"
    );
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(&admission.path).expect("remove fixture");
}

/// No-goal and complete-goal sessions are unmistakably distinguished from
/// each other and from a live assignment.
#[tokio::test]
async fn completed_turn_without_a_goal_hands_off_assignment_absent() {
    let PublishedSessionMessage {
        admission,
        caused_request_id,
        ..
    } = gents_toolcalls::call::lifecycle::admission_fixture::published_session_message(
        gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmissionOptions {
            name: "handoff-no-goal".into(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        },
    )
    .await
    .expect("publish accepted session message");
    let node = &admission.node;
    let did = &admission.node_did;
    let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
    gents_inference::test_support::install_test_agent(node, did, "general").await;
    gents_toolcalls::call::lifecycle::admission_fixture::complete_child_managed_verified(
        node,
        &caused_request_id,
        "child done",
        &admission
            .tool
            .access()
            .unwrap()
            .managed_agent_signer()
            .unwrap(),
        admission.runtime.clone(),
    )
    .await;

    let settled = settle_running_session_message_rows_with_access(
        node,
        &admission.tool.access().unwrap(),
        did,
    )
    .await
    .unwrap();
    assert_eq!(settled, 1);
    // One settle call persists the suffix, publishes the notice and
    // advances the row; the notice is where the absent-assignment
    // distinction lives (a no-goal session never reads as success).
    let row = tool_call_row(&admission.tool.access().unwrap(), &tool_doc_id).await;
    assert_eq!(row["status"], "completed");
    let notifications = session_message_notifications(
        &admission.tool.access().unwrap(),
        did,
        admission.tool.session_id(),
    )
    .await;
    assert_eq!(notifications.len(), 1);
    assert!(
        notifications[0].contains("<reason>assignment_absent</reason>"),
        "first notice names the absent assignment: {}",
        notifications[0]
    );
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(&admission.path).expect("remove fixture");
}

#[tokio::test]
async fn completed_turn_with_complete_goal_is_the_reasonless_success_form() {
    let PublishedSessionMessage {
        admission,
        caused_request_id,
        caused_request_doc_id,
        ..
    } = gents_toolcalls::call::lifecycle::admission_fixture::published_session_message(
        gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmissionOptions {
            name: "handoff-complete-goal".into(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        },
    )
    .await
    .expect("publish accepted session message");
    let node = &admission.node;
    let did = &admission.node_did;
    let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
    gents_inference::test_support::install_test_agent(node, did, "general").await;
    gents_automation::goal::set_goal_from_access(
        &admission.tool.access().unwrap(),
        did,
        &caused_session_id(&admission.tool.access().unwrap(), &caused_request_doc_id).await,
        Some("done"),
        Some(gents_protocol::goal_status::GoalStatus::Complete),
        None,
        None,
    )
    .await
    .expect("persist a complete Goal in the caused session");
    bind_caused_assignment(
        &admission.tool.access().unwrap(),
        did,
        &caused_request_doc_id,
    )
    .await;
    gents_toolcalls::call::lifecycle::admission_fixture::complete_child_managed_verified(
        node,
        &caused_request_id,
        "child done",
        &admission
            .tool
            .access()
            .unwrap()
            .managed_agent_signer()
            .unwrap(),
        admission.runtime.clone(),
    )
    .await;

    let settled = settle_running_session_message_rows_with_access(
        node,
        &admission.tool.access().unwrap(),
        did,
    )
    .await
    .unwrap();
    assert_eq!(settled, 1);
    let row = tool_call_row(&admission.tool.access().unwrap(), &tool_doc_id).await;
    assert_eq!(row["status"], "completed");
    let notifications = session_message_notifications(
        &admission.tool.access().unwrap(),
        did,
        admission.tool.session_id(),
    )
    .await;
    assert_eq!(notifications.len(), 1);
    assert!(
        !notifications[0].contains("<reason>"),
        "a complete assignment stays reasonless: {}",
        notifications[0]
    );
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(&admission.path).expect("remove fixture");
}

/// The persisted suffix, not live assignment state, is the replay
/// authority: settlement persists `completionPending:<reason>` and a
/// crash before the notice leaves it outstanding; the redrive re-renders
/// the suffix byte-identically and advances the row to `completed`. The
/// replayed reason is invariant across a Goal status change between
/// settlement and redrive (the source snapshot never changes on replay).
#[tokio::test]
async fn crash_before_notice_redrive_renders_the_persisted_assignment_reason() {
    let PublishedSessionMessage {
        admission,
        caused_request_id,
        caused_request_doc_id,
        ..
    } = gents_toolcalls::call::lifecycle::admission_fixture::published_session_message(
        gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmissionOptions {
            name: "handoff-redrive".into(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        },
    )
    .await
    .expect("publish accepted session message");
    let node = &admission.node;
    let did = &admission.node_did;
    let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
    let session_id = admission.tool.session_id().to_owned();
    gents_inference::test_support::install_test_agent(node, did, "general").await;
    let goal_session_id =
        caused_session_id(&admission.tool.access().unwrap(), &caused_request_doc_id).await;
    gents_automation::goal::set_goal_from_access(
        &admission.tool.access().unwrap(),
        did,
        &goal_session_id,
        Some("hold"),
        Some(gents_protocol::goal_status::GoalStatus::Active),
        None,
        None,
    )
    .await
    .expect("persist an active Goal in the caused session");
    bind_caused_assignment(
        &admission.tool.access().unwrap(),
        did,
        &caused_request_doc_id,
    )
    .await;
    gents_toolcalls::call::lifecycle::admission_fixture::complete_child_managed_verified(
        node,
        &caused_request_id,
        "child done",
        &admission
            .tool
            .access()
            .unwrap()
            .managed_agent_signer()
            .unwrap(),
        admission.runtime.clone(),
    )
    .await;

    // Settle the lifecycle row only: a crash before the notice side
    // effect leaves `completionPending:assignment_active` outstanding.
    // The row is owned by the invoking Agent; rehydrate it as that Agent
    // exactly as settlement does.
    let agent_access = admission.tool.access().unwrap();
    let requester = agent_access
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ requester_did }} }}"#,
            escape_graphql_string(&tool_doc_id)
        ))
        .await
        .unwrap()["data"]["AgentToolCall"][0]["requester_did"]
        .as_str()
        .map(str::to_owned);
    let mut lifecycle = agent_access
        .transact_readonly("test.session_message.rehydrate", |txn| {
            let (node, tool_doc_id, session_id, requester) =
                (node.clone(), &tool_doc_id, &session_id, &requester);
            Box::pin(async move {
                let Some(mut lifecycle) = ToolCallLifecycle::load_by_doc_id_in_txn(
                    txn,
                    node,
                    tool_doc_id,
                    did,
                    session_id,
                    requester.as_deref(),
                )
                .await?
                else {
                    return Ok::<_, anyhow::Error>(None);
                };
                lifecycle.bind_agent_signer(txn.managed_agent_signer())?;
                Ok(Some(lifecycle))
            })
        })
        .await
        .unwrap()
        .expect("rehydrated session-message row");
    let caused = match gents_toolcalls::background::caused::observe_caused_request(node, &lifecycle)
        .await
        .unwrap()
    {
        gents_toolcalls::background::caused::CausedObservation::Bound(caused) => caused,
        gents_toolcalls::background::caused::CausedObservation::Unbound(reason) => {
            panic!("expected a bound caused request, got Unbound({reason})")
        }
        gents_toolcalls::background::caused::CausedObservation::NotVisible => {
            panic!("expected a bound caused request, got NotVisible")
        }
    };
    let caused_doc_id = caused.doc_id.as_deref().unwrap().to_owned();
    let Some(terminal) = gents_toolcalls::background::status::load_caused_request_terminals(
        &agent_access,
        std::slice::from_ref(&caused_doc_id),
    )
    .await
    .unwrap()
    .remove(&caused_doc_id) else {
        panic!("the completed child is a terminal observation");
    };
    let handoff = turn_assignment_handoff(
        &gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            lifecycle.actor().clone(),
        ),
        &caused,
        &terminal,
    )
    .await;
    let handoff_reason = handoff.reason();
    assert_eq!(handoff_reason, Some("assignment_active"));
    assert!(lifecycle
        .settle_session_message(&terminal, handoff_reason)
        .await
        .unwrap());
    // Pre-delivery persisted snapshot: the suffix is durable before any
    // notification exists (lifecycle state is terminal; only the notice
    // side effect is outstanding).
    let row = tool_call_row(&admission.tool.access().unwrap(), &tool_doc_id).await;
    assert_eq!(row["status"], "completionPending:assignment_active");
    assert!(
        session_message_notifications(&admission.tool.access().unwrap(), did, &session_id)
            .await
            .is_empty(),
        "no notice exists yet: this is the crash-before-notice state"
    );

    // The assignment changes under the replay (the Goal completes), but
    // the redrive must re-render the persisted suffix, never re-observe
    // live state.
    gents_automation::goal::set_goal_from_access(
        &admission.tool.access().unwrap(),
        did,
        &goal_session_id,
        Some("hold"),
        Some(gents_protocol::goal_status::GoalStatus::Complete),
        None,
        None,
    )
    .await
    .expect("the Goal completes after settlement");

    // The redrive re-renders from the row's persisted suffix.
    let report =
        gents_toolcalls::call::lifecycle::ToolCallLifecycle::reconcile_background_completion_side_effects(
            node, did, &recovery_identities(&admission),
        )
        .await
        .unwrap();
    assert_eq!(report.side_effects_converged, 1);
    let notifications =
        session_message_notifications(&admission.tool.access().unwrap(), did, &session_id)
            .await;
    assert_eq!(notifications.len(), 1);
    assert!(
        notifications[0].contains("<reason>assignment_active</reason>"),
        "replay renders the persisted assignment reason, not live state: {}",
        notifications[0]
    );
    let row = tool_call_row(&admission.tool.access().unwrap(), &tool_doc_id).await;
    assert_eq!(row["status"], "completed", "side effects converged");
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(&admission.path).expect("remove fixture");
}

/// Native background completions keep their reasonless success form: the
/// persisted suffix path is reserved for session-message settlement
/// (verified through the real redrive in
/// `crash_before_notice_redrive_renders_the_persisted_assignment_reason`
/// and by the projection regression in the recovery module).
#[tokio::test]
async fn session_message_settlement_persists_assignment_reason_not_native_status() {
    // Non-session-message completions never call settle_session_message;
    // the accessor at tool_call_lifecycle.rs keeps formatting any reason
    // it is given, so the native vocabulary is unchanged there.
    let terminal = gents_toolcalls::call::lifecycle::CausedRequestTerminal::Completed {
        output: "child done".into(),
    };
    assert_eq!(terminal.completion_reason(), None);
    assert_eq!(terminal.notification_status(), "completed");
}

async fn tool_call_row(
    access: &gents_store::config_client::ConfigAccess,
    tool_doc_id: &str,
) -> serde_json::Value {
    let escaped = escape_graphql_string(tool_doc_id);
    let response = access
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ status lifecycle_state }} }}"#
        ))
        .await
        .expect("read the tool call row as its owning Agent");
    response["data"]["AgentToolCall"][0].clone()
}

/// Notices are owned by the Agent that published them, so they are read
/// through that Agent's access rather than anonymously.
async fn session_message_notifications(
    access: &gents_store::config_client::ConfigAccess,
    did: &str,
    session_id: &str,
) -> Vec<String> {
    let escaped = escape_graphql_string(session_id);
    let response = access
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{escaped}" }} }}) {{ _docID requester_did }} }}"#
        ))
        .await
        .expect("read session messages as their owning Agent");
    let mut notifications = Vec::new();
    for row in response["data"]["AgentMessage"].as_array().unwrap() {
        let (_, message) = gents_session::session::load_canonical_message(
            access,
            row["_docID"].as_str().unwrap(),
            did,
            row["requester_did"].as_str(),
        )
        .await
        .expect("reconstruct canonical session message");
        if let gents_inference::llm::message::Message::User { content } = message {
            for item in content {
                if let gents_inference::llm::message::UserContent::Text(text) = item {
                    if text.text.contains("<tool-completion") {
                        notifications.push(text.text);
                    }
                }
            }
        }
    }
    notifications
}

async fn bind_caused_assignment(
    access: &gents_store::config_client::ConfigAccess,
    did: &str,
    request_doc: &str,
) {
    let session = caused_session_id(access, request_doc).await;
    let goal = access
        .transact_readonly("test.load_caused_goal", |txn| {
            Box::pin(gents_automation::goal::load_canonical_goal_in_txn(txn, did, &session))
        })
        .await
        .unwrap()
        .unwrap();
    access.write("test.bind_caused_assignment",
        &format!("mutation {{update_Goal(docID: \"{}\", input: {{assignment_root_request_doc_id: \"{}\"}}) {{_docID}}}}",
            escape_graphql_string(&goal.doc_id), escape_graphql_string(request_doc))).await.unwrap();
}

async fn caused_session_id(
    access: &gents_store::config_client::ConfigAccess,
    caused_request_doc_id: &str,
) -> String {
    let doc_id = escape_graphql_string(caused_request_doc_id);
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ session_id }} }}"#
        ))
        .await
        .unwrap();
    response["data"]["AgentRequest"][0]["session_id"]
        .as_str()
        .expect("the caused request names its session")
        .to_owned()
}
