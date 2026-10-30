use super::plan::PlannedWrite;
use super::*;
use crate::tools::call::lifecycle::admission_fixture::{
    published_session_message, PublishedAdmissionOptions,
};
use crate::tools::call::lifecycle::AwaitMode;

pub(super) fn cause(
    message: &crate::tools::call::lifecycle::admission_fixture::PublishedSessionMessage,
    caller_hop: u32,
) -> SessionMessageCause {
    SessionMessageCause {
        caller_node_did: message.admission.node_did.clone(),
        caller_agent_id: "caller-agent".to_owned(),
        caller_session_id: "caller-session".to_owned(),
        caller_requester_did: None,
        caller_request_id: "caller-request".to_owned(),
        caller_request_doc_id: message.admission.tool.request_doc_id().unwrap().to_owned(),
        caller_hop,
        tool_call_id: "hop-tool".to_owned(),
        tool_call_doc_id: "hop-tool-doc".to_owned(),
        correlation: None,
    }
}

#[tokio::test]
async fn generated_session_message_writes_carry_the_calling_edge() {
    let message = published_session_message(PublishedAdmissionOptions {
        name: "session-message-writes".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = message.admission.node.clone();
    let caused = crate::request::binding::load_agent_request_by_doc_id(
        &node,
        &message.caused_request_doc_id,
    )
    .await
    .unwrap()
    .unwrap();
    let target = SessionMessageTarget {
        node_did: caused.node_did.clone(),
        agent_id: caused.agent_id.clone(),
        session_id: caused.session_id.clone(),
    };
    let cases = &crate::lean_vocab_test::lean_contract_snapshot()
        .causal_hop_contract
        .write_cases;
    assert!(cases.iter().any(|case| case.delivery == "queue"));
    assert!(cases.iter().any(|case| case.delivery == "steer"));
    for case in cases {
        let name = case.name.as_str();
        assert_eq!(case.own_hop, caused.request_hop, "{name}");
        let state = if case.delivery == "request" {
            "pending"
        } else {
            "processing"
        };
        let response = node
            .execute(&format!(
                r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "{state}" }}) {{ _docID }} }}"#,
                escape_graphql_string(&message.caused_request_doc_id)
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let cause = cause(&message, case.caller_hop);
        let body = RenderedBody {
            content: "again".to_owned(),
            goal: None,
        };
        let planned = plan_with_delivery(
            &node,
            &cause,
            &target,
            body,
            None,
            if case.delivery == "steer" {
                AgentMessageDelivery::Steer
            } else {
                AgentMessageDelivery::Queue
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(planned.delivery().as_str(), case.delivery, "{name}");
        assert_eq!(planned.hop(), case.expected_hop, "{name}");
        let PlannedWrite::Request { create, .. } = &planned.write else {
            panic!("{name}: a message without a Goal is a request");
        };
        assert_eq!(create.request_hop, case.expected_hop, "{name}");
        assert_eq!(
            create.caused_by_parent_request_doc_id.as_deref(),
            Some(cause.caller_request_doc_id.as_str()),
            "{name}"
        );
        assert_eq!(
            create.caused_by_parent_tool_call_doc_id.as_deref() == Some("hop-tool-doc"),
            case.names_caller_tool_call,
            "{name}"
        );
        let queued_after = create
            .input
            .queue
            .as_ref()
            .and_then(|queue| queue.queued_after_request_id.as_deref());
        assert_eq!(
            queued_after == Some(caused.request_id.as_str()),
            case.queued_after_active,
            "{name}"
        );
    }
    node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn stage_planned_write_in_txn_creates_the_request_and_leaves_the_tool_row() {
    let message = published_session_message(PublishedAdmissionOptions {
        name: "session-message-staging".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = message.admission.node.clone();
    let tool = &message.admission.tool;
    let tool_doc_id = tool.doc_id().unwrap().to_owned();
    let caused = crate::request::binding::load_agent_request_by_doc_id(
        &node,
        &message.caused_request_doc_id,
    )
    .await
    .unwrap()
    .unwrap();
    let target = SessionMessageTarget {
        node_did: caused.node_did.clone(),
        agent_id: caused.agent_id.clone(),
        session_id: caused.session_id.clone(),
    };
    let cause = cause(&message, 0);
    let body = RenderedBody {
        content: "staged".to_owned(),
        goal: None,
    };
    let planned = plan(&node, &cause, &target, body, None, false)
        .await
        .unwrap()
        .unwrap();
    let load_tool = || {
        crate::tools::call::lifecycle::ToolCallLifecycle::load_by_doc_id(
            node.clone(),
            &tool_doc_id,
            tool.node_did(),
            tool.session_id(),
            tool.requester_did(),
        )
    };
    let before = load_tool().await.unwrap().unwrap();
    let receipt_before = load_receipt(
        &node,
        &tool_doc_id,
        tool.node_did(),
        tool.session_id(),
        tool.requester_did(),
    )
    .await
    .unwrap();
    let (cause, planned) = (&cause, &planned);
    let enqueued = crate::config_client::ConfigAccess::transact_local_write(
        &node,
        None,
        "session_message.test_stage",
        move |txn| Box::pin(async move { stage_planned_write_in_txn(txn, cause, planned).await }),
    )
    .await
    .unwrap();
    let staged = crate::request::binding::load_agent_request_by_doc_id(&node, &enqueued.doc_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(staged.request_id, enqueued.request_id);
    assert_eq!(staged.session_id, target.session_id);
    let after = load_tool().await.unwrap().unwrap();
    assert_eq!(after.state(), before.state());
    assert_eq!(after.is_running(), before.is_running());
    let receipt_after = load_receipt(
        &node,
        &tool_doc_id,
        tool.node_did(),
        tool.session_id(),
        tool.requester_did(),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(&receipt_after).unwrap(),
        serde_json::to_value(&receipt_before).unwrap()
    );
    node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn stage_planned_write_in_txn_refuses_a_session_that_moved_past_the_planned_hop() {
    let message = published_session_message(PublishedAdmissionOptions {
        name: "session-message-stale-hop".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = message.admission.node.clone();
    let caused = crate::request::binding::load_agent_request_by_doc_id(
        &node,
        &message.caused_request_doc_id,
    )
    .await
    .unwrap()
    .unwrap();
    let target = SessionMessageTarget {
        node_did: caused.node_did.clone(),
        agent_id: caused.agent_id.clone(),
        session_id: caused.session_id.clone(),
    };
    let stale_cause = cause(&message, 0);
    let body = RenderedBody {
        content: "stale".to_owned(),
        goal: None,
    };
    let planned = plan(&node, &stale_cause, &target, body, None, false)
        .await
        .unwrap()
        .unwrap();
    let ahead_cause = cause(&message, 10);
    let ahead = plan(
        &node,
        &ahead_cause,
        &target,
        RenderedBody {
            content: "ahead".to_owned(),
            goal: None,
        },
        None,
        false,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ahead.hop() > planned.hop());
    let (ahead_cause, ahead) = (&ahead_cause, &ahead);
    crate::config_client::ConfigAccess::transact_local_write(
        &node,
        None,
        "session_message.test_ahead_stage",
        move |txn| {
            Box::pin(async move { stage_planned_write_in_txn(txn, ahead_cause, ahead).await })
        },
    )
    .await
    .unwrap();
    let (stale_cause, planned) = (&stale_cause, &planned);
    let error = crate::config_client::ConfigAccess::transact_local_write(
        &node,
        None,
        "session_message.test_stale_stage",
        move |txn| {
            Box::pin(async move { stage_planned_write_in_txn(txn, stale_cause, planned).await })
        },
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("moved to hop"),
        "unexpected error: {error:#}"
    );
    let PlannedWrite::Request { create, .. } = &planned.write else {
        panic!("a message without a Goal is a request");
    };
    let response = node
        .execute(&format!(
            r#"query {{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(&create.request_id)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let data = response.data.unwrap();
    assert!(data["AgentRequest"].as_array().unwrap().is_empty());
    node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}
