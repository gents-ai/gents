#[tokio::test(start_paused = true)]
async fn run_loop_to_text_retract_discards_the_partial_before_the_resample() {
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
