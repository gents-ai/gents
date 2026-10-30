use super::*;

#[test]
fn parse_prompt_request_reads_audited_meta() {
    let params = json ! ({ "sessionId" : "session-1" , "prompt" : [{ "type" : "text" , "text" : "hello" , "meta" : { "skillTokenRanges" : [[0 , 5]] } , } ,] , "_meta" : { "promptId" : "prompt-1" , "screenMode" : "fullscreen" , "sendNow" : true , } , });
    let request = parse_prompt_request(&params, Some(json!(7))).unwrap();
    assert_eq!(request.session_id, "session-1");
    assert_eq!(request.prompt_id.as_deref(), Some("prompt-1"));
    assert_eq!(request.screen_mode.as_deref(), Some("fullscreen"));
    assert!(request.send_now);
    assert_eq!(request.id, Some(json!(7)));
    assert_eq!(request.prompt.len(), 1);
    assert!(request.prompt[0].has_skill_token_ranges());
    assert_eq!(request.prompt[0].bash_command(), None);
}

#[test]
fn parse_prompt_request_defaults_optional_meta() {
    let params = json ! ({ "sessionId" : "session-1" , "prompt" : [text_block ("hi")] , });
    let request = parse_prompt_request(&params, None).unwrap();
    assert_eq!(request.prompt_id, None);
    assert_eq!(request.screen_mode, None);
    assert!(!request.send_now);
    assert!(!request.prompt[0].has_skill_token_ranges());
}

#[test]
fn parse_prompt_request_rejects_missing_session_id() {
    let params = json!({"prompt": [text_block("hi")]});
    assert!(parse_prompt_request(&params, None).is_err());
}

#[test]
fn parse_prompt_request_rejects_unknown_screen_mode() {
    let params = json!({
        "sessionId": "session-1",
        "prompt": [text_block("hi")],
        "_meta": {"screenMode": "sideways"},
    });
    assert!(parse_prompt_request(&params, None).is_err());
}

#[test]
fn parse_prompt_request_rejects_empty_prompt() {
    let params = json!({"sessionId": "session-1", "prompt": []});
    assert!(parse_prompt_request(&params, None).is_err());
}

#[test]
fn parse_prompt_request_rejects_block_without_text() {
    let params = json!({"sessionId": "session-1", "prompt": [{"type": "text"}]});
    assert!(parse_prompt_request(&params, None).is_err());
}

#[test]
fn parse_cancel_notification_reads_audited_meta() {
    let params = json ! ({ "sessionId" : "session-1" , "_meta" : { "cancelTrigger" : "user" , "rewindIfNoOutput" : true , "rewindIfPristine" : true , "promptId" : "prompt-1" , } , });
    let notification = parse_cancel_notification(&params).unwrap();
    assert_eq!(notification.session_id, "session-1");
    assert_eq!(notification.cancel_trigger.as_deref(), Some("user"));
    assert!(notification.rewind_if_no_output);
    assert!(notification.rewind_if_pristine);
    assert_eq!(notification.prompt_id.as_deref(), Some("prompt-1"));
    let meta = notification.meta();
    assert_eq!(meta["cancelTrigger"], json!("user"));
    assert_eq!(meta["rewindIfNoOutput"], json!(true));
    assert_eq!(meta["rewindIfPristine"], json!(true));
    assert_eq!(meta["promptId"], json!("prompt-1"));
}

#[test]
fn parse_cancel_notification_omits_absent_optional_keys() {
    let params = json ! ({ "sessionId" : "session-1" , "_meta" : { } });
    let notification = parse_cancel_notification(&params).unwrap();
    assert_eq!(notification.cancel_trigger, None);
    assert!(!notification.rewind_if_no_output);
    assert!(!notification.rewind_if_pristine);
    assert_eq!(notification.prompt_id, None);
    let meta = notification.meta();
    assert!(meta.get("cancelTrigger").is_none());
    assert!(meta.get("promptId").is_none());
}

#[test]
fn parse_cancel_notification_requires_session_id() {
    assert!(parse_cancel_notification(&json!({"_meta": {}})).is_err());
}

#[test]
fn stop_reason_projection_prefers_interrupted_lifecycle() {
    assert_eq!(
        stop_reason_from_rows("interrupted", None),
        Some(StopReason::Cancelled)
    );
    assert_eq!(
        stop_reason_from_rows("completed", None),
        Some(StopReason::EndTurn)
    );
    assert_eq!(
        stop_reason_from_rows("failed", None),
        Some(StopReason::Error)
    );
    assert_eq!(stop_reason_from_rows("processing", None), None);
}

#[test]
fn stop_reason_projection_maps_interrupted_at_marker() {
    assert_eq!(
        stop_reason_from_rows("processing", Some("2026-01-01T00:00:00Z")),
        None
    );
    assert_eq!(stop_reason_from_rows("processing", Some("  ")), None);
    assert_eq!(
        stop_reason_from_rows("superseded", Some("2026-01-01T00:00:00Z")),
        Some(StopReason::Cancelled)
    );
    assert_eq!(
        stop_reason_from_rows("superseded", None),
        Some(StopReason::Error)
    );
}

#[test]
fn prompt_result_reports_an_errored_turn_as_a_jsonrpc_error() {
    let error = prompt_result(TerminalOutcome {
        stop_reason: StopReason::Error,
        error_message: Some("fail-closed: tool_use observed (bash_unrestricked)".to_string()),
    })
    .unwrap_err();
    assert!(error.to_string().contains("bash_unrestricked"), "{error}");

    let error = prompt_result(StopReason::Error.into()).unwrap_err();
    assert_eq!(error.to_string(), "gents request failed");

    assert_eq!(
        prompt_result(StopReason::EndTurn.into()).unwrap(),
        json!({"stopReason": "end_turn"})
    );
    assert_eq!(
        prompt_result(StopReason::Cancelled.into()).unwrap(),
        json!({"stopReason": "cancelled"})
    );
}

#[test]
fn stop_reason_wire_names_are_audited_values() {
    assert_eq!(StopReason::EndTurn.wire_name(), "end_turn");
    assert_eq!(StopReason::Cancelled.wire_name(), "cancelled");
    assert_eq!(StopReason::Refusal.wire_name(), "refusal");
    assert_eq!(StopReason::Error.wire_name(), "error");
    assert_eq!(stop_reason_from_wire("cancelled"), StopReason::Cancelled);
    assert_eq!(stop_reason_from_wire("end_turn"), StopReason::EndTurn);
    assert_eq!(stop_reason_from_wire("nonsense"), StopReason::Cancelled);
}

#[test]
fn prompt_text_joins_text_blocks_and_preserves_other_kinds() {
    let request = PromptRequest {
        session_id: "s".to_string(),
        prompt: vec![
            PromptBlock {
                kind: "text".to_string(),
                text: "one".to_string(),
                meta: None,
            },
            PromptBlock {
                kind: "image".to_string(),
                text: "binary".to_string(),
                meta: Some(json!({"mime": "image/png"})),
            },
        ],
        prompt_id: None,
        screen_mode: None,
        send_now: false,
        id: None,
    };
    let text = prompt_text(&request);
    assert!(text.starts_with("one\n"));
    assert!(text.contains("\"type\":\"image\""));
}

#[tokio::test]
async fn transient_read_retry_budget_is_consecutive_and_bounded() {
    let transient = anyhow::Error::new(
        reqwest::Client::new()
            .post("http://127.0.0.1:1/api/v0/graphql")
            .send()
            .await
            .expect_err("nothing listens on port 1"),
    )
    .context("reading request terminal state");
    let fatal = anyhow::anyhow!("invalid query");
    // DefraDB's HTTP handler already retried auto-commit read conflicts.
    let mut fresh = 0;
    assert!(!register_transient_read_retry(
        &anyhow::anyhow!("transaction conflict. please retry"),
        &mut fresh
    ));
    let mut consecutive = 0;
    for expected in 1..=MAX_TRANSIENT_GRAPHQL_READ_RETRIES {
        assert!(register_transient_read_retry(&transient, &mut consecutive));
        assert_eq!(consecutive, expected);
    }
    assert!(!register_transient_read_retry(&transient, &mut consecutive));
    consecutive = 0;
    assert!(register_transient_read_retry(&transient, &mut consecutive));
    assert!(!register_transient_read_retry(&fatal, &mut consecutive));
}

#[tokio::test]
async fn user_echo_uses_audited_chunk_shape() {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let sender = PromptSender::Buffer {
        buffer: buffer.clone(),
    };
    let session_updates = SessionUpdateChannel::new(Arc::new(ProjectionSequencer::new()));
    sender
        .send_user_message_chunk(
            &session_updates,
            "session-1",
            "prompt-1",
            &PromptBlock {
                kind: "text".to_string(),
                text: "hello".to_string(),
                meta: None,
            },
            0,
        )
        .await
        .unwrap();
    let lines = buffer.lock().await;
    assert_eq!(lines.len(), 1);
    let value: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(value["method"], json!("session/update"));
    assert_eq!(value["params"]["sessionId"], json!("session-1"));
    assert_eq!(
        value["params"]["update"]["sessionUpdate"],
        json!("user_message_chunk")
    );
    assert_eq!(value["params"]["update"]["content"]["type"], json!("text"));
    assert_eq!(value["params"]["update"]["content"]["text"], json!("hello"));
    assert_eq!(value["params"]["update"]["_meta"]["promptIndex"], json!(0));
    assert_eq!(
        value["params"]["update"]["_meta"]["hideFromScrollback"],
        json!(false)
    );
    assert!(value["params"]["update"]["content"].get("meta").is_none());
    assert_eq!(value["params"]["_meta"]["promptId"], json!("prompt-1"));
    assert_eq!(value["params"]["_meta"]["isReplay"], json!(false));
    assert_eq!(value["params"]["_meta"]["totalTokens"], json!(0));
    assert_eq!(value["params"]["_meta"]["eventId"], json!("session-1-1"));
}
