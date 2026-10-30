use super::tagged_history::tagged_from_sourced;
use super::*;
use crate::claude_messages_body::{ReasoningWitness, ReplayIssuer, ReplayOrigin};
use crate::provider_input::ProviderInputProfile;
use gents_protocol::message::Text;

fn provider_tag(turn_index: u32) -> ReplayTag {
    ReplayTag {
        request_doc_id: "request".into(),
        source: OutputSource::ProviderTurn {
            scope: "inference.1".parse().unwrap(),
            turn_index,
            attempt: 0,
        },
    }
}

fn associated_assistant(tag: &ReplayTag) -> TaggedMessage {
    TaggedMessage {
        message: Message::assistant("native assistant row"),
        source: Some(tag.clone()),
        physical_header: None,
        block_indices: Vec::new(),
    }
}

#[test]
fn replay_prefix_bound_preserves_the_entire_required_capture_prefix() {
    let earlier = provider_tag(0);
    let required = provider_tag(1);
    let rows = vec![
        TaggedMessage::unassociated(Message::system("preamble")),
        TaggedMessage::unassociated(Message::user("first")),
        associated_assistant(&earlier),
        TaggedMessage::unassociated(Message::user("second")),
        TaggedMessage::unassociated(Message::assistant("authored history")),
        associated_assistant(&required),
    ];
    assert_eq!(
        replay_compaction_prefix_bound(&rows, &[required]).unwrap(),
        Some(0),
        "summarizing any earlier row can invalidate the required capture"
    );
}

#[test]
fn replay_prefix_bound_without_required_sources_is_unbounded() {
    let rows = vec![
        TaggedMessage::unassociated(Message::user("input")),
        associated_assistant(&provider_tag(0)),
    ];
    assert_eq!(replay_compaction_prefix_bound(&rows, &[]), Ok(None));
}

#[test]
fn replay_prefix_bound_reuses_checkpoint_missing_and_duplicate_errors() {
    let required = provider_tag(1);
    let other = provider_tag(0);
    let missing = vec![associated_assistant(&other)];
    assert_eq!(
        replay_compaction_prefix_bound(&missing, std::slice::from_ref(&required)),
        Err(ReplayCheckpointError::MissingRequired)
    );

    let duplicated = vec![
        associated_assistant(&required),
        associated_assistant(&required),
    ];
    assert_eq!(
        replay_compaction_prefix_bound(&duplicated, &[required]),
        Err(ReplayCheckpointError::DuplicateAssociation)
    );
}

#[test]
fn replay_prefix_bound_rejects_tagged_non_assistant() {
    let required = provider_tag(0);
    let rows = vec![TaggedMessage {
        message: Message::user("input"),
        source: Some(required.clone()),
        physical_header: None,
        block_indices: Vec::new(),
    }];
    assert_eq!(
        replay_compaction_prefix_bound(&rows, &[required]),
        Err(ReplayCheckpointError::InvalidAssociation)
    );
}

#[test]
fn provider_projection_composes_original_physical_block_indices() {
    let tag = provider_tag(0);
    let originals = vec![TaggedMessage {
        message: Message::Assistant {
            id: None,
            content: vec![
                AssistantContent::Text(Text {
                    text: "first".into(),
                }),
                AssistantContent::Text(Text {
                    text: "second".into(),
                }),
            ],
        },
        source: Some(tag.clone()),
        physical_header: Some("physical-header".into()),
        block_indices: vec![2, 4],
    }];
    let shaped = vec![crate::compaction::history::SourcedMessage {
        source_index: 0,
        block_indices: vec![1],
        message: Message::Assistant {
            id: None,
            content: vec![AssistantContent::Text(Text {
                text: "second".into(),
            })],
        },
    }];
    let actual = tagged_from_sourced(&originals, shaped).expect("source-index mapping");
    assert_eq!(actual[0].source, Some(tag));
    assert_eq!(
        actual[0].physical_header.as_deref(),
        Some("physical-header")
    );
    assert_eq!(actual[0].block_indices, [4]);
}

#[test]
fn association_rejects_a_provider_order_applied_before_it() {
    let original = TaggedMessage {
        message: Message::Assistant {
            id: None,
            content: vec![
                AssistantContent::Reasoning(gents_protocol::message::Reasoning {
                    id: None,
                    content: vec![gents_protocol::message::ReasoningContent::Summary(
                        "summary".into(),
                    )],
                }),
                AssistantContent::Text(Text {
                    text: "visible".into(),
                }),
            ],
        },
        source: Some(provider_tag(0)),
        physical_header: Some("physical-header".into()),
        block_indices: vec![2, 4],
    };
    let sourced = crate::compaction::history::normalize_assistant_content_order_sourced(
        ProviderInputProfile::OpenAiResponsesNormalized,
        vec![crate::compaction::history::SourcedMessage {
            source_index: 0,
            block_indices: vec![0, 1],
            message: original.message.clone(),
        }],
    );
    assert!(
        tagged_from_sourced(&[original], sourced).is_err(),
        "a grouped reorder cannot be carried on persisted block identity"
    );
}

#[test]
fn grouped_projection_of_a_projected_row_keeps_persisted_block_identity() {
    use gents_protocol::message::{ToolCall, ToolFunction, ToolResult, ToolResultContent};
    let tag = provider_tag(0);
    let persisted = vec![
        TaggedMessage::unassociated(Message::user("start")),
        TaggedMessage {
            message: Message::Assistant {
                id: None,
                content: vec![
                    AssistantContent::Reasoning(gents_protocol::message::Reasoning::new(
                        "thinking",
                    )),
                    AssistantContent::Text(Text {
                        text: "visible".into(),
                    }),
                    AssistantContent::ToolCall(ToolCall {
                        id: "call-1".into(),
                        call_id: Some("call-1".into()),
                        function: ToolFunction {
                            name: "read".into(),
                            arguments: serde_json::json!({}),
                        },
                        signature: None,
                        additional_params: None,
                    }),
                ],
            },
            source: Some(tag.clone()),
            physical_header: Some("physical-header".into()),
            block_indices: vec![0, 1, 2],
        },
        TaggedMessage::unassociated(Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                id: "call-1".into(),
                call_id: Some("call-1".into()),
                content: vec![ToolResultContent::text("ok")],
            })],
        }),
        TaggedMessage::unassociated(Message::user("wake")),
    ];
    let once = provider_view_tagged(persisted.clone()).expect("first projection");
    assert_eq!(
        once[1], persisted[1],
        "association keeps native order and indices"
    );
    let entry = sanitize_tagged_history(once.clone())
        .expect("loop entry re-sanitizes the daemon's projection");
    assert_eq!(entry, once);
    let repaired = sanitize_tagged_history(entry.clone()).expect("repair re-sanitizes");
    assert_eq!(repaired, once);
    assert_eq!(provider_view_tagged(entry).expect("re-projection"), once);

    let native = message_values(&once);
    for profile in [
        ProviderInputProfile::OpenAiChatCompletions,
        ProviderInputProfile::OpenAiResponsesNormalized,
    ] {
        let ordered = provider_messages(profile, &once);
        let Message::Assistant { content, .. } = &ordered[1] else {
            panic!("assistant row")
        };
        assert!(
            matches!(
                content.as_slice(),
                [
                    AssistantContent::Text(_),
                    AssistantContent::Reasoning(_),
                    AssistantContent::ToolCall(_)
                ]
            ),
            "{profile:?} orders at the send boundary"
        );
        assert_eq!(ordered.len(), native.len());
    }
    assert_eq!(
        provider_messages(ProviderInputProfile::ClaudeMessages, &once),
        native,
        "Claude keeps native block order"
    );
}

#[tokio::test]
async fn repeated_sources_are_resolved_once_per_selection() {
    let tag = ReplayTag {
        request_doc_id: "request".into(),
        source: OutputSource::ProviderTurn {
            scope: "inference.1".parse().unwrap(),
            turn_index: 0,
            attempt: 0,
        },
    };
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    let expected = vec![tag.clone()];
    let replay = LoopReplayInput {
        resolve: Some(Arc::new(move |requested| {
            assert_eq!(requested, expected);
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async { Ok(Vec::new()) })
        })),
        ..LoopReplayInput::default()
    };
    assert!(resolve_replay_evidence(&replay, &[tag.clone(), tag])
        .await
        .unwrap()
        .is_empty());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

fn evidence_row(tag: &ReplayTag, header: &str, reasoning: ReasoningWitness) -> ReplayEvidenceRow {
    ReplayEvidenceRow {
        tag: tag.clone(),
        evidence: ResolvedReplayEvidence {
            origin: ReplayOrigin::AcceptedProvider,
            reasoning,
            issuer: ReplayIssuer {
                family: "claude".into(),
                endpoint: "https://provider.test".into(),
            },
            wire: crate::claude_messages_body::ReplayWire::ClaudeMessages,
            physical_header: header.to_owned(),
            complete: true,
            captured: None,
        },
    }
}

fn counting_resolver(
    rows: Vec<ReplayEvidenceRow>,
    expected: Vec<ReplayTag>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
) -> crate::loop_stream::ReplayEvidenceResolver {
    Arc::new(move |requested| {
        assert_eq!(
            requested, expected,
            "the resolver sees only unresolved tags"
        );
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let rows = rows.clone();
        Box::pin(async move { Ok(rows) })
    })
}

fn armed_seed(rows: Vec<ReplayEvidenceRow>) -> ReplayEstimateSeed {
    let seed = ReplayEstimateSeed::default();
    seed.arm();
    seed.record(rows);
    seed
}

#[tokio::test]
async fn a_seeded_lookup_serves_an_identical_tag_set_without_invoking_the_resolver() {
    let first = provider_tag(0);
    let second = provider_tag(1);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let replay = LoopReplayInput {
        resolve: Some(counting_resolver(
            Vec::new(),
            vec![first.clone(), second.clone()],
            calls.clone(),
        )),
        estimate_seed: Some(armed_seed(vec![
            evidence_row(&first, "header-0", Vec::new()),
            evidence_row(&second, "header-1", Vec::new()),
        ])),
        ..LoopReplayInput::default()
    };
    let served = resolve_replay_evidence(&replay, &[first.clone(), second])
        .await
        .unwrap();
    assert_eq!(
        served
            .iter()
            .map(|row| row.evidence.physical_header.clone())
            .collect::<Vec<_>>(),
        ["header-0", "header-1"],
        "the dispatch lookup serves the estimate's rows unchanged"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "an identical tag set needs no second canonical pass"
    );
}

#[tokio::test]
async fn a_seeded_lookup_resolves_only_the_tags_the_seed_does_not_cover() {
    let seeded = provider_tag(0);
    let missing = provider_tag(1);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let replay = LoopReplayInput {
        resolve: Some(counting_resolver(
            vec![evidence_row(&missing, "header-1", Vec::new())],
            vec![missing.clone()],
            calls.clone(),
        )),
        estimate_seed: Some(armed_seed(vec![evidence_row(
            &seeded,
            "header-0",
            Vec::new(),
        )])),
        ..LoopReplayInput::default()
    };
    let served = resolve_replay_evidence(&replay, &[seeded.clone(), missing.clone()])
        .await
        .unwrap();
    assert_eq!(
        served
            .iter()
            .map(|row| row.evidence.physical_header.clone())
            .collect::<Vec<_>>(),
        ["header-0", "header-1"],
        "seeded rows and freshly resolved rows compose one answer"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "only the uncovered tag crosses the canonical resolver"
    );
}

#[tokio::test]
async fn a_seed_is_taken_once_so_the_next_lookup_re_resolves() {
    let tag = provider_tag(0);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let replay = LoopReplayInput {
        resolve: Some(counting_resolver(
            vec![evidence_row(&tag, "header-0", Vec::new())],
            vec![tag.clone()],
            calls.clone(),
        )),
        estimate_seed: Some(armed_seed(vec![evidence_row(&tag, "header-0", Vec::new())])),
        ..LoopReplayInput::default()
    };
    resolve_replay_evidence(&replay, std::slice::from_ref(&tag))
        .await
        .unwrap();
    let re_resolved = resolve_replay_evidence(&replay, std::slice::from_ref(&tag))
        .await
        .unwrap();
    assert_eq!(
        re_resolved
            .iter()
            .map(|row| row.evidence.physical_header.clone())
            .collect::<Vec<_>>(),
        ["header-0"],
        "the second lookup still answers the same evidence"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "every lookup after the seeded dispatch revalidates the store"
    );
}

#[tokio::test]
async fn a_seeded_lookup_serves_only_rows_for_the_requested_tags() {
    let requested = provider_tag(0);
    let superset = provider_tag(1);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let replay = LoopReplayInput {
        resolve: Some(counting_resolver(Vec::new(), vec![], calls.clone())),
        estimate_seed: Some(armed_seed(vec![
            evidence_row(&requested, "header-0", Vec::new()),
            evidence_row(&superset, "header-1", Vec::new()),
        ])),
        ..LoopReplayInput::default()
    };
    let served = resolve_replay_evidence(&replay, &[requested])
        .await
        .unwrap();
    assert_eq!(
        served
            .iter()
            .map(|row| row.evidence.physical_header.clone())
            .collect::<Vec<_>>(),
        ["header-0"],
        "a subset dispatch never sees the estimate's other tags"
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[derive(Clone, Default)]
struct AssemblyOnlyModel;

#[allow(refining_impl_trait)]
impl rig::completion::CompletionModel for AssemblyOnlyModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_client: &Self::Client, _model: impl Into<String>) -> Self {
        unreachable!("constructed directly")
    }

    async fn completion(
        &self,
        _request: rig::completion::CompletionRequest,
    ) -> Result<rig::completion::CompletionResponse<Self::Response>, rig::completion::CompletionError>
    {
        unreachable!("assembly tests never dispatch")
    }

    async fn stream(
        &self,
        _request: rig::completion::CompletionRequest,
    ) -> Result<
        rig::streaming::StreamingCompletionResponse<Self::StreamingResponse>,
        rig::completion::CompletionError,
    > {
        unreachable!("assembly tests never dispatch")
    }
}

fn replay_assembly_config() -> LoopConfig {
    LoopConfig {
        replay: LoopReplayInput::default(),
        provider_input_counter: Arc::new(crate::provider_input::ProviderInputCounter::new(
            crate::backend_provider::BackendProviderKind::ClaudeCliSubscription,
            crate::openai_wire::OpenAiWireApi::ChatCompletions,
            "test-model",
        )),
        preamble: None,
        context_message: None,
        temperature: None,
        max_tokens: None,
        aggregate_token_budget: None,
        additional_params: None,
        structured_output: None,
        tool_choice: None,
        on_rendered_request: None,
        turn_compactor: None,
        active_reduction_keys: Vec::new(),
        reduction_chain_keys: Vec::new(),
        initial_turn_index: 0,
        resume_from_history: false,
        context_window: 128_000,
        compaction_threshold: 0.75,
        context_calibration: Default::default(),
        retry_policy: CompletionRetryPolicy::interactive_default(),
        provider_idle_timeout: None,
        deadline: None,
        max_turns: 4,
        output_obligation_gate: None,
        folded_prompts: Vec::new(),
        authored_input: None,
    }
}

#[cfg(feature = "native")]
#[tokio::test]
async fn cross_provider_tool_identity_reaches_dispatch_and_rebuilt_history() {
    use crate::backend_provider::BackendProviderKind;
    use crate::provider_input::tool_call_identity_tests::{
        assert_responses_body, cases, counter, messages,
    };
    for case in cases().into_iter().filter(|case| case.responses) {
        let original = messages(&case);
        let prompt = original.last().unwrap().clone();
        let history = &original[..original.len() - 1];
        for provider in [
            BackendProviderKind::OpenAiCompatible,
            BackendProviderKind::ChatGptCodex,
            BackendProviderKind::XaiGrokOAuth,
        ] {
            let mut config = replay_assembly_config();
            config.provider_input_counter = Arc::new(counter(provider));
            config.preamble = Some("System instructions".into());
            let mut request = super::build_request(
                &AssemblyOnlyModel,
                prompt.clone(),
                history,
                &[],
                &[],
                &config,
            )
            .await
            .unwrap();
            let dispatch_body = |request: &rig::completion::CompletionRequest| {
                let dto = rig::providers::openai::responses_api::CompletionRequest::try_from((
                    "test-model".into(),
                    request.clone(),
                ))
                .unwrap();
                let mut body = serde_json::to_value(dto).unwrap();
                match provider {
                    BackendProviderKind::OpenAiCompatible => {
                        crate::responses_normalize::normalize_responses_assistant_items(&mut body)
                    }
                    BackendProviderKind::ChatGptCodex => {
                        if let Some(patched) = crate::provider_patches::patch_instructions_body(
                            &serde_json::to_vec(&body).unwrap(),
                        ) {
                            body = serde_json::from_slice(&patched).unwrap();
                        }
                    }
                    _ => {}
                }
                body
            };
            let openai = provider != BackendProviderKind::XaiGrokOAuth;
            assert_responses_body(&dispatch_body(&request), &case, openai);
            assert_responses_body(
                &config
                    .provider_input_counter
                    .project_body(&request)
                    .unwrap(),
                &case,
                openai,
            );
            super::replace_core_chat_history(
                &mut request,
                &config,
                &history
                    .iter()
                    .cloned()
                    .map(TaggedMessage::unassociated)
                    .collect::<Vec<_>>(),
                &[TaggedMessage::unassociated(prompt.clone())],
            )
            .unwrap();
            assert_responses_body(&dispatch_body(&request), &case, openai);
            assert_eq!(original, messages(&case));
        }
    }
}

async fn producing_capture(
    model: &AssemblyOnlyModel,
    config: &LoopConfig,
    content: &[AssistantContent],
) -> ResolvedReplayEvidence {
    use crate::claude_messages_body::ReplayWire;
    let producing = super::build_request(model, Message::user("prompt"), &[], &[], &[], config)
        .await
        .expect("producing request builds");
    let body = config
        .provider_input_counter
        .project_body(&producing)
        .expect("producing body projects");
    ResolvedReplayEvidence {
        reasoning: crate::claude_messages_body::reasoning_witness(content),
        captured: Some(
            crate::provider_input::replay_frontier::flatten(&body, ReplayWire::ClaudeMessages)
                .expect("producing body flattens"),
        ),
        ..evidence_row(&provider_tag(0), "header", Vec::new()).evidence
    }
}

fn replay_assembly_content() -> Vec<AssistantContent> {
    use gents_protocol::message::{Reasoning, Text};
    vec![
        AssistantContent::Reasoning(Reasoning::new_with_signature(
            "thinking",
            Some("sig".into()),
        )),
        AssistantContent::Text(Text {
            text: "visible".into(),
        }),
    ]
}

fn replay_assembly_history(tag: &ReplayTag) -> Vec<TaggedMessage> {
    vec![
        TaggedMessage::unassociated(Message::user("prompt")),
        TaggedMessage {
            message: Message::Assistant {
                id: None,
                content: replay_assembly_content(),
            },
            source: Some(tag.clone()),
            physical_header: Some("header".into()),
            block_indices: vec![0, 1],
        },
    ]
}

#[tokio::test]
async fn a_seeded_dispatch_assembles_the_same_request_as_a_fresh_resolution() {
    let tag = provider_tag(0);
    let config = replay_assembly_config();
    let issuer = ReplayIssuer {
        family: "claude".into(),
        endpoint: "https://provider.test".into(),
    };
    let evidence = producing_capture(&AssemblyOnlyModel, &config, &replay_assembly_content()).await;
    let rows = vec![ReplayEvidenceRow {
        tag: tag.clone(),
        evidence,
    }];
    let fresh_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fresh = LoopReplayInput {
        issuer: Some(issuer.clone()),
        wire: Some(crate::claude_messages_body::ReplayWire::ClaudeMessages),
        resolve: Some(counting_resolver(
            rows.clone(),
            vec![tag.clone()],
            fresh_calls.clone(),
        )),
        ..LoopReplayInput::default()
    };
    let seeded_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seeded = LoopReplayInput {
        issuer: Some(issuer),
        wire: Some(crate::claude_messages_body::ReplayWire::ClaudeMessages),
        resolve: Some(counting_resolver(
            rows.clone(),
            vec![tag.clone()],
            seeded_calls.clone(),
        )),
        estimate_seed: Some(armed_seed(rows)),
        ..LoopReplayInput::default()
    };

    let mut fresh_history = replay_assembly_history(&tag);
    let mut fresh_new = vec![TaggedMessage::unassociated(Message::user("next"))];
    let fresh_request = assemble_provider_request(
        &AssemblyOnlyModel,
        &mut fresh_history,
        &mut fresh_new,
        &[],
        &config,
        &fresh,
    )
    .await
    .unwrap();
    let mut seeded_history = replay_assembly_history(&tag);
    let mut seeded_new = vec![TaggedMessage::unassociated(Message::user("next"))];
    let seeded_request = assemble_provider_request(
        &AssemblyOnlyModel,
        &mut seeded_history,
        &mut seeded_new,
        &[],
        &config,
        &seeded,
    )
    .await
    .unwrap();

    assert_eq!(
        fresh_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the fresh pass crosses the resolver once"
    );
    assert_eq!(
        seeded_calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the seeded dispatch crosses the resolver zero times"
    );
    assert_eq!(
        format!("{fresh_request:?}"),
        format!("{seeded_request:?}"),
        "seeding changes no byte of the assembled provider request"
    );
    let body = config
        .provider_input_counter
        .project_body(&fresh_request)
        .unwrap();
    let flat = crate::provider_input::replay_frontier::flatten(
        &body,
        crate::claude_messages_body::ReplayWire::ClaudeMessages,
    )
    .unwrap();
    assert!(
        flat.iter().any(|item| matches!(
            item,
            crate::provider_input::replay_frontier::FlatItem::Reasoning(_)
        )),
        "the fixture keeps the replayed reasoning so the comparison is not vacuous"
    );
}

#[test]
fn stripping_all_reasoning_keeps_ordinary_blocks_and_their_indices() {
    let reasoning = |text: &str| {
        AssistantContent::Reasoning(gents_protocol::message::Reasoning::new_with_signature(
            text,
            Some(format!("sig-{text}")),
        ))
    };
    let mut rows = vec![
        TaggedMessage {
            message: Message::Assistant {
                id: None,
                content: vec![
                    reasoning("a"),
                    AssistantContent::Text(Text {
                        text: "visible".into(),
                    }),
                    reasoning("b"),
                ],
            },
            source: Some(provider_tag(0)),
            physical_header: Some("header".into()),
            block_indices: vec![0, 1, 2],
        },
        TaggedMessage::unassociated(Message::user("next")),
    ];
    strip_all_reasoning(&mut rows);
    let Message::Assistant { content, .. } = &rows[0].message else {
        panic!("assistant row")
    };
    assert!(matches!(content.as_slice(), [AssistantContent::Text(text)] if text.text == "visible"));
    assert_eq!(rows[0].block_indices, [1]);
    assert_eq!(rows[1].message, Message::user("next"));
}

#[test]
fn canonical_violation_retains_its_type() {
    let error = ReplayEvidenceResolveError::Violation(ReplayEvidenceViolation(
        "invalid canonical join".into(),
    ));
    let StreamingError::Completion(CompletionError::RequestError(error)) =
        replay_resolution_error(error)
    else {
        panic!("canonical violation must remain a typed local input failure");
    };
    assert!(error.downcast_ref::<ReplayEvidenceViolation>().is_some());
}

#[test]
fn storage_error_is_not_reclassified_by_message_text() {
    let error = ReplayEvidenceResolveError::Storage {
        source: "canonical replay tag belongs to another physical request".into(),
    };
    assert!(matches!(
        replay_resolution_error(error),
        StreamingError::Completion(CompletionError::ProviderError(_))
    ));
}

/// A compactor whose reduced transcript is under the provider budget by
/// the raw estimate but over it at the session ratio.
fn under_raw_over_calibrated_config(
    compactor_calls: Arc<std::sync::atomic::AtomicUsize>,
) -> LoopConfig {
    let mut config = replay_assembly_config();
    config.context_window = 100_000;
    config.compaction_threshold = 0.75;
    config.context_calibration =
        crate::provider_input::budget::ContextCalibration::with_ratio(20_000);
    config.turn_compactor = Some(Arc::new(move |_request: TurnCompactionRequest| {
        compactor_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async {
            Ok(TurnCompactionOutcome::Reduced {
                messages: vec![
                    TaggedMessage::unassociated(Message::user("s".repeat(160_000))),
                    TaggedMessage::unassociated(Message::user("compacted prompt")),
                ],
                reduction_key: "still-too-big".to_string(),
            })
        })
    }));
    config
}

#[tokio::test]
async fn rebuilt_request_over_the_calibrated_budget_cannot_fit_and_keeps_the_session() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let config = under_raw_over_calibrated_config(calls.clone());
    let original_history = vec![TaggedMessage::unassociated(Message::user(
        "h".repeat(400_000),
    ))];
    let original_prompt = vec![TaggedMessage::unassociated(Message::user("next"))];
    let mut history = original_history.clone();
    let mut new_messages = original_prompt.clone();
    let mut replay = LoopReplayInput::default();
    let mut chain_keys = vec!["earlier-reduction".to_string()];
    let mut active_keys = vec!["earlier-reduction".to_string()];
    config
        .context_calibration
        .anchor_accepted(crate::provider_input::budget::ContextAnchor {
            input_tokens: 1,
            output_tokens: 0,
            estimated_input_tokens: 1,
        });

    let error = build_budgeted_request(
        &AssemblyOnlyModel,
        &mut history,
        &mut new_messages,
        &[],
        &config,
        &mut replay,
        3,
        &mut chain_keys,
        &mut active_keys,
    )
    .await
    .expect_err("the rebuilt request is over the provider budget at the session ratio");

    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(
        format!("{error:?}").contains("CannotFit"),
        "the post-reduction check counts provider tokens: {error:?}"
    );
    let rebuilt_raw =
        crate::provider_input::estimate_json(&serde_json::json!("s".repeat(160_000))).unwrap();
    assert!(
        rebuilt_raw < 75_000,
        "the raw estimate ({rebuilt_raw}) alone would have passed the 75k budget"
    );
    assert_eq!(history, original_history, "the transcript is unchanged");
    assert_eq!(new_messages, original_prompt);
    assert_eq!(chain_keys, vec!["earlier-reduction".to_string()]);
    assert_eq!(active_keys, vec!["earlier-reduction".to_string()]);
    assert!(
        config.context_calibration.anchor().is_some(),
        "a failed reduction keeps the anchor"
    );
}
