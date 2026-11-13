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
    for case in gents_lean_vocab::families::causal_hop_contracts_write::cases::<String>() {
        let input = &case.input;
        let name = input.name.as_str();
        assert_eq!(input.own_hop, u64::from(caused.request_hop), "{name}");
        let state = if input.delivery == "request" {
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
        let cause = cause(&message, u32::try_from(input.caller_hop).unwrap()).await;
        let body = RenderedBody {
            content: "again".to_owned(),
            goal: None,
        };
        let planned = plan_with_delivery(
            &node,
            &NoSessionMessageGoals,
            &cause,
            &target,
            (body, None),
            if input.delivery == "steer" {
                AgentMessageDelivery::Steer
            } else {
                AgentMessageDelivery::Queue
            },
            &signer(&message),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(planned.delivery().as_str(), input.delivery, "{name}");
        let PlannedWrite::Request { create, .. } = &planned.write else {
            panic!("{name}: a message without a Goal is a request");
        };
        assert_eq!(create.request_hop, planned.hop(), "{name}");
        assert_eq!(
            create.caused_by_parent_request_doc_id.as_deref(),
            Some(cause.caller_request_doc_id.as_str()),
            "{name}"
        );
        let queued_after = create
            .input
            .queue
            .as_ref()
            .and_then(|queue| queue.queued_after_request_id.as_deref());
        case.check(
            gents_lean_vocab::families::causal_hop_contracts_write::WriteOutput {
                hop: u64::from(planned.hop()),
                names_caller_tool_call: create.caused_by_parent_tool_call_doc_id.as_deref()
                    == Some(cause.tool_call_doc_id.as_str()),
                queued_after_active: queued_after == Some(caused.request_id.as_str()),
            },
        );
    }
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}
