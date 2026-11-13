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

/// A staged per-turn reduction writes its row only once the rebuilt request
/// fits (#3283).
#[tokio::test]
async fn staged_turn_reduction_commits_its_row_only_when_the_rebuilt_request_fits() {
    use gents_store::identity::NodeIdentity as _;
    for case in gents_lean_vocab::families::contract_cases_compaction_projection_join_staged_turn_reduction::cases() {
        let (retained, max_tokens) = match case.input.name.as_str() {
            "rebuilt_still_over" => ("x".repeat(8_000), 1_000),
            "zero_output_capacity" => ("compacted prompt".to_string(), 0),
            "threshold_plus_one" => ("compacted prompt".to_string(), 1_000),
            other => panic!("no staged-turn fixture builds {other}"),
        };
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
        let result = match error.as_deref() {
            None if text == "done" => "dispatch",
            Some(error) if error.contains("NoOutputCapacity") => "zero_output_capacity",
            Some(error) if error.contains("CannotFit") => "rebuilt_still_over",
            _ => panic!("{}: unexpected terminal state: {error:?} after {text:?}", case.input.name),
        };
        case.check(
            gents_lean_vocab::families::contract_cases_compaction_projection_join_staged_turn_reduction::StagedTurnOutput {
                result: result.to_owned(),
                committed_reductions: rows.len() as u64,
            },
        );
    }
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
