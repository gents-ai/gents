#[tokio::test]
async fn single_turn_no_tools_yields_text_then_final() {
    let (_node, hook) = test_hook().await;
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("Hello ".to_string()),
        RawStreamingChoice::Message("world".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);

    let stream = run_loop_stream(
        model,
        Some(hook),
        Message::user("hi"),
        Vec::new(),
        Arc::new(Vec::new()),
        config(0),
    );
    futures::pin_mut!(stream);

    let mut texts = Vec::new();
    let mut final_text = None;
    while let Some(item) = stream.next().await {
        match item.expect("loop item should be Ok") {
            LoopStreamItem::Item(MultiTurnStreamItem::StreamAssistantItem(
                StreamedAssistantContent::Text(text),
            )) => {
                texts.push(text.text);
            }
            LoopStreamItem::Item(MultiTurnStreamItem::FinalResponse(final_response)) => {
                final_text = Some(final_response.response().to_string());
            }
            _ => {}
        }
    }

    assert_eq!(texts, vec!["Hello ".to_string(), "world".to_string()]);
    assert_eq!(final_text.as_deref(), Some("Hello world"));
}

#[tokio::test]
async fn unmet_output_obligation_blocks_terminal_and_continues_with_runtime_reminder() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::Message("premature answer".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("continuing".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let mut loop_config = config(2);
    loop_config.output_obligation_gate =
        Some(crate::agent::output_obligation::OutputObligationGate::new(
            node.clone(),
            "request-doc-unmet",
            vec![crate::agent::output_obligation::ActiveOutputObligation {
                tool_name: "write_result".to_string(),
                contract: crate::document_config::WriteToolOutputObligation {
                    scope: crate::document_config::WriteToolOutputObligationScope::Request,
                    minimum_writes: 1,
                    expected_count_field: None,
                },
            }],
        ));
    let stream = run_loop_stream(
        model.clone(),
        None,
        Message::user("do the work"),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    );
    futures::pin_mut!(stream);

    let mut saw_pending = false;
    let mut saw_final = false;
    while let Some(item) = stream.next().await {
        match item.unwrap() {
            LoopStreamItem::OutputObligationPending { reminder } => {
                saw_pending = true;
                assert!(format!("{reminder:?}").contains("write_result"));
            }
            LoopStreamItem::Item(MultiTurnStreamItem::FinalResponse(_)) => saw_final = true,
            LoopStreamItem::Item(MultiTurnStreamItem::StreamAssistantItem(
                StreamedAssistantContent::Text(text),
            )) if saw_pending && text.text == "continuing" => break,
            _ => {}
        }
    }

    assert!(saw_pending);
    assert!(!saw_final);
    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 2);
    assert!(format!("{:?}", histories[1]).contains("configured output obligation is unmet"));
    node.shutdown().await;
}

#[tokio::test]
async fn exceeding_max_turns_terminates_with_error() {
    let (_node, hook) = test_hook().await;
    let prompt = Message::user("loop");

    // max_turns = 0 permits one tool round-trip (2 completions, matching rig);
    // a model that keeps calling tools is blocked on the completion past the cap
    // and surfaces a max-turns error.
    let model = ScriptedModel::new_turns(vec![echo_tool_turn(), echo_tool_turn()]);
    let stream = run_loop_stream(
        model,
        Some(hook),
        prompt,
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        config(0),
    );
    futures::pin_mut!(stream);

    let mut items = Vec::new();
    while let Some(item) = stream.next().await {
        items.push(item);
    }

    let last = items.last().expect("stream should yield at least one item");
    assert!(last.is_err(), "expected a terminal error; got {last:?}");
    // Permanent `StreamingError::Prompt(MaxTurnsError)` (rig's variant), not a
    // retryable `Completion(ResponseError)` — turn exhaustion must not retry.
    let error = last.as_ref().err().unwrap();
    assert!(
        matches!(
            error,
            rig::agent::StreamingError::Prompt(prompt_error)
                if matches!(**prompt_error, rig::completion::PromptError::MaxTurnsError { .. })
        ),
        "expected a max-turns Prompt error; got {last:?}"
    );
    // And it must classify as a permanent failure: retrying turn exhaustion would
    // re-run the loop (and its tools) to no purpose.
    assert!(
        !crate::error::classify_completion_error(error).is_retryable(),
        "max-turns exhaustion must be non-retryable; got {last:?}"
    );
    // The Harbor adapter (scripts/harbor/run_gents.sh) classifies budget
    // exhaustion by matching the persisted error message's exact prefix:
    // `agent stream failed: ` (agent/daemon/inference.rs) followed by this
    // display. If rig's wording changes, MaxTurn trials silently revert to
    // Harbor infrastructure exceptions instead of verifier-scored attempts.
    assert!(
        error.to_string().starts_with("PromptError: MaxTurnError: "),
        "max-turns error display must start with the anchored Harbor prefix; got {error}"
    );
}

#[tokio::test]
async fn managed_terminal_tool_result_terminates_loop() {
    let (_node, hook) = test_hook().await;
    let prompt = Message::user("run the slow tool");

    // With the typed outcome channel a tool CANNOT fabricate a managed
    // terminal: run the loop with an already-expired request deadline so the
    // dispatcher's own envelope produces `ToolOutcome::TimedOut`, and
    // on_tool_result terminates the loop.
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(FixedTool {
        name: "echo".to_string(),
        output: "unreachable".to_string(),
    })];
    let model = ScriptedModel::new_turns(vec![echo_tool_turn()]);
    let stream = run_loop_stream(
        model,
        Some(hook),
        prompt,
        Vec::new(),
        Arc::new(tools),
        config(4),
    );
    futures::pin_mut!(stream);

    // The daemon installs the tool runtime scope around stream polling; an
    // already-expired deadline makes the dispatcher's envelope resolve the
    // tool call to `ToolOutcome::TimedOut`.
    let items = crate::tool_call_lifecycle::runtime::scope_request_tool_execution(
        Some(chrono::Utc::now() - chrono::Duration::seconds(1)),
        tokio_util::sync::CancellationToken::new(),
        async {
            let mut items = Vec::new();
            while let Some(item) = stream.next().await {
                items.push(item);
            }
            items
        },
    )
    .await;

    let last = items.last().expect("stream should yield at least one item");
    assert!(last.is_err(), "expected a terminal error; got {last:?}");
    assert!(
        format!("{:?}", last.as_ref().err().unwrap()).contains("deadline"),
        "expected a deadline/timeout terminate; got {last:?}"
    );
}

#[tokio::test]
async fn threaded_assistant_turn_carries_provider_message_id() {
    // P2a regression: the in-loop assistant message threaded back to the provider
    // must carry the provider message id (OpenAI Responses / ChatGPT Codex
    // follow-up requests reference prior `msg_` ids). Turn 1 emits a MessageId
    // plus a tool call; the tool result drives turn 2, whose request history must
    // contain the assistant tool-call message tagged with that id.
    let (_node, hook) = test_hook().await;

    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::MessageId("msg_abc123".to_string()),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "echo".to_string(),
                serde_json::json!({}),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);

    let stream = run_loop_stream(
        model.clone(),
        Some(hook),
        Message::user("go"),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        config(4),
    );
    futures::pin_mut!(stream);
    while stream.next().await.is_some() {}

    let histories = model.seen_histories().await;
    assert_eq!(
        histories.len(),
        2,
        "expected two completion turns; got {histories:?}"
    );
    let assistant_id = histories[1].iter().find_map(|message| match message {
        Message::Assistant { id, .. } => Some(id.clone()),
        _ => None,
    });
    assert_eq!(
        assistant_id,
        Some(Some("msg_abc123".to_string())),
        "threaded assistant turn must carry the provider message id; history: {:?}",
        histories[1]
    );
}
