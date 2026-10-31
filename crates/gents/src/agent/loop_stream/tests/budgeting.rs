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
            crate::turn::provider_input::budget::threshold_budget(case.context_window, threshold),
            case.configured_threshold_budget,
            "{}: configured threshold drifted from Lean",
            case.name
        );
        assert_eq!(
            crate::turn::provider_input::budget::effective_input_budget(case.context_window, threshold),
            case.effective_input_budget,
            "{}: effective input budget drifted from Lean",
            case.name
        );
        let actual_output = case
            .turn_input_tokens
            .iter()
            .map(|tokens| {
                crate::turn::provider_input::budget::effective_output_budget(
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
        let compaction_budget = crate::turn::provider_input::budget::compaction_input_budget(
            case.context_window,
            threshold,
            case.max_output_tokens,
        );
        assert_eq!(
            compaction_budget, case.compaction_input_budget,
            "{}: compaction input budget drifted from Lean",
            case.name
        );
        let actual = case
            .turn_input_tokens
            .iter()
            .map(|tokens| {
                crate::inference::compaction::ReductionAdmission::for_context(
                    crate::turn::provider_input::budget::ContextCalibration::default().context(*tokens),
                    compaction_budget,
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
                    crate::turn::provider_input::budget::effective_output_budget(
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
        let actual = crate::turn::provider_input::budget::compaction_retention_target(
            case.configured_keep_recent,
            case.effective_input_budget,
            case.fixed_input,
            case.effective_summary_output,
        );
        assert_eq!(
            actual, case.retention_target,
            "{}: production retention target drifted from Lean",
            case.name
        );
        assert!(actual <= case.configured_keep_recent, "{}", case.name);
        assert!(actual <= case.available_input, "{}", case.name);
        let band = crate::turn::provider_input::budget::recovery_band_budget(case.effective_input_budget);
        let reserved = case.fixed_input + case.effective_summary_output;
        if reserved <= band {
            assert!(
                reserved + actual <= band,
                "{}: the post-compaction request left the recovery band",
                case.name
            );
        } else {
            assert_eq!(actual, 0, "{}: no band left for a tail", case.name);
        }
        assert_eq!(
            crate::turn::provider_input::budget::summary_output_ceiling(
                case.summary_max_output,
                case.effective_input_budget,
            ),
            case.effective_summary_output,
            "{}: bounded summary output ceiling drifted from Lean",
            case.name
        );
        assert_eq!(
            crate::turn::provider_input::budget::rolling_summary_input_budget(
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
        let retained =
            crate::turn::provider_input::budget::compaction_retention_target(usize::MAX, value, 0, 0);
        assert_eq!(retained.checked_add(value.div_ceil(5)), Some(value));
        assert_eq!(
            crate::turn::provider_input::budget::compaction_retention_target(
                usize::MAX,
                value,
                value,
                value
            ),
            0,
            "reserves past the band floor the tail at zero"
        );
    }

    let threshold_29 = crate::turn::provider_input::budget::threshold_budget(usize::MAX, 0.29);
    let threshold_57 = crate::turn::provider_input::budget::threshold_budget(usize::MAX, 0.57);
    let threshold_69 = crate::turn::provider_input::budget::threshold_budget(usize::MAX, 0.69);
    assert!(threshold_29 < threshold_57 && threshold_57 < threshold_69);
    assert!(threshold_29 < usize::MAX / 2);
    assert!(threshold_57 > usize::MAX / 2);
    assert_eq!(
        crate::turn::provider_input::budget::threshold_budget(usize::MAX, 1.0),
        usize::MAX
    );

    assert!(crate::turn::provider_input::budget::can_dispatch(
        usize::MAX - 1,
        usize::MAX,
        usize::MAX,
    ));
    assert!(!crate::turn::provider_input::budget::can_dispatch(
        usize::MAX,
        usize::MAX,
        usize::MAX,
    ));
    assert!(!crate::turn::provider_input::budget::can_dispatch(
        usize::MAX,
        usize::MAX,
        0
    ));
    assert_eq!(
        crate::turn::provider_input::budget::configured_output_ceiling(Some(u64::MAX)),
        usize::try_from(u64::MAX).unwrap_or(usize::MAX)
    );
}

#[test]
fn generated_aggregate_token_budget_cases_drive_the_owned_loop_ledger() {
    let cases = crate::lean_vocab_test::lean_aggregate_token_budget_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no aggregate token-budget cases"
    );

    for name in [
        "restart-zero-usage-adds-no-spend",
        "restart-mixed-rows-preserve-accounted-spend",
    ] {
        assert!(
            cases.iter().any(|case| case.name == name),
            "Lean must include restart accounting scenario {name}"
        );
    }

    for case in cases.iter().filter(|case| {
        case.prior_request_doc_ids
            .iter()
            .all(|id| id == &case.request_doc_id)
    }) {
        assert_eq!(
            case.prior_prompt_tokens.len(),
            case.prior_completion_tokens.len(),
            "{}: each prior usage row needs both durable components",
            case.name
        );
        let used = crate::provider_usage::sum_charged_from_persisted_parts(
            case.prior_prompt_tokens
                .iter()
                .zip(&case.prior_completion_tokens)
                .map(|(&prompt, &completion)| {
                    (
                        Some(i64::try_from(prompt).expect("fixture prompt tokens fit i64")),
                        Some(i64::try_from(completion).expect("fixture completion tokens fit i64")),
                    )
                }),
        )
        .expect("decoded Lean usage rows must rehydrate");
        assert_eq!(
            used, case.used,
            "{}: durable usage rehydration drifted from Lean",
            case.name
        );
        let mut ledger = AggregateTokenBudget::with_prior_usage(case.limit, used)
            .snapshot()
            .expect("rehydrated ledger is available");
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

/// The second dispatch's context size after the first call's usage: its
/// estimate at the ratio that usage set, anchored on the first call when that
/// call's response was accepted into the transcript.
fn context_after_first_usage(
    dispatches: &[(u64, Option<u64>)],
    first_input: u64,
    accepted_output: Option<u64>,
) -> u64 {
    let first_estimate = usize::try_from(dispatches[0].0).unwrap();
    let calibration = crate::turn::provider_input::budget::ContextCalibration::default();
    calibration.observe(first_input, first_estimate).unwrap();
    if let Some(output_tokens) = accepted_output {
        calibration.anchor_accepted(crate::turn::provider_input::budget::ContextAnchor {
            input_tokens: first_input,
            output_tokens,
            estimated_input_tokens: first_estimate,
        });
    }
    calibration
        .context(usize::try_from(dispatches[1].0).unwrap())
        .tokens as u64
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
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(Message::user("use the tool")),
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
    assert_eq!(
        dispatches[1].1,
        Some(30_000 - context_after_first_usage(&dispatches, 10_000, Some(10_000))),
        "the later dispatch reserves its input anchored on the accepted tool turn"
    );
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
                Message::user("summarize"),
                Vec::new(),
                Arc::new(Vec::new()),
                config,
            )
            .await
            .map_err(crate::inference::compaction::CompactionError::GuidedBudgetExhausted)?;
            Ok(TurnCompactionOutcome::Reduced {
                messages: vec![TaggedMessage::unassociated(Message::user(
                    "compacted prompt",
                ))],
                reduction_key: "reduction-1".to_string(),
            })
        })
    }));

    let collected = collect_scripted_stream(run_loop_stream(
        outer_model.clone(),
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(Message::user("x".repeat(8_000))),
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
                messages: vec![TaggedMessage::unassociated(Message::user(
                    "repaired prompt",
                ))],
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
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(Message::user("x".repeat(8_000))),
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
            Err(crate::inference::compaction::CompactionError::GuidedBudgetExhausted(
                gents_loop::loop_stream::OneShotError::LoopStream(rig_compat::loop_verdict_error(
                    crate::error::LoopVerdict::AggregateTokenBudgetExhausted,
                    format!(
                        "{AGGREGATE_TOKEN_BUDGET_EXHAUSTED_PREFIX}limit=10000, used=9000, \
                     estimated_input_tokens=2000, remaining=1000"
                    ),
                )),
            )
            .into())
        })
    }));

    let collected = collect_scripted_stream(run_loop_stream(
        model.clone(),
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(Message::user("x".repeat(8_000))),
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
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(Message::user("return json")),
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
    assert_eq!(
        dispatches[1].1,
        Some(30_000 - context_after_first_usage(&dispatches, 10_000, None)),
        "a retracted attempt calibrates the ratio but does not anchor"
    );
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
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(Message::user("finish")),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    ))
    .await;

    assert_eq!(collected.error, None);
    assert_eq!(collected.final_text.as_deref(), Some("done"));
}

#[tokio::test]
async fn exact_aggregate_exhaustion_rejects_tool_intent_before_dispatch() {
    let calls = Arc::new(AtomicUsize::new(0));
    let model = UsageScriptedModel::new(vec![usage_echo_tool_turn(usage_response(
        1_000, 1_000, 2_000,
    ))]);
    let mut loop_config = config(2);
    loop_config.max_tokens = Some(1_000);
    loop_config.aggregate_token_budget = Some(AggregateTokenBudget::new(2_000));

    let collected = collect_scripted_stream(run_loop_stream(
        model,
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(Message::user("use the tool")),
        Vec::new(),
        Arc::new(vec![Box::new(CountingTool {
            name: "echo".to_string(),
            output: "ECHOED".to_string(),
            calls: calls.clone(),
        }) as Box<dyn ToolDyn>]),
        loop_config,
    ))
    .await;

    assert!(collected.tool_results.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
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
            None::<crate::tools::call::hook::DefraSessionHook>,
            TaggedMessage::unassociated(Message::user("finish")),
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
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(Message::user("finish")),
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
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(prompt),
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
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(prompt),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config.clone(),
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
        source.downcast_ref::<crate::turn::provider_input::budget::ContextBudgetError>(),
        Some(
            crate::turn::provider_input::budget::ContextBudgetError::NoOutputCapacity {
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
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(Message::user("an indivisible current prompt")),
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

    assert!(
        matches!(
            error.cause(),
            crate::error::LoopFailureCause::Completion {
                failure: crate::error::CompletionFailure::Request,
                ..
            }
        ),
        "expected typed request error, got {error}"
    );
    let reduction = std::iter::successors(
        Some(&error as &(dyn std::error::Error + 'static)),
        |cause| cause.source(),
    )
    .find_map(|cause| cause.downcast_ref::<crate::inference::compaction::ReductionError>());
    assert!(matches!(
        reduction,
        Some(crate::inference::compaction::ReductionError::CannotFit)
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
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(prompt),
        (vec![Message::assistant("x".repeat(20_000))])
            .into_iter()
            .map(TaggedMessage::unassociated)
            .collect(),
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
            let mut compacted = vec![TaggedMessage::unassociated(Message::user(
                "<system-reminder>compacted earlier turn</system-reminder>",
            ))];
            compacted.extend(request.messages.into_iter().skip(keep_from));
            Ok(TurnCompactionOutcome::Reduced {
                messages: compacted,
                reduction_key: "reduction-1".to_string(),
            })
        })
    }));

    let stream = run_loop_stream(
        model.clone(),
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(prompt),
        Vec::new(),
        tools,
        loop_config,
    );
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
    for with_tool in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let (calls_list, loop_config) = if with_tool {
            (
                vec![ScriptedCall::TurnWithMidStreamError(
                    vec![RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                        "call-1".to_string(),
                        "echo".to_string(),
                        serde_json::json!({}),
                    ))],
                    transient_provider_error("decode after tool"),
                )],
                config(4),
            )
        } else {
            (
                vec![
                    ScriptedCall::TurnWithMidStreamError(
                        vec![RawStreamingChoice::Message("partial".to_string())],
                        transient_provider_error("decode"),
                    ),
                    ScriptedCall::Turn(vec![
                        RawStreamingChoice::Message("must not run".to_string()),
                        RawStreamingChoice::FinalResponse(()),
                    ]),
                ],
                config(0),
            )
        };
        let model = ScriptedModel::new_calls(calls_list);
        let tools: Vec<Box<dyn ToolDyn>> = if with_tool {
            vec![Box::new(CountingTool {
                name: "echo".to_string(),
                output: "ECHOED".to_string(),
                calls: calls.clone(),
            })]
        } else {
            Vec::new()
        };
        let mut loop_config = loop_config;
        loop_config.aggregate_token_budget = Some(AggregateTokenBudget::new(10_000));

        let collected = collect_scripted_stream(run_loop_stream(
            model.clone(),
            None::<crate::tools::call::hook::DefraSessionHook>,
            TaggedMessage::unassociated(if with_tool {
                Message::user("use the echo tool")
            } else {
                Message::user("hi")
            }),
            Vec::new(),
            Arc::new(tools),
            loop_config,
        ))
        .await;

        if with_tool {
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(collected.tool_results.is_empty());
        }
        assert!(
            collected.error.as_deref().is_some_and(|error| error
                .starts_with("CompletionError: ProviderError: aggregate_token_usage_missing: ")),
            "unexpected terminal state: {collected:?}"
        );
        assert!(collected.retractions.is_empty());
        assert_eq!(
            model.seen_histories().await.len(),
            1,
            "the failed turn must fail closed without retrying"
        );
    }
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
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(Message::user("use the echo tool")),
        Vec::new(),
        Arc::new(tools),
        loop_config,
    );
    let collected = collect_scripted_stream(stream).await;

    assert_eq!(collected.final_text, None);
    assert_eq!(collected.tool_results, Vec::<String>::new());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let error = collected
        .error
        .expect("effectful retry exhaustion should fail");
    assert!(
        error.contains("completion retry budget exhausted")
            && error.contains("transport retry budget exhausted"),
        "terminal error must report exhausted effectful retry budget; got {error}"
    );
}

#[test]
fn generated_budget_cases_drive_dynamic_output_compaction_trigger() {
    let cases = crate::lean_vocab_test::lean_prompt_assembly_budget_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no PromptAssembly budget cases"
    );

    for case in cases {
        let threshold = case.threshold_basis_points as f64 / 10_000.0;
        let configured =
            crate::turn::provider_input::budget::threshold_budget(case.context_window, threshold);
        let effective =
            crate::turn::provider_input::budget::effective_input_budget(case.context_window, threshold);
        let input_tokens = case.prompt_tokens.saturating_add(case.request_tokens);
        let effective_output = crate::turn::provider_input::budget::effective_output_budget(
            input_tokens,
            case.context_window,
            case.max_output_tokens,
        );

        assert_eq!(
            configured, case.configured_threshold_budget,
            "{}: configured threshold budget drifted from Lean",
            case.name
        );
        assert_eq!(
            effective, case.effective_input_budget,
            "{}: effective input budget drifted from Lean",
            case.name
        );
        assert_eq!(
            effective_output, case.effective_output_tokens,
            "{}: effective output budget drifted from Lean",
            case.name
        );
        assert_eq!(
            input_tokens.saturating_add(effective_output) <= case.context_window,
            case.provider_safe,
            "{}: provider-safety witness drifted from Lean",
            case.name
        );
        assert_eq!(
            crate::turn::provider_input::budget::can_dispatch(
                input_tokens,
                case.context_window,
                case.max_output_tokens,
            ),
            case.can_dispatch,
            "{}: provider dispatch legality drifted from Lean",
            case.name
        );
        let compaction_budget = crate::turn::provider_input::budget::compaction_input_budget(
            case.context_window,
            threshold,
            case.max_output_tokens,
        );
        assert_eq!(
            compaction_budget, case.compaction_input_budget,
            "{}: compaction input budget drifted from Lean",
            case.name
        );
        assert_eq!(
            crate::inference::compaction::ReductionAdmission::for_context(
                crate::turn::provider_input::budget::ContextCalibration::default().context(input_tokens),
                compaction_budget,
            )
            .is_some(),
            case.should_compact,
            "{}: production compaction trigger drifted from Lean",
            case.name
        );
    }
}

#[test]
fn generated_compaction_trigger_cases_drive_the_production_trigger() {
    use crate::turn::provider_input::budget::{ContextAnchor, ContextCalibration};
    let cases = crate::lean_vocab_test::lean_prompt_assembly_compaction_trigger_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no compaction trigger cases"
    );

    for case in cases {
        let calibration = ContextCalibration::with_observations(
            case.prior_basis_points,
            case.seeded_observations.iter().copied(),
        );
        if let Some(anchor) = case.anchor {
            calibration.anchor_accepted(ContextAnchor {
                input_tokens: anchor.input_tokens,
                output_tokens: anchor.output_tokens,
                estimated_input_tokens: anchor.estimated_input_tokens,
            });
        }
        for (provider, estimate) in &case.observed {
            calibration.observe(*provider, *estimate);
        }
        if case.invalidate {
            calibration.invalidate_anchor();
        }
        let context = calibration.context(case.raw_input_tokens);
        assert_eq!(
            (
                context.ratio_basis_points,
                context.calibrated,
                context.anchored,
                context.tokens,
            ),
            (
                case.ratio_basis_points,
                case.calibrated_tokens,
                case.anchored_tokens,
                case.context_tokens,
            ),
            "{}: production context size drifted from Lean",
            case.name
        );
        let budget = crate::turn::provider_input::budget::compaction_input_budget(
            case.context_window,
            case.threshold_basis_points as f64 / 10_000.0,
            case.max_output_tokens,
        );
        assert_eq!(
            budget, case.compaction_input_budget,
            "{}: compaction input budget drifted from Lean",
            case.name
        );
        let admission = crate::inference::compaction::ReductionAdmission::for_context(context, budget)
            .map(|admission| (admission.input_tokens(), admission.effective_input_budget()));
        assert_eq!(
            admission,
            case.admission
                .map(|admission| (admission.input_tokens, admission.input_budget)),
            "{}: production compaction trigger drifted from Lean",
            case.name
        );
    }
}

/// The live Claude Messages rejection, as the transport renders it.
const CLAUDE_PROMPT_TOO_LONG: &str = r#"Claude Messages HTTP 400 Bad Request (request-id -) body={"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 1001685 tokens > 1000000 maximum"},"request_id":"req_011CgVVYfxcw64a1gDiAAhYC"}"#;

type AdmittedReduction = Arc<std::sync::Mutex<Vec<(usize, usize)>>>;

/// A 1M-window turn whose bytes/4 estimate (~561k) is below the 0.75
/// threshold, as in the live failure, with a compactor that records each
/// admission it receives.
fn overflow_loop_config(admitted: AdmittedReduction) -> LoopConfig {
    let mut loop_config = config(0);
    loop_config.context_window = 1_000_000;
    loop_config.compaction_threshold = 0.75;
    loop_config.max_tokens = Some(32_000);
    loop_config.turn_compactor = Some(Arc::new(move |request: TurnCompactionRequest| {
        admitted.lock().unwrap().push((
            request.admission.input_tokens(),
            request.admission.effective_input_budget(),
        ));
        Box::pin(async {
            Ok(TurnCompactionOutcome::Reduced {
                messages: vec![TaggedMessage::unassociated(Message::user(
                    "compacted prompt",
                ))],
                reduction_key: "overflow-reduction-1".to_string(),
            })
        })
    }));
    loop_config
}

fn live_sized_prompt() -> TaggedMessage {
    TaggedMessage::unassociated(Message::user("x".repeat(2_244_000)))
}

#[tokio::test(start_paused = true)]
async fn provider_context_overflow_compacts_once_and_retries() {
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::FailStream(CompletionError::ProviderError(
            CLAUDE_PROMPT_TOO_LONG.to_string(),
        )),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);
    let admitted = AdmittedReduction::default();

    let collected = collect_scripted_stream(run_loop_stream(
        model.clone(),
        None::<crate::tools::call::hook::DefraSessionHook>,
        live_sized_prompt(),
        Vec::new(),
        Arc::new(Vec::new()),
        overflow_loop_config(admitted.clone()),
    ))
    .await;

    assert_eq!(collected.error, None);
    assert_eq!(collected.final_text.as_deref(), Some("done"));
    let admitted = admitted.lock().unwrap().clone();
    assert_eq!(admitted.len(), 1, "exactly one forced reduction");
    let (rejected_estimate, estimator_budget) = admitted[0];
    assert!(
        rejected_estimate < 750_000,
        "the rejected request was below the threshold by estimate: {rejected_estimate}"
    );
    let ratio = crate::turn::provider_input::budget::observed_input_ratio_basis_points(
        1_001_685,
        rejected_estimate,
    )
    .unwrap();
    assert_eq!(
        estimator_budget,
        crate::turn::provider_input::budget::estimator_input_budget(750_000, ratio),
        "the forced budget is the provider limit's threshold scaled by the observed ratio"
    );
    assert_eq!(
        collected
            .attempts
            .iter()
            .map(|attempt| (attempt.attempt, attempt.will_retry, attempt.backoff))
            .collect::<Vec<_>>(),
        vec![(0, true, Duration::ZERO)]
    );
    let histories = model.seen_histories().await;
    assert_eq!(histories.len(), 2);
    assert_eq!(
        histories[1],
        vec![Message::user("compacted prompt")],
        "the retry dispatches the compacted provider input"
    );
}

#[tokio::test(start_paused = true)]
async fn second_provider_context_overflow_fails_after_the_one_compaction() {
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::FailStream(CompletionError::ProviderError(
            CLAUDE_PROMPT_TOO_LONG.to_string(),
        )),
        ScriptedCall::FailStream(CompletionError::ProviderError(
            CLAUDE_PROMPT_TOO_LONG.to_string(),
        )),
    ]);
    let admitted = AdmittedReduction::default();

    let collected = collect_scripted_stream(run_loop_stream(
        model.clone(),
        None::<crate::tools::call::hook::DefraSessionHook>,
        live_sized_prompt(),
        Vec::new(),
        Arc::new(Vec::new()),
        overflow_loop_config(admitted.clone()),
    ))
    .await;

    let error = collected.error.expect("a second overflow ends the request");
    assert!(
        error.contains("provider context overflow persisted after the one forced compaction"),
        "{error}"
    );
    assert_eq!(admitted.lock().unwrap().len(), 1);
    assert_eq!(model.seen_histories().await.len(), 2);
}

fn echo_call(id: &str) -> ScriptedCall {
    ScriptedCall::Turn(vec![
        RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
            id.to_string(),
            "echo".to_string(),
            serde_json::json!({}),
        )),
        RawStreamingChoice::FinalResponse(()),
    ])
}

/// The one forced compaction is per completion: a later turn of the same
/// request that overflows again compacts again, while a second overflow
/// within one turn still ends the request.
#[tokio::test(start_paused = true)]
async fn each_completion_turn_may_compact_once_after_an_overflow() {
    let overflow =
        || ScriptedCall::FailStream(CompletionError::ProviderError(CLAUDE_PROMPT_TOO_LONG.into()));
    let model = ScriptedModel::new_calls(vec![
        overflow(),
        echo_call("call-1"),
        echo_call("call-2"),
        overflow(),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);
    let admitted = AdmittedReduction::default();
    let mut loop_config = overflow_loop_config(admitted.clone());
    loop_config.max_turns = 4;
    let keys = Arc::new(AtomicUsize::new(0));
    loop_config.turn_compactor = Some(Arc::new({
        let admitted = admitted.clone();
        move |request: TurnCompactionRequest| {
            admitted.lock().unwrap().push((
                request.admission.input_tokens(),
                request.admission.effective_input_budget(),
            ));
            let key = keys.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                Ok(TurnCompactionOutcome::Reduced {
                    messages: vec![TaggedMessage::unassociated(Message::user(
                        "compacted prompt",
                    ))],
                    reduction_key: format!("overflow-reduction-{key}"),
                })
            })
        }
    }));

    let collected = collect_scripted_stream(run_loop_stream(
        model.clone(),
        None::<crate::tools::call::hook::DefraSessionHook>,
        live_sized_prompt(),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        loop_config,
    ))
    .await;

    assert_eq!(collected.error, None);
    assert_eq!(collected.final_text.as_deref(), Some("done"));
    assert_eq!(
        admitted.lock().unwrap().len(),
        2,
        "turn 1 and turn 3 each compact once"
    );
    assert_eq!(
        collected
            .attempts
            .iter()
            .map(|attempt| (attempt.turn, attempt.will_retry))
            .collect::<Vec<_>>(),
        vec![(0, true), (2, true)]
    );
    assert_eq!(model.seen_histories().await.len(), 5);
}

#[tokio::test(start_paused = true)]
async fn provider_context_overflow_without_a_compactor_fails_with_its_reason() {
    let model = ScriptedModel::new_calls(vec![ScriptedCall::FailStream(
        CompletionError::ProviderError(CLAUDE_PROMPT_TOO_LONG.to_string()),
    )]);
    let mut loop_config = overflow_loop_config(AdmittedReduction::default());
    loop_config.turn_compactor = None;

    let collected = collect_scripted_stream(run_loop_stream(
        model.clone(),
        None::<crate::tools::call::hook::DefraSessionHook>,
        live_sized_prompt(),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    ))
    .await;

    let error = collected
        .error
        .expect("an unreducible overflow ends the request");
    assert!(
        error.contains("no per-turn compactor") && error.contains("prompt is too long"),
        "{error}"
    );
    assert_eq!(model.seen_histories().await.len(), 1);
    assert!(collected.attempts.iter().all(|attempt| !attempt.will_retry));
}

type CapturedAccounting = Arc<std::sync::Mutex<Vec<crate::inference::rendered_request::ContextAccounting>>>;

fn capture_accounting(loop_config: &mut LoopConfig) -> CapturedAccounting {
    let captured = CapturedAccounting::default();
    let sink = captured.clone();
    loop_config.on_rendered_request = Some(Arc::new(move |_, _, _, trace| {
        let sink = sink.clone();
        Box::pin(async move {
            sink.lock().unwrap().extend(trace.context_accounting);
            Ok(())
        })
    }));
    captured
}

#[tokio::test(start_paused = true)]
async fn calibrated_estimate_compacts_before_the_provider_rejects_the_input() {
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("done".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let admitted = AdmittedReduction::default();
    let mut loop_config = overflow_loop_config(admitted.clone());
    loop_config.context_calibration = crate::turn::provider_input::budget::ContextCalibration::with_ratio(
        crate::turn::provider_input::budget::CLAUDE_MESSAGES_PRIOR_INPUT_RATIO_BASIS_POINTS,
    );
    let captured = capture_accounting(&mut loop_config);

    let collected = collect_scripted_stream(run_loop_stream(
        model.clone(),
        None::<crate::tools::call::hook::DefraSessionHook>,
        live_sized_prompt(),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    ))
    .await;

    assert_eq!(collected.error, None);
    assert_eq!(collected.final_text.as_deref(), Some("done"));
    let admitted = admitted.lock().unwrap().clone();
    assert_eq!(
        admitted.len(),
        1,
        "the calibrated estimate crossed the threshold"
    );
    let (raw_before, estimator_budget) = admitted[0];
    assert!(
        raw_before < 750_000,
        "raw estimate stayed below the threshold: {raw_before}"
    );
    assert_eq!(
        estimator_budget,
        crate::turn::provider_input::budget::estimator_input_budget(750_000, 18_000)
    );
    assert_eq!(
        model.seen_histories().await.len(),
        1,
        "no provider rejection"
    );
    let accounting = captured.lock().unwrap().clone();
    assert_eq!(accounting.len(), 1);
    let accounting = &accounting[0];
    assert_eq!(accounting.accounting_version, 3);
    assert_eq!(accounting.input_calibration_basis_points, Some(18_000));
    let raw = accounting.raw_estimated_input_tokens.expect("raw estimate");
    assert_eq!(raw, accounting.components.estimated_input_tokens());
    assert_eq!(
        accounting.estimated_input_tokens,
        crate::turn::provider_input::budget::calibrated_input_tokens(raw, 18_000)
    );
    assert_eq!(
        accounting.pre_compaction_input_tokens,
        Some(crate::turn::provider_input::budget::calibrated_input_tokens(
            raw_before, 18_000
        ))
    );
    assert_eq!(
        accounting.compaction_reason,
        ContextCompactionReason::Compacted
    );
}

#[tokio::test(start_paused = true)]
async fn provider_usage_calibrates_the_next_dispatch() {
    let model = UsageScriptedModel::new(vec![
        usage_echo_tool_turn(UsageResponse {
            usage: Some(Usage {
                input_tokens: 15_006,
                output_tokens: 10,
                total_tokens: 15_016,
                cached_input_tokens: 15_000,
                cache_creation_input_tokens: 3,
            }),
        }),
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(usage_response(15_100, 10, 15_110)),
        ],
    ]);
    let mut loop_config = config(2);
    let calibration = loop_config.context_calibration.clone();
    let captured = capture_accounting(&mut loop_config);

    let collected = collect_scripted_stream(run_loop_stream(
        model,
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(Message::user("x".repeat(40_000))),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        loop_config,
    ))
    .await;

    assert_eq!(collected.error, None);
    let accounting = captured.lock().unwrap().clone();
    assert_eq!(accounting.len(), 2);
    assert_eq!(accounting[0].input_calibration_basis_points, Some(10_000));
    let first_raw = accounting[0].raw_estimated_input_tokens.unwrap();
    assert_eq!(accounting[0].estimated_input_tokens, first_raw);
    let observed =
        crate::turn::provider_input::budget::observed_input_ratio_basis_points(15_006, first_raw)
            .unwrap();
    assert!(observed > 10_000, "{observed}");
    assert_eq!(
        accounting[1].input_calibration_basis_points,
        Some(observed),
        "the first call's cache-inclusive usage calibrates the next dispatch"
    );
    let second_raw = accounting[1].raw_estimated_input_tokens.unwrap();
    let anchored = crate::turn::provider_input::budget::ContextCalibration::with_ratio(
        observed,
    )
    .anchored_at(
        crate::turn::provider_input::budget::ContextAnchor {
            input_tokens: 15_006,
            output_tokens: 10,
            estimated_input_tokens: first_raw,
        },
    )
    .context(second_raw);
    assert!(anchored.anchored.is_some());
    assert_eq!(
        accounting[1].anchored_input_tokens, anchored.anchored,
        "the accepted tool turn anchors the next dispatch"
    );
    assert_eq!(accounting[1].estimated_input_tokens, anchored.tokens);
    assert_eq!(
        calibration.ratio_basis_points(),
        crate::turn::provider_input::budget::observed_input_ratio_basis_points(15_100, second_raw)
            .unwrap()
            .max(observed),
        "the seeding owner sees the loop's observations, highest first"
    );
}

async fn anchored_compaction_run(
    first_output_tokens: u64,
) -> (
    Vec<(usize, usize)>,
    Vec<crate::inference::rendered_request::ContextAccounting>,
) {
    let model = UsageScriptedModel::new(vec![
        usage_echo_tool_turn(UsageResponse {
            usage: Some(Usage {
                input_tokens: 700_000,
                output_tokens: first_output_tokens,
                total_tokens: 700_000 + first_output_tokens,
                cached_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }),
        }),
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(usage_response(1_000, 10, 1_010)),
        ],
    ]);
    let admitted = AdmittedReduction::default();
    let mut loop_config = overflow_loop_config(admitted.clone());
    let captured = capture_accounting(&mut loop_config);

    let collected = collect_scripted_stream(run_loop_stream(
        model,
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(Message::user("x".repeat(1_600_000))),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        loop_config,
    ))
    .await;
    assert_eq!(collected.error, None);
    let admitted = admitted.lock().unwrap().clone();
    let accounting = captured.lock().unwrap().clone();
    (admitted, accounting)
}

#[tokio::test]
async fn anchored_provider_count_triggers_compaction_the_ratio_alone_misses() {
    let (admitted, accounting) = anchored_compaction_run(0).await;
    assert!(admitted.is_empty(), "700k counted, no output: below 750k");
    assert_eq!(accounting.len(), 2);
    assert!(accounting[1].anchored_input_tokens.is_some());
    assert!(accounting[1].estimated_input_tokens < 750_000);

    let (admitted, accounting) = anchored_compaction_run(60_000).await;
    assert_eq!(
        admitted.len(),
        1,
        "the anchored 760k crosses the 750k threshold"
    );
    assert_eq!(accounting.len(), 2);
    let second = &accounting[1];
    assert_eq!(second.compaction_reason, ContextCompactionReason::Compacted);
    assert!(
        second.pre_compaction_input_tokens.unwrap() >= 760_000,
        "{:?}",
        second.pre_compaction_input_tokens
    );
    assert_eq!(
        second.anchored_input_tokens, None,
        "the reduction drops the anchor for the rebuilt request"
    );
    let raw = second.raw_estimated_input_tokens.unwrap();
    assert_eq!(
        second.estimated_input_tokens,
        crate::turn::provider_input::budget::calibrated_input_tokens(
            raw,
            second.input_calibration_basis_points.unwrap()
        )
    );
}

async fn pre_turn_admissions(incoming_bytes: usize) -> usize {
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("done".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let admitted = AdmittedReduction::default();
    let mut loop_config = overflow_loop_config(admitted.clone());
    loop_config.context_calibration = crate::turn::provider_input::budget::ContextCalibration::with_ratio(
        crate::turn::provider_input::budget::CLAUDE_MESSAGES_PRIOR_INPUT_RATIO_BASIS_POINTS,
    );
    let history = vec![
        TaggedMessage::unassociated(Message::user("h".repeat(1_200_000))),
        TaggedMessage::unassociated(Message::assistant("ok")),
    ];

    let collected = collect_scripted_stream(run_loop_stream(
        model,
        None::<crate::tools::call::hook::DefraSessionHook>,
        TaggedMessage::unassociated(Message::user("y".repeat(incoming_bytes))),
        history,
        Arc::new(Vec::new()),
        loop_config,
    ))
    .await;
    assert_eq!(collected.error, None);
    let admitted = admitted.lock().unwrap().len();
    admitted
}

#[tokio::test(start_paused = true)]
async fn pre_turn_check_counts_the_incoming_user_message() {
    assert_eq!(
        pre_turn_admissions(16).await,
        0,
        "the history alone (~540k calibrated) is under the 750k budget"
    );
    assert_eq!(
        pre_turn_admissions(600_000).await,
        1,
        "the incoming message takes the same turn over the budget"
    );
}
