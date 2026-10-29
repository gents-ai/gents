use super::*;

fn notification_payload(method: &str, params: Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "method": wire_method(method),
        "params": params,
    }))
    .expect("notification payload")
}

#[tokio::test]
async fn request_validation_failures_answer_invalid_params() {
    let (_staging, service) = test_service().await;
    service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-table" } }),
        ))
        .await;

    let text_block = json!({"type": "text", "text": "hello"});
    let cases: Vec<(&str, Value)> = vec![
        (
            "session/set_model",
            json!({ "sessionId": "s-table", "modelId": "" }),
        ),
        ("session/set_model", json!({ "sessionId": "s-table" })),
        (
            "session/set_model",
            json!({ "sessionId": "s-table", "modelId": "gpt-9" }),
        ),
        (
            "session/new",
            json!({ "cwd": "/tmp", "_meta": { "sessionId": "s-never", "modelId": "gpt-9" } }),
        ),
        ("session/set_model", json!({ "modelId": "GLM-5.3-NVFP4" })),
        (
            "session/set_model",
            json!({ "sessionId": "  ", "modelId": "GLM-5.3-NVFP4" }),
        ),
        (
            "session/set_mode",
            json!({ "sessionId": "s-table", "modeId": "" }),
        ),
        ("session/set_mode", json!({ "sessionId": "s-table" })),
        ("authenticate", json!({ "methodId": "oauth" })),
        ("authenticate", json!({ "methodId": "" })),
        ("session/prompt", json!({ "prompt": [text_block.clone()] })),
        ("session/prompt", json!({ "sessionId": "s-table" })),
        (
            "session/prompt",
            json!({ "sessionId": "s-table", "prompt": [] }),
        ),
        (
            "session/prompt",
            json!({ "sessionId": "s-table", "prompt": [{"type": "text"}] }),
        ),
        (
            "session/prompt",
            json!({
                "sessionId": "s-table",
                "prompt": [text_block.clone()],
                "_meta": { "screenMode": "sideways" },
            }),
        ),
        (
            "session/set_model",
            json!({
                "sessionId": "s-table",
                "modelId": "GLM-5.3-NVFP4",
                "_meta": { "reasoningEffort": "high" },
            }),
        ),
        ("x.ai/subagent/get", json!({})),
        ("x.ai/subagent/get", json!({ "sessionId": 7 })),
        ("x.ai/subagent/get", json!({ "sessionId": "  " })),
        ("x.ai/subagent/list_running", json!({})),
        ("x.ai/subagent/list_running", json!({ "sessionId": 7 })),
        ("x.ai/subagent/list_running", json!({ "sessionId": "  " })),
        ("x.ai/subagent/get", json!({ "sessionId": "s-table" })),
        (
            "x.ai/subagent/get",
            json!({ "sessionId": "s-table", "subagentId": 7 }),
        ),
        (
            "x.ai/subagent/get",
            json!({ "sessionId": "s-table", "subagentId": "  " }),
        ),
    ];
    for (method, params) in cases {
        let dispatch = service
            .handle_acp_payload(&request_payload(method, params.clone()))
            .await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(
            response["error"]["code"], -32602,
            "{method} with {params} must answer invalid params"
        );
        assert!(
            !response["error"]["message"]
                .as_str()
                .expect("message")
                .is_empty(),
            "{method} must carry a diagnostic message"
        );
    }

    for (method, params) in [
        (
            "x.ai/interject",
            json!({ "sessionId": "s-table", "text": "hi" }),
        ),
        (
            "x.ai/compact_conversation",
            json!({ "sessionId": "s-table" }),
        ),
        ("terminal/wait_for_exit", json!({})),
        ("terminal/create", json!({})),
        ("some/unknown/method", json!({})),
        ("x.ai/subagent/invent", json!({ "sessionId": "s-table" })),
    ] {
        let dispatch = service
            .handle_acp_payload(&request_payload(method, params))
            .await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(
            response["error"]["code"], -32601,
            "{method} must stay method-not-found"
        );
    }

    let get = service
        .handle_acp_payload(&request_payload(
            "x.ai/subagent/get",
            json!({ "sessionId": "s-table", "subagentId": "child-1" }),
        ))
        .await;
    let get = parse_response(get.response.as_deref().expect("response line"));
    assert_eq!(get["result"], json!({ "result": {"snapshot": null} }));

    let list = service
        .handle_acp_payload(&request_payload(
            "x.ai/subagent/list_running",
            json!({ "sessionId": "s-table" }),
        ))
        .await;
    let list = parse_response(list.response.as_deref().expect("response line"));
    assert_eq!(list["result"], json!({ "result": {"subagents": []} }));

    let cancel = service
        .handle_acp_payload(&request_payload(
            "x.ai/subagent/cancel",
            json!({ "sessionId": "s-table", "subagentId": "child-1" }),
        ))
        .await;
    let cancel = parse_response(cancel.response.as_deref().expect("response line"));
    assert_eq!(
        cancel["result"],
        json!({ "result": {"subagentId": "child-1", "cancelled": false,
            "outcome": {"kind": "not_found"}} })
    );

    let dispatch = service
        .handle_acp_payload(&request_payload(
            SUBAGENT_GET_METHOD,
            json!({"subagentId": "unknown-child"}),
        ))
        .await;
    let response = parse_response(dispatch.response.as_deref().unwrap());
    assert!(response.get("error").is_none(), "{response}");
    assert!(response["result"].get("result").is_some());
    for method in [SUBAGENT_GET_METHOD, SUBAGENT_LIST_RUNNING_METHOD] {
        let dispatch = service
            .handle_acp_payload(&request_payload(
                method,
                json!({"sessionId": "not-registered", "subagentId": "child-1"}),
            ))
            .await;
        let response = parse_response(dispatch.response.as_deref().unwrap());
        assert_eq!(response["error"]["code"], -32602, "{response}");
    }

    let node = service.config.node.clone();
    let query = r#"{
            AgentRequest { request_id }
            AgentMessage { message_key }
            AgentSession { session_id }
        }"#
    .to_string();
    let response = node.execute(&query).await;
    ensure_no_errors(&response, "rejected-input document check").expect("query ok");
    let count = |key: &str| -> usize {
        response
            .data
            .as_ref()
            .and_then(|data| data.get(key))
            .and_then(Value::as_array)
            .map(|rows| rows.len())
            .unwrap_or_default()
    };
    assert_eq!(
        count("AgentRequest"),
        0,
        "no AgentRequest rows were fabricated"
    );
    assert_eq!(
        count("AgentMessage"),
        0,
        "no AgentMessage rows were fabricated"
    );
    assert_eq!(
        count("AgentSession"),
        1,
        "only the legitimate s-table session exists"
    );
}

#[tokio::test]
async fn operational_failures_stay_internal_errors() {
    let (_staging, service) = test_service().await;
    let node = service.config.node.clone();
    let seed = r#"mutation {
            create_AgentSession(input: {
                session_id: "s-internal-op",
                node_did: "did:test:foreign-agent",
                requester_did: "did:test:foreign-agent",
                agent_id: "did:test:grok-shim:default",
                created_at: "2026-08-31T22:46:45Z",
                closed_at: "2026-08-31T22:46:46Z"
            }) { _docID }
        }"#
    .to_string();
    let response = node.execute(&seed).await;
    ensure_no_errors(&response, "foreign session seed").expect("seed ok");

    let dispatch = service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({
                "cwd": "/tmp",
                "mcpServers": [],
                "_meta": { "sessionId": "s-internal-op" },
            }),
        ))
        .await;
    let response = parse_response(dispatch.response.as_deref().expect("response line"));
    assert_eq!(
        response["error"]["code"], -32603,
        "a durable identity mismatch is an operational internal error"
    );
}

#[tokio::test]
async fn notifications_never_produce_a_response_body() {
    let (_staging, service) = test_service().await;
    let dispatch = service
        .handle_acp_payload(&notification_payload(
            "session/cancel",
            json!({ "sessionId": "s1", "_meta": { "promptId": "p1" } }),
        ))
        .await;
    assert!(dispatch.response.is_none());
    assert!(dispatch.notifications.is_empty());

    let unknown = service
        .handle_acp_payload(&notification_payload("x.ai/unknown", json!({})))
        .await;
    assert!(
        unknown.response.is_none(),
        "unknown notifications are dropped"
    );
}
