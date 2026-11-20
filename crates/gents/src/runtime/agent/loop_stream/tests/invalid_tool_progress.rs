#[tokio::test]
async fn invalid_tool_progress_composition_drives_owned_loop() {
    let cases = gents_lean_vocab::families::invalid_tool_progress_contracts_composed::cases::<
        String,
        String,
        String,
    >();
    for case in cases {
        let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
        let node = db.node.clone();
        let name = case.input.name.clone();
        let mut turns = case
            .input
            .outcomes
            .iter()
            .enumerate()
            .map(|(index, outcome)| {
                let fixture = match outcome.as_str() {
                    "invalid_arguments" => "invalidArguments",
                    "policy_denied" => "policyDenied",
                    "unknown_tool" => "unknownTool",
                    "success" => "success",
                    "ordinary_failure" => "ordinaryFailure",
                    other => panic!("{name}: no fixture outcome for {other}"),
                };
                vec![
                    invalid_progress_call(index, fixture),
                    RawStreamingChoice::FinalResponse(()),
                ]
            })
            .collect::<Vec<_>>();
        turns.push(vec![
            RawStreamingChoice::Message("done".into()),
            RawStreamingChoice::FinalResponse(()),
        ]);
        let model = scripted(turns);
        let mut config = owned_config_for(64, &lifecycle);
        config.max_invalid_tool_calls = case.input.max_invalid_tool_calls.map_or(
            gents_loop::loop_stream::DEFAULT_MAX_INVALID_TOOL_CALLS,
            |limit| usize::try_from(limit).unwrap(),
        );
        let stream = run_loop_stream(
            model.clone(),
            Some(hook.clone()),
            TaggedMessage::unassociated(Message::user("exercise typed outcomes")),
            Vec::new(),
            Arc::new(vec![Box::new(InvalidProgressProbe) as Box<dyn ToolDyn>]),
            config,
        );
        let collected = collect_owned_scripted_stream(
            stream,
            &hook,
            &writer,
            &mut lifecycle,
            gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
        )
        .await;
        let ending = collected.error.as_deref().map(|error| {
            if error.contains("invalid_tool_call_budget_exhausted:") {
                "invalid_exhausted".to_string()
            } else if error.contains(REPEATED_TOOL_FAILURE_PREFIX) {
                "repeated_failure".to_string()
            } else {
                panic!("{name}: unexpected loop error {error}")
            }
        });
        let calls = model.requests().len() - usize::from(ending.is_none());
        let actions = (0..calls.max(collected.tool_results.len()))
            .map(|index| match collected.tool_results.get(index) {
                Some(result) if result.contains("repeated_tool_call_not_run") => {
                    "suppress".to_string()
                }
                Some(_) => "dispatch".to_string(),
                None => "stop".to_string(),
            })
            .collect::<Vec<_>>();
        let rows = tool_call_rows(&lifecycle.access().unwrap()).await;
        let answered_rows = (0..collected.tool_results.len())
            .map(|index| {
                rows.iter()
                    .find(|row| row["tool_call_id"] == format!("invalid-progress-{index}"))
                    .unwrap_or_else(|| panic!("{name}: missing durable row {index}"))
            })
            .collect::<Vec<_>>();
        assert!(answered_rows.iter().all(|row| matches!(
            row["lifecycle_state"].as_str(),
            Some("completed" | "failed")
        )));
        let charged = answered_rows
            .iter()
            .filter(|row| {
                matches!(
                    row["tool_failure_class"].as_str(),
                    Some("argumentInvalid" | "policyDenied")
                )
            })
            .count();
        for (index, row) in answered_rows.iter().enumerate() {
            let output = agent_tool_output(&lifecycle, row["_docID"].as_str().unwrap()).await;
            assert!(
                !output.is_empty(),
                "{name}: canonical output for {index} must not be empty"
            );
        }
        node.shutdown().await.expect("node shutdown");
        case.check(
            gents_lean_vocab::families::invalid_tool_progress_contracts_composed::ComposedOutput {
                composed_actions: actions,
                composed_ending: ending,
                composed_invalid_used: charged as u64,
            },
        );
    }
}
