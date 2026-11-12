use gents_protocol::request_lifecycle::SessionMessageTarget;
use gents_protocol::tool_call::AwaitMode;
use gents_session::session::message::plan::PlannedWrite;
use gents_session::session::message::*;
use gents_store::graphql::escape_graphql_string;
use gents_toolcalls::call::lifecycle::admission_fixture::{
    cause, published_session_message, signer, PublishedAdmissionOptions,
};

#[tokio::test]
async fn generated_session_message_writes_carry_the_calling_edge() {
    let message = published_session_message(PublishedAdmissionOptions {
        name: "session-message-writes".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        composition: gents_automation::lifecycle_hooks::composition(),
        ..Default::default()
    })
    .await
    .unwrap();
    let node = message.admission.node.clone();
    let caused = gents_session::request::binding::load_agent_request_by_doc_id_with_access(
        &message.admission.tool.access().unwrap(),
        &message.caused_request_doc_id,
    )
    .await
    .unwrap()
    .unwrap();
    let target = SessionMessageTarget {
        node_did: caused.node_did.clone(),
        agent_did: caused.agent_did.clone(),
        session_id: caused.session_id.clone(),
    };
    let cases = &gents_lean_vocab::lean_contract_snapshot().causal_hop_write_cases;
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
        message
            .admission
            .tool
            .access()
            .unwrap()
            .write(
                "test.set_hop_state",
                &format!(
                    r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "{state}" }}) {{ _docID }} }}"#,
                    escape_graphql_string(&message.caused_request_doc_id)
                ),
            )
            .await
            .unwrap();
        let cause = cause(&message, case.caller_hop).await;
        let body = RenderedBody {
            content: "again".to_owned(),
            goal: None,
        };
        let planned = plan_with_delivery(
            &node,
            &NoSessionMessageGoals,
            &cause,
            &target,
            body,
            None,
            if case.delivery == "steer" {
                AgentMessageDelivery::Steer
            } else {
                AgentMessageDelivery::Queue
            },
            &signer(&message),
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
            create.caused_by_parent_tool_call_doc_id.as_deref()
                == Some(cause.tool_call_doc_id.as_str()),
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
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}
