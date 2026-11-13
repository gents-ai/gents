#[tokio::test]
async fn generated_invalid_tool_progress_cases_drive_owned_loop() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().invalid_tool_progress_cases;
    assert_eq!(cases.len(), 11);
    for case in cases {
        let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
        let node = db.node.clone();
        let outcomes = &case.outcomes;
        let mut turns = outcomes
            .iter()
            .enumerate()
            .map(|(index, outcome)| {
                vec![
                    invalid_progress_call(index, outcome.as_str()),
                    RawStreamingChoice::FinalResponse(()),
                ]
            })
            .collect::<Vec<_>>();
        turns.push(vec![
            RawStreamingChoice::Message("done".into()),
            RawStreamingChoice::FinalResponse(()),
        ]);
        let model = ScriptedModel::new_turns(turns);
        let stream = run_loop_stream(
            model.clone(),
            Some(hook.clone()),
            TaggedMessage::unassociated(Message::user("exercise typed outcomes")),
            Vec::new(),
            Arc::new(vec![Box::new(InvalidProgressProbe) as Box<dyn ToolDyn>]),
            owned_config(64),
        );
        let collected = collect_owned_scripted_stream(
            stream,
            &hook,
            &writer,
            &mut lifecycle,
            gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
        )
        .await;
        let name = case.name.as_str();
        let actions = case
            .composed_actions
            .iter()
            .map(|action| action.as_str())
            .collect::<Vec<_>>();
        let ending = case.composed_ending.as_deref();
        let error = collected.error.as_deref();
        assert_eq!(error.is_some(), ending.is_some(), "{name}: {error:?}");
        match (ending, error) {
            (Some("invalidExhausted"), Some(error)) => {
                assert!(
                    error.contains("invalid_tool_call_budget_exhausted:"),
                    "{error}"
                )
            }
            (Some("repeatedFailure"), Some(error)) => {
                assert!(error.contains(REPEATED_TOOL_FAILURE_PREFIX), "{error}")
            }
            (None, None) => {}
            other => panic!("{name}: unexpected ending {other:?}"),
        }
        let answered = actions.iter().filter(|action| **action != "stop").count();
        assert_eq!(
            collected.tool_results.len(),
            answered,
            "{name} must emit the last result before failing"
        );
        assert_eq!(
            model.seen_requests().await.len(),
            actions.len() + usize::from(ending.is_none()),
            "{name} must not dispatch a suffix"
        );
        let rows = tool_call_rows(&lifecycle.access().unwrap()).await;
        let answered_rows = (0..answered)
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
        assert_eq!(
            charged as u64, case.composed_invalid_used,
            "{name} typed outcome mapping"
        );
        for (index, row) in answered_rows.iter().enumerate() {
            let output = agent_tool_output(&lifecycle, row["_docID"].as_str().unwrap()).await;
            assert!(
                !output.is_empty(),
                "{name}: canonical output for {index} must not be empty"
            );
        }
        node.shutdown().await.expect("node shutdown");
    }
}
