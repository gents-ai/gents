#[tokio::test]
async fn run_loop_to_text_returns_the_final_assistant_reply() {
    // Ownership mapping: the durable assistant-reply regression that lived here
    // is owned by the StreamProcessor/canonical-writer layer
    // (crates/gents/src/agent/stream_processor/tests.rs, FinalResponse ->
    // publish_native_turn). This auxiliary only returns the final text.
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("the answer".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);

    let reply = run_loop_to_text(
        model,
        Message::user("the question"),
        Vec::new(),
        Arc::new(Vec::new()),
        config(0),
    )
    .await
    .expect("run_loop_to_text should succeed");
    assert_eq!(reply, "the answer");
}

#[tokio::test]
async fn run_loop_to_text_threads_tool_calls_and_returns_the_final_reply() {
    // Ownership mapping: the durable tool-transcript regression that lived here
    // (assistant tool-call turn + tool-result message persisted) is owned by the
    // StreamProcessor/canonical-writer layer
    // (crates/gents/src/agent/stream_processor/tests.rs, ToolCall ->
    // persist_inflight_assistant_turn, ToolResult -> pair closure). The
    // auxiliary must still surface tool returns through the stream and return
    // the final reply.
    let model = ScriptedModel::new_turns(vec![
        echo_tool_turn(),
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);

    let reply = run_loop_to_text(
        model,
        Message::user("use the echo tool"),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        config(4),
    )
    .await
    .expect("run_loop_to_text should succeed");
    assert_eq!(reply, "done");
}

#[tokio::test(start_paused = true)]
async fn run_loop_to_text_retract_discards_the_partial_before_the_resample() {
    // The nonpersistent one-shot consumer must reset its accumulator on
    // TurnRetracted so the returned final text is the resample, not the
    // retracted partial concatenated with it. The durable-side regression
    // (a retracted partial must never persist as an assistant message) is owned
    // by the StreamProcessor layer
    // (crates/gents/src/agent/stream_processor/tests.rs,
    // turn_retraction_resets_live_tail_and_discards_partial_assistant).
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
        Message::user("hi"),
        Vec::new(),
        Arc::new(Vec::new()),
        config(0),
    )
    .await
    .expect("run_loop_to_text should succeed after a mid-stream retract");
    assert_eq!(reply, "The answer is 42");
}
