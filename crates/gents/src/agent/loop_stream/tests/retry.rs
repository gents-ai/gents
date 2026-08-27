#[tokio::test(start_paused = true)]
async fn pre_stream_transport_failure_retries_and_succeeds() {
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::FailStream(transient_provider_error("first")),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("recovered".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);

    let stream = run_loop_stream(
        model.clone(),
        None,
        Message::user("hi"),
        Vec::new(),
        Arc::new(Vec::new()),
        config(0),
    );
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(collected.final_text.as_deref(), Some("recovered"));
    assert_eq!(collected.error, None);
    assert_eq!(collected.attempts.len(), 1);
    assert_eq!(collected.attempts[0].turn, 0);
    assert_eq!(collected.attempts[0].attempt, 0);
    assert!(collected.attempts[0].will_retry);
    assert_duration_in_range(collected.attempts[0].backoff, 3_750, 6_250);

    let histories = model.seen_histories().await;
    assert_eq!(
        histories.len(),
        2,
        "one failed attempt plus one successful retry"
    );
    assert_eq!(
        histories[0], histories[1],
        "transport retry must reissue the identical provider request"
    );
}

#[tokio::test(start_paused = true)]
async fn transport_ladder_exhaustion_fails_with_last_error() {
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::FailStream(transient_provider_error("still down 1")),
        ScriptedCall::FailStream(transient_provider_error("still down 2")),
        ScriptedCall::FailStream(transient_provider_error("still down 3")),
        ScriptedCall::FailStream(transient_provider_error("still down 4")),
    ]);

    let stream = run_loop_stream(
        model,
        None,
        Message::user("hi"),
        Vec::new(),
        Arc::new(Vec::new()),
        config(0),
    );
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(collected.final_text, None);
    assert_eq!(collected.attempts.len(), 4);
    assert!(collected.attempts[..3]
        .iter()
        .all(|attempt| attempt.will_retry));
    assert!(!collected.attempts[3].will_retry);
    assert_eq!(collected.attempts[3].attempt, 3);
    assert_eq!(collected.attempts[3].backoff, Duration::ZERO);
    let error = collected
        .error
        .expect("retry exhaustion should end in error");
    assert!(
        error.contains("completion retry budget exhausted") && error.contains("still down 4"),
        "terminal error must include budget exhaustion and the last provider error; got {error}"
    );
}

#[tokio::test(start_paused = true)]
async fn three_minute_outage_recovers_within_ladder() {
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::FailStream(transient_provider_error("outage 1")),
        ScriptedCall::FailStream(transient_provider_error("outage 2")),
        ScriptedCall::FailStream(transient_provider_error("outage 3")),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("back".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);

    let stream = run_loop_stream(
        model,
        None,
        Message::user("hi"),
        Vec::new(),
        Arc::new(Vec::new()),
        config(0),
    );
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(collected.final_text.as_deref(), Some("back"));
    assert_eq!(collected.error, None);
    assert_eq!(collected.attempts.len(), 3);
    assert!(collected.attempts.iter().all(|attempt| attempt.will_retry));
    let total_backoff = collected
        .attempts
        .iter()
        .fold(Duration::ZERO, |total, attempt| total + attempt.backoff);
    assert_duration_in_range(total_backoff, 116_250, 193_750);
}

#[tokio::test(start_paused = true)]
async fn parse_400_resamples_once_then_repairs_on_identical_error() {
    let poison = format!("bad{}value", '\u{0007}');
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::Turn(vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "echo".to_string(),
                serde_json::json!({ "note": poison }),
            )),
            RawStreamingChoice::FinalResponse(()),
        ]),
        ScriptedCall::FailStream(parse_400_error("same")),
        ScriptedCall::FailStream(parse_400_error("same")),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("repaired".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);
    let mut loop_config = config(4);
    loop_config.context_message = Some(Message::user("<context>\nrepair-test\n</context>"));

    let stream = run_loop_stream(
        model.clone(),
        None,
        Message::user("use the echo tool"),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        loop_config,
    );
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(collected.final_text.as_deref(), Some("repaired"));
    assert_eq!(collected.error, None);
    assert_eq!(collected.attempts.len(), 2);
    assert!(collected.attempts.iter().all(|attempt| attempt.will_retry));
    assert_duration_in_range(collected.attempts[0].backoff, 3_750, 6_250);
    assert_eq!(collected.attempts[1].backoff, Duration::ZERO);

    let histories = model.seen_histories().await;
    assert_eq!(
        histories.len(),
        4,
        "tool turn, parse failure, resample, and repaired retry"
    );
    assert_eq!(
        histories[1], histories[2],
        "first parse-400 retry must resample the same provider request"
    );
    assert!(
        history_has_control_char_tool_arg(&histories[1]),
        "dirty tool arguments should be present before repair: {:?}",
        histories[1]
    );
    assert!(
        !history_has_control_char_tool_arg(&histories[3]),
        "repair must sanitize provider-bound tool arguments: {:?}",
        histories[3]
    );
    assert!(
        histories[3].iter().any(is_request_context_message),
        "repair must preserve the current request context: {:?}",
        histories[3]
    );
    assert_provider_request_invariants(4, &histories[3]);
}

#[tokio::test(start_paused = true)]
async fn first_stream_poll_parse_400_uses_pre_stream_retry_policy() {
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::TurnWithMidStreamError(Vec::new(), parse_400_error("same")),
        ScriptedCall::TurnWithMidStreamError(Vec::new(), parse_400_error("same")),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("repaired".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);

    let stream = run_loop_stream(
        model.clone(),
        None,
        Message::user("hi"),
        Vec::new(),
        Arc::new(Vec::new()),
        config(0),
    );
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(collected.final_text.as_deref(), Some("repaired"));
    assert_eq!(collected.error, None);
    assert_eq!(collected.attempts.len(), 2);
    assert!(collected.attempts.iter().all(|attempt| attempt.will_retry));
    assert_duration_in_range(collected.attempts[0].backoff, 3_750, 6_250);
    assert_eq!(collected.attempts[1].backoff, Duration::ZERO);

    let histories = model.seen_histories().await;
    assert_eq!(
        histories.len(),
        3,
        "first-poll parse failure, resample, and repaired retry"
    );
    assert_eq!(
        histories[0], histories[1],
        "first parse-400 retry must resample the same provider request"
    );
}

#[tokio::test(start_paused = true)]
async fn permanent_400_fails_immediately() {
    let model =
        ScriptedModel::new_calls(vec![ScriptedCall::FailStream(permanent_provider_error())]);

    let stream = run_loop_stream(
        model,
        None,
        Message::user("hi"),
        Vec::new(),
        Arc::new(Vec::new()),
        config(0),
    );
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(collected.final_text, None);
    assert_eq!(collected.attempts.len(), 1);
    assert!(!collected.attempts[0].will_retry);
    assert_eq!(collected.attempts[0].backoff, Duration::ZERO);
    let error = collected.error.expect("permanent 400 should fail");
    assert!(
        error.contains("duplicate field max_tokens")
            && !error.contains("completion retry budget exhausted"),
        "permanent 400 should not be retried or wrapped as budget exhaustion; got {error}"
    );
}

#[tokio::test(start_paused = true)]
async fn deadline_fail_fast_pre_sleep() {
    let model = ScriptedModel::new_calls(vec![ScriptedCall::FailStream(transient_provider_error(
        "too late",
    ))]);
    let mut loop_config = config(0);
    loop_config.retry_policy = crate::agent::completion_retry::CompletionRetryPolicy {
        transport_backoff: vec![Duration::from_secs(30)],
        max_resample: 0,
        allow_repair: false,
    };
    loop_config.deadline = Some(chrono::Utc::now() + chrono::Duration::seconds(10));

    let stream = run_loop_stream(
        model,
        None,
        Message::user("hi"),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    );
    futures::pin_mut!(stream);
    let started_at = tokio::time::Instant::now();

    let first = stream
        .next()
        .await
        .expect("deadline failure should yield attempt event")
        .expect("attempt event should be Ok");
    match first {
        LoopStreamItem::AttemptFailed {
            attempt,
            will_retry,
            backoff,
            ..
        } => {
            assert_eq!(attempt, 0);
            assert!(!will_retry);
            assert_eq!(backoff, Duration::ZERO);
        }
        other => panic!("expected AttemptFailed, got {other:?}"),
    }

    let second = stream
        .next()
        .await
        .expect("deadline failure should yield terminal error");
    assert!(
        second.is_err(),
        "expected terminal deadline error: {second:?}"
    );
    assert_eq!(
        tokio::time::Instant::now(),
        started_at,
        "deadline fail-fast must not sleep before failing"
    );
}

#[tokio::test(start_paused = true)]
async fn retry_reissues_same_request() {
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::FailStream(transient_provider_error("reset")),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("ok".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);

    let stream = run_loop_stream(
        model.clone(),
        None,
        Message::user("use the tool"),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        config(1),
    );
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(collected.final_text.as_deref(), Some("ok"));
    assert_eq!(collected.error, None);
    let histories = model.seen_histories().await;
    let tools = model.seen_tools().await;
    assert_eq!(histories.len(), 2);
    assert_eq!(tools.len(), 2);
    assert_eq!(histories[0], histories[1]);
    assert_eq!(tools[0], tools[1]);
}

#[tokio::test(start_paused = true)]
async fn mid_stream_decode_error_without_effects_retracts_and_resamples() {
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::TurnWithMidStreamError(
            vec![RawStreamingChoice::Message("Hel".to_string())],
            transient_provider_error("decode"),
        ),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("Hello ".to_string()),
            RawStreamingChoice::Message("world".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);

    let stream = run_loop_stream(
        model.clone(),
        None,
        Message::user("hi"),
        Vec::new(),
        Arc::new(Vec::new()),
        config(0),
    );
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(
        collected.text_chunks,
        vec!["Hel".to_string(), "Hello ".to_string(), "world".to_string()]
    );
    assert_eq!(collected.retractions, vec![(0, 0)]);
    assert_eq!(collected.final_text.as_deref(), Some("Hello world"));
    assert_eq!(collected.error, None);

    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 2);
    assert_eq!(
        histories[0], histories[1],
        "mid-stream retraction must reissue the same turn request"
    );
}

#[tokio::test(start_paused = true)]
async fn reasoning_only_completion_retracts_and_resamples() {
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::Turn(vec![
            RawStreamingChoice::ReasoningDelta {
                id: None,
                reasoning: "still thinking".to_string(),
            },
            RawStreamingChoice::FinalResponse(()),
        ]),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("finished answer".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);

    let stream = run_loop_stream(
        model.clone(),
        None,
        Message::user("solve this"),
        Vec::new(),
        Arc::new(Vec::new()),
        config(0),
    );
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(collected.retractions, vec![(0, 0)]);
    assert_eq!(collected.final_text.as_deref(), Some("finished answer"));
    assert_eq!(collected.error, None);
    assert_eq!(
        model.seen_histories().await.len(),
        2,
        "the reasoning-only turn must be resampled as the same provider turn"
    );
}

#[tokio::test(start_paused = true)]
async fn mid_stream_failure_after_tool_ran_closes_turn_and_continues() {
    let calls = Arc::new(AtomicUsize::new(0));
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::TurnWithMidStreamError(
            vec![RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "echo".to_string(),
                serde_json::json!({}),
            ))],
            transient_provider_error("decode after tool"),
        ),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(CountingTool {
        name: "echo".to_string(),
        output: "ECHOED".to_string(),
        calls: calls.clone(),
    })];

    let stream = run_loop_stream(
        model.clone(),
        None,
        Message::user("use the echo tool"),
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(collected.tool_results, vec!["ECHOED".to_string()]);
    assert_eq!(collected.final_text.as_deref(), Some("done"));
    assert_eq!(collected.error, None);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(collected.attempts.len(), 1);
    assert_eq!(collected.attempts[0].turn, 0);
    assert_eq!(collected.attempts[0].attempt, 0);
    assert!(collected.attempts[0].will_retry);
    assert_duration_in_range(collected.attempts[0].backoff, 3_750, 6_250);

    let histories = model.seen_histories().await;
    assert_eq!(
        histories.len(),
        2,
        "effectful mid-stream failure should close the turn then continue"
    );
    assert!(
        history_has_tool_call(&histories[1], "echo"),
        "continued request must include the assistant tool call: {:?}",
        histories[1]
    );
    assert!(
        history_has_tool_result_text(&histories[1], "ECHOED"),
        "continued request must include the tool result: {:?}",
        histories[1]
    );
}
