#[tokio::test]
async fn generated_repeated_tool_failure_cases_drive_owned_loop() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().repeated_tool_failure_cases;
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let events = case["events"].as_array().unwrap();
        let actions = case["expected_actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|action| action.as_str().unwrap())
            .collect::<Vec<_>>();
        let ending = case["expected_ending"].as_str();
        let observed = &events[..actions.len()];
        let script = Arc::new(std::sync::Mutex::new(
            observed
                .iter()
                .zip(&actions)
                .filter(|(event, action)| {
                    **action == "dispatch" && event["outcome"] != "unknownTool"
                })
                .map(|(event, _)| event.clone())
                .collect::<std::collections::VecDeque<_>>(),
        ));
        let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
        let node = db.node.clone();
        let mut turns = observed
            .iter()
            .enumerate()
            .map(|(index, event)| {
                vec![
                    repeat_call(index, event),
                    RawStreamingChoice::FinalResponse(()),
                ]
            })
            .collect::<Vec<_>>();
        if ending.is_none() {
            turns.push(vec![
                RawStreamingChoice::Message("done".into()),
                RawStreamingChoice::FinalResponse(()),
            ]);
        }
        let model = ScriptedModel::new_turns(turns);
        let stream = run_loop_stream(
            model.clone(),
            Some(hook.clone()),
            TaggedMessage::unassociated(Message::user("exercise repeated failures")),
            Vec::new(),
            Arc::new(vec![Box::new(RepeatProbe {
                script: Arc::clone(&script),
            }) as Box<dyn ToolDyn>]),
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
        assert!(
            script.lock().unwrap().is_empty(),
            "{name}: every modeled dispatch must run"
        );
        let error = collected.error.as_deref();
        assert_eq!(error.is_some(), ending.is_some(), "{name}: {error:?}");
        match (ending, error) {
            (Some("repeatedFailure"), Some(error)) => {
                assert!(error.contains(REPEATED_TOOL_FAILURE_PREFIX), "{error}");
                assert!(error.contains("fixture error"), "{name}: {error}");
            }
            (Some("invalidExhausted"), Some(error)) => {
                assert!(
                    error.contains("invalid_tool_call_budget_exhausted:"),
                    "{error}"
                )
            }
            (None, None) => {}
            other => panic!("{name}: unexpected ending {other:?}"),
        }
        let answered = actions.iter().filter(|action| **action != "stop").count();
        assert_eq!(collected.tool_results.len(), answered, "{name}: results");
        for (result, action) in collected.tool_results.iter().zip(&actions) {
            assert_eq!(
                result.contains("repeated_tool_call_not_run"),
                *action == "suppress",
                "{name}: {result}"
            );
        }
        assert_eq!(
            model.seen_requests().await.len(),
            actions.len() + usize::from(ending.is_none()),
            "{name}: provider requests"
        );
        let rows = tool_call_rows(&lifecycle.access().unwrap()).await;
        let mut charged = 0;
        for (index, action) in actions.iter().enumerate() {
            if *action == "stop" {
                continue;
            }
            let row = rows
                .iter()
                .find(|row| row["tool_call_id"] == format!("repeat-{index}"))
                .unwrap_or_else(|| panic!("{name}: missing durable row {index}"));
            if matches!(
                row["tool_failure_class"].as_str(),
                Some("argumentInvalid" | "policyDenied")
            ) {
                charged += 1;
            }
            if *action == "suppress" {
                assert_eq!(row["lifecycle_state"], "failed", "{name}: {row}");
                assert_eq!(row["tool_failure_class"], "policyDenied", "{name}: {row}");
                let output = agent_tool_output(&lifecycle, row["_docID"].as_str().unwrap()).await;
                assert!(
                    output.contains("repeated_tool_call_not_run"),
                    "{name}: durable notice {output}"
                );
            }
        }
        assert_eq!(
            charged,
            case["expected_invalid_used"].as_u64().unwrap(),
            "{name}: invalid allowance"
        );
        if ending == Some("repeatedFailure") {
            lifecycle
                .terminalize_owned(
                    gents_session::request::lifecycle::RequestTerminalOutcome::Failed,
                    writer.terminal_output(&lifecycle.request().doc_id).await,
                    error,
                )
                .await
                .expect("terminal owner settles the accepted, undispatched repeat");
            let stop = tool_call_rows(&lifecycle.access().unwrap())
                .await
                .into_iter()
                .find(|row| row["tool_call_id"] == format!("repeat-{}", actions.len() - 1))
                .unwrap_or_else(|| panic!("{name}: accepted stop call is durable"));
            assert_eq!(stop["lifecycle_state"], "cancelled", "{name}: {stop}");
        }
        node.shutdown().await.expect("node shutdown");
    }
}
