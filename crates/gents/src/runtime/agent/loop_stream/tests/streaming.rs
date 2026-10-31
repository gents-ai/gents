#[tokio::test]
async fn unmet_output_obligation_blocks_terminal_and_continues_with_runtime_reminder() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
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
    loop_config.output_obligation_gate = Some(Arc::new(
        crate::runtime::agent::output_obligation::OutputObligationGate::new(
            node.clone(),
            "request-doc-unmet",
            vec![crate::runtime::agent::output_obligation::ActiveOutputObligation {
                tool_name: "write_result".to_string(),
                contract: crate::documents::WriteToolOutputObligation {
                    scope: crate::documents::WriteToolOutputObligationScope::Request,
                    minimum_writes: 1,
                    expected_count_field: None,
                },
            }],
        ),
    )
        as Arc<dyn gents_loop::output_obligation::OutputObligationCheck>);
    let stream = run_loop_stream(
        model.clone(),
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(Message::user("do the work")),
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
            LoopStreamItem::Final { .. } => saw_final = true,
            LoopStreamItem::Text(text) if saw_pending && text == "continuing" => break,
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
    let prompt = Message::user("loop");

    let model = ScriptedModel::new_turns(vec![echo_tool_turn(), echo_tool_turn()]);
    let stream = run_loop_stream(
        model,
        None::<gents_loop::session_hook::NoopSessionHook>,
        TaggedMessage::unassociated(prompt),
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
    let error = last.as_ref().err().unwrap();
    assert_eq!(
        error.cause(),
        &crate::error::LoopFailureCause::MaxTurns,
        "expected a max-turns Prompt error; got {last:?}"
    );
    assert!(
        !error.classify().is_retryable(),
        "max-turns exhaustion must be non-retryable; got {last:?}"
    );
    // Harbor classifies budget exhaustion by matching the persisted error message prefix.
    assert!(
        error.to_string().starts_with("PromptError: MaxTurnError: "),
        "max-turns error display must start with the anchored Harbor prefix; got {error}"
    );
}

#[tokio::test]
async fn managed_terminal_tool_result_terminates_loop() {
    let (_node, hook, writer, mut lifecycle) = owned_test_hook().await;
    let prompt = Message::user("run the slow tool");

    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(FixedTool {
        name: "echo".to_string(),
        output: "unreachable".to_string(),
    })];
    let model = ScriptedModel::new_turns(vec![echo_tool_turn()]);
    let stream = run_loop_stream(
        model,
        Some(hook.clone()),
        TaggedMessage::unassociated(prompt),
        Vec::new(),
        Arc::new(tools),
        owned_config(4),
    );

    let collected = crate::tools::call::lifecycle::runtime::scope_request_tool_execution(
        Some(chrono::Utc::now() - chrono::Duration::seconds(1)),
        tokio_util::sync::CancellationToken::new(),
        Box::pin(collect_owned_scripted_stream(
            stream,
            &hook,
            &writer,
            &mut lifecycle,
            gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
        )),
    )
    .await;

    let error = collected
        .error
        .as_deref()
        .expect("stream should end with a terminal error");
    assert!(
        error.contains("PromptCancelled"),
        "expected the typed PromptCancelled terminal; got {error}"
    );
    assert!(
        error.contains("tool call deadline exceeded"),
        "expected the precise managed-timeout reason; got {error}"
    );
}

#[tokio::test]
async fn unavailable_goal_attribution_stops_the_provider_before_another_turn() {
    let (_node, hook, writer, mut lifecycle) = owned_test_hook().await;
    let hook = hook.with_goal_tool_authority(true, true);
    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "goal-call".into(),
                crate::goal::GET_GOAL_TOOL_NAME.into(),
                serde_json::json!({}),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("repeat the failed status".into()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let stream = run_loop_stream(
        model.clone(),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("check the Goal")),
        Vec::new(),
        Arc::new(vec![Box::new(FixedTool {
            name: crate::goal::GET_GOAL_TOOL_NAME.into(),
            output: "must be intercepted".into(),
        }) as Box<dyn ToolDyn>]),
        owned_config(4),
    );
    let collected = collect_owned_scripted_stream(
        stream,
        &hook,
        &writer,
        &mut lifecycle,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
    )
    .await;
    let error = collected.error.expect("unavailable attribution must terminate");
    assert!(error.contains("PromptCancelled"), "{error}");
    assert!(error.contains("attribution"), "{error}");
    assert_eq!(model.seen_histories().await.len(), 1);
}

#[tokio::test]
async fn threaded_assistant_turn_carries_provider_message_id() {
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
        None::<gents_loop::session_hook::NoopSessionHook>,
        TaggedMessage::unassociated(Message::user("go")),
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

#[tokio::test]
async fn retry_entry_does_not_publish_recorded_prompt_again() {
    for case in &crate::lean_vocab_test::lean_contract_snapshot().retry_entry_cases {
        let model = ScriptedModel::new(vec![
            RawStreamingChoice::Message("continued".into()),
            RawStreamingChoice::FinalResponse(()),
        ]);
        let mut loop_config = config(1);
        loop_config.resume_from_history = case["resume"].as_bool().unwrap();
        let context = crate::inference::rendered_request::RenderedRequestContext {
            request_doc_id: "retry-input-doc".into(),
            request_commit_cid: "bafy-retry".into(),
            request_id: "retry-input".into(),
            node_did: "did:key:agent".into(),
            requester_did: String::new(),
            agent_id: "general".into(),
            session_id: "retry-session".into(),
            model_name: "test-model".into(),
            provider_family: None,
        };
        let scope = crate::inference::rendered_request::scope::test_scope(
            context,
            Arc::new(|_| Box::pin(async { Ok(()) })),
        );
        loop_config.on_rendered_request =
            Some(crate::inference::rendered_request::scope::ambient_arming_sink(
                crate::inference::rendered_request::scope::CaptureScopeKind::Inference,
            ));
        let authored = crate::inference::rendered_request::scope::scope_request(scope, async {
            let stream = run_loop_stream(
                model,
                Some(gents_loop::session_hook::NoopSessionHook),
                TaggedMessage::unassociated(Message::user("durable frontier")),
                Vec::new(),
                Arc::new(Vec::new()),
                loop_config,
            );
            futures::pin_mut!(stream);
            let mut authored = 0;
            while let Some(item) = stream.next().await {
                match item.unwrap() {
                    LoopStreamItem::AuthoredInputReady { reply, .. } => {
                        authored += 1;
                        let _ = reply.send(Ok(Vec::new()));
                    }
                    LoopStreamItem::SteeringBoundary { reply } => {
                        let _ = reply.send(Ok(Vec::new()));
                    }
                    LoopStreamItem::FinishOrIntake { reply, .. } => {
                        let _ = reply.send(Ok(gents_loop::loop_stream::FinishOrIntake::Finished));
                    }
                    _ => {}
                }
            }
            authored
        })
        .await;
        assert_eq!(
            authored,
            usize::from(case["publish"].as_bool().unwrap()),
            "{case}"
        );
    }
}
