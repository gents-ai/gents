use super::*;
use crate::call::lifecycle::admission_fixture::{
    caused_session_id, recovery_identities, session_message_notifications, PublishedSessionMessage,
};
use gents_protocol::background_completion::background_completion_notification_message_key;
use gents_protocol::tool_call::AwaitMode;

/// A completed turn must not read as assignment evidence: only a
/// complete assignment is the reasonless success form.
#[tokio::test]
async fn folded_session_message_waits_for_exact_head_and_replays_once() {
    let PublishedSessionMessage {
        admission,
        caused_request_doc_id,
        ..
    } = crate::call::lifecycle::admission_fixture::published_session_message(
        crate::call::lifecycle::admission_fixture::PublishedAdmissionOptions {
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
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&caused_agent),
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
    let foreign_accepted =
        crate::background::status::load_session_message_terminal(&owner, &caused_request_doc_id)
            .await
            .is_ok();
    assert_eq!(foreign_accepted, foreign_model.expected);
    assert_eq!(
        settle_running_session_message_rows_with_access(
            node,
            &admission.tool.access().unwrap(),
            &gents_session::session::message::NoSessionMessageGoals,
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
    let foreign_agent_accepted =
        crate::background::status::load_session_message_terminal(&owner, &caused_request_doc_id)
            .await
            .is_ok();
    assert_eq!(foreign_agent_accepted, foreign_agent_model.expected);
    assert_eq!(
        settle_running_session_message_rows_with_access(
            node,
            &admission.tool.access().unwrap(),
            &gents_session::session::message::NoSessionMessageGoals,
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
    let mismatch_accepted =
        crate::background::status::load_session_message_terminal(&owner, &caused_request_doc_id)
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
    let matching_accepted =
        crate::background::status::load_session_message_terminal(&owner, &caused_request_doc_id)
            .await
            .is_ok();
    assert_eq!(matching_accepted, matching_model.expected);
    assert_eq!(
        settle_running_session_message_rows_with_access(
            node,
            &admission.tool.access().unwrap(),
            &gents_session::session::message::NoSessionMessageGoals,
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
    crate::call::lifecycle::admission_fixture::complete_child_managed_verified(
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
            &gents_session::session::message::NoSessionMessageGoals,
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
        &gents_session::session::message::NoSessionMessageGoals,
        did,
    )
    .await
    .unwrap();
    crate::call::lifecycle::ToolCallLifecycle::reconcile_background_completion_side_effects(
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
    } = crate::call::lifecycle::admission_fixture::published_session_message(
        crate::call::lifecycle::admission_fixture::PublishedAdmissionOptions {
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
        gents_store::identity::KeyIdentity::load_existing(admission.path.join("node.key"), None)
            .expect("open the retained fixture runtime identity"),
    );
    crate::call::lifecycle::admission_fixture::complete_child_managed_verified(
        node,
        &caused_request_id,
        "ECHO-2468",
        &access.managed_agent_signer().unwrap(),
        runtime,
    )
    .await;
    assert_eq!(
        settle_running_session_message_rows_with_access(
            node,
            &access,
            &gents_session::session::message::NoSessionMessageGoals,
            &admission.node_did
        )
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
    settle_running_session_message_rows_with_access(
        node,
        &access,
        &gents_session::session::message::NoSessionMessageGoals,
        &admission.node_did,
    )
    .await
    .unwrap();
    let replay = access.execute(&query).await.unwrap();
    assert_eq!(replay["data"]["AgentMessage"].as_array().unwrap().len(), 1);
    assert_eq!(replay["data"]["AgentToolCall"][0], *tool);
    node.shutdown().await.unwrap();
    std::fs::remove_dir_all(&admission.path).unwrap();
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
    let terminal = crate::call::lifecycle::CausedRequestTerminal::Completed {
        output: "child done".into(),
    };
    assert_eq!(terminal.completion_reason(), None);
    assert_eq!(terminal.notification_status(), "completed");
}
