use std::sync::Arc;

use defra_node::EmbeddedNode;
use gents_protocol::message::Message;
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, ReasoningPart, SegmentRun,
    SourceClose, StreamDeclaration, StreamPayload, TranscriptMessage,
};
use gents_protocol::rendered_request::RenderedRequestSource;

use super::canonical_rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use super::output::{load_current_request_assistant_candidates, CanonicalReplayScope};
use crate::provider_context_reduction::{capture_source_boundary, SourceBoundary};

use gents_loop::claude_messages_body::{
    prepare_replay_checkpoint, restore_and_narrow_replay, NarrowedAssistantRow,
    ReplayCheckpointError, ReplayTag, TaggedAssistantRow,
};
use gents_loop::loop_stream::TaggedMessage;

const AGENT_DID: &str = "did:test:replay-agent";
const SESSION_ID: &str = "replay-session";
const REQUEST_ID: &str = "replay-request";

struct ReplayFixture {
    node: Arc<EmbeddedNode>,
    request_doc_id: String,
    request_commit_cid: String,
    header_doc_id: String,
    header: TranscriptMessage,
    boundary: SourceBoundary,
    scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
}

impl ReplayFixture {
    fn scope(&self) -> CanonicalReplayScope<'_> {
        CanonicalReplayScope {
            agent_did: AGENT_DID,
            requester_did: None,
            session_id: SESSION_ID,
            request_id: REQUEST_ID,
            request_doc_id: &self.request_doc_id,
            request_commit_cid: &self.request_commit_cid,
            expected_scope_kind: self.scope_kind,
        }
    }

    async fn candidates(&self) -> anyhow::Result<Vec<super::output::CanonicalAssistantCandidate>> {
        load_current_request_assistant_candidates(&self.node, self.scope(), &self.boundary).await
    }

    async fn insert_capture(&self, source: RenderedRequestSource) {
        let capture_scope = format!("{}.1", self.scope_kind);
        let capture_key = gents_loop::rendered_request::capture_key(
            AGENT_DID,
            SESSION_ID,
            &self.request_doc_id,
            &capture_scope,
            0,
            0,
        )
        .unwrap();
        let source = serde_json::to_value(source).unwrap();
        let source = source.as_str().unwrap();
        let quote = |value: &str| format!("\"{}\"", crate::graphql::escape_graphql_string(value));
        let mutation = format!(
            r#"mutation {{ create_RenderedRequest(input: {{
                capture_key: {}, request_doc_id: {}, request_commit_cid: {},
                request_id: {}, session_id: {}, agent_did: {}, requester_did: "",
                behavior_id: "general", capture_scope: {},
                turn_index: 0, attempt: 0, capture_version: {},
                model_name: "test-model", source: {}, request_json: "{{}}",
                provenance_json: "{{}}", created_at: {}
            }}) {{ _docID }} }}"#,
            quote(&capture_key),
            quote(&self.request_doc_id),
            quote(&self.request_commit_cid),
            quote(REQUEST_ID),
            quote(SESSION_ID),
            quote(AGENT_DID),
            quote(&capture_scope),
            gents_protocol::rendered_request::CAPTURE_VERSION,
            quote(source),
            quote(&chrono::Utc::now().to_rfc3339()),
        );
        let response = self.node.execute(&mutation).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }

    async fn insert_authored_assistant(&mut self) {
        let now = chrono::Utc::now().to_rfc3339();
        let source = OutputSource::Authored {
            key: "assistant-note".into(),
        };
        let segment = OutputSegment {
            agent_did: AGENT_DID.into(),
            requester_did: None,
            session_id: SESSION_ID.into(),
            request_doc_id: self.request_doc_id.clone(),
            source,
            writer: OutputWriter::RequestExecution {
                execution_generation: "generation".into(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: 4,
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            }],
            payload: "note".into(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![4],
            }),
            created_at: now.clone(),
        };
        let created = self
            .node
            .execute_request_with_retry(
                defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                    .with_variables(output_segment_create_variables(&segment).unwrap()),
                defra_node::ExecuteRetryPolicy::default(),
            )
            .await;
        assert!(!created.has_errors(), "{:?}", created.errors);
        let close_doc_id =
            crate::graphql::single_mutation_document(&created, "create_AgentOutputSegment")
                .unwrap()
                .unwrap()["_docID"]
                .as_str()
                .unwrap()
                .to_string();
        let header = TranscriptMessage {
            message_key: super::canonical_rows::authored_message_key(
                &self.request_doc_id,
                "assistant-note",
            ),
            session_id: SESSION_ID.into(),
            agent_did: AGENT_DID.into(),
            requester_did: None,
            request_doc_id: Some(self.request_doc_id.clone()),
            publication: MessagePublication::RequestExecution {
                execution_generation: "generation".into(),
            },
            outcome: OutputOutcome::Complete,
            sequence: 2,
            role: MessageRole::Assistant,
            native_id: None,
            blocks: vec![MessageBlock::Text {
                text: PresentedPayload {
                    output: PayloadRef {
                        close_doc_id,
                        stream: 0,
                    },
                    presentation: PayloadPresentation::Full,
                },
            }],
            created_at: now,
        };
        let created = self
            .node
            .execute_request_with_retry(
                defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                    .with_variables(transcript_message_create_variables(&header).unwrap()),
                defra_node::ExecuteRetryPolicy::default(),
            )
            .await;
        assert!(!created.has_errors(), "{:?}", created.errors);
        self.boundary = capture_source_boundary(
            &self.node,
            SESSION_ID,
            AGENT_DID,
            None,
            &self.request_doc_id,
            &self.request_commit_cid,
        )
        .await
        .unwrap();
    }
}

async fn fixture() -> ReplayFixture {
    fixture_with_provider_payload(false).await
}

async fn signed_fixture() -> ReplayFixture {
    fixture_with_provider_payload(true).await
}

#[tokio::test]
async fn signed_oneshot_continuation_resolves_its_own_physical_capture_scope() {
    use gents_protocol::rendered_request::CaptureScopeKind;

    let fixture = fixture_with_provider_payload_and_scope(true, CaptureScopeKind::OneShot).await;
    fixture
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await;
    let candidate = fixture.candidates().await.unwrap().pop().unwrap();
    assert_eq!(candidate.coordinate.scope.kind, CaptureScopeKind::OneShot);
    let tag = ReplayTag {
        request_doc_id: fixture.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: candidate.coordinate.scope,
            turn_index: candidate.coordinate.turn_index,
            attempt: candidate.coordinate.attempt,
        },
    };
    let evidence = super::output::resolve_current_replay_tag(
        &fixture.node,
        fixture.scope(),
        &fixture.boundary,
        &tag,
    )
    .await
    .unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(
        evidence[0].origin,
        gents_loop::claude_messages_body::ReplayOrigin::ClaudeSubscription
    );
    assert_eq!(evidence[0].reasoning.len(), 2);

    let mut absent = tag.clone();
    let OutputSource::ProviderTurn { attempt, .. } = &mut absent.source else {
        unreachable!("fixture tag is provider-owned")
    };
    *attempt += 1;
    let batched = super::output::resolve_current_replay_tags(
        &fixture.node,
        fixture.scope(),
        &fixture.boundary,
        &[tag.clone(), absent.clone(), tag.clone()],
    )
    .await
    .unwrap();
    assert_eq!(
        batched
            .iter()
            .map(|(_, evidence)| evidence.len())
            .collect::<Vec<_>>(),
        vec![1, 0, 1]
    );
    assert_eq!(batched[0].0, tag);
    assert_eq!(batched[1].0, absent);

    let wrong_scope = CanonicalReplayScope {
        expected_scope_kind: CaptureScopeKind::Inference,
        ..fixture.scope()
    };
    let failure = super::output::resolve_current_replay_tag(
        &fixture.node,
        wrong_scope,
        &fixture.boundary,
        &tag,
    )
    .await
    .unwrap_err();
    assert!(failure
        .downcast_ref::<gents_loop::loop_stream::ReplayEvidenceViolation>()
        .is_some());
}

async fn fixture_with_provider_payload(signed: bool) -> ReplayFixture {
    fixture_with_provider_payload_and_scope(
        signed,
        gents_protocol::rendered_request::CaptureScopeKind::Inference,
    )
    .await
}

async fn fixture_with_provider_payload_and_scope(
    signed: bool,
    scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
) -> ReplayFixture {
    fixture_with_provider_payload_and_scope_and_tool(signed, scope_kind, true).await
}

async fn fixture_with_provider_payload_and_scope_and_tool(
    signed: bool,
    scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
    with_tool: bool,
) -> ReplayFixture {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    let created = node
        .execute(&format!(
            r#"mutation {{ create_AgentRequest(input: {{
                request_id: "{}", agent_did: "{}",
                behavior_id: "general", session_id: "{}", content: "prompt",
                lifecycle_state: "pending", execution_origin: "interactive",
                created_at: "{}", retry_count: 0, max_retries: 3, subagent_depth: 0
            }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(REQUEST_ID),
            crate::graphql::escape_graphql_string(AGENT_DID),
            crate::graphql::escape_graphql_string(SESSION_ID),
            crate::graphql::escape_graphql_string(&now),
        ))
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let request_doc_id = crate::graphql::single_mutation_document(&created, "create_AgentRequest")
        .unwrap()
        .unwrap()["_docID"]
        .as_str()
        .unwrap()
        .to_string();
    let request_commit_cid = crate::graphql::newest_document_composite_commit(
        &node,
        &request_doc_id,
        "replay fixture request",
    )
    .await
    .unwrap()
    .unwrap()
    .cid;

    let source = OutputSource::ProviderTurn {
        scope: format!("{scope_kind}.1").parse().unwrap(),
        turn_index: 0,
        attempt: 0,
    };
    let (runs, payload, stream_bytes) = if signed {
        let parts = ["思考", "answer", "", "{\"x\":1}"];
        let mut runs = vec![
            SegmentRun {
                stream: 0,
                bytes: parts[0].len() as u32,
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Reasoning,
                }),
            },
            SegmentRun {
                stream: 1,
                bytes: parts[1].len() as u32,
                declaration: Some(StreamDeclaration {
                    block_index: 1,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            },
            SegmentRun {
                stream: 2,
                bytes: 0,
                declaration: Some(StreamDeclaration {
                    block_index: 2,
                    part_index: 0,
                    payload: StreamPayload::Reasoning,
                }),
            },
            SegmentRun {
                stream: 3,
                bytes: parts[3].len() as u32,
                declaration: Some(StreamDeclaration {
                    block_index: 3,
                    part_index: 0,
                    payload: StreamPayload::ToolArguments {
                        id: "toolu-1".into(),
                        call_id: None,
                        name: "echo".into(),
                    },
                }),
            },
        ];
        if !with_tool {
            runs.pop();
        }
        let selected = if with_tool { &parts[..] } else { &parts[..3] };
        (
            runs,
            selected.concat(),
            selected.iter().map(|part| part.len() as u64).collect(),
        )
    } else {
        (
            vec![SegmentRun {
                stream: 0,
                bytes: 6,
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            }],
            "answer".into(),
            vec![6],
        )
    };
    let segment = OutputSegment {
        agent_did: AGENT_DID.into(),
        requester_did: None,
        session_id: SESSION_ID.into(),
        request_doc_id: request_doc_id.clone(),
        source: source.clone(),
        writer: OutputWriter::RequestExecution {
            execution_generation: "generation".into(),
        },
        ordinal: Some(0),
        runs,
        payload,
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes,
        }),
        created_at: now.clone(),
    };
    let created = node
        .execute_request_with_retry(
            defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(&segment).unwrap()),
            defra_node::ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let close_doc_id =
        crate::graphql::single_mutation_document(&created, "create_AgentOutputSegment")
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_string();
    let header = TranscriptMessage {
        message_key: format!(
            "provider:{}:{}",
            request_doc_id,
            serde_json::to_string(&source).unwrap()
        ),
        session_id: SESSION_ID.into(),
        agent_did: AGENT_DID.into(),
        requester_did: None,
        request_doc_id: Some(request_doc_id.clone()),
        publication: MessagePublication::RequestExecution {
            execution_generation: "generation".into(),
        },
        outcome: OutputOutcome::Complete,
        sequence: 1,
        role: MessageRole::Assistant,
        native_id: signed.then(|| "provider-id".into()),
        blocks: if signed {
            let reference = |stream| PayloadRef {
                close_doc_id: close_doc_id.clone(),
                stream,
            };
            let mut blocks = vec![
                MessageBlock::Reasoning {
                    id: None,
                    parts: vec![ReasoningPart::Text {
                        text: reference(0),
                        signature: Some("sig-α".into()),
                    }],
                },
                MessageBlock::Text {
                    text: PresentedPayload {
                        output: reference(1),
                        presentation: PayloadPresentation::Full,
                    },
                },
                MessageBlock::Reasoning {
                    id: None,
                    parts: vec![ReasoningPart::Text {
                        text: reference(2),
                        signature: Some("sig-empty".into()),
                    }],
                },
            ];
            if with_tool {
                blocks.push(MessageBlock::ToolCall {
                    tool_call_doc_id: "tool-doc".into(),
                    id: "toolu-1".into(),
                    call_id: None,
                    name: "echo".into(),
                    arguments: reference(3),
                    signature: None,
                    additional_params: None,
                });
            }
            blocks
        } else {
            vec![MessageBlock::Text {
                text: PresentedPayload {
                    output: PayloadRef {
                        close_doc_id,
                        stream: 0,
                    },
                    presentation: PayloadPresentation::Full,
                },
            }]
        },
        created_at: now,
    };
    let created = node
        .execute_request_with_retry(
            defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&header).unwrap()),
            defra_node::ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let header_doc_id = crate::graphql::single_mutation_document(&created, "create_AgentMessage")
        .unwrap()
        .unwrap()["_docID"]
        .as_str()
        .unwrap()
        .to_string();
    let boundary = capture_source_boundary(
        &node,
        SESSION_ID,
        AGENT_DID,
        None,
        &request_doc_id,
        &request_commit_cid,
    )
    .await
    .unwrap();
    ReplayFixture {
        node,
        request_doc_id,
        request_commit_cid,
        header_doc_id,
        header,
        boundary,
        scope_kind,
    }
}

/// The checkpoint stores only the native projection and physical source tags;
/// expected reasoning comes back from the canonical header/segment reader.
async fn persist_and_restore_signed(
    fixture: &ReplayFixture,
) -> (
    Vec<Message>,
    Result<NarrowedAssistantRow, ReplayCheckpointError>,
) {
    use crate::provider_context_reduction::{
        load_for_request, persist, NewProviderContextReduction, ReplayAssociations,
    };
    use gents_protocol::message::{Text, ToolResult, ToolResultContent, UserContent};

    let original = fixture.candidates().await.unwrap().remove(0).message;
    let tag = ReplayTag {
        request_doc_id: fixture.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: "inference.1".parse().unwrap(),
            turn_index: 0,
            attempt: 0,
        },
    };
    let prefix = vec![Message::user("earlier")];
    let suffix = vec![
        original.clone(),
        Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                id: "toolu-1".into(),
                call_id: None,
                content: vec![ToolResultContent::Text(Text {
                    text: "tool result".into(),
                })],
            })],
        },
        Message::user("next prompt"),
    ];
    let tagged_prefix = vec![TaggedMessage::unassociated(prefix[0].clone())];
    let tagged_suffix = vec![
        TaggedMessage {
            message: suffix[0].clone(),
            source: Some(tag.clone()),
        },
        TaggedMessage::unassociated(suffix[1].clone()),
        TaggedMessage::unassociated(suffix[2].clone()),
    ];
    let associations =
        ReplayAssociations::from_tagged_split(vec![tag.clone()], &tagged_prefix, &tagged_suffix);
    persist(
        &fixture.node,
        NewProviderContextReduction {
            agent_did: AGENT_DID,
            requester_did: None,
            session_id: SESSION_ID,
            request_id: REQUEST_ID,
            request_doc_id: &fixture.request_doc_id,
            request_commit_cid: &fixture.request_commit_cid,
            reduction_index: 1,
            turn_index: 0,
            parent_reduction_key: None,
            producer_call: None,
            source_boundary: &fixture.boundary,
            compacted_prefix: &prefix,
            retained_suffix: &suffix,
            checkpoint_messages: &suffix,
            replay_associations: &associations,
            summary: "",
            original_tokens: 100,
            compacted_tokens: 50,
        },
    )
    .await
    .unwrap();

    let row = load_for_request(&fixture.node, &fixture.request_doc_id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let associations = row.replay_associations().unwrap();
    assert_eq!(associations.required, vec![tag.clone()]);
    let persisted_native = row.checkpoint_messages().unwrap();
    assert_eq!(persisted_native, suffix);
    let tagged = row.checkpoint_tagged_messages().unwrap();
    assert_eq!(tagged[0].message, original);
    assert_eq!(tagged[0].source, Some(tag.clone()));
    let evidence = super::output::resolve_current_replay_tag(
        &fixture.node,
        fixture.scope(),
        &row.source_boundary().unwrap(),
        &tag,
    )
    .await
    .unwrap();
    assert_eq!(evidence.len(), 1);
    let assistant_rows = tagged
        .iter()
        .filter_map(|row| match &row.message {
            Message::Assistant { id, content } => Some(TaggedAssistantRow {
                source: row.source.clone(),
                id: id.clone(),
                content: content.clone(),
            }),
            _ => None,
        })
        .collect();
    let checkpoint = prepare_replay_checkpoint(associations.required, assistant_rows, 0).unwrap();
    let result =
        restore_and_narrow_replay(&checkpoint, |_| evidence.clone()).map(|mut rows| rows.remove(0));
    (persisted_native, result)
}

#[tokio::test]
async fn signed_claude_checkpoint_reloads_canonical_reasoning_and_body_order() {
    use gents_protocol::message::{AssistantContent, ReasoningContent};

    let fixture = signed_fixture().await;
    fixture
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await;
    let (persisted_native, narrowed) = persist_and_restore_signed(&fixture).await;
    let narrowed = narrowed.unwrap();
    let Message::Assistant {
        id: original_id,
        content: original_content,
    } = persisted_native[0].clone()
    else {
        panic!("canonical fixture must reconstruct an assistant")
    };
    assert!(matches!(
        original_content.as_slice(),
        [AssistantContent::Reasoning(first), AssistantContent::Text(_),
         AssistantContent::Reasoning(second), AssistantContent::ToolCall(_)]
        if matches!(first.content.as_slice(),
            [ReasoningContent::Text { text, signature: Some(signature) }]
            if text == "思考" && signature == "sig-α")
        && matches!(second.content.as_slice(),
            [ReasoningContent::Text { text, signature: Some(signature) }]
            if text.is_empty() && signature == "sig-empty")
    ));
    assert_eq!(narrowed.id, original_id);
    assert_eq!(narrowed.content, original_content);
    let body = gents_loop::claude_messages_body::build_messages_body_native(
        "claude-test",
        None,
        None,
        &[
            Message::Assistant {
                id: narrowed.id,
                content: narrowed.content,
            },
            persisted_native[1].clone(),
            persisted_native[2].clone(),
        ],
        &[],
    )
    .unwrap();
    assert_eq!(
        body["messages"][0]["content"],
        serde_json::Value::Array(narrowed.wire_blocks)
    );
    assert_eq!(body["messages"][1]["content"][0]["type"], "tool_result");
    assert_eq!(body["messages"][1]["content"][0]["tool_use_id"], "toolu-1");
    assert_eq!(
        body["messages"][0]["content"],
        serde_json::json!([
            {"type": "thinking", "thinking": "思考", "signature": "sig-α"},
            {"type": "text", "text": "answer"},
            {"type": "thinking", "thinking": "", "signature": "sig-empty"},
            {"type": "tool_use", "id": "toolu-1", "name": "echo", "input": {"x": 1}},
        ])
    );
}

#[tokio::test]
async fn signed_checkpoint_rejects_missing_and_foreign_capture_on_reload() {
    use gents_loop::claude_messages_body::ReplayEvidenceError;

    let missing = signed_fixture().await;
    let (_, result) = persist_and_restore_signed(&missing).await;
    assert!(matches!(
        result,
        Err(ReplayCheckpointError::Evidence(
            ReplayEvidenceError::MissingOrigin
        ))
    ));

    let foreign = signed_fixture().await;
    foreign
        .insert_capture(RenderedRequestSource::OpenAiChatCompletions)
        .await;
    let (_, result) = persist_and_restore_signed(&foreign).await;
    assert!(matches!(
        result,
        Err(ReplayCheckpointError::Evidence(
            ReplayEvidenceError::ForeignOrigin
        ))
    ));
}

#[tokio::test]
async fn exact_capture_source_classifies_canonical_assistant() {
    use gents_loop::claude_messages_body::ReplayOrigin;
    let observed = fixture().await;
    let candidates = observed.candidates().await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].header_doc_id, observed.header_doc_id);
    assert_eq!(candidates[0].sequence, 1);
    assert_eq!(candidates[0].message, Message::assistant("answer"));
    assert_eq!(candidates[0].origin, ReplayOrigin::Missing);
    assert_eq!(candidates[0].coordinate.scope.to_string(), "inference.1");
    assert_eq!(candidates[0].coordinate.turn_index, 0);
    assert_eq!(candidates[0].coordinate.attempt, 0);
    observed
        .insert_capture(RenderedRequestSource::OpenAiChatCompletions)
        .await;
    assert_eq!(
        observed.candidates().await.unwrap()[0].origin,
        ReplayOrigin::Foreign
    );
    // A later mutable lease-field version must not invalidate the capture's
    // historical physical request CID. This fixture only tests version
    // membership, not RenewalTask authorization or scheduling.
    let later_deadline = crate::graphql::escape_graphql_string(
        &(chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(),
    );
    let updated = observed
        .node
        .execute(&format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_lease_expires_at: "{later_deadline}" }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(&observed.request_doc_id),
        ))
        .await;
    assert!(!updated.has_errors(), "{:?}", updated.errors);
    let newest = crate::graphql::newest_document_composite_commit(
        &observed.node,
        &observed.request_doc_id,
        "later replay request version",
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(newest.cid, observed.request_commit_cid);
    assert_eq!(
        observed.candidates().await.unwrap()[0].origin,
        ReplayOrigin::Foreign
    );

    let claude = fixture().await;
    claude
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await;
    assert_eq!(
        claude.candidates().await.unwrap()[0].origin,
        ReplayOrigin::ClaudeSubscription
    );
}

#[tokio::test]
async fn authored_assistant_is_history_not_provider_continuation() {
    let mut observed = fixture().await;
    observed.insert_authored_assistant().await;
    let candidates = observed.candidates().await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].header_doc_id, observed.header_doc_id);
    assert_eq!(candidates[0].sequence, 1);
}

#[tokio::test]
async fn non_required_signed_history_with_wrong_provider_scope_stays_permissive() {
    use gents_loop::loop_stream::{narrow_tagged_history, provider_view_tagged, LoopReplayInput};
    use gents_loop::provider_input::ProviderInputProfile;
    use gents_protocol::message::AssistantContent;
    use gents_protocol::rendered_request::CaptureScopeKind;

    // This is a complete, physically published provider assistant, not a
    // body-only lookalike. Its OneShot close is invalid for the Inference
    // resolver, but it has no tool call and therefore needs no current replay.
    let fixture =
        fixture_with_provider_payload_and_scope_and_tool(true, CaptureScopeKind::OneShot, false)
            .await;
    assert_eq!(fixture.candidates().await.unwrap().len(), 1);
    let inference_scope = CanonicalReplayScope {
        expected_scope_kind: CaptureScopeKind::Inference,
        ..fixture.scope()
    };
    assert!(load_current_request_assistant_candidates(
        &fixture.node,
        inference_scope,
        &fixture.boundary,
    )
    .await
    .unwrap()
    .is_empty());

    let history = super::output::load_sequenced_messages(
        &fixture.node,
        SESSION_ID,
        AGENT_DID,
        None,
        None,
        None,
        Some(&fixture.request_doc_id),
        Some(ProviderInputProfile::ClaudeMessages),
    )
    .await
    .unwrap();
    assert_eq!(history.len(), 1);
    assert!(history[0].provider_source.is_none());
    assert!(matches!(&history[0].message,
        Message::Assistant { content, .. }
        if content.iter().filter(|block| matches!(block, AssistantContent::Reasoning(_))).count() == 2
    ));

    let mut replay = LoopReplayInput {
        request_doc_id: Some(fixture.request_doc_id.clone()),
        ..LoopReplayInput::default()
    };
    let tagged = crate::provider_input::replay::tag_canonical_history(
        &history,
        &mut replay,
        ProviderInputProfile::ClaudeMessages,
    );
    assert!(replay.required.is_empty());
    let mut projected = provider_view_tagged(ProviderInputProfile::ClaudeMessages, tagged).unwrap();
    narrow_tagged_history(
        ProviderInputProfile::ClaudeMessages,
        &mut projected,
        &mut replay,
    )
    .await
    .unwrap();
    assert_eq!(projected.len(), 1);
    assert!(projected[0].source.is_none());
    assert!(matches!(&projected[0].message,
        Message::Assistant { content, .. }
        if matches!(content.as_slice(), [AssistantContent::Text(text)] if text.text == "answer")
    ));
}

#[tokio::test]
async fn replay_high_water_and_header_twins_fail_closed() {
    let observed = fixture().await;
    let mut forged = observed.boundary.clone();
    forged.canonical_through.as_mut().unwrap().commit_cid = "not-a-header-commit".into();
    let bad_high_water =
        load_current_request_assistant_candidates(&observed.node, observed.scope(), &forged)
            .await
            .err()
            .expect("forged high-water must fail");
    assert!(bad_high_water
        .downcast_ref::<gents_loop::loop_stream::ReplayEvidenceViolation>()
        .is_some());
    let mut forged_request = observed.boundary.clone();
    forged_request.request_commit_cid = "not-a-request-commit".into();
    let forged_scope = CanonicalReplayScope {
        request_commit_cid: &forged_request.request_commit_cid,
        ..observed.scope()
    };
    assert!(load_current_request_assistant_candidates(
        &observed.node,
        forged_scope,
        &forged_request
    )
    .await
    .is_err());

    let mut twin = observed.header.clone();
    twin.message_key.push_str(":twin");
    let created = observed
        .node
        .execute_request_with_retry(
            defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&twin).unwrap()),
            defra_node::ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    assert!(observed.candidates().await.is_err());
}
