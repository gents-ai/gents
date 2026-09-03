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
            crate::provider_input::budget::threshold_budget(case.context_window, threshold),
            case.configured_threshold_budget,
            "{}: configured threshold drifted from Lean",
            case.name
        );
        assert_eq!(
            crate::provider_input::budget::effective_input_budget(case.context_window, threshold),
            case.effective_input_budget,
            "{}: effective input budget drifted from Lean",
            case.name
        );
        let actual_output = case
            .turn_input_tokens
            .iter()
            .map(|tokens| {
                crate::provider_input::budget::effective_output_budget(
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
                crate::compaction::ReductionAdmission::for_input(
                    *tokens,
                    case.context_window,
                    threshold,
                )
                .is_some()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actual, case.turn_should_compact,
            "{}: a later completion turn bypassed the Lean dispatch gate",
            case.name
        );
        let actual_can_dispatch = case
            .turn_input_tokens
            .iter()
            .map(|tokens| {
                let mut request = CompletionRequest {
                    model: None,
                    preamble: None,
                    chat_history: rig::one_or_many::OneOrMany::one(rig::completion::Message::user(
                        "generated Lean budget witness",
                    )),
                    documents: Vec::new(),
                    tools: Vec::new(),
                    temperature: None,
                    max_tokens: u64::try_from(case.max_output_tokens).ok(),
                    tool_choice: None,
                    additional_params: None,
                    output_schema: None,
                };
                let mut witness_config = config(0);
                witness_config.context_window = case.context_window;
                witness_config.max_tokens = u64::try_from(case.max_output_tokens).ok();
                clamp_request_output_budget(&mut request, &witness_config, *tokens);
                let clamped = request
                    .max_tokens
                    .and_then(|output| usize::try_from(output).ok())
                    .unwrap_or_default();
                assert_eq!(
                    clamped,
                    crate::provider_input::budget::effective_output_budget(
                        *tokens,
                        case.context_window,
                        case.max_output_tokens,
                    ),
                    "{}: production request clamp drifted from Lean",
                    case.name
                );
                ensure_context_can_dispatch(&request, &witness_config, *tokens).is_ok()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actual_can_dispatch, case.turn_can_dispatch,
            "{}: a zero-capacity completion passed the Lean dispatch gate",
            case.name
        );
    }
}

#[test]
fn generated_retention_cases_drive_production_compaction_target() {
    let cases = crate::lean_vocab_test::lean_prompt_assembly_retention_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no PromptAssembly retention cases"
    );

    for case in cases {
        assert_eq!(
            case.effective_input_budget.saturating_sub(case.fixed_input),
            case.available_input,
            "{}: available input drifted from Lean natural subtraction",
            case.name
        );
        let actual = crate::provider_input::budget::compaction_retention_target(
            case.configured_keep_recent,
            case.effective_input_budget,
            case.fixed_input,
        );
        assert_eq!(
            actual, case.retention_target,
            "{}: production retention target drifted from Lean",
            case.name
        );
        assert!(actual <= case.configured_keep_recent, "{}", case.name);
        assert!(actual <= case.available_input, "{}", case.name);
        assert!(
            actual
                .checked_add(case.available_input.div_ceil(4))
                .is_some_and(|used| used <= case.available_input),
            "{}: retention did not preserve the checkpoint quarter",
            case.name
        );
        assert_eq!(
            crate::provider_input::budget::summary_output_ceiling(
                case.summary_max_output,
                case.effective_input_budget,
            ),
            case.effective_summary_output,
            "{}: bounded summary output ceiling drifted from Lean",
            case.name
        );
        assert_eq!(
            crate::provider_input::budget::rolling_summary_input_budget(
                case.effective_input_budget,
                case.effective_summary_output,
            ),
            case.rolling_summary_input_budget,
            "{}: rolling summary input budget drifted from Lean",
            case.name
        );
    }
}

#[test]
fn machine_width_budget_arithmetic_is_exact_and_fail_closed() {
    let third = usize::MAX / 3;
    for value in [0, 1, third, third.saturating_add(1), usize::MAX] {
        let retained = crate::provider_input::budget::compaction_retention_target(usize::MAX, value, 0);
        assert_eq!(retained.checked_add(value.div_ceil(4)), Some(value));
    }

    let threshold_29 = crate::provider_input::budget::threshold_budget(usize::MAX, 0.29);
    let threshold_57 = crate::provider_input::budget::threshold_budget(usize::MAX, 0.57);
    let threshold_69 = crate::provider_input::budget::threshold_budget(usize::MAX, 0.69);
    assert!(threshold_29 < threshold_57 && threshold_57 < threshold_69);
    assert!(threshold_29 < usize::MAX / 2);
    assert!(threshold_57 > usize::MAX / 2);
    assert_eq!(
        crate::provider_input::budget::threshold_budget(usize::MAX, 1.0),
        usize::MAX
    );

    assert!(crate::provider_input::budget::can_dispatch(
        usize::MAX - 1,
        usize::MAX,
        usize::MAX,
    ));
    assert!(!crate::provider_input::budget::can_dispatch(
        usize::MAX,
        usize::MAX,
        usize::MAX,
    ));
    assert!(!crate::provider_input::budget::can_dispatch(usize::MAX, usize::MAX, 0));
    assert_eq!(
        crate::provider_input::budget::configured_output_ceiling(Some(u64::MAX)),
        usize::try_from(u64::MAX).unwrap_or(usize::MAX)
    );
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
            Ok(TurnCompactionOutcome::Reduced {
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
async fn provider_view_repair_is_not_reported_as_a_durable_compaction() {
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("done".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let mut loop_config = config(0);
    loop_config.max_tokens = Some(1_000);
    loop_config.context_window = 6_500;
    loop_config.compaction_threshold = 0.25;
    loop_config.turn_compactor = Some(Arc::new(|_| {
        Box::pin(async {
            Ok(TurnCompactionOutcome::ProviderViewRepaired {
                messages: vec![Message::user("repaired prompt")],
            })
        })
    }));
    let captured_reason = Arc::new(Mutex::new(None));
    let captured_reason_for_sink = captured_reason.clone();
    loop_config.on_rendered_request = Some(Arc::new(move |_, _, _, trace| {
        let captured_reason = captured_reason_for_sink.clone();
        Box::pin(async move {
            *captured_reason.lock().await = trace
                .context_accounting
                .map(|accounting| accounting.compaction_reason);
            Ok(())
        })
    }));

    let collected = collect_scripted_stream(run_loop_stream(
        model,
        None,
        Message::user("x".repeat(8_000)),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    ))
    .await;

    assert_eq!(collected.error, None);
    assert_eq!(collected.final_text.as_deref(), Some("done"));
    assert_eq!(
        *captured_reason.lock().await,
        Some(ContextCompactionReason::ProviderViewRepaired)
    );
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
    let input_tokens =
        completion_request_input_components(&request, loop_config.provider_input_counter.as_ref())
            .expect("provider projection")
            .estimated_input_tokens;
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
async fn zero_remaining_capacity_is_not_captured_or_dispatched() {
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("must not dispatch".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let prompt = Message::user("input exactly fills context");
    let mut loop_config = config(0);
    loop_config.max_tokens = Some(1_000);
    loop_config.compaction_threshold = 1.0;

    let request = build_request(&model, prompt.clone(), &[], &[], &[], &loop_config)
        .await
        .expect("request should build");
    let input_tokens =
        completion_request_input_components(&request, loop_config.provider_input_counter.as_ref())
            .expect("provider projection")
            .estimated_input_tokens;
    loop_config.context_window = input_tokens;

    let capture_calls = Arc::new(AtomicUsize::new(0));
    let capture_calls_for_sink = capture_calls.clone();
    loop_config.on_rendered_request = Some(Arc::new(move |_, _, _, _| {
        capture_calls_for_sink.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }));

    let collected = collect_scripted_stream(run_loop_stream(
        model.clone(),
        None,
        prompt,
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    ))
    .await;

    assert!(
        collected
            .error
            .as_deref()
            .is_some_and(|error| error.contains("provider_input_has_no_output_capacity")),
        "unexpected terminal state: {collected:?}"
    );
    assert_eq!(capture_calls.load(Ordering::SeqCst), 0);
    assert!(
        model.seen_max_tokens().await.is_empty(),
        "zero-capacity request reached the provider model"
    );
}

#[test]
fn zero_remaining_capacity_uses_the_typed_provider_input_error() {
    let mut loop_config = config(0);
    loop_config.context_window = 100;
    let mut request = CompletionRequest {
        model: None,
        preamble: None,
        chat_history: rig::one_or_many::OneOrMany::one(rig::completion::Message::user("x")),
        documents: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_tokens: Some(100),
        tool_choice: None,
        additional_params: None,
        output_schema: None,
    };
    clamp_request_output_budget(&mut request, &loop_config, 100);
    let error = ensure_context_can_dispatch(&request, &loop_config, 100).unwrap_err();

    let StreamingError::Completion(CompletionError::RequestError(source)) = error else {
        panic!("expected typed request error, got {error}");
    };
    assert!(matches!(
        source.downcast_ref::<crate::provider_input::budget::ContextBudgetError>(),
        Some(
            crate::provider_input::budget::ContextBudgetError::NoOutputCapacity {
                estimated_input_tokens: 100,
                context_window: 100,
                effective_max_output_tokens: 0,
            }
        )
    ));
}

#[tokio::test]
async fn reduction_cannot_fit_is_typed_and_never_dispatched() {
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("must not dispatch".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let mut loop_config = config(0);
    loop_config.compaction_threshold = 0.0;
    loop_config.turn_compactor = Some(Arc::new(|_| {
        Box::pin(async { Ok(TurnCompactionOutcome::CannotFit) })
    }));
    let capture_calls = Arc::new(AtomicUsize::new(0));
    let capture_calls_for_sink = capture_calls.clone();
    loop_config.on_rendered_request = Some(Arc::new(move |_, _, _, _| {
        capture_calls_for_sink.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }));

    let stream = run_loop_stream(
        model.clone(),
        None,
        Message::user("an indivisible current prompt"),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    );
    futures::pin_mut!(stream);
    let error = stream
        .next()
        .await
        .expect("cannot-fit produces one terminal error")
        .expect_err("cannot-fit cannot reach provider dispatch");

    let StreamingError::Completion(CompletionError::RequestError(source)) = error else {
        panic!("expected typed request error, got {error}");
    };
    assert!(matches!(
        source.downcast_ref::<crate::compaction::ReductionError>(),
        Some(crate::compaction::ReductionError::CannotFit)
    ));
    assert_eq!(capture_calls.load(Ordering::SeqCst), 0);
    assert!(model.seen_max_tokens().await.is_empty());
}

#[tokio::test]
async fn final_fit_failure_after_compaction_is_typed_and_never_dispatched() {
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("must not dispatch".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let prompt = Message::user("current prompt survives compaction");
    let mut loop_config = config(0);
    loop_config.max_tokens = Some(1_000);
    // Keep this below 100% so the post-compaction threshold diagnostic is
    // also eligible. The stricter typed context-fit result must win when the
    // rebuilt request has no positive output capacity.
    loop_config.compaction_threshold = 0.5;

    let compacted_request = build_request(&model, prompt.clone(), &[], &[], &[], &loop_config)
        .await
        .unwrap();
    loop_config.context_window = completion_request_input_components(
        &compacted_request,
        loop_config.provider_input_counter.as_ref(),
    )
    .unwrap()
    .estimated_input_tokens;

    let compactions = Arc::new(AtomicUsize::new(0));
    let compactions_for_callback = compactions.clone();
    loop_config.turn_compactor = Some(Arc::new(move |request| {
        let compactions = compactions_for_callback.clone();
        Box::pin(async move {
            compactions.fetch_add(1, Ordering::SeqCst);
            Ok(TurnCompactionOutcome::Reduced {
                messages: vec![request.messages.last().unwrap().clone()],
                reduction_key: "final-fit-reduction".to_string(),
            })
        })
    }));
    let capture_calls = Arc::new(AtomicUsize::new(0));
    let capture_calls_for_sink = capture_calls.clone();
    loop_config.on_rendered_request = Some(Arc::new(move |_, _, _, _| {
        capture_calls_for_sink.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }));

    let collected = collect_scripted_stream(run_loop_stream(
        model.clone(),
        None,
        prompt,
        vec![Message::assistant("x".repeat(20_000))],
        Arc::new(Vec::new()),
        loop_config,
    ))
    .await;

    assert!(
        collected
            .error
            .as_deref()
            .is_some_and(|error| error.contains("provider_input_has_no_output_capacity")),
        "unexpected terminal state: {collected:?}"
    );
    assert_eq!(compactions.load(Ordering::SeqCst), 1);
    assert_eq!(capture_calls.load(Ordering::SeqCst), 0);
    assert!(model.seen_max_tokens().await.is_empty());
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
    let first_tokens = completion_request_input_components(
        &first_request,
        loop_config.provider_input_counter.as_ref(),
    )
    .expect("provider projection")
    .estimated_input_tokens;
    loop_config.context_window = first_tokens + 100 + 100;

    let compactions = Arc::new(AtomicUsize::new(0));
    let compactions_for_callback = compactions.clone();
    loop_config.turn_compactor = Some(Arc::new(move |request| {
        let compactions = compactions_for_callback.clone();
        Box::pin(async move {
            compactions.fetch_add(1, Ordering::SeqCst);
            let keep_from = request.messages.len().saturating_sub(2);
            let mut compacted = vec![Message::user(
                "<system-reminder>compacted earlier turn</system-reminder>",
            )];
            compacted.extend(request.messages.into_iter().skip(keep_from));
            Ok(TurnCompactionOutcome::Reduced {
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
