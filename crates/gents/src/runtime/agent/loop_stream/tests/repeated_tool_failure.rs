#[tokio::test]
async fn repeated_tool_failures_follow_the_lean_model() {
    for case in gents_lean_vocab::families::repeated_tool_failure_contracts_owned_loop::cases::<
        String,
        String,
        String,
    >() {
        let label = format!("{:?}", case.input);
        let events = case
            .input
            .events
            .iter()
            .map(|event| serde_json::to_value(event).unwrap())
            .collect::<Vec<_>>();
        let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
        let node = db.node.clone();
        let mut turns = events
            .iter()
            .enumerate()
            .map(|(index, event)| {
                vec![
                    repeat_call(index, event),
                    RawStreamingChoice::FinalResponse(()),
                ]
            })
            .collect::<Vec<_>>();
        turns.push(vec![
            RawStreamingChoice::Message("done".into()),
            RawStreamingChoice::FinalResponse(()),
        ]);
        let model = ScriptedModel::new_turns(turns);
        let dispatched = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stream = run_loop_stream(
            model.clone(),
            Some(hook.clone()),
            TaggedMessage::unassociated(Message::user("exercise repeated failures")),
            Vec::new(),
            Arc::new(vec![Box::new(RepeatProbe {
                events: events.clone(),
                model: model.clone(),
                dispatched: Arc::clone(&dispatched),
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
        let error = collected.error.as_deref();
        let ending = error.map(|error| {
            if error.contains(REPEATED_TOOL_FAILURE_PREFIX) {
                assert!(error.contains("fixture error"), "{label}: {error}");
                "repeated_failure".to_string()
            } else if error.contains("invalid_tool_call_budget_exhausted:") {
                "invalid_exhausted".to_string()
            } else {
                panic!("{label}: unexpected ending {error}")
            }
        });
        let requests = model.seen_requests().await.len();
        let call_turns = requests - usize::from(ending.is_none());
        assert!(call_turns <= events.len(), "{label}: provider requests");
        let answered = collected.tool_results.len();
        assert!(
            answered == call_turns || answered + 1 == call_turns,
            "{label}: results"
        );
        let dispatched = dispatched.lock().unwrap().clone();
        let rows = tool_call_rows(&lifecycle.access().unwrap()).await;
        let mut actions = Vec::new();
        let mut charged = 0;
        for index in 0..call_turns {
            let Some(result) = collected.tool_results.get(index) else {
                actions.push("stop".to_string());
                continue;
            };
            let row = rows
                .iter()
                .find(|row| row["tool_call_id"] == format!("repeat-{index}"))
                .unwrap_or_else(|| panic!("{label}: missing durable row {index}"));
            if matches!(
                row["tool_failure_class"].as_str(),
                Some("argumentInvalid" | "policyDenied")
            ) {
                charged += 1;
            }
            if result.contains("repeated_tool_call_not_run") {
                assert_eq!(row["lifecycle_state"], "failed", "{label}: {row}");
                assert_eq!(row["tool_failure_class"], "policyDenied", "{label}: {row}");
                let output = agent_tool_output(&lifecycle, row["_docID"].as_str().unwrap()).await;
                assert!(
                    output.contains("repeated_tool_call_not_run"),
                    "{label}: durable notice {output}"
                );
                actions.push("suppress".to_string());
            } else if dispatched.contains(&index) || !row["tool_failure_class"].is_null() {
                actions.push("dispatch".to_string());
            } else {
                actions.push("skip".to_string());
            }
        }
        if ending.as_deref() == Some("repeated_failure") {
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
                .find(|row| row["tool_call_id"] == format!("repeat-{}", call_turns - 1))
                .unwrap_or_else(|| panic!("{label}: accepted stop call is durable"));
            assert_eq!(stop["lifecycle_state"], "cancelled", "{label}: {stop}");
        }
        node.shutdown().await.expect("node shutdown");
        case.check(
            gents_lean_vocab::families::repeated_tool_failure_contracts_owned_loop::Output {
                actions,
                ending,
                invalid_used: charged,
            },
        );
    }
}
