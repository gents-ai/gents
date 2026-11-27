use super::tagged_history::{tagged_from_sourced, SourceTagError};
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

#[test]
fn replay_prefix_bound_rejects_tagged_non_assistant() {
    let required = provider_tag(0);
    let rows = vec![TaggedMessage {
        message: Message::user("input"),
        source: Some(required.clone()),
        physical_header: None,
        block_indices: Vec::new(),
        stub_receipts: Vec::new(),
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
            request_id: None,
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
        stub_receipts: Vec::new(),
    }];
    let shaped = vec![crate::compaction::history::SourcedMessage {
        source_index: 0,
        block_indices: vec![1],
        message: Message::Assistant {
            request_id: None,
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
            request_id: None,
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
        stub_receipts: Vec::new(),
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
                request_id: None,
                id: None,
                content: vec![
                    AssistantContent::Reasoning(gents_protocol::message::Reasoning::new(
                        "thinking",
                    )),
                    AssistantContent::Text(Text {
                        text: "visible".into(),
                    }),
                    AssistantContent::ToolCall(ToolCall {
                        raw_arguments: None,
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
            stub_receipts: Vec::new(),
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

/// A provider for assembly tests, which never dispatch: any call panics.
fn assembly_only() -> crate::test_support::scripted_provider::ScriptedProvider {
    crate::test_support::scripted_provider::ScriptedProvider::new([])
}

fn replay_assembly_config() -> LoopConfig {
    LoopConfig {
        provider_call: Default::default(),
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
        configuration_guard: None,
        tool_rebinding: None,
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
        max_invalid_tool_calls: crate::loop_stream::DEFAULT_MAX_INVALID_TOOL_CALLS,
        repeated_tool_failure_limit: crate::loop_stream::DEFAULT_REPEATED_TOOL_FAILURE_LIMIT,
        output_obligation_gate: None,
        folded_prompts: Vec::new(),
        authored_input: None,
        tool_context: crate::tool::ToolCallEnvelope::default(),
    }
}

fn assistant_with(request_id: Option<&str>, call: Option<(&str, &str)>) -> Message {
    Message::Assistant {
        id: Some("msg_x".into()),
        request_id: request_id.map(str::to_owned),
        content: vec![match call {
            Some((id, raw)) => {
                let mut call = gents_protocol::message::ToolCall::new(
                    id.into(),
                    gents_protocol::message::ToolFunction::new("t".into(), serde_json::json!({})),
                );
                call.raw_arguments = Some(raw.to_owned());
                AssistantContent::ToolCall(call)
            }
            None => AssistantContent::text("answer"),
        }],
    }
}

#[tokio::test]
async fn claude_requests_carry_the_previous_request_id_and_raw_tool_text() {
    use crate::claude_messages_body::{
        CLAUDE_REQUEST_PARAM, PREVIOUS_REQUEST_ID_PARAM, RAW_TOOL_ARGUMENTS_BY_CALL_PARAM,
    };
    let mut config = replay_assembly_config();
    config.additional_params = Some(serde_json::json!({ CLAUDE_REQUEST_PARAM: {} }));
    let history = vec![
        Message::user("one"),
        assistant_with(Some("req_1"), Some(("c1", "{ }"))),
        Message::User {
            content: vec![gents_protocol::message::UserContent::tool_result(
                "c1",
                vec![gents_protocol::message::ToolResultContent::text("ok")],
            )],
        },
        assistant_with(Some("req_2"), None),
        Message::user("two"),
    ];
    let params =
        |request: &rig::completion::CompletionRequest| request.additional_params.clone().unwrap();
    let request = super::build_request(
        &assembly_only(),
        Message::user("three"),
        &history,
        &[],
        &[],
        &config,
    )
    .await
    .unwrap();
    let sent = params(&request);
    assert_eq!(sent[PREVIOUS_REQUEST_ID_PARAM], "req_2");
    assert_eq!(
        sent[RAW_TOOL_ARGUMENTS_BY_CALL_PARAM],
        serde_json::json!({"c1": "{ }"})
    );

    // A prior in-loop assistant row is the latest in the history being sent.
    let prior = vec![assistant_with(Some("req_3"), None)];
    let request = super::build_request(
        &assembly_only(),
        Message::user("three"),
        &history,
        &prior,
        &[],
        &config,
    )
    .await
    .unwrap();
    assert_eq!(params(&request)[PREVIOUS_REQUEST_ID_PARAM], "req_3");

    // A compacted history keeps no assistant row: neither key is sent.
    let request = super::build_request(
        &assembly_only(),
        Message::user("three"),
        &[Message::user("summary")],
        &[],
        &[],
        &config,
    )
    .await
    .unwrap();
    let sent = params(&request);
    assert!(sent.get(PREVIOUS_REQUEST_ID_PARAM).is_none());
    assert!(sent.get(RAW_TOOL_ARGUMENTS_BY_CALL_PARAM).is_none());

    // The rebuilt request after compaction replaces stale values in place.
    let mut rebuilt = super::build_request(
        &assembly_only(),
        Message::user("three"),
        &history,
        &[],
        &[],
        &config,
    )
    .await
    .unwrap();
    super::replace_core_chat_history(
        &mut rebuilt,
        &config,
        &[TaggedMessage::unassociated(Message::user("summary"))],
        &[TaggedMessage::unassociated(Message::user("three"))],
    )
    .unwrap();
    assert!(params(&rebuilt).get(PREVIOUS_REQUEST_ID_PARAM).is_none());
}

#[tokio::test]
async fn other_providers_never_receive_the_reserved_parameters() {
    let mut config = replay_assembly_config();
    let history = vec![
        Message::user("one"),
        assistant_with(Some("req_1"), Some(("c1", "{ }"))),
    ];
    for params in [None, Some(serde_json::json!({"prompt_cache_key": "k"}))] {
        config.additional_params = params.clone();
        let request = super::build_request(
            &assembly_only(),
            Message::user("two"),
            &history,
            &[],
            &[],
            &config,
        )
        .await
        .unwrap();
        assert_eq!(request.additional_params, params);
    }
}

#[cfg(feature = "native")]
#[tokio::test]
async fn cross_provider_tool_identity_reaches_dispatch_and_rebuilt_history() {
    use crate::backend_provider::BackendProviderKind;
    use crate::provider_input::tool_call_identity_tests::{
        body_identities, counter, messages, observed_identities,
    };
    use gents_lean_vocab::families::provider_tool_identity_assembled::IdentityWire;
    let cases = gents_lean_vocab::families::provider_tool_identity_assembled::cases();
    for case in &cases {
        let identities = case
            .input
            .input
            .iter()
            .map(|wire| (wire.id.clone(), wire.call_id.clone()))
            .collect::<Vec<_>>();
        let original = messages(&identities);
        let prompt = original.last().unwrap().clone();
        let history = &original[..original.len() - 1];
        let mut bodies = Vec::new();
        for provider in [
            BackendProviderKind::OpenAiCompatible,
            BackendProviderKind::ChatGptCodex,
            BackendProviderKind::XaiGrokOAuth,
        ] {
            let mut config = replay_assembly_config();
            config.provider_input_counter = Arc::new(counter(provider));
            config.preamble = Some("System instructions".into());
            let mut request =
                super::build_request(&assembly_only(), prompt.clone(), history, &[], &[], &config)
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
            let dispatched = dispatch_body(&request);
            assert_eq!(
                body_identities(
                    &config
                        .provider_input_counter
                        .project_body(&request)
                        .unwrap()
                ),
                body_identities(&dispatched),
                "{provider:?}"
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
            assert_eq!(
                body_identities(&dispatch_body(&request)),
                body_identities(&dispatched),
                "{provider:?}"
            );
            assert_eq!(original, messages(&identities));
            bodies.push(dispatched);
        }
        case.check(
            observed_identities(&bodies[0], &bodies[1], &bodies[2])
                .into_iter()
                .map(|observed| IdentityWire {
                    id: observed.id,
                    call_id: observed.call_id,
                    openai_item_id: observed.openai_item_id,
                })
                .collect(),
        );
    }
}

async fn producing_capture(
    model: &crate::test_support::scripted_provider::ScriptedProvider,
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
                request_id: None,
                id: None,
                content: replay_assembly_content(),
            },
            source: Some(tag.clone()),
            physical_header: Some("header".into()),
            block_indices: vec![0, 1],
            stub_receipts: Vec::new(),
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
    let evidence = producing_capture(&assembly_only(), &config, &replay_assembly_content()).await;
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
        &assembly_only(),
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
        &assembly_only(),
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

#[tokio::test]
async fn reasoning_identity_matches_the_lean_model() {
    for case in
        gents_lean_vocab::families::request_assembly_reasoning_identity::cases::<String, String>()
    {
        let tag = provider_tag(0);
        let config = replay_assembly_config();
        let mut evidence =
            producing_capture(&assembly_only(), &config, &replay_assembly_content()).await;
        evidence.origin = match case.input.origin.as_str() {
            "accepted_provider" => ReplayOrigin::AcceptedProvider,
            "recovered_provider" => ReplayOrigin::RecoveredProvider,
            other => panic!("unknown origin {other}"),
        };
        evidence.complete = case.input.complete;
        let replay = LoopReplayInput {
            issuer: Some(evidence.issuer.clone()),
            wire: Some(crate::claude_messages_body::ReplayWire::ClaudeMessages),
            resolve: Some(counting_resolver(
                vec![ReplayEvidenceRow {
                    tag: tag.clone(),
                    evidence,
                }],
                vec![tag.clone()],
                Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            )),
            ..LoopReplayInput::default()
        };
        let mut history = replay_assembly_history(&tag);
        let Message::Assistant { content, .. } = &mut history[1].message else {
            panic!("assistant row")
        };
        *content = case
            .input
            .content
            .iter()
            .map(|block| match block.as_str() {
                "reasoning" => replay_assembly_content()[0].clone(),
                "text" => replay_assembly_content()[1].clone(),
                other => panic!("unknown block {other}"),
            })
            .collect();
        history[1].block_indices = (0..content.len()).collect();
        if !case.input.has_identity {
            history[1].physical_header = None;
            history[1].block_indices.clear();
        }
        let mut next = vec![TaggedMessage::unassociated(Message::user("next"))];
        let request = assemble_provider_request(
            &assembly_only(),
            &mut history,
            &mut next,
            &[],
            &config,
            &replay,
        )
        .await
        .unwrap();
        let body = config
            .provider_input_counter
            .project_body(&request)
            .unwrap();
        let flat = crate::provider_input::replay_frontier::flatten(
            &body,
            crate::claude_messages_body::ReplayWire::ClaudeMessages,
        )
        .unwrap();
        let Message::Assistant { content, .. } = &history[1].message else {
            panic!("assistant row")
        };
        case.check(
            gents_lean_vocab::families::request_assembly_reasoning_identity::IdentityObservation {
                reasoning_replayed: flat.iter().any(|item| {
                    matches!(
                        item,
                        crate::provider_input::replay_frontier::FlatItem::Reasoning(_)
                    )
                }),
                retained: content
                    .iter()
                    .map(|block| match block {
                        AssistantContent::Reasoning(_) => "reasoning".to_string(),
                        AssistantContent::Text(_) => "text".to_string(),
                        other => panic!("unexpected block {other:?}"),
                    })
                    .collect(),
            },
        );
    }
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
                request_id: None,
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
            stub_receipts: Vec::new(),
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

async fn sized_user_text(config: &LoopConfig, raw: usize, fill: char, prompt: &str) -> String {
    let estimate = |text: String| async move {
        let mut history = vec![TaggedMessage::unassociated(Message::user(text))];
        let mut new_messages = vec![TaggedMessage::unassociated(Message::user(prompt))];
        let request = assemble_provider_request(
            &assembly_only(),
            &mut history,
            &mut new_messages,
            &[],
            config,
            &LoopReplayInput::default(),
        )
        .await
        .expect("assembles");
        config
            .provider_input_counter
            .estimate_request(&request)
            .expect("estimate")
    };
    let base = estimate(fill.to_string()).await;
    let text = fill.to_string().repeat(1 + (raw - base) * 4);
    assert_eq!(estimate(text.clone()).await, raw);
    text
}

#[tokio::test]
async fn turn_rebuild_matches_the_lean_model() {
    for case in gents_lean_vocab::families::request_assembly_turn_rebuild::cases::<String>() {
        let i = &case.input;
        let mut config = replay_assembly_config();
        config.context_window = i.context_window as usize;
        config.compaction_threshold = i.threshold_basis_points as f64 / 10_000.0;
        config.max_tokens = Some(i.max_output_tokens);
        config.context_calibration =
            crate::provider_input::budget::ContextCalibration::with_ratio(i.ratio);
        let reduced = sized_user_text(&config, i.after_raw as usize, 's', "compacted prompt").await;
        let original_text = sized_user_text(&config, i.before_raw as usize, 'h', "next").await;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let commits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let compactor_calls = calls.clone();
        let compactor_commits = commits.clone();
        config.turn_compactor = Some(Arc::new(move |_request: TurnCompactionRequest| {
            compactor_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let commits = compactor_commits.clone();
            let reduced = reduced.clone();
            Box::pin(async move {
                Ok(TurnCompactionOutcome::Reduced {
                    messages: vec![
                        TaggedMessage::unassociated(Message::user(reduced)),
                        TaggedMessage::unassociated(Message::user("compacted prompt")),
                    ],
                    reduction_key: "reduced".to_string(),
                    commit: crate::loop_stream::TurnReductionCommit::new(move || async move {
                        commits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok(())
                    }),
                })
            })
        }));
        let original_history = vec![TaggedMessage::unassociated(Message::user(original_text))];
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
        let result = build_budgeted_request(
            &TurnInputs {
                model: &assembly_only(),
                tools: &[],
                config: &config,
                turn_index: 3,
            },
            &mut TurnState {
                history: &mut history,
                new_messages: &mut new_messages,
                replay: &mut replay,
                reduction_chain_keys: &mut chain_keys,
                active_reduction_keys: &mut active_keys,
            },
        )
        .await;
        let rebuild = match result {
            Ok((_, decision)) => match decision.reason {
                gents_protocol::rendered_request::ContextCompactionReason::BelowThreshold => {
                    "below_threshold"
                }
                gents_protocol::rendered_request::ContextCompactionReason::Compacted => "committed",
                other => panic!("{i:?}: unexpected decision {other:?}"),
            },
            Err(error) => {
                if gents_protocol::error_chain::find_source::<crate::compaction::ReductionError>(
                    &error,
                )
                .is_some_and(|reason| {
                    matches!(reason, crate::compaction::ReductionError::CannotFit)
                }) {
                    "cannot_fit"
                } else if gents_protocol::error_chain::find_source::<
                    crate::provider_input::budget::ContextBudgetError,
                >(&error)
                .is_some()
                {
                    "no_output_capacity"
                } else {
                    panic!("{i:?}: unexpected error {error:?}")
                }
            }
        };
        let unchanged = history == original_history
            && new_messages == original_prompt
            && chain_keys == ["earlier-reduction"]
            && active_keys == ["earlier-reduction"];
        let replaced = history.len() == 1
            && new_messages
                == [TaggedMessage::unassociated(Message::user(
                    "compacted prompt",
                ))]
            && chain_keys == ["earlier-reduction", "reduced"]
            && active_keys == ["reduced"];
        assert!(
            unchanged || replaced,
            "{i:?}: the session is partly replaced"
        );
        case.check(
            gents_lean_vocab::families::request_assembly_turn_rebuild::RebuildObservation {
                rebuild: rebuild.to_string(),
                compactor_calls: calls.load(std::sync::atomic::Ordering::SeqCst) as u64,
                commits: commits.load(std::sync::atomic::Ordering::SeqCst) as u64,
                replaces_session: replaced,
                keeps_anchor: config.context_calibration.anchor().is_some(),
            },
        );
    }
}

#[tokio::test]
async fn every_provider_assembly_rechecks_configuration_before_reading_input() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let mut config = replay_assembly_config();
    config.configuration_guard = Some(Arc::new({
        let calls = Arc::clone(&calls);
        move || {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(())
                } else {
                    Err(ProviderConfigurationGuardError::Unavailable(Box::new(
                        std::io::Error::other("revoked"),
                    )))
                }
            })
        }
    }));
    let replay = LoopReplayInput::default();
    let mut history = Vec::new();
    let mut new_messages = vec![TaggedMessage::unassociated(Message::user("first"))];
    assemble_provider_request(
        &assembly_only(),
        &mut history,
        &mut new_messages,
        &[],
        &config,
        &replay,
    )
    .await
    .unwrap();
    let before = new_messages.clone();
    let result = assemble_provider_request(
        &assembly_only(),
        &mut history,
        &mut new_messages,
        &[],
        &config,
        &replay,
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        new_messages, before,
        "denied assembly must not consume pending inputs"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// A dispatch reports the stubs of the rows it sends: once a turn reduction
/// drops the stubbed row, the rebuilt request reports none.
#[tokio::test]
async fn a_reduction_that_drops_a_stubbed_row_drops_its_stub_from_the_dispatch() {
    use gents_protocol::message::{ToolResult, ToolResultContent};
    let mut config = replay_assembly_config();
    config.context_window = 4_000;
    config.compaction_threshold = 0.1;
    config.max_tokens = Some(10);
    config.turn_compactor = Some(Arc::new(|request: TurnCompactionRequest| {
        Box::pin(async move {
            Ok(TurnCompactionOutcome::Reduced {
                messages: vec![request.messages.last().unwrap().clone()],
                reduction_key: "drop-stub".into(),
                commit: crate::loop_stream::TurnReductionCommit::new(|| async { Ok(()) }),
            })
        })
    }));
    let mut history = provider_view_tagged(vec![
        TaggedMessage::unassociated(Message::user("budget pressure ".repeat(4_000))),
        TaggedMessage::unassociated(assistant_with(None, Some(("stubbed-call", "{}")))),
        TaggedMessage::unassociated(Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                id: "stubbed-call".into(),
                call_id: Some("stubbed-call".into()),
                content: vec![ToolResultContent::text("x".repeat(4_000))],
            })],
        }),
    ])
    .unwrap();
    let mut new_messages = vec![TaggedMessage::unassociated(Message::user("only this"))];
    assert_eq!(
        super::tagged_history::dispatched_stubbing(history.iter().chain(new_messages.iter()))
            .stubbed_results,
        1
    );

    let (_, decision) = build_budgeted_request(
        &TurnInputs {
            model: &assembly_only(),
            tools: &[],
            config: &config,
            turn_index: 1,
        },
        &mut TurnState {
            history: &mut history,
            new_messages: &mut new_messages,
            replay: &mut LoopReplayInput::default(),
            reduction_chain_keys: &mut Vec::new(),
            active_reduction_keys: &mut Vec::new(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        decision.reason,
        gents_protocol::rendered_request::ContextCompactionReason::Compacted
    );
    assert_eq!(
        super::tagged_history::dispatched_stubbing(history.iter().chain(new_messages.iter())),
        Default::default()
    );
}

#[tokio::test]
async fn hidden_tools_stay_out_of_the_provider_request_through_request_binding() {
    use crate::test_support::fixed_tool::FixedTool;

    let tools: Vec<Box<dyn ToolDyn>> = vec![
        Box::new(FixedTool::new("script", "").with_description("")),
        Box::new(FixedTool::new("query", "").with_description("")),
    ];
    let tools = crate::tool::bind_request_tools(
        Arc::new(crate::tool_policy::presentation::present_tools(
            tools,
            &["query".into()],
        )),
        crate::tool::ToolRequestContext {
            request_id: "request".into(),
            request_doc_id: "request".into(),
            node_did: "did:key:node".into(),
            agent_did: "did:key:agent".into(),
            session_id: "session".into(),
            caused_by_source_doc_id: None,
            admitted_client: None,
        },
    );
    let request = super::build_request(
        &assembly_only(),
        Message::user("prompt"),
        &[],
        &[],
        &tools,
        &replay_assembly_config(),
    )
    .await
    .unwrap();
    let names: Vec<_> = request
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    assert_eq!(names, ["script"]);
    assert!(tools[1].hidden());
}

#[cfg(feature = "native")]
#[tokio::test]
async fn responses_reasoning_returns_only_to_its_issuer_whatever_the_prefix_became() {
    use crate::backend_provider::BackendProviderKind;
    use crate::claude_messages_body::ReplayWire;
    use crate::openai_wire::OpenAiWireApi;
    use crate::provider_input::replay_frontier::FlatItem;
    use gents_protocol::message::{Reasoning, ReasoningContent};

    let tag = provider_tag(0);
    let issuer = ReplayIssuer {
        family: "OpenAiCompatible".into(),
        endpoint: "https://api.openai.test".into(),
    };
    let content = vec![
        AssistantContent::Reasoning(Reasoning {
            id: Some("rs_1".into()),
            content: vec![ReasoningContent::Encrypted("sealed-1".into())],
        }),
        AssistantContent::Text(Text {
            text: "visible".into(),
        }),
    ];
    let rows = vec![ReplayEvidenceRow {
        tag: tag.clone(),
        evidence: ResolvedReplayEvidence {
            reasoning: crate::claude_messages_body::reasoning_witness(&content),
            issuer: issuer.clone(),
            wire: ReplayWire::Responses,
            captured: Some(vec![FlatItem::Ordinary(
                b"a prefix this request lacks".to_vec(),
            )]),
            ..evidence_row(&tag, "header", Vec::new()).evidence
        },
    }];
    let body_for =
        |kind: BackendProviderKind, wire: OpenAiWireApi, replay_issuer: Option<ReplayIssuer>| {
            let rows = rows.clone();
            let tag = tag.clone();
            let content = content.clone();
            async move {
                let mut config = replay_assembly_config();
                config.provider_input_counter = Arc::new(
                    crate::provider_input::ProviderInputCounter::new(kind, wire, "test-model"),
                );
                let replay = LoopReplayInput {
                    issuer: replay_issuer,
                    wire: config.provider_input_counter.profile().replay_wire(),
                    resolve: Some(Arc::new(move |_| {
                        let rows = rows.clone();
                        Box::pin(async move { Ok(rows) })
                    })),
                    ..LoopReplayInput::default()
                };
                let mut history = vec![
                    TaggedMessage::unassociated(Message::user("prompt")),
                    TaggedMessage {
                        message: Message::Assistant {
                            request_id: None,
                            id: Some("msg_1".into()),
                            content,
                        },
                        source: Some(tag),
                        physical_header: Some("header".into()),
                        block_indices: vec![0, 1],
                        stub_receipts: Vec::new(),
                    },
                ];
                let mut new = vec![TaggedMessage::unassociated(Message::user("next"))];
                let request = assemble_provider_request(
                    &assembly_only(),
                    &mut history,
                    &mut new,
                    &[],
                    &config,
                    &replay,
                )
                .await
                .unwrap();
                config
                    .provider_input_counter
                    .project_body(&request)
                    .unwrap()
            }
        };

    let same = body_for(
        BackendProviderKind::OpenAiCompatible,
        OpenAiWireApi::Responses,
        Some(issuer.clone()),
    )
    .await;
    assert_eq!(
        same["input"].as_array().unwrap()[1..3],
        [
            serde_json::json!({"type": "reasoning", "id": "rs_1", "summary": [],
                "encrypted_content": "sealed-1"}),
            serde_json::json!({"type": "message", "id": "msg_1", "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": "visible", "annotations": []}]}),
        ],
        "{same}"
    );

    let other_backend = body_for(
        BackendProviderKind::OpenAiCompatible,
        OpenAiWireApi::Responses,
        Some(ReplayIssuer {
            endpoint: "https://other.test".into(),
            ..issuer.clone()
        }),
    )
    .await;
    let chat = body_for(
        BackendProviderKind::OpenAiCompatible,
        OpenAiWireApi::ChatCompletions,
        Some(issuer),
    )
    .await;
    for body in [other_backend, chat] {
        let text = body.to_string();
        assert!(
            !text.contains("sealed-1") && !text.contains("rs_1") && text.contains("visible"),
            "{text}"
        );
    }
}

#[test]
fn source_tagging_matches_the_lean_model() {
    use crate::compaction::history::{SourcedMessage, StubReceipt, StubSizes};
    use gents_lean_vocab::families::request_assembly_source_tagging::{
        self as family, SourceTagWire, SourceTaggingOutput, TaggedRowWire,
    };
    use gents_protocol::message::{ToolResult, ToolResultContent};
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

    let to_native_tag = |tag: &SourceTagWire| ReplayTag {
        request_doc_id: format!("req-{}", tag.request),
        source: OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: tag.scope,
            },
            turn_index: u32::try_from(tag.turn).expect("turn fits"),
            attempt: u32::try_from(tag.attempt).expect("attempt fits"),
        },
    };
    let to_wire_tag = |tag: &ReplayTag| match &tag.source {
        OutputSource::ProviderTurn {
            scope,
            turn_index,
            attempt,
        } => SourceTagWire {
            request: tag
                .request_doc_id
                .strip_prefix("req-")
                .expect("synthetic request prefix")
                .parse()
                .expect("request id"),
            scope: scope.seq,
            turn: u64::from(*turn_index),
            attempt: u64::from(*attempt),
        },
        other => panic!("unexpected replay source {other:?}"),
    };
    let to_wire_role = |message: &Message| match message {
        Message::User { .. } => "user".to_string(),
        Message::Assistant { .. } => "assistant".to_string(),
        Message::System { .. } => "system".to_string(),
    };
    let to_wire_error = |error: SourceTagError| match error {
        SourceTagError::NonAssistantSource => "non_assistant_source",
        SourceTagError::ReorderedOrDuplicatedSourceRow => "reordered_or_duplicated_source_row",
        SourceTagError::OutOfRangeSourceIndex => "out_of_range_source_index",
        SourceTagError::RoleMismatch => "role_mismatch",
        SourceTagError::MisalignedAssistantBlockIndices => "misaligned_assistant_block_indices",
        SourceTagError::ReorderedAssistantBlocks => "reordered_assistant_blocks",
        SourceTagError::OutOfRangeAssistantBlockIndex => "out_of_range_assistant_block_index",
        SourceTagError::InvalidOriginalAssociation => "invalid_original_association",
    };

    family::replay::<String, String>(|case| {
        let originals = case
            .originals
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let message = match row.role.as_str() {
                    "user" => Message::user(format!("u-{index}")),
                    "system" => Message::System {
                        content: format!("s-{index}"),
                    },
                    "assistant" => Message::Assistant {
                        request_id: None,
                        id: None,
                        content: (0..row.block_count)
                            .map(|block| {
                                AssistantContent::Text(Text {
                                    text: format!("a-{index}-{block}"),
                                })
                            })
                            .collect(),
                    },
                    other => panic!("unknown role {other}"),
                };
                TaggedMessage {
                    message,
                    source: row.source.as_ref().map(to_native_tag),
                    physical_header: row.physical_header.clone(),
                    block_indices: row
                        .block_indices
                        .iter()
                        .map(|position| usize::try_from(*position).expect("block index fits"))
                        .collect(),
                    stub_receipts: row
                        .stub_receipt_keys
                        .iter()
                        .map(|key| StubReceipt {
                            source_index: index,
                            result_key: key.clone(),
                            sizes: StubSizes {
                                original_bytes: 128,
                                stub_bytes: 16,
                                stub_shaped: false,
                            },
                        })
                        .collect(),
                }
            })
            .collect::<Vec<_>>();
        let sourced = case
            .sourced
            .iter()
            .map(|item| {
                let message = match item.role.as_str() {
                    "user" => {
                        if item.kept_tool_result_keys.is_empty() {
                            Message::user("projected-user")
                        } else {
                            Message::User {
                                content: item
                                    .kept_tool_result_keys
                                    .iter()
                                    .map(|key| {
                                        UserContent::ToolResult(ToolResult {
                                            id: key.clone(),
                                            call_id: Some(key.clone()),
                                            content: vec![ToolResultContent::text("ok")],
                                        })
                                    })
                                    .collect(),
                            }
                        }
                    }
                    "system" => Message::System {
                        content: "projected-system".to_string(),
                    },
                    "assistant" => Message::Assistant {
                        request_id: None,
                        id: None,
                        content: (0..item.block_count)
                            .map(|block| {
                                AssistantContent::Text(Text {
                                    text: format!("projected-{block}"),
                                })
                            })
                            .collect(),
                    },
                    other => panic!("unknown role {other}"),
                };
                SourcedMessage {
                    source_index: usize::try_from(item.source_index).expect("source index fits"),
                    block_indices: item
                        .block_indices
                        .iter()
                        .map(|position| usize::try_from(*position).expect("block index fits"))
                        .collect(),
                    message,
                }
            })
            .collect::<Vec<_>>();

        match tagged_from_sourced(&originals, sourced) {
            Err(error) => {
                let tag_error = *SourceTagError::in_chain(&error).unwrap_or_else(|| {
                    panic!("{}: unexpected non-SourceTagError {error:?}", case.name)
                });
                SourceTaggingOutput {
                    error: Some(to_wire_error(tag_error).to_string()),
                    rows: Vec::new(),
                }
            }
            Ok(rows) => SourceTaggingOutput {
                error: None,
                rows: rows
                    .into_iter()
                    .map(|row| TaggedRowWire {
                        role: to_wire_role(&row.message),
                        source: row.source.as_ref().map(to_wire_tag),
                        physical_header: row.physical_header,
                        block_indices: row
                            .block_indices
                            .into_iter()
                            .map(|position| u64::try_from(position).expect("block index fits"))
                            .collect(),
                        stub_receipt_keys: row
                            .stub_receipts
                            .into_iter()
                            .map(|receipt| receipt.result_key)
                            .collect(),
                    })
                    .collect(),
            },
        }
    });
}
