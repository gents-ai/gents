use super::*;

#[tokio::test]
async fn set_model_emits_models_update_and_rejects_unknown_model() {
    let (_staging, service) = test_service().await;
    service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-set-model" } }),
        ))
        .await;

    let rejected = service
        .handle_acp_payload(&request_payload(
            "session/set_model",
            json!({ "sessionId": "s-set-model", "modelId": "nope" }),
        ))
        .await;
    let response = parse_response(rejected.response.as_deref().expect("response line"));
    assert_eq!(
        response["error"]["code"], -32602,
        "a model outside the bound catalog is a client invalid-params error"
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("message")
            .contains("bound catalog"),
        "rejection must name the bound catalog"
    );

    let dispatch = service
        .handle_acp_payload(&request_payload(
            "session/set_model",
            json!({
                "sessionId": "s-set-model",
                "modelId": "GLM-5.3-NVFP4",
            }),
        ))
        .await;
    assert_eq!(dispatch.notifications.len(), 1);
    let notification = parse_response(&dispatch.notifications[0]);
    assert_eq!(notification["method"], "_x.ai/models/update");
    assert_eq!(notification["params"]["currentModelId"], "GLM-5.3-NVFP4");
    assert!(notification["params"].get("models").is_none());
    let response = parse_response(dispatch.response.as_deref().expect("response line"));
    assert_eq!(response["result"], json!({}), "set_model result is empty");
}

#[tokio::test]
async fn set_model_rejects_a_reasoning_effort_as_invalid_params() {
    let (_staging, service) = test_service().await;
    service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-effort" } }),
        ))
        .await;
    for effort in ["high", "extreme", "none", "low"] {
        let dispatch = service
            .handle_acp_payload(&request_payload(
                "session/set_model",
                json!({
                    "sessionId": "s-effort",
                    "modelId": "GLM-5.3-NVFP4",
                    "_meta": { "reasoningEffort": effort },
                }),
            ))
            .await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(
            response["error"]["code"], -32602,
            "reasoningEffort {effort:?} must be rejected as invalid params: the \
                     feature is unadvertised and unimplemented"
        );
        assert!(
            response["error"]["message"]
                .as_str()
                .expect("message")
                .contains("reasoningEffort"),
            "the rejection must name the unsupported key: {effort}"
        );
        assert!(
            dispatch.notifications.is_empty(),
            "a rejected set_model must not emit a catalog update"
        );
    }
}

#[tokio::test]
async fn set_mode_emits_current_mode_update() {
    let (_staging, service) = test_service().await;
    service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-set-mode" } }),
        ))
        .await;

    let dispatch = service
        .handle_acp_payload(&request_payload(
            "session/set_mode",
            json!({ "sessionId": "s-set-mode", "modeId": "yolo" }),
        ))
        .await;
    assert_eq!(dispatch.notifications.len(), 1);
    let notification = parse_response(&dispatch.notifications[0]);
    assert_eq!(notification["method"], "session/update");
    assert_eq!(
        notification["params"]["update"]["sessionUpdate"],
        "current_mode_update"
    );
    assert_eq!(notification["params"]["update"]["currentModeId"], "yolo");
    assert_eq!(notification["params"]["sessionId"], "s-set-mode");
    assert_eq!(
        notification["params"]["_meta"]["eventId"], "s-set-mode-1",
        "the MCP completion extension must not consume a session-update event id"
    );
    let response = parse_response(dispatch.response.as_deref().expect("response line"));
    assert_eq!(response["result"], json!({}), "set_mode result is empty");
}

#[tokio::test]
async fn event_ids_are_monotonic_and_session_scoped_through_one_sequencer() {
    let (_staging, service) = test_service().await;
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let sender = PromptSender::Buffer {
        buffer: buffer.clone(),
    };
    for _ in 0..3 {
        sender
            .send_user_message_chunk(
                service.projections.session_updates(),
                "s-events",
                "prompt-1",
                &PromptBlock {
                    kind: "text".to_string(),
                    text: "hello".to_string(),
                    meta: None,
                },
                0,
            )
            .await
            .expect("echo send");
    }
    let lines = buffer.lock().await;
    assert_eq!(lines.len(), 3, "three sends enqueue three lines");
    let event_ids: Vec<String> = lines
        .iter()
        .map(|line| {
            parse_response(line)["params"]["_meta"]["eventId"]
                .as_str()
                .expect("eventId")
                .to_string()
        })
        .collect();
    assert_eq!(event_ids, vec!["s-events-1", "s-events-2", "s-events-3"]);

    let other_lines = {
        let other_buffer = Arc::new(Mutex::new(Vec::new()));
        let other_sender = PromptSender::Buffer {
            buffer: other_buffer.clone(),
        };
        other_sender
            .send_user_message_chunk(
                service.projections.session_updates(),
                "s-other",
                "prompt-1",
                &PromptBlock {
                    kind: "text".to_string(),
                    text: "hello".to_string(),
                    meta: None,
                },
                0,
            )
            .await
            .expect("echo send");
        let lines = other_buffer.lock().await;
        lines
            .iter()
            .map(|line| {
                parse_response(line)["params"]["_meta"]["eventId"]
                    .as_str()
                    .expect("eventId")
                    .to_string()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(other_lines, vec!["s-other-1"]);
    assert_eq!(
        service
            .projections
            .sequencer_arc()
            .event_counter("s-events"),
        3
    );
    assert_eq!(
        service.projections.sequencer_arc().event_counter("s-other"),
        1
    );
}

#[tokio::test]
async fn set_mode_and_prompt_never_duplicate_an_event_id() {
    let (_staging, service) = test_service().await;
    service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-dup" } }),
        ))
        .await;
    let mode_dispatch = service
        .handle_acp_payload(&request_payload(
            "session/set_mode",
            json!({ "sessionId": "s-dup", "modeId": "yolo" }),
        ))
        .await;
    let mode_notification = parse_response(&mode_dispatch.notifications[0]);
    let mode_event_id = mode_notification["params"]["_meta"]["eventId"]
        .as_str()
        .expect("mode eventId")
        .to_string();

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let sender = PromptSender::Buffer {
        buffer: buffer.clone(),
    };
    sender
        .send_user_message_chunk(
            service.projections.session_updates(),
            "s-dup",
            "prompt-1",
            &crate::commands::grok_shim::turn::wire::PromptBlock {
                kind: "text".to_string(),
                text: "hello".to_string(),
                meta: None,
            },
            0,
        )
        .await
        .expect("echo send");
    let lines = buffer.lock().await;
    let echo: Value = serde_json::from_str(&lines[0]).expect("echo line");
    let echo_event_id = echo["params"]["_meta"]["eventId"]
        .as_str()
        .expect("echo eventId")
        .to_string();

    assert_ne!(mode_event_id, echo_event_id);
    assert_eq!(mode_event_id, "s-dup-1");
    assert_eq!(echo_event_id, "s-dup-2");
}

#[tokio::test]
async fn set_model_and_set_mode_require_a_known_session() {
    let (_staging, service) = test_service().await;
    let set_model = service
        .handle_acp_payload(&request_payload(
            "session/set_model",
            json!({ "sessionId": "missing", "modelId": "GLM-5.3-NVFP4" }),
        ))
        .await;
    assert!(
        parse_response(set_model.response.as_deref().expect("response"))
            .get("error")
            .is_some(),
        "set_model on an unknown session must fail"
    );
    let set_mode = service
        .handle_acp_payload(&request_payload(
            "session/set_mode",
            json!({ "sessionId": "missing", "modeId": "yolo" }),
        ))
        .await;
    assert!(
        parse_response(set_mode.response.as_deref().expect("response"))
            .get("error")
            .is_some(),
        "set_mode on an unknown session must fail"
    );
}
