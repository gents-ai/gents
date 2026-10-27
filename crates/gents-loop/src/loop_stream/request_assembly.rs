use super::*;
use crate::claude_messages_body::{
    prepare_replay_checkpoint, replay_stage, select_replay, ReplayCheckpointError, ReplayTag,
    ResolvedReplayEvidence, TaggedAssistantRow,
};
use crate::provider_input::ProviderInputProfile;

pub(super) fn message_values(rows: &[TaggedMessage]) -> Vec<Message> {
    rows.iter().map(|row| row.message.clone()).collect()
}

pub fn replay_compaction_prefix_bound(
    rows: &[TaggedMessage],
    required: &[ReplayTag],
) -> Result<Option<usize>, ReplayCheckpointError> {
    let mut assistants = Vec::new();
    for row in rows {
        match &row.message {
            Message::Assistant { id, content } => assistants.push(TaggedAssistantRow {
                source: row.source.clone(),
                physical_header: row.physical_header.clone(),
                block_indices: row.block_indices.clone(),
                id: id.clone(),
                content: content.clone(),
            }),
            _ if row.source.is_some() => return Err(ReplayCheckpointError::InvalidAssociation),
            _ => {}
        }
    }
    prepare_replay_checkpoint(required.to_vec(), assistants, 0)?;
    Ok((!required.is_empty()).then_some(0))
}

fn replay_input_error(message: impl Into<String>) -> StreamingError {
    StreamingError::Completion(CompletionError::RequestError(Box::new(
        std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into()),
    )))
}

pub(super) fn replay_resolution_error(error: anyhow::Error) -> StreamingError {
    match error.downcast::<ReplayEvidenceViolation>() {
        Ok(violation) => {
            StreamingError::Completion(CompletionError::RequestError(Box::new(violation)))
        }
        Err(error) => StreamingError::Completion(CompletionError::ProviderError(format!(
            "loading canonical Claude replay evidence failed: {error:#}",
        ))),
    }
}

pub async fn resolve_replay_evidence(
    replay: &LoopReplayInput,
    requested: &[ReplayTag],
) -> Result<Vec<ReplayEvidenceRow>, StreamingError> {
    let mut distinct = Vec::new();
    for tag in requested {
        if !distinct.contains(tag) {
            distinct.push(tag.clone());
        }
    }
    if distinct.is_empty() {
        return Ok(Vec::new());
    }
    let mut evidence = replay
        .estimate_seed
        .as_ref()
        .and_then(|seed| seed.take())
        .unwrap_or_default()
        .into_iter()
        .filter(|row| distinct.contains(&row.tag))
        .collect::<Vec<_>>();
    let missing = distinct
        .iter()
        .filter(|tag| !evidence.iter().any(|row| &row.tag == *tag))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let resolve = replay.resolve.as_ref().ok_or_else(|| {
            replay_input_error("reasoning replay has no canonical evidence resolver")
        })?;
        evidence.extend(resolve(missing).await.map_err(replay_resolution_error)?);
    }
    if evidence.iter().any(|row| !distinct.contains(&row.tag)) {
        return Err(replay_input_error(
            "canonical resolver returned an unrequested replay source",
        ));
    }
    Ok(evidence)
}

#[cfg(test)]
mod replay_error_tests {
    use super::*;
    use crate::claude_messages_body::{ReasoningWitness, ReplayIssuer, ReplayOrigin};
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

    fn evidence_row(
        tag: &ReplayTag,
        header: &str,
        reasoning: ReasoningWitness,
    ) -> ReplayEvidenceRow {
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
        ) -> Result<
            rig::completion::CompletionResponse<Self::Response>,
            rig::completion::CompletionError,
        > {
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
                            crate::responses_normalize::normalize_responses_assistant_items(
                                &mut body,
                            )
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
        let evidence =
            producing_capture(&AssemblyOnlyModel, &config, &replay_assembly_content()).await;
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
        assert!(
            matches!(content.as_slice(), [AssistantContent::Text(text)] if text.text == "visible")
        );
        assert_eq!(rows[0].block_indices, [1]);
        assert_eq!(rows[1].message, Message::user("next"));
    }

    #[test]
    fn canonical_violation_retains_its_type_through_context() {
        let error = anyhow::Error::new(ReplayEvidenceViolation("invalid canonical join".into()))
            .context("resolving required replay");
        let StreamingError::Completion(CompletionError::RequestError(error)) =
            replay_resolution_error(error)
        else {
            panic!("canonical violation must remain a typed local input failure");
        };
        assert!(error.downcast_ref::<ReplayEvidenceViolation>().is_some());
    }

    #[test]
    fn storage_error_is_not_reclassified_by_message_text() {
        let error = anyhow::anyhow!("canonical replay tag belongs to another physical request");
        assert!(matches!(
            replay_resolution_error(error),
            StreamingError::Completion(CompletionError::ProviderError(_))
        ));
    }
}

pub fn select_tagged_assistant_blocks(
    source: &TaggedMessage,
    retained_original_indices: &[usize],
) -> Result<TaggedMessage, ReplayCheckpointError> {
    let Message::Assistant { id, content } = &source.message else {
        return Err(ReplayCheckpointError::InvalidAssociation);
    };
    if source.block_indices.len() != content.len()
        || source
            .block_indices
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
    {
        return Err(ReplayCheckpointError::InvalidAssociation);
    }
    let (block_indices, selected): (Vec<_>, Vec<_>) = source
        .block_indices
        .iter()
        .copied()
        .zip(content)
        .filter(|(index, _)| retained_original_indices.contains(index))
        .map(|(index, block)| (index, block.clone()))
        .unzip();
    Ok(TaggedMessage {
        message: Message::Assistant {
            id: id.clone(),
            content: selected,
        },
        source: source.source.clone(),
        physical_header: source.physical_header.clone(),
        block_indices,
    })
}

fn tagged_from_sourced(
    originals: &[TaggedMessage],
    sourced: Vec<crate::compaction::history::SourcedMessage>,
) -> Result<Vec<TaggedMessage>, StreamingError> {
    if originals
        .iter()
        .any(|row| row.source.is_some() && !matches!(row.message, Message::Assistant { .. }))
    {
        return Err(replay_input_error(
            "canonical provider source was attached to a non-assistant row",
        ));
    }
    let mut previous_source = None;
    sourced
        .into_iter()
        .map(|item| {
            if previous_source.is_some_and(|previous| item.source_index <= previous) {
                return Err(replay_input_error(
                    "provider projection reordered or duplicated a source row",
                ));
            }
            previous_source = Some(item.source_index);
            let original = originals.get(item.source_index).ok_or_else(|| {
                replay_input_error("provider projection emitted an out-of-range source index")
            })?;
            if original.source.is_some() && !matches!(item.message, Message::Assistant { .. }) {
                return Err(replay_input_error(
                    "canonical provider source was attached to a non-assistant row",
                ));
            }
            if let Message::Assistant { content, .. } = &item.message {
                if item.block_indices.len() != content.len() {
                    return Err(replay_input_error(
                        "provider projection misaligned assistant block indices",
                    ));
                }
                let mut selected = if original.physical_header.is_none()
                    && original.block_indices.is_empty()
                {
                    TaggedMessage {
                        message: original.message.clone(),
                        source: original.source.clone(),
                        physical_header: None,
                        block_indices: Vec::new(),
                    }
                } else {
                    if item.block_indices.windows(2).any(|pair| pair[0] >= pair[1]) {
                        return Err(replay_input_error(
                            "provider projection reordered associated assistant blocks",
                        ));
                    }
                    let original_indices = item.block_indices.iter().map(|position| {
                        original.block_indices.get(*position).copied().ok_or_else(|| {
                            replay_input_error("provider projection emitted an out-of-range assistant block index")
                        })
                    }).collect::<Result<Vec<_>, _>>()?;
                    select_tagged_assistant_blocks(original, &original_indices).map_err(|error| {
                        replay_input_error(format!("provider projection: {error}"))
                    })?
                };
                selected.message = item.message;
                Ok(selected)
            } else {
                Ok(TaggedMessage {
                    message: item.message,
                    source: original.source.clone(),
                    physical_header: original.physical_header.clone(),
                    block_indices: Vec::new(),
                })
            }
        })
        .collect()
}

pub fn sanitize_tagged_history(
    rows: Vec<TaggedMessage>,
) -> Result<Vec<TaggedMessage>, StreamingError> {
    let messages = rows.iter().map(|row| row.message.clone()).collect();
    tagged_from_sourced(
        &rows,
        crate::compaction::sanitize_association_with_sources(messages),
    )
}

pub fn provider_view_tagged(
    rows: Vec<TaggedMessage>,
) -> Result<Vec<TaggedMessage>, StreamingError> {
    let messages = rows.iter().map(|row| row.message.clone()).collect();
    let (sourced, _) = crate::compaction::association_view_with_sources(messages);
    tagged_from_sourced(&rows, sourced)
}

pub fn provider_messages(profile: ProviderInputProfile, rows: &[TaggedMessage]) -> Vec<Message> {
    provider_order(profile, message_values(rows))
}

pub(super) fn provider_order(
    profile: ProviderInputProfile,
    messages: Vec<Message>,
) -> Vec<Message> {
    crate::compaction::history::normalize_assistant_content_order(profile, messages)
}

pub fn association_reduction(
    profile: ProviderInputProfile,
    rows: &[TaggedMessage],
    exact: crate::compaction::ExactReduction<'_>,
) -> anyhow::Result<crate::compaction::AssociationReduction> {
    let split = exact.compacted_prefix.len();
    anyhow::ensure!(
        split <= rows.len(),
        "exact reduction split exceeds its association rows"
    );
    let (prefix, suffix) = rows.split_at(split);
    exact.over_association_view(profile, message_values(prefix), message_values(suffix))
}

fn assistant_rows(rows: &[TaggedMessage]) -> (Vec<usize>, Vec<TaggedAssistantRow>) {
    rows.iter()
        .enumerate()
        .filter_map(|(index, row)| match &row.message {
            Message::Assistant { id, content } => Some((
                index,
                TaggedAssistantRow {
                    source: row.source.clone(),
                    physical_header: row.physical_header.clone(),
                    block_indices: row.block_indices.clone(),
                    id: id.clone(),
                    content: content.clone(),
                },
            )),
            _ => None,
        })
        .unzip()
}

fn apply_assistant_rows(
    rows: &mut [TaggedMessage],
    indices: &[usize],
    selected: Vec<TaggedAssistantRow>,
) -> Result<(), StreamingError> {
    if selected.len() != indices.len() {
        return Err(replay_input_error(
            "reasoning replay changed assistant row cardinality",
        ));
    }
    for (index, selected) in indices.iter().zip(selected) {
        let row = &mut rows[*index];
        if row.source != selected.source {
            return Err(replay_input_error(
                "reasoning replay changed assistant source association",
            ));
        }
        let Message::Assistant { content, .. } = &mut row.message else {
            unreachable!("indices name assistant rows")
        };
        *content = selected.content;
        row.block_indices = selected.block_indices;
        row.physical_header = selected.physical_header;
    }
    Ok(())
}

pub(super) fn strip_all_reasoning(rows: &mut [TaggedMessage]) {
    for row in rows {
        let Message::Assistant { content, .. } = &mut row.message else {
            continue;
        };
        let mut indices = row.block_indices.iter();
        let mut kept_indices = Vec::new();
        content.retain(|block| {
            let index = indices.next();
            let keep = !matches!(block, AssistantContent::Reasoning(_));
            if keep {
                kept_indices.extend(index.copied());
            }
            keep
        });
        if !row.block_indices.is_empty() {
            row.block_indices = kept_indices;
        }
    }
}

fn split_joined(
    mut joined: Vec<TaggedMessage>,
    history_len: usize,
) -> (Vec<TaggedMessage>, Vec<TaggedMessage>) {
    let new_messages = joined.split_off(history_len);
    (joined, new_messages)
}

async fn build_core<M>(
    model: &M,
    history: &[TaggedMessage],
    new_messages: &[TaggedMessage],
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
) -> Result<CompletionRequest, StreamingError>
where
    M: CompletionModel,
{
    let prompt = new_messages
        .last()
        .expect("new messages retain a prompt")
        .message
        .clone();
    let prior = message_values(&new_messages[..new_messages.len() - 1]);
    build_request(
        model,
        prompt,
        &message_values(history),
        &prior,
        tools,
        config,
    )
    .await
}

pub fn assemble_new_messages(
    context_message: Option<Message>,
    prompt: TaggedMessage,
) -> Vec<TaggedMessage> {
    let mut new_messages: Vec<TaggedMessage> = Vec::with_capacity(2);
    if let Some(context_message) = context_message {
        new_messages.push(TaggedMessage::unassociated(context_message));
    }
    new_messages.push(prompt);
    new_messages
}

pub fn is_request_context_message(message: &Message) -> bool {
    let Message::User { content } = message else {
        return false;
    };
    let [UserContent::Text(text)] = content.as_slice() else {
        return false;
    };
    let trimmed = text.text.trim();
    trimmed.starts_with("<context>") && trimmed.ends_with("</context>")
}

#[derive(Debug, thiserror::Error)]
#[error("provider_input_repair_removed_complete_prompt")]
struct ProviderInputRepairError;

pub fn repair_provider_input(
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
) -> Result<(), StreamingError> {
    let mut provider_messages = std::mem::take(history);
    provider_messages.append(new_messages);
    repair_messages(&mut provider_messages);
    let mut provider_messages = sanitize_tagged_history(provider_messages)?;
    let prompt = provider_messages.pop().ok_or_else(|| {
        StreamingError::Completion(CompletionError::RequestError(Box::new(
            ProviderInputRepairError,
        )))
    })?;
    *history = provider_messages;
    new_messages.push(prompt);
    Ok(())
}

fn repair_messages(messages: &mut [TaggedMessage]) {
    for row in messages.iter_mut() {
        let Message::Assistant { content, .. } = &mut row.message else {
            continue;
        };
        for item in content {
            let AssistantContent::ToolCall(tool_call) = item else {
                continue;
            };
            let mut repaired = crate::tool::normalize_tool_call_arguments(
                "repair",
                &tool_call.function.name,
                &tool_call.function.arguments,
            );
            sanitize_json_string_leaves(&mut repaired);
            tool_call.function.arguments = repaired;
        }
    }
}

fn sanitize_json_string_leaves(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                sanitize_json_string_leaves(value);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values_mut() {
                sanitize_json_string_leaves(value);
            }
        }
        serde_json::Value::String(text) => {
            *text = sanitize_provider_arg_string(text);
        }
        _ => {}
    }
}

fn sanitize_provider_arg_string(text: &str) -> String {
    let mut sanitized = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => sanitized.push_str("\\n"),
            '\t' => sanitized.push_str("\\t"),
            ch if ch.is_control() => {}
            ch => sanitized.push(ch),
        }
    }
    sanitized
}

pub fn completion_request_input_components(
    request: &CompletionRequest,
    counter: &crate::provider_input::ProviderInputCounter,
) -> Result<crate::provider_input::ProviderInputProjection, StreamingError> {
    counter.project_request(request).map_err(|error| {
        StreamingError::Completion(CompletionError::RequestError(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("provider_input_projection_failed: {error:#}"),
            ),
        )))
    })
}

fn completion_request_input_tokens(
    request: &CompletionRequest,
    counter: &crate::provider_input::ProviderInputCounter,
) -> Result<usize, StreamingError> {
    counter.estimate_request(request).map_err(|error| {
        StreamingError::Completion(CompletionError::RequestError(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("provider_input_projection_failed: {error:#}"),
            ),
        )))
    })
}

#[derive(Clone, Debug)]
pub(super) struct PreparedDispatch {
    request: CompletionRequest,
    projection: crate::provider_input::ProviderInputProjection,
}

impl PreparedDispatch {
    pub(super) fn request(&self) -> &CompletionRequest {
        &self.request
    }

    pub(super) fn projection(&self) -> &crate::provider_input::ProviderInputProjection {
        &self.projection
    }
}

pub(super) fn prepare_dispatch_attempt(
    assembled_request: &CompletionRequest,
    config: &LoopConfig,
    aggregate_token_budget: Option<&AggregateTokenBudget>,
) -> Result<PreparedDispatch, StreamingError> {
    let mut request = assembled_request.clone();
    let input_tokens =
        completion_request_input_tokens(&request, config.provider_input_counter.as_ref())?;
    clamp_request_output_budget(&mut request, config, input_tokens);
    ensure_context_can_dispatch(&request, config, input_tokens)?;
    super::aggregate_budget::clamp_request_aggregate_token_budget(
        &mut request,
        aggregate_token_budget,
        input_tokens,
    )?;
    let projection =
        completion_request_input_components(&request, config.provider_input_counter.as_ref())?;
    debug_assert_eq!(projection.estimated_input_tokens, input_tokens);
    Ok(PreparedDispatch {
        request,
        projection,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TurnContextDecision {
    pub(super) reason: ContextCompactionReason,
    pub(super) pre_compaction_input_tokens: Option<usize>,
}

pub(super) fn context_accounting_for_request(
    dispatch: &PreparedDispatch,
    config: &LoopConfig,
    turn_index: usize,
    attempt: u32,
    compaction_reason: ContextCompactionReason,
    pre_compaction_input_tokens: Option<usize>,
) -> ContextAccounting {
    let request = dispatch.request();
    let projection = dispatch.projection();
    let estimated_input_tokens = projection.estimated_input_tokens;
    ContextAccounting {
        accounting_version: CONTEXT_ACCOUNTING_VERSION,
        turn_index,
        attempt,
        estimator: projection.estimator.to_string(),
        components: projection.components.clone(),
        estimated_input_tokens,
        context_window: config.context_window,
        compaction_threshold_basis_points: crate::provider_input::budget::threshold_basis_points(
            config.compaction_threshold,
        ),
        compaction_threshold_tokens: crate::provider_input::budget::threshold_budget(
            config.context_window,
            config.compaction_threshold,
        ),
        configured_max_output_tokens: config.max_tokens,
        effective_max_output_tokens: request.max_tokens,
        compaction_reason,
        pre_compaction_input_tokens,
    }
}

pub fn clamp_request_output_budget(
    request: &mut CompletionRequest,
    config: &LoopConfig,
    input_tokens: usize,
) {
    let configured_max =
        crate::provider_input::budget::configured_output_ceiling(request.max_tokens);
    let effective_max = crate::provider_input::budget::effective_output_budget(
        input_tokens,
        config.context_window,
        configured_max,
    );
    if effective_max < configured_max {
        tracing::debug!(
            target: "gents::agent::loop_stream",
            input_tokens,
            context_window = config.context_window,
            configured_max_output_tokens = configured_max,
            effective_max_output_tokens = effective_max,
            "clamped completion output to remaining provider context"
        );
    }
    request.max_tokens = Some(
        u64::try_from(effective_max)
            .expect("a usize provider output ceiling is representable as u64"),
    );
}

pub fn ensure_context_can_dispatch(
    request: &CompletionRequest,
    config: &LoopConfig,
    input_tokens: usize,
) -> Result<(), StreamingError> {
    let configured_max =
        crate::provider_input::budget::configured_output_ceiling(request.max_tokens);
    if crate::provider_input::budget::can_dispatch(
        input_tokens,
        config.context_window,
        configured_max,
    ) {
        return Ok(());
    }
    Err(StreamingError::Completion(CompletionError::RequestError(
        Box::new(
            crate::provider_input::budget::ContextBudgetError::NoOutputCapacity {
                estimated_input_tokens: input_tokens,
                context_window: config.context_window,
                effective_max_output_tokens: configured_max,
            },
        ),
    )))
}

pub async fn assemble_provider_request<M>(
    model: &M,
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
    replay: &LoopReplayInput,
) -> Result<CompletionRequest, StreamingError>
where
    M: CompletionModel,
{
    let profile_wire = config.provider_input_counter.profile().replay_wire();
    if replay.wire.is_some() && replay.wire != profile_wire {
        return Err(replay_input_error(
            "replay wire differs from built provider profile",
        ));
    }
    let history_len = history.len();
    let mut joined = std::mem::take(history);
    joined.append(new_messages);
    let (indices, mut rows) = assistant_rows(&joined);
    let has_reasoning = rows.iter().any(|row| {
        row.content
            .iter()
            .any(|block| matches!(block, AssistantContent::Reasoning(_)))
    });
    let selection = match (has_reasoning, profile_wire, replay.issuer.as_ref()) {
        (false, ..) => None,
        (true, Some(wire), Some(issuer)) if replay.wire == Some(wire) => Some((wire, issuer)),
        (true, ..) => {
            strip_all_reasoning(&mut joined);
            None
        }
    };
    let Some((wire, issuer)) = selection else {
        (*history, *new_messages) = split_joined(joined, history_len);
        return build_core(model, history, new_messages, tools, config).await;
    };
    let tags = rows
        .iter()
        .filter(|row| {
            row.content
                .iter()
                .any(|block| matches!(block, AssistantContent::Reasoning(_)))
        })
        .filter_map(|row| row.source.clone())
        .collect::<Vec<_>>();
    let evidence = resolve_replay_evidence(replay, &tags).await?;
    let resolve = |tag: &ReplayTag| -> Vec<ResolvedReplayEvidence> {
        evidence
            .iter()
            .filter(|row| row.tag == *tag)
            .map(|row| row.evidence.clone())
            .collect()
    };
    for row in rows
        .iter_mut()
        .filter(|row| row.physical_header.is_none() && row.block_indices.is_empty())
    {
        if let Some(tag) = &row.source {
            if let [evidence] = resolve(tag).as_slice() {
                row.physical_header = Some(evidence.physical_header.clone());
                row.block_indices = (0..row.content.len()).collect();
            }
        }
    }
    apply_assistant_rows(
        &mut joined,
        &indices,
        replay_stage(&rows, issuer, wire, resolve),
    )?;
    let (stage_history, stage_new) = split_joined(joined.clone(), history_len);
    let mut core = build_core(model, &stage_history, &stage_new, tools, config).await?;
    let body = config
        .provider_input_counter
        .project_body(&core)
        .map_err(|error| replay_input_error(format!("provider replay projection: {error:#}")))?;
    let stage_body = crate::provider_input::replay_frontier::flatten(&body, wire)
        .map_err(|error| replay_input_error(format!("provider replay projection: {error:#}")))?;
    apply_assistant_rows(
        &mut joined,
        &indices,
        select_replay(&rows, issuer, wire, resolve, &stage_body),
    )?;
    (*history, *new_messages) = split_joined(joined, history_len);
    super::replace_core_chat_history(&mut core, config, history, new_messages)?;
    Ok(core)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn build_budgeted_request<M>(
    model: &M,
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
    replay: &mut LoopReplayInput,
    turn_index: usize,
    reduction_chain_keys: &mut Vec<String>,
    active_reduction_keys: &mut Vec<String>,
) -> Result<(CompletionRequest, TurnContextDecision), StreamingError>
where
    M: CompletionModel,
{
    let request =
        assemble_provider_request(model, history, new_messages, tools, config, replay).await?;
    let projection =
        completion_request_input_components(&request, config.provider_input_counter.as_ref())?;
    let before_tokens = projection.estimated_input_tokens;

    let Some(compactor) = config.turn_compactor.as_ref() else {
        return Ok((
            request,
            TurnContextDecision {
                reason: ContextCompactionReason::CompactorUnavailable,
                pre_compaction_input_tokens: None,
            },
        ));
    };
    let Some(admission) = crate::compaction::ReductionAdmission::for_input(
        before_tokens,
        config.context_window,
        config.compaction_threshold,
    ) else {
        return Ok((
            request,
            TurnContextDecision {
                reason: ContextCompactionReason::BelowThreshold,
                pre_compaction_input_tokens: None,
            },
        ));
    };
    let provider_messages = history
        .iter()
        .chain(new_messages.iter())
        .cloned()
        .collect::<Vec<_>>();
    let outcome = compactor(TurnCompactionRequest {
        messages: provider_messages,
        required: replay.required.clone(),
        admission,
        turn_index,
        prior_reduction_keys: reduction_chain_keys.clone(),
    })
    .await
    .map_err(|error| {
        if matches!(
            error.downcast_ref::<crate::compaction::ReductionError>(),
            Some(crate::compaction::ReductionError::CannotFit)
        ) || error.is::<ReplayCheckpointError>()
            || error.is::<ReplayEvidenceViolation>()
        {
            return StreamingError::Completion(CompletionError::RequestError(
                error.into_boxed_dyn_error(),
            ));
        }
        aggregate_token_budget_exhaustion_message(&error).map_or_else(
            || {
                StreamingError::Completion(CompletionError::ProviderError(format!(
                    "per-turn provider-input compaction failed: {error:#}"
                )))
            },
            |reason| StreamingError::Completion(CompletionError::ProviderError(reason)),
        )
    })?;
    let (mut compacted, reduction_key, reason) = match outcome {
        TurnCompactionOutcome::ProviderViewRepaired { messages } => (
            messages,
            None,
            ContextCompactionReason::ProviderViewRepaired,
        ),
        TurnCompactionOutcome::Reduced {
            messages,
            reduction_key,
        } => (
            messages,
            Some(reduction_key),
            ContextCompactionReason::Compacted,
        ),
        TurnCompactionOutcome::CannotFit => {
            return Err(StreamingError::Completion(CompletionError::RequestError(
                Box::new(crate::compaction::ReductionError::CannotFit),
            )));
        }
    };
    let compacted_prompt = compacted.pop().ok_or_else(|| {
        StreamingError::Completion(CompletionError::RequestError(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "per-turn provider-input compaction returned no prompt",
            ),
        )))
    })?;
    *history = compacted;
    *new_messages = vec![compacted_prompt.clone()];
    if let Some(reduction_key) = reduction_key {
        reduction_chain_keys.push(reduction_key.clone());
        active_reduction_keys.clear();
        active_reduction_keys.push(reduction_key);
    }

    let rebuilt =
        assemble_provider_request(model, history, new_messages, tools, config, replay).await?;
    let rebuilt_projection =
        completion_request_input_components(&rebuilt, config.provider_input_counter.as_ref())?;
    let after_tokens = rebuilt_projection.estimated_input_tokens;
    let effective_input_budget = crate::provider_input::budget::effective_input_budget(
        config.context_window,
        config.compaction_threshold,
    );
    tracing::info!(
        target: "gents::agent::loop_stream",
        turn = turn_index,
        before_tokens,
        after_tokens,
        effective_input_budget,
        context_window = config.context_window,
        max_output_tokens = config.max_tokens.unwrap_or_default(),
        "compacted provider input before completion dispatch"
    );

    let rebuilt_can_dispatch = crate::provider_input::budget::can_dispatch(
        after_tokens,
        config.context_window,
        crate::provider_input::budget::configured_output_ceiling(rebuilt.max_tokens),
    );
    if rebuilt_can_dispatch
        && crate::compaction::ReductionAdmission::for_input(
            after_tokens,
            config.context_window,
            config.compaction_threshold,
        )
        .is_some()
    {
        tracing::warn!(
            estimated_input_tokens = after_tokens,
            effective_input_budget,
            "provider input remains over threshold after reduction"
        );
        return Err(StreamingError::Completion(CompletionError::RequestError(
            Box::new(crate::compaction::ReductionError::CannotFit),
        )));
    }

    Ok((
        rebuilt,
        TurnContextDecision {
            reason,
            pre_compaction_input_tokens: Some(before_tokens),
        },
    ))
}

pub(super) async fn repair_and_rebuild_request<M>(
    model: &M,
    history: &mut Vec<TaggedMessage>,
    new_messages: &mut Vec<TaggedMessage>,
    tools: &[Box<dyn ToolDyn>],
    config: &LoopConfig,
) -> Result<CompletionRequest, StreamingError>
where
    M: CompletionModel,
{
    repair_provider_input(history, new_messages)?;
    strip_all_reasoning(history);
    strip_all_reasoning(new_messages);
    build_core(model, history, new_messages, tools, config).await
}
