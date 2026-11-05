use super::*;

#[test]
fn agent_delivery_matches_lean() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().agent_delivery_cases;
    assert_eq!(cases.len(), 12);
    for case in cases {
        let mut input = serde_json::json!({
            "session_id": "target", "message": "hello",
            "delivery": case.delivery, "interrupt": case.interrupt
        });
        if case.interrupt.is_none() {
            input.as_object_mut().unwrap().remove("interrupt");
        }
        let args: AgentMessageArgs = serde_json::from_value(input).unwrap();
        let actual = args.delivery().ok().map(|mode| match mode {
            AgentMessageDelivery::Queue => "queue",
            AgentMessageDelivery::Steer => "steer",
            AgentMessageDelivery::Interrupt => "interrupt",
        });
        assert_eq!(actual, case.expected.as_deref(), "{case:?}");
    }
}

#[tokio::test]
async fn remote_delivery_waits_for_authenticated_http_transport() {
    let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let claim = snapshot["remote_agent_delivery_claim_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "native-read-no-atomic-authority")
        .unwrap();
    assert_eq!(claim["document_visible"], true);
    assert_eq!(claim["atomic_requester_authorization"], false);
    assert_eq!(claim["claim"], false);
    let cases = snapshot["remote_agent_delivery_cases"].as_array().unwrap();
    let case = cases
        .iter()
        .find(|case| case["name"] == "committed-response-observed")
        .unwrap();
    assert_eq!(case["result"], "committed");
    let intent = &case["intent"];
    use crate::store::identity::NodeIdentity;
    let temp = tempfile::tempdir().unwrap();
    let root =
        crate::store::identity::KeyIdentity::load_or_create(temp.path().join("node.key"), None)
            .unwrap();
    let binding = crate::store::identity::provision_agent_identity(&root).unwrap();
    let agent = crate::store::identity::resolve_agent_identity(&root, &binding).unwrap();
    let signer = agent.managed_agent_signer().unwrap();
    let node = std::sync::Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(root.did())
            .build()
            .await
            .unwrap(),
    );
    let cause = crate::request::lifecycle::SessionMessageCause {
        caller_node_did: root.did().into(),
        caller_agent_did: agent.did().into(),
        caller_session_id: "source-session".into(),
        caller_requester_did: None,
        caller_request_id: "request".into(),
        caller_request_doc_id: "request-doc".into(),
        caller_hop: 0,
        tool_call_id: "tool".into(),
        tool_call_doc_id: "tool-doc".into(),
        correlation: None,
    };
    let target = crate::request::lifecycle::SessionMessageTarget {
        node_did: "target-runtime".into(),
        agent_did: intent["target"].as_str().unwrap().into(),
        session_id: "target-session".into(),
    };
    let result = super::plan::plan_with_delivery(
        &node,
        &cause,
        &target,
        RenderedBody {
            content: "message".into(),
            goal: None,
        },
        None,
        AgentMessageDelivery::Queue,
        &signer,
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(reason) if reason.contains("authenticated HTTP delivery path")));
    node.shutdown().await.expect("shut down test node");
}
