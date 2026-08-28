#[test]
fn generated_turn_budget_cases_drive_every_completion_dispatch() {
    let cases = crate::lean_vocab_test::lean_prompt_assembly_turn_budget_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no owned-loop turn budget cases"
    );

    for case in cases {
        let threshold = case.threshold_basis_points as f64 / 10_000.0;
        assert_eq!(
            crate::compaction::threshold_budget(case.context_window, threshold),
            case.configured_threshold_budget,
            "{}: configured threshold drifted from Lean",
            case.name
        );
        assert_eq!(
            crate::compaction::effective_input_budget(
                case.context_window,
                case.max_output_tokens,
                threshold,
            ),
            case.effective_input_budget,
            "{}: effective input budget drifted from Lean",
            case.name
        );
        let actual_output = case
            .turn_input_tokens
            .iter()
            .map(|tokens| {
                crate::compaction::effective_output_budget(
                    *tokens,
                    case.context_window,
                    case.max_output_tokens,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actual_output, case.turn_output_tokens,
            "{}: per-turn output clamp drifted from Lean",
            case.name
        );
        let actual = case
            .turn_input_tokens
            .iter()
            .map(|tokens| {
                crate::compaction::input_exceeds_budget(
                    *tokens,
                    case.context_window,
                    case.max_output_tokens,
                    threshold,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actual, case.turn_should_compact,
            "{}: a later completion turn bypassed the Lean dispatch gate",
            case.name
        );
    }
}

#[test]
fn generated_aggregate_token_budget_cases_drive_the_owned_loop_ledger() {
    let cases = crate::lean_vocab_test::lean_aggregate_token_budget_cases();
    assert_eq!(
        cases.len(),
        11,
        "Lean should emit the aggregate token-budget witness set"
    );

    for case in cases {
        let mut ledger = AggregateTokenLedger {
            limit: case.limit,
            used: case.used,
        };
        assert_eq!(
            ledger.effective_output_tokens(case.input_tokens, case.configured_max_output_tokens,),
            case.effective_output_tokens,
            "{}: dispatch clamp drifted from Lean",
            case.name
        );
        assert_eq!(
            ledger.can_dispatch(case.input_tokens, case.configured_max_output_tokens),
            case.can_dispatch,
            "{}: dispatch decision drifted from Lean",
            case.name
        );

        let usage = case.usage_present.then_some(Usage {
            input_tokens: case.reported_input_tokens,
            output_tokens: case.reported_output_tokens,
            total_tokens: case.reported_total_tokens,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
        });
        assert_eq!(
            usage
                .map(crate::provider_usage::charged_usage_total)
                .unwrap_or_default(),
            case.charged_tokens,
            "{}: charged total drifted from Lean",
            case.name
        );
        let actual = ledger.charge_reported(usage);
        let actual_name = match actual {
            AggregateTokenCharge::Missing => "missing",
            AggregateTokenCharge::Within => "within",
            AggregateTokenCharge::Exhausted => "exhausted",
            AggregateTokenCharge::Overrun => "overrun",
        };
        assert_eq!(
            actual_name, case.charge_result,
            "{}: charge classification drifted from Lean",
            case.name
        );
        assert_eq!(
            (actual != AggregateTokenCharge::Missing).then_some(ledger.used),
            case.next_used,
            "{}: post-charge ledger drifted from Lean",
            case.name
        );
        let actual_action = match aggregate_post_charge_action(actual, case.terminal_valid) {
            AggregatePostChargeAction::Continue => "continue",
            AggregatePostChargeAction::Succeed => "succeed",
            AggregatePostChargeAction::Fail => "fail",
        };
        assert_eq!(
            actual_action, case.post_charge_action,
            "{}: post-charge terminal legality drifted from Lean",
            case.name
        );
    }
}

#[tokio::test]
async fn aggregate_budget_charges_tool_turn_before_clamping_later_dispatch() {
    let model = UsageScriptedModel::new(vec![
        usage_echo_tool_turn(usage_response(10_000, 10_000, 20_000)),
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(usage_response(100, 100, 1_000)),
        ],
    ]);
    let mut loop_config = config(2);
    loop_config.max_tokens = Some(40_000);
    loop_config.aggregate_token_budget = Some(AggregateTokenBudget::new(50_000));

    let stream = run_loop_stream(
        model.clone(),
        None,
        Message::user("use the tool"),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        loop_config,
    );
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(collected.error, None);
    assert_eq!(collected.final_text.as_deref(), Some("done"));
    let dispatches = model.seen_dispatches().await;
    assert_eq!(dispatches.len(), 2);
    assert_eq!(dispatches[0].1, Some(40_000));
    assert_eq!(dispatches[1].1, Some(30_000 - dispatches[1].0));
}

#[tokio::test]
async fn nested_compaction_charges_the_same_request_budget() {
    let outer_model = UsageScriptedModel::new(vec![vec![
        RawStreamingChoice::Message("done".to_string()),
        RawStreamingChoice::FinalResponse(usage_response(400, 600, 1_000)),
    ]]);
    let compaction_model = UsageScriptedModel::new(vec![vec![
        RawStreamingChoice::Message("summary".to_string()),
        RawStreamingChoice::FinalResponse(usage_response(4_000, 1_000, 5_000)),
    ]]);
    let budget = AggregateTokenBudget::new(10_000);
    let mut compaction_config = config(0);
    compaction_config.max_tokens = Some(1_000);
    compaction_config.aggregate_token_budget = Some(budget.clone());

    let mut loop_config = config(0);
    loop_config.max_tokens = Some(6_000);
    loop_config.context_window = 6_500;
    loop_config.compaction_threshold = 0.25;
    loop_config.aggregate_token_budget = Some(budget.clone());
    loop_config.turn_compactor = Some(Arc::new(move |_request| {
        let model = compaction_model.clone();
        let config = compaction_config.clone();
        Box::pin(async move {
            run_loop_to_text(
                model,
                None,
                Message::user("summarize"),
                Vec::new(),
                Arc::new(Vec::new()),
                config,
            )
            .await?;
            Ok(TurnCompactionOutcome {
                messages: vec![Message::user("compacted prompt")],
                reduction_key: "reduction-1".to_string(),
            })
        })
    }));

    let collected = collect_scripted_stream(run_loop_stream(
        outer_model.clone(),
        None,
        Message::user("x".repeat(8_000)),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    ))
    .await;

    assert_eq!(collected.error, None);
    assert_eq!(collected.final_text.as_deref(), Some("done"));
    assert_eq!(budget.snapshot().unwrap().used, 6_000);
    let dispatches = outer_model.seen_dispatches().await;
    assert_eq!(dispatches.len(), 1);
    assert_eq!(dispatches[0].1, Some(5_000 - dispatches[0].0));
}

#[tokio::test]
async fn per_turn_compaction_preserves_canonical_budget_exhaustion() {
    let model = UsageScriptedModel::new(Vec::new());
    let mut loop_config = config(0);
    loop_config.max_tokens = Some(6_000);
    loop_config.context_window = 6_500;
    loop_config.compaction_threshold = 0.25;
    loop_config.turn_compactor = Some(Arc::new(move |_request| {
        Box::pin(async move {
            Err(
                anyhow::Error::new(StreamingError::Completion(CompletionError::ProviderError(
                    format!(
                        "{AGGREGATE_TOKEN_BUDGET_EXHAUSTED_PREFIX}limit=10000, used=9000, \
                     estimated_input_tokens=2000, remaining=1000"
                    ),
                )))
                .context("guided compaction exhausted the request token budget"),
            )
        })
    }));

    let collected = collect_scripted_stream(run_loop_stream(
        model.clone(),
        None,
        Message::user("x".repeat(8_000)),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    ))
    .await;

    assert!(
        collected.error.as_deref().is_some_and(|error| error
            .starts_with("CompletionError: ProviderError: aggregate_token_budget_exhausted: ")),
        "unexpected terminal state: {collected:?}"
    );
    assert!(model.seen_dispatches().await.is_empty());
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct BudgetedStructuredResponse {
    #[allow(dead_code)]
    answer: String,
}

#[tokio::test(start_paused = true)]
async fn aggregate_budget_charges_retracted_structured_output_attempt() {
    let model = UsageScriptedModel::new(vec![
        vec![
            RawStreamingChoice::Message("not json".to_string()),
            RawStreamingChoice::FinalResponse(usage_response(10_000, 10_000, 20_000)),
        ],
        vec![
            RawStreamingChoice::Message(r#"{"answer":"ok"}"#.to_string()),
            RawStreamingChoice::FinalResponse(usage_response(100, 100, 1_000)),
        ],
    ]);
    let mut loop_config = config(0);
    loop_config.max_tokens = Some(40_000);
    loop_config.aggregate_token_budget = Some(AggregateTokenBudget::new(50_000));
    loop_config.structured_output =
        Some(StructuredOutputConfig::for_type::<BudgetedStructuredResponse>());

    let stream = run_loop_stream(
        model.clone(),
        None,
        Message::user("return json"),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    );
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(collected.error, None);
    assert_eq!(collected.retractions, vec![(0, 0)]);
    let dispatches = model.seen_dispatches().await;
    assert_eq!(dispatches.len(), 2);
    assert_eq!(dispatches[0].1, Some(40_000));
    assert_eq!(dispatches[1].1, Some(30_000 - dispatches[1].0));
}

#[tokio::test]
async fn exact_aggregate_exhaustion_allows_a_valid_terminal_response() {
    let model = UsageScriptedModel::new(vec![vec![
        RawStreamingChoice::Message("done".to_string()),
        RawStreamingChoice::FinalResponse(usage_response(1_000, 1_000, 2_000)),
    ]]);
    let mut loop_config = config(0);
    loop_config.max_tokens = Some(1_000);
    loop_config.aggregate_token_budget = Some(AggregateTokenBudget::new(2_000));

    let collected = collect_scripted_stream(run_loop_stream(
        model,
        None,
        Message::user("finish"),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    ))
    .await;

    assert_eq!(collected.error, None);
    assert_eq!(collected.final_text.as_deref(), Some("done"));
}

#[tokio::test]
async fn exact_aggregate_exhaustion_after_tool_effect_is_terminal_and_preserved() {
    let model = UsageScriptedModel::new(vec![usage_echo_tool_turn(usage_response(
        1_000, 1_000, 2_000,
    ))]);
    let mut loop_config = config(2);
    loop_config.max_tokens = Some(1_000);
    loop_config.aggregate_token_budget = Some(AggregateTokenBudget::new(2_000));

    let collected = collect_scripted_stream(run_loop_stream(
        model,
        None,
        Message::user("use the tool"),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        loop_config,
    ))
    .await;

    assert_eq!(collected.tool_results, vec!["ECHOED"]);
    assert!(
        collected.error.as_deref().is_some_and(|error| error
            .starts_with("CompletionError: ProviderError: aggregate_token_budget_exhausted: ")),
        "unexpected terminal state: {collected:?}"
    );
}

#[tokio::test]
async fn aggregate_budget_fails_closed_on_missing_or_zero_usage() {
    for response in [UsageResponse { usage: None }, usage_response(0, 0, 0)] {
        let model = UsageScriptedModel::new(vec![vec![
            RawStreamingChoice::Message("unscored".to_string()),
            RawStreamingChoice::FinalResponse(response),
        ]]);
        let mut loop_config = config(0);
        loop_config.max_tokens = Some(1_000);
        loop_config.aggregate_token_budget = Some(AggregateTokenBudget::new(2_000));

        let collected = collect_scripted_stream(run_loop_stream(
            model,
            None,
            Message::user("finish"),
            Vec::new(),
            Arc::new(Vec::new()),
            loop_config,
        ))
        .await;

        assert!(
            collected
                .error
                .as_deref()
                .is_some_and(|error| error.contains("aggregate_token_usage_missing")),
            "unexpected terminal state: {collected:?}"
        );
        assert_eq!(collected.final_text, None);
    }
}

#[tokio::test]
async fn aggregate_budget_fails_closed_on_provider_reported_overrun() {
    let model = UsageScriptedModel::new(vec![vec![
        RawStreamingChoice::Message("unscored".to_string()),
        RawStreamingChoice::FinalResponse(usage_response(1_000, 1_001, 2_001)),
    ]]);
    let mut loop_config = config(0);
    loop_config.max_tokens = Some(1_000);
    loop_config.aggregate_token_budget = Some(AggregateTokenBudget::new(2_000));

    let collected = collect_scripted_stream(run_loop_stream(
        model,
        None,
        Message::user("finish"),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    ))
    .await;

    assert!(
        collected
            .error
            .as_deref()
            .is_some_and(|error| error.contains("aggregate_token_budget_overrun")),
        "unexpected terminal state: {collected:?}"
    );
    assert_eq!(collected.final_text, None);
}

#[tokio::test]
async fn completion_output_ceiling_is_clamped_to_remaining_context() {
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("done".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let prompt = Message::user("fit the output dynamically");
    let mut loop_config = config(0);
    loop_config.max_tokens = Some(1_000);
    loop_config.compaction_threshold = 1.0;

    let request = build_request(&model, prompt.clone(), &[], &[], &[], &loop_config)
        .await
        .expect("request should build");
    let input_tokens = completion_request_input_components(&request).estimated_input_tokens();
    loop_config.context_window = input_tokens + 250;

    let stream = run_loop_stream(
        model.clone(),
        None,
        prompt,
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    );
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(collected.error, None);
    assert_eq!(model.seen_max_tokens().await, vec![Some(250)]);
}

#[tokio::test]
async fn later_completion_turn_is_compacted_before_provider_dispatch() {
    let model = ScriptedModel::new_turns(vec![
        echo_tool_turn(),
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(vec![Box::new(EchoTool {
        name: "echo".to_string(),
        output: "r".repeat(12_000),
    })]);
    let prompt = Message::user("p".repeat(20_000));
    let mut loop_config = config(2);
    loop_config.max_tokens = Some(100);
    loop_config.compaction_threshold = 1.0;
    loop_config.active_reduction_keys = vec!["reduction-previous".to_string()];
    loop_config.reduction_chain_keys = vec!["reduction-previous".to_string()];
    let reduction_captures = Arc::new(Mutex::new(Vec::new()));
    let reduction_captures_for_sink = reduction_captures.clone();
    loop_config.on_rendered_request = Some(Arc::new(move |turn, _attempt, _request, trace| {
        let reduction_captures = reduction_captures_for_sink.clone();
        Box::pin(async move {
            let accounting = trace
                .context_accounting
                .expect("every captured dispatch carries context accounting");
            reduction_captures.lock().await.push((
                turn,
                trace.reduction_keys,
                accounting.compaction_reason,
                accounting.estimated_input_tokens,
                accounting.pre_compaction_input_tokens,
            ));
            Ok(())
        })
    }));

    let first_request = build_request(
        &model,
        prompt.clone(),
        &[],
        &[],
        tools.as_slice(),
        &loop_config,
    )
    .await
    .expect("first request should build");
    let first_tokens = completion_request_input_components(&first_request).estimated_input_tokens();
    loop_config.context_window = first_tokens + 100 + 100;

    let compactions = Arc::new(AtomicUsize::new(0));
    let compactions_for_callback = compactions.clone();
    let keep_recent_target = Arc::new(AtomicUsize::new(usize::MAX));
    let keep_recent_target_for_callback = keep_recent_target.clone();
    loop_config.turn_compactor = Some(Arc::new(move |request| {
        let compactions = compactions_for_callback.clone();
        let keep_recent_target = keep_recent_target_for_callback.clone();
        Box::pin(async move {
            compactions.fetch_add(1, Ordering::SeqCst);
            keep_recent_target.store(request.keep_recent_target, Ordering::SeqCst);
            let keep_from = request.messages.len().saturating_sub(2);
            let mut compacted = vec![Message::user(
                "<system-reminder>compacted earlier turn</system-reminder>",
            )];
            compacted.extend(request.messages.into_iter().skip(keep_from));
            Ok(TurnCompactionOutcome {
                messages: compacted,
                reduction_key: "reduction-1".to_string(),
            })
        })
    }));

    let stream = run_loop_stream(model.clone(), None, prompt, Vec::new(), tools, loop_config);
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(collected.error, None);
    assert_eq!(collected.final_text.as_deref(), Some("done"));
    assert_eq!(
        compactions.load(Ordering::SeqCst),
        1,
        "the safe entry turn must dispatch directly and the grown second turn must compact"
    );
    assert!(
        keep_recent_target.load(Ordering::SeqCst) < 20_000,
        "the per-turn target must reserve room for static request layers and the summary"
    );
    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 2);
    assert!(
        histories[1].iter().any(|message| message
            .rag_text()
            .is_some_and(|text| { text.contains("compacted earlier turn") })),
        "the second provider request must use the compacted provider view"
    );
    let captures = reduction_captures.lock().await;
    assert_eq!(captures.len(), 2);
    assert_eq!(captures[0].0, 0);
    assert_eq!(captures[0].1, vec!["reduction-previous".to_string()]);
    assert_eq!(captures[0].2, ContextCompactionReason::BelowThreshold);
    assert_eq!(captures[0].3, first_tokens);
    assert_eq!(captures[0].4, None);
    assert_eq!(captures[1].0, 1);
    assert_eq!(captures[1].1, vec!["reduction-1".to_string()]);
    assert_eq!(captures[1].2, ContextCompactionReason::Compacted);
    assert!(captures[1].3 < captures[1].4.expect("pre-compaction estimate"));
}

#[tokio::test(start_paused = true)]
async fn aggregate_budget_fails_closed_on_mid_stream_error_before_retry() {
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::TurnWithMidStreamError(
            vec![RawStreamingChoice::Message("partial".to_string())],
            transient_provider_error("decode"),
        ),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("must not run".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);
    let mut loop_config = config(0);
    loop_config.aggregate_token_budget = Some(AggregateTokenBudget::new(10_000));

    let collected = collect_scripted_stream(run_loop_stream(
        model.clone(),
        None,
        Message::user("hi"),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    ))
    .await;

    assert!(
        collected.error.as_deref().is_some_and(|error| error
            .starts_with("CompletionError: ProviderError: aggregate_token_usage_missing: ")),
        "unexpected terminal state: {collected:?}"
    );
    assert!(collected.retractions.is_empty());
    assert_eq!(model.seen_histories().await.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn aggregate_budget_fails_closed_after_mid_stream_tool_effect() {
    let calls = Arc::new(AtomicUsize::new(0));
    let model = ScriptedModel::new_calls(vec![ScriptedCall::TurnWithMidStreamError(
        vec![RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
            "call-1".to_string(),
            "echo".to_string(),
            serde_json::json!({}),
        ))],
        transient_provider_error("decode after tool"),
    )]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(CountingTool {
        name: "echo".to_string(),
        output: "ECHOED".to_string(),
        calls: calls.clone(),
    })];
    let mut loop_config = config(4);
    loop_config.aggregate_token_budget = Some(AggregateTokenBudget::new(10_000));

    let collected = collect_scripted_stream(run_loop_stream(
        model.clone(),
        None,
        Message::user("use the echo tool"),
        Vec::new(),
        Arc::new(tools),
        loop_config,
    ))
    .await;

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(collected.tool_results, vec!["ECHOED"]);
    assert!(
        collected.error.as_deref().is_some_and(|error| error
            .starts_with("CompletionError: ProviderError: aggregate_token_usage_missing: ")),
        "unexpected terminal state: {collected:?}"
    );
    assert_eq!(model.seen_histories().await.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn mid_stream_failure_after_tool_budget_exhausted_fails() {
    let calls = Arc::new(AtomicUsize::new(0));
    let model = ScriptedModel::new_calls(vec![ScriptedCall::TurnWithMidStreamError(
        vec![RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
            "call-1".to_string(),
            "echo".to_string(),
            serde_json::json!({}),
        ))],
        transient_provider_error("decode after tool"),
    )]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(CountingTool {
        name: "echo".to_string(),
        output: "ECHOED".to_string(),
        calls: calls.clone(),
    })];
    let mut loop_config = config(4);
    loop_config.retry_policy = crate::agent::completion_retry::CompletionRetryPolicy {
        transport_backoff: Vec::new(),
        max_resample: 0,
        allow_repair: false,
    };

    let stream = run_loop_stream(
        model,
        None,
        Message::user("use the echo tool"),
        Vec::new(),
        Arc::new(tools),
        loop_config,
    );
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(collected.final_text, None);
    assert_eq!(collected.tool_results, Vec::<String>::new());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let error = collected
        .error
        .expect("effectful retry exhaustion should fail");
    assert!(
        error.contains("completion retry budget exhausted")
            && error.contains("transport retry budget exhausted"),
        "terminal error must report exhausted effectful retry budget; got {error}"
    );
}
