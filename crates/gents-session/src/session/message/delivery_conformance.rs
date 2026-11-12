use super::*;

#[test]
fn agent_delivery_matches_lean() {
    gents_lean_vocab::families::contracts_agent_delivery::replay::<String>(|case| {
        let mut input = serde_json::json!({
            "session_id": "target", "message": "hello", "delivery": case.delivery
        });
        if let Some(interrupt) = case.interrupt {
            input["interrupt"] = interrupt.into();
        }
        let args: AgentMessageArgs = serde_json::from_value(input).unwrap();
        args.delivery().ok().map(|mode| {
            match mode {
                AgentMessageDelivery::Queue => "queue",
                AgentMessageDelivery::Steer => "steer",
                AgentMessageDelivery::Interrupt => "interrupt",
            }
            .to_owned()
        })
    });
}

#[tokio::test]
async fn remote_delivery_waits_for_authenticated_http_transport() {
    let snapshot = gents_lean_vocab::lean_contract_snapshot();
    let claim = snapshot
        .remote_agent_delivery_claim_cases
        .iter()
        .find(|case| case.name == "native-read-no-atomic-authority")
        .unwrap();
    assert_eq!(claim.document_visible, true);
    assert_eq!(claim.atomic_requester_authorization, false);
    assert_eq!(claim.claim, false);
    let case = snapshot
        .remote_agent_delivery_cases
        .iter()
        .find(|case| case.name == "committed-response-observed")
        .unwrap();
    assert_eq!(case.result, "committed");
    let intent = &case.intent;
    use gents_store::identity::NodeIdentity;
    let node_db = gents_inference::test_support::test_db(
        "remote-delivery-waits-for-authenticated-",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = node_db.node.clone();
    let root = gents_store::test_support::held_runtime_identity(node_db.node_identity.did());
    let binding = gents_store::identity::provision_agent_identity(&root).unwrap();
    let agent = gents_store::identity::resolve_agent_identity(&root, &binding).unwrap();
    let signer = agent.managed_agent_signer().unwrap();
    let cause = gents_protocol::request_lifecycle::SessionMessageCause {
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
    let target = gents_protocol::request_lifecycle::SessionMessageTarget {
        node_did: "target-runtime".into(),
        agent_did: intent.target.as_str().into(),
        session_id: "target-session".into(),
    };
    let result = super::plan::plan_with_delivery(
        &node,
        &NoSessionMessageGoals,
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
