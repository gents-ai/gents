#[test]
fn generated_turn_budget_cases_drive_every_completion_dispatch() {
    let cases = gents_lean_vocab::lean_prompt_assembly_turn_budget_cases();
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
            crate::turn::provider_input::budget::effective_input_budget(
                case.context_window,
                threshold
            ),
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
                gents_inference::compaction::ReductionAdmission::for_context(
                    crate::turn::provider_input::budget::ContextCalibration::default()
                        .context(*tokens),
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

/// A staged per-turn reduction writes its row only once the rebuilt request
/// fits (#3283). Each expected row count is the model's
/// `commitRequestLocalAfterRebuild` for the matching rebuilt result.
#[tokio::test]
async fn staged_turn_reduction_commits_its_row_only_when_the_rebuilt_request_fits() {
    use gents_store::identity::NodeIdentity as _;
    let cases = gents_lean_vocab::lean_compaction_projection_join_cases();
    let committed = |result: &str| {
        cases
            .iter()
            .find(|case| case.result == result)
            .unwrap_or_else(|| panic!("Lean emitted no {result} projection-join case"))
            .committed_reductions
    };
    for (retained, max_tokens, rebuilt_result) in [
        ("x".repeat(8_000), 1_000, "rebuilt_still_over"),
        ("compacted prompt".to_string(), 0, "zero_output_capacity"),
        ("compacted prompt".to_string(), 1_000, "dispatch"),
    ] {
        let expected_rows = committed(rebuilt_result);
        let (node_db, _user, _user_access, binding) =
            gents_inference::test_support::signed_test_node(
                "staged-turn-reduction-commits-its-row-on",
                gents_automation::lifecycle_hooks::composition(),
            )
            .await;
        let node = node_db.node.clone();
        let root = gents_store::test_support::held_runtime_identity(node_db.node_identity.did());
        let agent = gents_store::identity::resolve_agent_identity(&root, &binding).unwrap();
        let root_did = gents_store::graphql::escape_graphql_string(root.did());
        let agent_did = gents_store::graphql::escape_graphql_string(agent.did());
        gents_store::config_client::ConfigAccess::local_managed_agent(node.clone(), agent.did().parse().unwrap(), agent.as_ref()).unwrap()
            .write("test.reduction.session", &format!(r#"mutation {{ create_AgentSession(input: {{ session_id: "session-3283", node_did: "{root_did}", agent_did: "{agent_did}", created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#)).await.unwrap();
        let request_doc_id = "request-doc-3283";
        let model = ScriptedModel::new(vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]);
        let mut loop_config = config(0);
        loop_config.max_tokens = Some(max_tokens);
        loop_config.context_window = 6_500;
        loop_config.compaction_threshold = 0.25;
        let compaction_node = node.clone();
        let compaction_agent_did = agent.did().to_owned();
        let compaction_node_did = root.did().to_owned();
        let compaction_signer = agent.managed_agent_signer().unwrap();
        loop_config.turn_compactor = Some(Arc::new(move |_| {
            let node = compaction_node.clone();
            let agent_did = compaction_agent_did.clone();
            let node_did = compaction_node_did.clone();
            let signer = compaction_signer.clone();
            let retained = retained.clone();
            Box::pin(async move {
                let compacted_prefix = [Message::user("x".repeat(8_000))];
                let retained_suffix = [Message::user(retained)];
                let tagged = |messages: &[Message]| {
                    messages
                        .iter()
                        .cloned()
                        .map(TaggedMessage::unassociated)
                        .collect::<Vec<_>>()
                };
                let associations =
                    gents_session::session::provider_context_reduction::ReplayAssociations::from_tagged_split(
                        Vec::new(),
                        &tagged(&compacted_prefix),
                        &tagged(&retained_suffix),
                    );
                let boundary = gents_protocol::transcript::SourceBoundary {
                    boundary_version: 2,
                    request_doc_id: request_doc_id.to_string(),
                    request_commit_cid: "claim-cid-3283".to_string(),
                    canonical_through: None,
                };
                gents_session::session::provider_context_reduction::stage(
                    gents_session::session::provider_context_reduction::NewProviderContextReduction {
                        agent_did: &agent_did,
                        node_did: &node_did,
                        requester_did: None,
                        session_id: "session-3283",
                        request_id: "request-3283",
                        request_doc_id,
                        request_commit_cid: "claim-cid-3283",
                        reduction_index: 1,
                        turn_index: 0,
                        parent_reduction_key: None,
                        producer_call: None,
                        source_boundary: &boundary,
                        compacted_prefix: &compacted_prefix,
                        retained_suffix: &retained_suffix,
                        checkpoint_messages: &retained_suffix,
                        replay_associations: &associations,
                        summary: "",
                        original_tokens: 2_000,
                        compacted_tokens: 10,
                    },
                )
                .and_then(|staged| staged.into_turn_outcome(node, signer))
                .map_err(|error| TurnCompactionError::Compactor(error.into()))
            })
        }));

        // The commit writes through the database, so drive the stream on the
        // real clock rather than the paused-clock scripted collector.
        let stream = run_loop_stream(
            model,
            None::<gents_toolcalls::call::hook::DefraSessionHook>,
            TaggedMessage::unassociated(Message::user("x".repeat(8_000))),
            Vec::new(),
            Arc::new(Vec::new()),
            loop_config,
        );
        futures::pin_mut!(stream);
        let mut text = String::new();
        let mut error = None;
        while let Some(item) = stream.next().await {
            match item {
                Ok(LoopStreamItem::Text(chunk)) => text.push_str(&chunk),
                Ok(_) => {}
                Err(failure) => {
                    error = Some(format!("{failure:?}"));
                    break;
                }
            }
        }

        // The reduction rows are agent-owned, so they are read as the Agent that
        // wrote them; the node identity cannot see them.
        let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
            node.clone(),
            agent.did().parse().unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        let rows = gents_session::session::provider_context_reduction::load_for_request(
            &agent_access,
            request_doc_id,
        )
        .await
        .unwrap();
        assert_eq!(
            rows.len(),
            expected_rows,
            "{rebuilt_result}: durable reduction rows after the request (error: {error:?})"
        );
        if expected_rows == 0 {
            assert!(
                error.as_deref().is_some_and(|error| error.contains(
                    if rebuilt_result == "zero_output_capacity" {
                        "NoOutputCapacity"
                    } else {
                        "CannotFit"
                    }
                )),
                "{rebuilt_result}: unexpected terminal state: {error:?}"
            );
        } else {
            assert_eq!(error, None, "{rebuilt_result}");
            assert_eq!(text, "done", "{rebuilt_result}");
        }
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct BudgetedStructuredResponse {
    #[allow(dead_code)]
    answer: String,
}

#[test]
fn generated_budget_cases_drive_dynamic_output_compaction_trigger() {
    let cases = gents_lean_vocab::lean_prompt_assembly_budget_cases();
    assert!(
        !cases.is_empty(),
        "Lean emitted no PromptAssembly budget cases"
    );

    for case in cases {
        let threshold = case.threshold_basis_points as f64 / 10_000.0;
        let configured =
            crate::turn::provider_input::budget::threshold_budget(case.context_window, threshold);
        let effective = crate::turn::provider_input::budget::effective_input_budget(
            case.context_window,
            threshold,
        );
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
            gents_inference::compaction::ReductionAdmission::for_context(
                crate::turn::provider_input::budget::ContextCalibration::default()
                    .context(input_tokens),
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
    let cases = gents_lean_vocab::lean_prompt_assembly_compaction_trigger_cases();
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
        let admission =
            gents_inference::compaction::ReductionAdmission::for_context(context, budget)
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
                commit: TurnReductionCommit::new(|| async { Ok(()) }),
            })
        })
    }));
    loop_config
}

fn live_sized_prompt() -> TaggedMessage {
    TaggedMessage::unassociated(Message::user("x".repeat(2_244_000)))
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

type CapturedAccounting =
    Arc<std::sync::Mutex<Vec<gents_inference::rendered_request::ContextAccounting>>>;

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

async fn anchored_compaction_run(
    first_output_tokens: u64,
) -> (
    Vec<(usize, usize)>,
    Vec<gents_inference::rendered_request::ContextAccounting>,
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
        None::<gents_toolcalls::call::hook::DefraSessionHook>,
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

async fn pre_turn_admissions(incoming_bytes: usize) -> usize {
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("done".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let admitted = AdmittedReduction::default();
    let mut loop_config = overflow_loop_config(admitted.clone());
    loop_config.context_calibration =
        crate::turn::provider_input::budget::ContextCalibration::with_ratio(
            crate::turn::provider_input::budget::CLAUDE_MESSAGES_PRIOR_INPUT_RATIO_BASIS_POINTS,
        );
    let history = vec![
        TaggedMessage::unassociated(Message::user("h".repeat(1_200_000))),
        TaggedMessage::unassociated(Message::assistant("ok")),
    ];

    let collected = collect_scripted_stream(run_loop_stream(
        model,
        None::<gents_toolcalls::call::hook::DefraSessionHook>,
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
