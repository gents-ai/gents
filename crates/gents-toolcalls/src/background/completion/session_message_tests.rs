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
    let fold_to = |named: &str, doc: &str| {
        format!(
            r#"mutation {{ update_AgentRequest(docID: "{}", input: {{
        lifecycle_state: "superseded", superseded_by_request: "{}", superseded_by_request_doc_id: "{}",
        failure_reason: "{}"
    }}) {{ _docID }} }}"#,
            escape_graphql_string(&caused_request_doc_id),
            escape_graphql_string(named),
            escape_graphql_string(doc),
            escape_graphql_string(gents_session::request::lifecycle::FOLDED_REASON)
        )
    };
    for (index, case) in gents_lean_vocab::families::contracts_agent_folded_binding::cases()
        .into_iter()
        .enumerate()
    {
        let binding = &case.input;
        let foreign_requester = binding.caused_requester != binding.head_requester;
        let foreign_agent = binding.caused_agent != binding.head_agent;
        let (actual_id, actual_doc) = if foreign_requester || foreign_agent {
            let foreign_id = format!("foreign-head-{index}");
            let mut foreign_create = create.replace(
                &format!("request_id: \"{}\"", head_id),
                &format!("request_id: \"{foreign_id}\""),
            );
            if foreign_requester {
                foreign_create = foreign_create.replace(
                    &format!(
                        "requester_did: \"{}\"",
                        escape_graphql_string(&caused_requester)
                    ),
                    "requester_did: \"did:test:foreign\"",
                );
            }
            if foreign_agent {
                foreign_create = foreign_create.replace(
                    &format!("agent_did: \"{}\"", escape_graphql_string(&caused_agent)),
                    "agent_did: \"foreign-agent\"",
                );
            }
            let foreign = owner
                .write("test.foreign_head", &foreign_create)
                .await
                .unwrap();
            (
                foreign_id,
                gents_store::graphql::created_doc_id(&foreign, "AgentRequest").unwrap(),
            )
        } else {
            (head_id.clone(), head_doc.clone())
        };
        let named = if binding.named_head == binding.actual_head {
            actual_id.as_str()
        } else {
            "wrong-head"
        };
        owner
            .write("test.fold", &fold_to(named, &actual_doc))
            .await
            .unwrap();
        let accepted = crate::background::status::load_session_message_terminal(
            &owner,
            &caused_request_doc_id,
        )
        .await
        .is_ok();
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
        case.check(accepted);
    }
    owner
        .write("test.folded_cause", &fold_to(&head_id, &head_doc))
        .await
        .unwrap();
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
