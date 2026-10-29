use super::*;

#[test]
fn extension_methods_use_sdk_wire_prefix_and_normalize_at_dispatch() {
    for method in [
        MCP_INITIALIZED_METHOD,
        MODELS_UPDATE_METHOD,
        "x.ai/session_notification",
        SUBAGENT_GET_METHOD,
    ] {
        let encoded = wire_method(method);
        assert_eq!(encoded, format!("_{method}"));
        assert_eq!(
            wire_method(&encoded),
            encoded,
            "encoding must not double-prefix"
        );
        let request = AcpRequest::from_payload(
            &json!({
                "jsonrpc": "2.0", "id": 1, "method": encoded, "params": {},
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(request.method, method);
    }
    for method in [
        SESSION_PROMPT_METHOD,
        "_unknown/extension",
        "__x.ai/subagent/get",
    ] {
        assert_eq!(wire_method(method), method);
        let request = AcpRequest::from_payload(
            &json!({
                "jsonrpc": "2.0", "id": 1, "method": method,
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            request.method, method,
            "only the exact _x.ai/ namespace is normalized"
        );
    }
}

#[test]
fn shaped_stub_messages_name_the_owned_transition() {
    let interject = shaped_stub_error(
        INTERJECT_METHOD,
        &json!({ "sessionId": "s1", "text": "hi", "interjectionId": "i1" }),
    );
    let message = interject.to_string();
    assert!(
        message.contains("no formally specified injection transition"),
        "interject stub must explain the formal-transition gap: {message}"
    );
    assert!(
        message.contains("No AgentMessage or AgentRequest document was fabricated"),
        "interject stub must state that nothing was fabricated: {message}"
    );

    let compact = shaped_stub_error(COMPACT_CONVERSATION_METHOD, &json!({ "sessionId": "s1" }));
    let message = compact.to_string();
    assert!(
        message.contains("tokens_before") && message.contains("tokens_after"),
        "compact stub must name the invented fields it refuses: {message}"
    );
    assert!(
        message.contains("no usage counters"),
        "compact stub must name the missing AgentSession counters: {message}"
    );
}

#[test]
fn shaped_stub_maps_to_method_not_found_code() {
    let error = shaped_stub_error(SESSION_LOAD_METHOD, &json!({ "sessionId": "s1" }));
    assert_eq!(error_code_for(&error), -32601);
    let unknown = anyhow::Error::new(ShapedMethodNotFound {
        message: "unsupported".to_string(),
    });
    assert_eq!(error_code_for(&unknown), -32601);
}

#[test]
fn internal_and_param_errors_map_to_their_codes() {
    let missing_session = required_session_id(&json!({})).expect_err("missing session id");
    assert_eq!(error_code_for(&missing_session), -32602);
    let internal = anyhow::anyhow!("boom");
    assert_eq!(error_code_for(&internal), -32603);
    let internal_looking =
        anyhow::anyhow!("storage failed while reading session requires a non-empty sessionId");
    assert_eq!(error_code_for(&internal_looking), -32603);
    let typed = invalid_params("any message");
    assert_eq!(error_code_for(&typed), -32602);
    let parser_error = crate::commands::grok_shim::turn::wire::parse_prompt_request(&json!({}), None)
        .expect_err("missing params");
    assert!(
        parser_error.to_string().contains("session/prompt"),
        "the source parser message is preserved: {parser_error}"
    );
    let wrapped = invalid_params(parser_error.to_string());
    assert_eq!(error_code_for(&wrapped), -32602);
    assert_eq!(
        wrapped.to_string(),
        parser_error.to_string(),
        "the wrapped error message matches the source verbatim"
    );
}

#[tokio::test]
async fn interject_and_compact_answer_method_not_found() {
    let (_staging, service) = test_service().await;
    for (method, params) in [
        (
            "x.ai/interject",
            json!({ "sessionId": "s1", "text": "hi", "interjectionId": "i1" }),
        ),
        ("x.ai/compact_conversation", json!({ "sessionId": "s1" })),
    ] {
        let dispatch = service
            .handle_acp_payload(&request_payload(method, params))
            .await;
        assert!(dispatch.notifications.is_empty());
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(
            response["error"]["code"], -32601,
            "{method} must answer with the audited method-not-found code"
        );
        assert!(
            !response["error"]["message"]
                .as_str()
                .expect("message")
                .is_empty(),
            "{method} must carry the owned-transition explanation"
        );
    }
}

#[tokio::test]
async fn stubs_never_write_documents() {
    let (_staging, service) = test_service().await;
    service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-stubs" } }),
        ))
        .await;
    for (method, params) in [
        (
            "x.ai/interject",
            json!({ "sessionId": "s-stubs", "text": "hi", "interjectionId": "i1" }),
        ),
        (
            "x.ai/compact_conversation",
            json!({ "sessionId": "s-stubs" }),
        ),
    ] {
        service
            .handle_acp_payload(&request_payload(method, params))
            .await;
    }
    let node = service.config.node.clone();
    let query = r#"{
            AgentRequest(filter: { session_id: { _eq: "s-stubs" } }) { request_id }
            AgentMessage(filter: { session_id: { _eq: "s-stubs" } }) { sequence }
        }"#
    .to_string();
    let response = node.execute(&query).await;
    ensure_no_errors(&response, "stub document check").expect("query ok");
    let requests = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .expect("AgentRequest array");
    let messages = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(Value::as_array)
        .expect("AgentMessage array");
    assert!(
        requests.is_empty() && messages.is_empty(),
        "rejected stubs must not fabricate AgentRequest or AgentMessage rows"
    );
}

#[tokio::test]
async fn unknown_methods_answer_method_not_found() {
    let (_staging, service) = test_service().await;
    let dispatch = service
        .handle_acp_payload(&request_payload("terminal/invent", json!({})))
        .await;
    let response = parse_response(dispatch.response.as_deref().expect("response line"));
    assert_eq!(response["error"]["code"], -32601);
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("message")
            .contains("terminal/invent"),
        "the unrouted method must be named in the error"
    );
}

#[tokio::test]
async fn wait_for_exit_answers_the_pagers_exact_method_not_found_error() {
    let (_staging, service) = test_service().await;
    let dispatch = service
        .handle_acp_payload(&request_payload("terminal/wait_for_exit", json!({})))
        .await;
    let response = parse_response(dispatch.response.as_deref().expect("response line"));
    assert_eq!(response["error"]["code"], -32601);
    assert_eq!(
        response["error"]["message"], PAGER_WAIT_FOR_EXIT_MESSAGE,
        "wait_for_exit must answer with the pager's exact message"
    );
}

#[tokio::test]
async fn other_terminal_methods_answer_the_shaped_not_supported_error() {
    let (_staging, service) = test_service().await;
    for method in [
        "terminal/create",
        "terminal/output",
        "terminal/kill",
        "terminal/release",
    ] {
        let dispatch = service
            .handle_acp_payload(&request_payload(method, json!({})))
            .await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(response["error"]["code"], -32601, "{method}");
        let message = response["error"]["message"].as_str().expect("message");
        assert!(
            message.contains(&format!("{method}: ")),
            "{method} must be named with the shaped prefix: {message}"
        );
        assert!(
            message.contains("clientTerminal=false"),
            "{method} must explain the agent-side terminal routing: {message}"
        );
        assert!(
            !message.contains("is not supported by the Gents Grok shim"),
            "{method} must not collapse to the generic method-not-found: {message}"
        );
    }
}

#[tokio::test]
async fn undecodable_payloads_answer_invalid_request() {
    let (_staging, service) = test_service().await;
    let dispatch = service.handle_acp_payload("not json").await;
    let response = parse_response(dispatch.response.as_deref().expect("response line"));
    assert_eq!(response["id"], Value::Null);
    assert_eq!(response["error"]["code"], -32600);
}
