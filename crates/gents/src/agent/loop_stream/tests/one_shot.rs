#[tokio::test]
async fn run_loop_to_text_persists_assistant_reply() {
    // Regression: one-shot (run_loop_to_text) must persist the assistant reply,
    // not just the user prompt.
    let (node, hook) = test_hook().await;
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("the answer".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);

    let reply = run_loop_to_text(
        model,
        Some(hook.clone()),
        Message::user("the question"),
        Vec::new(),
        Arc::new(Vec::new()),
        config(0),
    )
    .await
    .expect("run_loop_to_text should succeed");
    assert_eq!(reply, "the answer");

    let session_id = hook.session_id().await.expect("session id");
    let history = crate::session::load_history(&node, &session_id)
        .await
        .unwrap();
    assert!(
        history.iter().any(|message| matches!(message,
            Message::Assistant { content, .. }
                if content.iter().any(|c| matches!(c, AssistantContent::Text(text)
                    if text.text == "the answer")))),
        "one-shot must persist the assistant reply; history: {history:?}"
    );
}

#[tokio::test]
async fn run_loop_to_text_persists_tool_using_transcript() {
    // Regression: for tool-using one-shots, both the assistant tool-call turn and
    // the tool-result message must be persisted (tool-result persistence gates on
    // the assistant turn being persisted first).
    let (node, hook) = test_hook().await;
    let model = ScriptedModel::new_turns(vec![
        echo_tool_turn(),
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);

    let reply = run_loop_to_text(
        model,
        Some(hook.clone()),
        Message::user("use the echo tool"),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        config(4),
    )
    .await
    .expect("run_loop_to_text should succeed");
    assert_eq!(reply, "done");

    let session_id = hook.session_id().await.expect("session id");
    let history = crate::session::load_history(&node, &session_id)
        .await
        .unwrap();
    assert!(
        history.iter().any(|message| matches!(message,
            Message::User { content }
                if content.iter().any(|c| matches!(c, UserContent::ToolResult(result)
                    if tool_result_text(first_content(&result.content)) == "ECHOED")))),
        "tool-using one-shot must persist the tool-result message; history: {history:?}"
    );
    assert!(
        history.iter().any(|message| matches!(message,
            Message::Assistant { content, .. }
                if content.iter().any(|c| matches!(c, AssistantContent::Text(text)
                    if text.text == "done")))),
        "tool-using one-shot must persist the final assistant reply; history: {history:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn run_loop_to_text_retract_persists_only_the_resample() {
    // The one-shot consumer must reset its accumulator on TurnRetracted.
    // Without the reset, the
    // retracted partial ("Based on") concatenates with the resample and the
    // durable assistant message becomes "Based onThe answer is 42" — corrupting
    // the transcript that feeds future history and training capture, even though
    // the returned string is correct. This fences that exact regression.
    let (node, hook) = test_hook().await;
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::TurnWithMidStreamError(
            vec![RawStreamingChoice::Message("Based on".to_string())],
            transient_provider_error("decode"),
        ),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("The answer is 42".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);

    let reply = run_loop_to_text(
        model,
        Some(hook.clone()),
        Message::user("hi"),
        Vec::new(),
        Arc::new(Vec::new()),
        config(0),
    )
    .await
    .expect("run_loop_to_text should succeed after a mid-stream retract");
    assert_eq!(reply, "The answer is 42");

    let session_id = hook.session_id().await.expect("session id");
    let history = crate::session::load_history(&node, &session_id)
        .await
        .unwrap();
    let assistant_texts: Vec<String> = history
        .iter()
        .filter_map(|message| match message {
            Message::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .flatten()
        .filter_map(|content| match content {
            AssistantContent::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        assistant_texts,
        vec!["The answer is 42".to_string()],
        "retract must discard the partial; persisted assistant text: {assistant_texts:?}"
    );
}
