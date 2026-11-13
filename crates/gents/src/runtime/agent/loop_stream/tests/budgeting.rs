#[test]
fn turn_budgets_follow_the_lean_model() {
    gents_lean_vocab::families::contracts_prompt_assembly_turn_budget::replay(|case| {
        let context_window = usize::try_from(case.context_window).unwrap();
        let max_output_tokens = usize::try_from(case.max_output_tokens).unwrap();
        let turn_input_tokens = case
            .turn_input_tokens
            .iter()
            .map(|tokens| usize::try_from(*tokens).unwrap())
            .collect::<Vec<_>>();
        let threshold = case.threshold_basis_points as f64 / 10_000.0;
        let compaction_budget = crate::turn::provider_input::budget::compaction_input_budget(
            context_window,
            threshold,
            max_output_tokens,
        );
        let turn_can_dispatch = turn_input_tokens
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
                    max_tokens: u64::try_from(max_output_tokens).ok(),
                    tool_choice: None,
                    additional_params: None,
                    output_schema: None,
                };
                let mut witness_config = config(0);
                witness_config.context_window = context_window;
                witness_config.max_tokens = u64::try_from(max_output_tokens).ok();
                clamp_request_output_budget(&mut request, &witness_config, *tokens);
                let clamped = request
                    .max_tokens
                    .and_then(|output| usize::try_from(output).ok())
                    .unwrap_or_default();
                assert_eq!(
                    clamped,
                    crate::turn::provider_input::budget::effective_output_budget(
                        *tokens,
                        context_window,
                        max_output_tokens,
                    ),
                    "{case:?}: production request clamp differs from the output budget owner"
                );
                ensure_context_can_dispatch(&request, &witness_config, *tokens).is_ok()
            })
            .collect();
        gents_lean_vocab::families::contracts_prompt_assembly_turn_budget::PromptAssemblyTurnBudgetOutput {
            configured_threshold_budget: crate::turn::provider_input::budget::threshold_budget(
                context_window,
                threshold,
            ) as u64,
            effective_input_budget: crate::turn::provider_input::budget::effective_input_budget(
                context_window,
                threshold,
            ) as u64,
            compaction_input_budget: compaction_budget as u64,
            turn_output_tokens: turn_input_tokens
                .iter()
                .map(|tokens| {
                    crate::turn::provider_input::budget::effective_output_budget(
                        *tokens,
                        context_window,
                        max_output_tokens,
                    ) as u64
                })
                .collect(),
            turn_should_compact: turn_input_tokens
                .iter()
                .map(|tokens| {
                    gents_inference::compaction::ReductionAdmission::for_context(
                        crate::turn::provider_input::budget::ContextCalibration::default()
                            .context(*tokens),
                        compaction_budget,
                    )
                    .is_some()
                })
                .collect(),
            turn_can_dispatch,
        }
    });
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

#[test]
fn prompt_budgets_follow_the_lean_model() {
    gents_lean_vocab::families::contracts_prompt_assembly_budget::replay(|case| {
        let context_window = usize::try_from(case.context_window).unwrap();
        let max_output_tokens = usize::try_from(case.max_output_tokens).unwrap();
        let threshold = case.threshold_basis_points as f64 / 10_000.0;
        let input_tokens = usize::try_from(case.prompt_tokens)
            .unwrap()
            .saturating_add(usize::try_from(case.request_tokens).unwrap());
        let effective_output = crate::turn::provider_input::budget::effective_output_budget(
            input_tokens,
            context_window,
            max_output_tokens,
        );
        let compaction_budget = crate::turn::provider_input::budget::compaction_input_budget(
            context_window,
            threshold,
            max_output_tokens,
        );
        gents_lean_vocab::families::contracts_prompt_assembly_budget::PromptAssemblyBudgetOutput {
            configured_threshold_budget: crate::turn::provider_input::budget::threshold_budget(
                context_window,
                threshold,
            ) as u64,
            effective_input_budget: crate::turn::provider_input::budget::effective_input_budget(
                context_window,
                threshold,
            ) as u64,
            compaction_input_budget: compaction_budget as u64,
            effective_output_tokens: effective_output as u64,
            should_compact: gents_inference::compaction::ReductionAdmission::for_context(
                crate::turn::provider_input::budget::ContextCalibration::default()
                    .context(input_tokens),
                compaction_budget,
            )
            .is_some(),
            provider_safe: input_tokens.saturating_add(effective_output) <= context_window,
            can_dispatch: crate::turn::provider_input::budget::can_dispatch(
                input_tokens,
                context_window,
                max_output_tokens,
            ),
        }
    });
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
