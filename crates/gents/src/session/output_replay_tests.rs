use gents_protocol::error_chain::ErrorChain;
use std::sync::Arc;

use defra_node::EmbeddedNode;
use gents_protocol::message::Message;
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, ReasoningPart, SegmentRun,
    SourceClose, StreamDeclaration, StreamPayload, TranscriptMessage,
};
use gents_protocol::rendered_request::RenderedRequestSource;
use sha2::{Digest, Sha256};

use super::canonical_rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use super::output::{
    load_canonical_assistant_candidates, CanonicalReplayScope, ReplayRequestReader,
};
use crate::provider_context_reduction::{capture_source_boundary, SourceBoundary};

use gents_loop::claude_messages_body::{
    replay_stage, select_replay, ReplayIssuer, ReplayTag, ReplayWire, TaggedAssistantRow,
};
use gents_loop::provider_input::replay_frontier::flatten;

#[derive(Clone, Copy)]
enum CaptureStorage {
    Exact,
    Corrupt,
    MissingDeltaBase,
}

#[derive(Clone)]
struct CapturedRequest {
    issuer: ReplayIssuer,
    wire: ReplayWire,
    body: serde_json::Value,
}

const NODE_DID: &str = "did:test:replay-agent";
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
            node_did: NODE_DID,
            requester_did: None,
            session_id: SESSION_ID,
            request_id: REQUEST_ID,
            request_doc_id: &self.request_doc_id,
            request_commit_cid: &self.request_commit_cid,
            expected_scope_kind: self.scope_kind,
        }
    }

    async fn candidates(&self) -> anyhow::Result<Vec<super::output::CanonicalAssistantCandidate>> {
        load_canonical_assistant_candidates(&self.node, self.scope(), &self.boundary).await
    }

    async fn insert_capture(&self, source: RenderedRequestSource) -> Option<CapturedRequest> {
        self.insert_capture_stored(source, CaptureStorage::Exact)
            .await
    }

    async fn insert_capture_stored(
        &self,
        source: RenderedRequestSource,
        storage: CaptureStorage,
    ) -> Option<CapturedRequest> {
        let (body, wire) = match source {
            RenderedRequestSource::ClaudeCliSubscription => (
                serde_json::json!({
                    "model": "claude-test", "system": [{"type":"text","text":"system"}],
                    "messages": [{"role":"user","content":[{"type":"text","text":"prompt"}]}],
                    "tools": [], "stream": true,
                }),
                Some(ReplayWire::ClaudeMessages),
            ),
            RenderedRequestSource::OpenAiChatCompletions => (
                serde_json::json!({"model":"test-model","messages":[{"role":"user","content":"prompt"}]}),
                None,
            ),
            RenderedRequestSource::OpenAiResponses => (
                serde_json::json!({"model":"test-model","input":[{"role":"user","content":"prompt"}]}),
                Some(ReplayWire::Responses),
            ),
        };
        self.insert_capture_row(source, 0, body, storage, wire)
            .await
    }

    async fn insert_capture_row(
        &self,
        source: RenderedRequestSource,
        turn_index: u32,
        body: serde_json::Value,
        storage: CaptureStorage,
        wire: Option<ReplayWire>,
    ) -> Option<CapturedRequest> {
        use crate::rendered_request::{
            build_rendered_completion_request, AssemblyBuildPath, AssemblyTrace,
            DefraRenderedRequestSink, RenderedRequestComponents, RenderedRequestContext,
        };

        let capture_scope = format!("{}.1", self.scope_kind);
        let uri = match source {
            RenderedRequestSource::ClaudeCliSubscription => crate::claude_messages::MESSAGES_URI,
            RenderedRequestSource::OpenAiChatCompletions => {
                "https://api.openai.com/v1/chat/completions"
            }
            RenderedRequestSource::OpenAiResponses => "https://api.openai.com/v1/responses",
        };
        let family = match source {
            RenderedRequestSource::ClaudeCliSubscription => {
                gents_loop::backend_provider::BackendProviderKind::ClaudeCliSubscription.as_str()
            }
            _ => gents_loop::backend_provider::BackendProviderKind::OpenAiCompatible.as_str(),
        };
        let destination: rig::http_client::Uri = uri.parse().expect("fixture destination");
        let endpoint = format!(
            "{}://{}",
            destination.scheme_str().expect("scheme"),
            destination.authority().expect("authority")
        );
        let route_path_sha256 = format!("{:x}", Sha256::digest(destination.path().as_bytes()));
        let issuer = gents_loop::rendered_request::transport::replay_issuer_for_destination(
            family,
            &destination,
        )
        .expect("fixture route has no query");
        let trace = AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, Vec::new());
        let rendered = build_rendered_completion_request(
            &RenderedRequestContext {
                request_doc_id: self.request_doc_id.clone(),
                request_commit_cid: self.request_commit_cid.clone(),
                request_id: REQUEST_ID.into(),
                node_did: NODE_DID.into(),
                requester_did: String::new(),
                agent_id: "general".into(),
                session_id: SESSION_ID.into(),
                model_name: "test-model".into(),
                provider_family: Some(family.into()),
            },
            &capture_scope,
            source,
            Some(endpoint),
            Some(route_path_sha256),
            turn_index as usize,
            0,
            trace,
            RenderedRequestComponents::from_provider_body(body.clone(), source),
            None,
        )
        .expect("canonical capture fixture");
        let sink = DefraRenderedRequestSink::new(self.node.clone());
        match storage {
            CaptureStorage::Exact => sink.capture(rendered).await.expect("durable capture owner"),
            CaptureStorage::Corrupt => sink
                .create_stored_for_test(&rendered, "not a capture record")
                .await
                .expect("corrupt capture row"),
            CaptureStorage::MissingDeltaBase => {
                use crate::rendered_request::encoding::{
                    encode_against, encode_container, encode_full, BaseWitness,
                };
                let mut value = body.clone();
                value["padding"] = serde_json::json!("x".repeat(4096));
                let mut base_value = value.clone();
                base_value["stream"] = serde_json::json!(false);
                let request = encode_against(
                    &value,
                    &base_value,
                    BaseWitness {
                        doc_id: "bae-00000000-0000-5000-8000-000000000000".into(),
                        field_commit_cid: "missing-base-commit".into(),
                        depth: 0,
                        node_did: NODE_DID.into(),
                        requester_did: String::new(),
                        session_id: SESSION_ID.into(),
                        source: serde_json::to_value(source)
                            .unwrap()
                            .as_str()
                            .unwrap()
                            .to_owned(),
                        capture_scope: capture_scope.clone(),
                    },
                )
                .unwrap();
                let provenance = encode_full(&rendered.provenance_payload_json).unwrap();
                let stored = encode_container(&request, &provenance).unwrap();
                assert!(
                    stored.contains("missing-base-commit"),
                    "fixture must store a delta"
                );
                sink.create_stored_for_test(&rendered, &stored)
                    .await
                    .expect("delta capture row");
            }
        }
        wire.map(|wire| CapturedRequest { issuer, wire, body })
    }

    async fn insert_provider_turn(&mut self, turn_index: u32, body: serde_json::Value) {
        let now = chrono::Utc::now().to_rfc3339();
        let source = OutputSource::ProviderTurn {
            scope: format!("{}.1", self.scope_kind).parse().unwrap(),
            turn_index,
            attempt: 0,
        };
        let reasoning = format!("think-{turn_index}");
        let signature = format!("sig-{turn_index}");
        let answer = format!("answer-{turn_index}");
        let parts = [reasoning.as_str(), signature.as_str(), answer.as_str()];
        let segment = OutputSegment {
            node_did: NODE_DID.into(),
            requester_did: None,
            session_id: SESSION_ID.into(),
            request_doc_id: self.request_doc_id.clone(),
            source: source.clone(),
            writer: OutputWriter::RequestExecution {
                execution_generation: "generation".into(),
            },
            ordinal: Some(0),
            runs: vec![
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
                        block_index: 0,
                        part_index: 0,
                        payload: StreamPayload::ReasoningSignature,
                    }),
                },
                SegmentRun {
                    stream: 2,
                    bytes: parts[2].len() as u32,
                    declaration: Some(StreamDeclaration {
                        block_index: 1,
                        part_index: 0,
                        payload: StreamPayload::Text,
                    }),
                },
            ],
            payload: parts.concat(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: parts.iter().map(|part| part.len() as u64).collect(),
            }),
            created_at: now.clone(),
        };
        let created = self
            .node
            .execute_request(
                defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                    .with_variables(output_segment_create_variables(&segment).unwrap()),
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
                self.request_doc_id,
                serde_json::to_string(&source).unwrap()
            ),
            session_id: SESSION_ID.into(),
            node_did: NODE_DID.into(),
            requester_did: None,
            request_doc_id: Some(self.request_doc_id.clone()),
            publication: MessagePublication::RequestExecution {
                execution_generation: "generation".into(),
            },
            outcome: OutputOutcome::Complete,
            sequence: turn_index + 1,
            role: MessageRole::Assistant,
            native_id: Some(format!("provider-{turn_index}")),
            blocks: vec![
                MessageBlock::Reasoning {
                    id: None,
                    parts: vec![ReasoningPart::Text {
                        text: PayloadRef {
                            close_doc_id: close_doc_id.clone(),
                            stream: 0,
                        },
                        signature: Some(signature),
                    }],
                },
                MessageBlock::Text {
                    text: PresentedPayload {
                        output: PayloadRef {
                            close_doc_id,
                            stream: 2,
                        },
                        presentation: PayloadPresentation::Full,
                    },
                },
            ],
            created_at: now,
        };
        let created = self
            .node
            .execute_request(
                defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                    .with_variables(transcript_message_create_variables(&header).unwrap()),
            )
            .await;
        assert!(!created.has_errors(), "{:?}", created.errors);
        self.insert_capture_row(
            RenderedRequestSource::ClaudeCliSubscription,
            turn_index,
            body,
            CaptureStorage::Exact,
            Some(ReplayWire::ClaudeMessages),
        )
        .await
        .expect("turn capture");
        self.boundary = capture_source_boundary(
            &self.node,
            SESSION_ID,
            NODE_DID,
            None,
            &self.request_doc_id,
            &self.request_commit_cid,
        )
        .await
        .unwrap();
    }

    async fn insert_authored_assistant(&mut self) {
        let now = chrono::Utc::now().to_rfc3339();
        let source = OutputSource::Authored {
            key: "assistant-note".into(),
        };
        let segment = OutputSegment {
            node_did: NODE_DID.into(),
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
            .execute_request(
                defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                    .with_variables(output_segment_create_variables(&segment).unwrap()),
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
            node_did: NODE_DID.into(),
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
            .execute_request(
                defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                    .with_variables(transcript_message_create_variables(&header).unwrap()),
            )
            .await;
        assert!(!created.has_errors(), "{:?}", created.errors);
        self.boundary = capture_source_boundary(
            &self.node,
            SESSION_ID,
            NODE_DID,
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
    let projection = fixture
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await
        .unwrap();
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
    let evidence = super::output::resolve_canonical_replay_tag(
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
        gents_loop::claude_messages_body::ReplayOrigin::AcceptedProvider
    );
    assert_eq!(evidence[0].reasoning.len(), 2);
    assert_eq!(
        evidence[0].captured,
        Some(flatten(&projection.body, projection.wire).unwrap())
    );

    let mut absent = tag.clone();
    let OutputSource::ProviderTurn { attempt, .. } = &mut absent.source else {
        unreachable!("fixture tag is provider-owned")
    };
    *attempt += 1;
    let batched = super::output::resolve_canonical_replay_tags(
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
    let without_scope = super::output::resolve_canonical_replay_tag(
        &fixture.node,
        wrong_scope,
        &fixture.boundary,
        &tag,
    )
    .await
    .unwrap();
    assert!(without_scope.is_empty());
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
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    let created = node
        .execute(&format!(
            r#"mutation {{ create_AgentRequest(input: {{
                request_id: "{}", purpose: "normal", node_did: "{}",
                agent_id: "general", session_id: "{}", content: "prompt",
                lifecycle_state: "pending", execution_origin: "interactive",
                created_at: "{}", retry_count: 0, max_retries: 3, request_hop: 0
            }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(REQUEST_ID),
            crate::graphql::escape_graphql_string(NODE_DID),
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
        let parts = ["思考", "sig-α", "answer", "", "sig-empty", "{\"x\":1}"];
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
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::ReasoningSignature,
                }),
            },
            SegmentRun {
                stream: 2,
                bytes: parts[2].len() as u32,
                declaration: Some(StreamDeclaration {
                    block_index: 1,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            },
            SegmentRun {
                stream: 3,
                bytes: parts[3].len() as u32,
                declaration: Some(StreamDeclaration {
                    block_index: 2,
                    part_index: 0,
                    payload: StreamPayload::Reasoning,
                }),
            },
            SegmentRun {
                stream: 4,
                bytes: parts[4].len() as u32,
                declaration: Some(StreamDeclaration {
                    block_index: 2,
                    part_index: 0,
                    payload: StreamPayload::ReasoningSignature,
                }),
            },
            SegmentRun {
                stream: 5,
                bytes: parts[5].len() as u32,
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
        let selected = if with_tool { &parts[..] } else { &parts[..5] };
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
        node_did: NODE_DID.into(),
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
        .execute_request(
            defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(&segment).unwrap()),
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
        node_did: NODE_DID.into(),
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
                        output: reference(2),
                        presentation: PayloadPresentation::Full,
                    },
                },
                MessageBlock::Reasoning {
                    id: None,
                    parts: vec![ReasoningPart::Text {
                        text: reference(3),
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
                    arguments: reference(5),
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
        .execute_request(
            defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&header).unwrap()),
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
        NODE_DID,
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

#[tokio::test]
async fn signed_capture_restores_exact_authenticated_suffix_and_rejects_rewritten_prefix() {
    use gents_protocol::message::AssistantContent;

    let fixture = signed_fixture().await;
    let projection = fixture
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await
        .unwrap();
    let candidate = fixture.candidates().await.unwrap().remove(0);
    let tag = ReplayTag {
        request_doc_id: fixture.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: candidate.coordinate.scope,
            turn_index: candidate.coordinate.turn_index,
            attempt: candidate.coordinate.attempt,
        },
    };
    let Message::Assistant { id, content } = candidate.message.clone() else {
        panic!("canonical fixture must reconstruct an assistant");
    };
    assert_eq!(content.len(), 4);
    let rows = vec![TaggedAssistantRow {
        source: Some(tag.clone()),
        physical_header: Some(candidate.header_doc_id.clone()),
        block_indices: (0..content.len()).collect(),
        id: id.clone(),
        content: content.clone(),
    }];
    let evidence = super::output::resolve_canonical_replay_tag(
        &fixture.node,
        fixture.scope(),
        &fixture.boundary,
        &tag,
    )
    .await
    .unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].reasoning.len(), 2);
    let resolve = |_: &ReplayTag| evidence.clone();
    let stage = replay_stage(&rows, &projection.issuer, projection.wire, resolve);
    assert_eq!(stage, rows, "provenance-valid turn stays in the stage");
    let next_body = |system: &str| {
        let mut body = projection.body.clone();
        body["system"] = serde_json::json!([{"type": "text", "text": system}]);
        let assistant =
            gents_loop::claude_messages_body::encode_assistant_content(&stage[0].content).unwrap();
        let messages = body["messages"].as_array_mut().unwrap();
        messages.push(serde_json::json!({"role": "assistant", "content": assistant}));
        messages.push(
            serde_json::json!({"role": "user", "content": [{"type": "text", "text": "next"}]}),
        );
        flatten(&body, projection.wire).unwrap()
    };
    let restored = select_replay(
        &rows,
        &projection.issuer,
        projection.wire,
        resolve,
        &next_body("system"),
    );
    assert_eq!(restored[0].content, content);
    assert_eq!(restored[0].block_indices, vec![0, 1, 2, 3]);

    let stripped = select_replay(
        &rows,
        &projection.issuer,
        projection.wire,
        resolve,
        &next_body("changed system"),
    );
    assert!(stripped[0]
        .content
        .iter()
        .all(|block| !matches!(block, AssistantContent::Reasoning(_))));
    assert_eq!(stripped[0].block_indices, vec![1, 3]);
    assert_eq!(
        stripped[0].content,
        vec![content[1].clone(), content[3].clone()]
    );
}

#[tokio::test]
async fn missing_or_foreign_capture_cannot_authorize_reasoning_suffix() {
    let missing = signed_fixture().await;
    let candidate = missing.candidates().await.unwrap().remove(0);
    let tag = ReplayTag {
        request_doc_id: missing.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: candidate.coordinate.scope,
            turn_index: candidate.coordinate.turn_index,
            attempt: candidate.coordinate.attempt,
        },
    };
    assert!(super::output::resolve_canonical_replay_tag(
        &missing.node,
        missing.scope(),
        &missing.boundary,
        &tag,
    )
    .await
    .unwrap()
    .is_empty());

    let foreign = signed_fixture().await;
    foreign
        .insert_capture(RenderedRequestSource::OpenAiChatCompletions)
        .await;
    let foreign_candidate = foreign.candidates().await.unwrap().remove(0);
    let foreign_tag = ReplayTag {
        request_doc_id: foreign.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: foreign_candidate.coordinate.scope,
            turn_index: foreign_candidate.coordinate.turn_index,
            attempt: foreign_candidate.coordinate.attempt,
        },
    };
    let foreign_evidence = super::output::resolve_canonical_replay_tag(
        &foreign.node,
        foreign.scope(),
        &foreign.boundary,
        &foreign_tag,
    )
    .await
    .unwrap();
    assert!(foreign_evidence.is_empty());
}

#[tokio::test]
async fn exact_capture_source_binds_canonical_assistant_across_request_versions() {
    let observed = fixture().await;
    let candidates = observed.candidates().await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].header_doc_id, observed.header_doc_id);
    assert_eq!(candidates[0].sequence, 1);
    assert_eq!(candidates[0].message, Message::assistant("answer"));
    assert_eq!(candidates[0].coordinate.scope.to_string(), "inference.1");
    assert_eq!(candidates[0].coordinate.turn_index, 0);
    assert_eq!(candidates[0].coordinate.attempt, 0);
    let projection = observed
        .insert_capture(RenderedRequestSource::OpenAiChatCompletions)
        .await;
    assert!(projection.is_none());
    assert_eq!(observed.candidates().await.unwrap().len(), 1);
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
    assert_eq!(observed.candidates().await.unwrap().len(), 1);

    let claude = fixture().await;
    let projection = claude
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await
        .unwrap();
    let candidate = claude.candidates().await.unwrap().remove(0);
    let tag = ReplayTag {
        request_doc_id: claude.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: candidate.coordinate.scope,
            turn_index: candidate.coordinate.turn_index,
            attempt: candidate.coordinate.attempt,
        },
    };
    let evidence = super::output::resolve_canonical_replay_tag(
        &claude.node,
        claude.scope(),
        &claude.boundary,
        &tag,
    )
    .await
    .unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].issuer, projection.issuer);
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
    use gents_loop::loop_stream::provider_view_tagged;
    use gents_loop::provider_input::ProviderInputProfile;
    use gents_protocol::message::AssistantContent;
    use gents_protocol::rendered_request::CaptureScopeKind;

    let fixture =
        fixture_with_provider_payload_and_scope_and_tool(true, CaptureScopeKind::OneShot, false)
            .await;
    assert_eq!(fixture.candidates().await.unwrap().len(), 1);
    let inference_scope = CanonicalReplayScope {
        expected_scope_kind: CaptureScopeKind::Inference,
        ..fixture.scope()
    };
    assert!(
        load_canonical_assistant_candidates(&fixture.node, inference_scope, &fixture.boundary,)
            .await
            .unwrap()
            .is_empty()
    );

    let history = super::output::load_sequenced_messages(
        &fixture.node,
        SESSION_ID,
        NODE_DID,
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

    let tagged = crate::provider_input::replay::tag_canonical_history(&history);
    let projected = provider_view_tagged(tagged).unwrap();
    let rows = projected
        .iter()
        .map(|row| {
            let Message::Assistant { id, content } = &row.message else {
                panic!("fixture history is one assistant")
            };
            TaggedAssistantRow {
                source: row.source.clone(),
                physical_header: row.physical_header.clone(),
                block_indices: row.block_indices.clone(),
                id: id.clone(),
                content: content.clone(),
            }
        })
        .collect::<Vec<_>>();
    let issuer = crate::llm::backend_client::claude_subscription_replay_issuer()
        .unwrap()
        .unwrap();
    let selected = select_replay(
        &rows,
        &issuer,
        ReplayWire::ClaudeMessages,
        |_| Vec::new(),
        &[],
    );
    let projected = [gents_loop::loop_stream::TaggedMessage {
        message: Message::Assistant {
            id: selected[0].id.clone(),
            content: selected[0].content.clone(),
        },
        source: selected[0].source.clone(),
        physical_header: selected[0].physical_header.clone(),
        block_indices: selected[0].block_indices.clone(),
    }];
    assert_eq!(projected.len(), 1);
    assert!(projected[0].source.is_none());
    assert!(matches!(&projected[0].message,
        Message::Assistant { content, .. }
        if matches!(content.as_slice(), [AssistantContent::Text(text)] if text.text == "answer")
    ));
}

#[tokio::test]
async fn replay_rejects_forged_high_water_and_storage_rejects_sequence_twins() {
    let observed = fixture().await;
    let mut forged = observed.boundary.clone();
    forged.canonical_through.as_mut().unwrap().commit_cid = "not-a-header-commit".into();
    let bad_high_water =
        load_canonical_assistant_candidates(&observed.node, observed.scope(), &forged)
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
    assert!(
        load_canonical_assistant_candidates(&observed.node, forged_scope, &forged_request)
            .await
            .is_err()
    );

    let mut twin = observed.header.clone();
    twin.message_key.push_str(":twin");
    let created = observed
        .node
        .execute_request(
            defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&twin).unwrap()),
        )
        .await;
    assert!(
        created.has_errors(),
        "a sequence slot cannot hold a second header"
    );
    assert!(created
        .errors
        .iter()
        .any(|error| error.message.contains("unique index")));
    assert!(observed.candidates().await.is_ok());
}

#[tokio::test]
async fn unverifiable_capture_drops_its_reasoning_without_failing_the_request() {
    use gents_protocol::message::AssistantContent;

    for storage in [CaptureStorage::Corrupt, CaptureStorage::MissingDeltaBase] {
        let fixture = signed_fixture().await;
        fixture
            .insert_capture_stored(RenderedRequestSource::ClaudeCliSubscription, storage)
            .await;
        let candidate = fixture.candidates().await.unwrap().remove(0);
        let tag = ReplayTag {
            request_doc_id: fixture.request_doc_id.clone(),
            source: OutputSource::ProviderTurn {
                scope: candidate.coordinate.scope,
                turn_index: candidate.coordinate.turn_index,
                attempt: candidate.coordinate.attempt,
            },
        };
        let evidence = super::output::resolve_canonical_replay_tag(
            &fixture.node,
            fixture.scope(),
            &fixture.boundary,
            &tag,
        )
        .await
        .expect("an unverifiable capture must not fail the request");
        assert!(evidence.is_empty());
        let Message::Assistant { id, content } = candidate.message.clone() else {
            panic!("canonical fixture must reconstruct an assistant");
        };
        let rows = vec![TaggedAssistantRow {
            source: Some(tag),
            physical_header: Some(candidate.header_doc_id.clone()),
            block_indices: (0..content.len()).collect(),
            id,
            content,
        }];
        let issuer = crate::llm::backend_client::claude_subscription_replay_issuer()
            .unwrap()
            .unwrap();
        let selected = select_replay(
            &rows,
            &issuer,
            ReplayWire::ClaudeMessages,
            |_| evidence.clone(),
            &[],
        );
        assert!(selected[0]
            .content
            .iter()
            .all(|block| !matches!(block, AssistantContent::Reasoning(_))));
        assert!(!selected[0].content.is_empty(), "ordinary blocks stay");
    }
}

#[derive(Clone, Copy, Debug)]
enum RequestFault {
    Missing,
    RowsReadFails,
    CommitsReadFails,
}

struct FaultingRequests<'a> {
    node: &'a EmbeddedNode,
    request_doc_id: &'a str,
    fault: Option<RequestFault>,
}

struct CountingRequests<'a> {
    node: &'a EmbeddedNode,
    rows: std::sync::atomic::AtomicUsize,
    commits: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl ReplayRequestReader for CountingRequests<'_> {
    async fn request_facts(
        &self,
        ids: &[String],
    ) -> anyhow::Result<std::collections::BTreeMap<String, super::output::ReplayRequestFacts>> {
        self.commits
            .fetch_add(ids.len(), std::sync::atomic::Ordering::Relaxed);
        self.rows
            .fetch_add(ids.len(), std::sync::atomic::Ordering::Relaxed);
        self.node.request_facts(ids).await
    }
}

#[tokio::test]
async fn replay_resolution_reads_shared_request_once_and_keeps_every_physical_candidate() {
    let mut fixture = signed_fixture().await;
    fixture
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await
        .unwrap();
    let mut duplicate = fixture.header.clone();
    duplicate.sequence += 1;
    duplicate.message_key.push_str("-physical-duplicate");
    let access = crate::config_client::ConfigAccess::local(fixture.node.clone());
    access
        .transact_write("test.replay.duplicate_header", |txn| {
            let duplicate = duplicate.clone();
            Box::pin(async move {
                txn.execute_with_variables(
                    CREATE_AGENT_MESSAGE_MUTATION,
                    &transcript_message_create_variables(&duplicate)?,
                )
                .await
            })
        })
        .await
        .unwrap();
    fixture.boundary = capture_source_boundary(
        &fixture.node,
        SESSION_ID,
        NODE_DID,
        None,
        &fixture.request_doc_id,
        &fixture.request_commit_cid,
    )
    .await
    .unwrap();
    let requests = CountingRequests {
        node: &fixture.node,
        rows: std::sync::atomic::AtomicUsize::new(0),
        commits: std::sync::atomic::AtomicUsize::new(0),
    };
    for resolution in 1..=2 {
        let (candidates, segment_scans) = crate::session::count_request_output_scans(
            super::output::load_canonical_assistant_candidates_with(
                &fixture.node,
                &requests,
                fixture.scope(),
                &fixture.boundary,
                None,
            ),
        )
        .await;
        let candidates = candidates.unwrap();
        assert_eq!(
            segment_scans, 1,
            "one pass scans a shared request's output once"
        );
        assert_eq!(candidates.len(), 2);
        assert_ne!(candidates[0].header_doc_id, candidates[1].header_doc_id);
        assert!(candidates.iter().all(|candidate| candidate.has_capture()));
        assert_eq!(
            requests.rows.load(std::sync::atomic::Ordering::Relaxed),
            resolution
        );
        assert_eq!(
            requests.commits.load(std::sync::atomic::Ordering::Relaxed),
            resolution
        );
    }

    let candidate = fixture.candidates().await.unwrap().remove(0);
    let tag = ReplayTag {
        request_doc_id: candidate.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: candidate.coordinate.scope,
            turn_index: candidate.coordinate.turn_index,
            attempt: candidate.coordinate.attempt,
        },
    };
    let selected = super::output::load_canonical_assistant_candidates_with(
        &fixture.node,
        &requests,
        fixture.scope(),
        &fixture.boundary,
        Some(std::slice::from_ref(&tag)),
    )
    .await
    .unwrap();
    assert_eq!(selected.len(), 2, "selection preserves physical twins");
    assert_ne!(selected[0].header_doc_id, selected[1].header_doc_id);
}

#[async_trait::async_trait]
impl ReplayRequestReader for FaultingRequests<'_> {
    async fn request_facts(
        &self,
        request_doc_ids: &[String],
    ) -> anyhow::Result<std::collections::BTreeMap<String, super::output::ReplayRequestFacts>> {
        if request_doc_ids.iter().any(|id| id == self.request_doc_id) {
            match self.fault {
                Some(RequestFault::RowsReadFails) => anyhow::bail!("injected AgentRequest read"),
                Some(RequestFault::CommitsReadFails) => {
                    anyhow::bail!("injected composite commit read")
                }
                _ => {}
            }
        }
        let mut facts = self.node.request_facts(request_doc_ids).await?;
        if matches!(self.fault, Some(RequestFault::Missing)) {
            if let Some(request) = facts.get_mut(self.request_doc_id) {
                request.rows.clear();
            }
        }
        Ok(facts)
    }
}

async fn load_with_fault(
    fixture: &ReplayFixture,
    scope: CanonicalReplayScope<'_>,
    boundary: &SourceBoundary,
    fault: Option<RequestFault>,
    selected_tags: Option<&[ReplayTag]>,
) -> anyhow::Result<Vec<super::output::CanonicalAssistantCandidate>> {
    let requests = FaultingRequests {
        node: &fixture.node,
        request_doc_id: &fixture.request_doc_id,
        fault,
    };
    super::output::load_canonical_assistant_candidates_with(
        &fixture.node,
        &requests,
        scope,
        boundary,
        selected_tags,
    )
    .await
}

#[tokio::test]
async fn historical_request_store_faults_propagate_instead_of_dropping_reasoning() {
    let fixture = signed_fixture().await;
    assert!(fixture
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await
        .is_some());
    let now = chrono::Utc::now().to_rfc3339();
    let created = fixture
        .node
        .execute(&format!(
            r#"mutation {{ create_AgentRequest(input: {{
                request_id: "replay-request-next", purpose: "normal", node_did: "{}",
                agent_id: "general", session_id: "{}", content: "next",
                lifecycle_state: "pending", execution_origin: "interactive",
                created_at: "{}", retry_count: 0, max_retries: 3, request_hop: 0
            }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(NODE_DID),
            crate::graphql::escape_graphql_string(SESSION_ID),
            crate::graphql::escape_graphql_string(&now),
        ))
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let current_doc_id = crate::graphql::single_mutation_document(&created, "create_AgentRequest")
        .unwrap()
        .unwrap()["_docID"]
        .as_str()
        .unwrap()
        .to_string();
    let current_commit_cid = crate::graphql::newest_document_composite_commit(
        &fixture.node,
        &current_doc_id,
        "replay fixture next request",
    )
    .await
    .unwrap()
    .unwrap()
    .cid;
    let boundary = capture_source_boundary(
        &fixture.node,
        SESSION_ID,
        NODE_DID,
        None,
        &current_doc_id,
        &current_commit_cid,
    )
    .await
    .unwrap();
    let scope = CanonicalReplayScope {
        request_id: "replay-request-next",
        request_doc_id: &current_doc_id,
        request_commit_cid: &current_commit_cid,
        ..fixture.scope()
    };
    let verified = load_with_fault(&fixture, scope, &boundary, None, None)
        .await
        .unwrap();
    assert_eq!(verified.len(), 1);
    assert_eq!(verified[0].request_doc_id, fixture.request_doc_id);
    assert!(verified[0].has_capture(), "the healthy turn replays");

    let candidate = &verified[0];
    let selected_tag = ReplayTag {
        request_doc_id: candidate.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: candidate.coordinate.scope,
            turn_index: candidate.coordinate.turn_index,
            attempt: candidate.coordinate.attempt,
        },
    };
    let OutputSource::ProviderTurn {
        scope: tag_scope,
        turn_index,
        attempt,
    } = &selected_tag.source
    else {
        unreachable!("selected replay tags are provider-owned")
    };
    let unselected_tag = ReplayTag {
        request_doc_id: selected_tag.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: *tag_scope,
            turn_index: *turn_index,
            attempt: *attempt + 1,
        },
    };
    assert!(
        load_with_fault(
            &fixture,
            scope,
            &boundary,
            Some(RequestFault::RowsReadFails),
            Some(std::slice::from_ref(&unselected_tag)),
        )
        .await
        .unwrap()
        .is_empty(),
        "an unselected historical request needs no physical witness"
    );
    assert!(
        load_with_fault(
            &fixture,
            scope,
            &boundary,
            Some(RequestFault::RowsReadFails),
            Some(std::slice::from_ref(&selected_tag)),
        )
        .await
        .is_err(),
        "selected evidence still validates its physical request"
    );

    assert!(
        load_with_fault(
            &fixture,
            scope,
            &boundary,
            Some(RequestFault::Missing),
            None
        )
        .await
        .unwrap()
        .is_empty(),
        "a verifiably missing request drops only its turn"
    );

    for fault in [RequestFault::RowsReadFails, RequestFault::CommitsReadFails] {
        let error = load_with_fault(&fixture, scope, &boundary, Some(fault), None)
            .await
            .err()
            .unwrap_or_else(|| panic!("{fault:?} must fail the lookup"));
        assert!(
            error
                .downcast_ref::<gents_loop::loop_stream::ReplayEvidenceViolation>()
                .is_none(),
            "{fault:?} is not a replay violation: {}",
            ErrorChain(error.as_ref())
        );
    }
}

async fn stored_agent_request(fixture: &ReplayFixture) -> crate::watcher::AgentRequest {
    let response = fixture
        .node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            crate::graphql::escape_graphql_string(&fixture.request_doc_id),
            crate::watcher::AGENT_REQUEST_FIELDS,
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row = response.data.unwrap()["AgentRequest"][0].clone();
    crate::watcher::AgentRequest::try_from(
        serde_json::from_value::<gents_protocol::row::AgentRequestRow>(row).unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn replay_resolution_observes_capture_arriving_for_existing_header() {
    let fixture = signed_fixture().await;
    let candidate = fixture.candidates().await.unwrap().remove(0);
    let tag = ReplayTag {
        request_doc_id: candidate.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: candidate.coordinate.scope,
            turn_index: candidate.coordinate.turn_index,
            attempt: candidate.coordinate.attempt,
        },
    };
    let replay = crate::provider_input::replay::owned_replay_input(
        fixture.node.clone(),
        stored_agent_request(&fixture).await,
        fixture.request_commit_cid.clone(),
        fixture.scope_kind,
        None,
        crate::provider_input::ProviderInputProfile::ClaudeMessages,
    );
    let resolve = replay.resolve.expect("canonical resolver");
    assert!(resolve(vec![tag.clone()]).await.unwrap().is_empty());
    fixture
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await
        .unwrap();
    assert_eq!(
        resolve(vec![tag]).await.unwrap().len(),
        1,
        "a capture arriving for the same physical header must become replay evidence"
    );
}

#[tokio::test]
async fn replay_resolution_observes_a_header_replicated_before_dispatch() {
    let fixture = signed_fixture().await;
    fixture
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await
        .unwrap();
    let candidate = fixture.candidates().await.unwrap().remove(0);
    let tag = ReplayTag {
        request_doc_id: candidate.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: candidate.coordinate.scope,
            turn_index: candidate.coordinate.turn_index,
            attempt: candidate.coordinate.attempt,
        },
    };
    let replay = crate::provider_input::replay::owned_replay_input(
        fixture.node.clone(),
        stored_agent_request(&fixture).await,
        fixture.request_commit_cid.clone(),
        fixture.scope_kind,
        None,
        crate::provider_input::ProviderInputProfile::ClaudeMessages,
    );
    let resolve = replay.resolve.expect("canonical resolver");
    let estimate = resolve(vec![tag.clone()]).await.unwrap();
    assert_eq!(estimate.len(), 1);
    let (unchanged, scans) =
        crate::session::count_request_output_scans(resolve(vec![tag.clone()])).await;
    assert_eq!(
        unchanged.unwrap().len(),
        1,
        "an unchanged view resolves the same evidence"
    );
    assert_eq!(
        scans, 1,
        "each dispatch lookup revalidates canonical evidence"
    );
    assert_eq!(resolve(vec![tag.clone()]).await.unwrap().len(), 1);

    let mut duplicate = fixture.header.clone();
    duplicate.sequence += 1;
    duplicate.message_key.push_str("-replicated-twin");
    let created = fixture
        .node
        .execute_request(
            defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&duplicate).unwrap()),
        )
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);

    let dispatch = resolve(vec![tag]).await.unwrap();
    assert_eq!(
        dispatch.len(),
        2,
        "the dispatch lookup sees the replicated physical twin"
    );
    assert_ne!(
        dispatch[0].evidence.physical_header,
        dispatch[1].evidence.physical_header
    );
}

#[tokio::test]
async fn the_dispatch_after_an_admission_estimate_serves_its_resolution_without_rereading() {
    let fixture = signed_fixture().await;
    fixture
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await
        .unwrap();
    let candidate = fixture.candidates().await.unwrap().remove(0);
    let tag = ReplayTag {
        request_doc_id: candidate.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: candidate.coordinate.scope,
            turn_index: candidate.coordinate.turn_index,
            attempt: candidate.coordinate.attempt,
        },
    };
    let replay = crate::provider_input::replay::owned_replay_input(
        fixture.node.clone(),
        stored_agent_request(&fixture).await,
        fixture.request_commit_cid.clone(),
        fixture.scope_kind,
        None,
        crate::provider_input::ProviderInputProfile::ClaudeMessages,
    );
    let resolve = replay.resolve.clone().expect("canonical resolver");
    let (estimate, estimate_scans) = crate::session::count_request_output_scans(async {
        let _estimate_seed = replay.arm_estimate_seed();
        resolve(vec![tag.clone()]).await.unwrap()
    })
    .await;
    assert_eq!(estimate.len(), 1);
    assert_eq!(
        estimate_scans, 1,
        "the admission estimate performs its own canonical pass"
    );

    let (dispatch, dispatch_scans) = crate::session::count_request_output_scans(
        gents_loop::loop_stream::resolve_replay_evidence(&replay, std::slice::from_ref(&tag)),
    )
    .await;
    assert_eq!(
        dispatch.as_ref().unwrap(),
        &estimate,
        "the immediate dispatch serves the estimate's resolution unchanged"
    );
    assert_eq!(
        dispatch_scans, 0,
        "the immediately following dispatch rereads no canonical evidence"
    );

    let (next, next_scans) = crate::session::count_request_output_scans(
        gents_loop::loop_stream::resolve_replay_evidence(&replay, std::slice::from_ref(&tag)),
    )
    .await;
    assert_eq!(next.unwrap(), estimate);
    assert_eq!(
        next_scans, 1,
        "every lookup after the seeded dispatch revalidates the store"
    );
}

#[tokio::test]
async fn a_header_twin_arriving_between_the_admission_estimate_and_dispatch_waits_for_the_next_lookup(
) {
    let fixture = signed_fixture().await;
    fixture
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await
        .unwrap();
    let candidate = fixture.candidates().await.unwrap().remove(0);
    let tag = ReplayTag {
        request_doc_id: candidate.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: candidate.coordinate.scope,
            turn_index: candidate.coordinate.turn_index,
            attempt: candidate.coordinate.attempt,
        },
    };
    let replay = crate::provider_input::replay::owned_replay_input(
        fixture.node.clone(),
        stored_agent_request(&fixture).await,
        fixture.request_commit_cid.clone(),
        fixture.scope_kind,
        None,
        crate::provider_input::ProviderInputProfile::ClaudeMessages,
    );
    let resolve = replay.resolve.clone().expect("canonical resolver");
    let estimate = {
        let _estimate_seed = replay.arm_estimate_seed();
        resolve(vec![tag.clone()]).await.unwrap()
    };
    assert_eq!(estimate.len(), 1);

    let mut duplicate = fixture.header.clone();
    duplicate.sequence += 1;
    duplicate.message_key.push_str("-estimate-twin");
    let created = fixture
        .node
        .execute_request(
            defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&duplicate).unwrap()),
        )
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);

    let dispatch =
        gents_loop::loop_stream::resolve_replay_evidence(&replay, std::slice::from_ref(&tag))
            .await
            .unwrap();
    assert_eq!(
        dispatch, estimate,
        "the seeded dispatch answers the estimate's window, not the replicated twin"
    );
    let observed =
        gents_loop::loop_stream::resolve_replay_evidence(&replay, std::slice::from_ref(&tag))
            .await
            .unwrap();
    assert_eq!(
        observed.len(),
        2,
        "the next resolution observes the replicated physical twin"
    );
}

#[tokio::test]
async fn a_pass_with_many_selected_turns_batches_capture_rows_and_block_commits() {
    let turns = 4;
    let mut fixture = signed_fixture().await;
    fixture
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await
        .unwrap();
    for turn in 1..turns {
        let body = serde_json::json!({
            "model": "claude-test",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "prompt"}]},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": format!("think-{turn}"), "signature": format!("sig-{turn}")},
                    {"type": "text", "text": format!("answer-{turn}")}
                ]},
                {"role": "user", "content": [{"type": "text", "text": "x".repeat(16 * 1024)}]}
            ]
        });
        fixture.insert_provider_turn(turn, body).await;
    }
    let candidates = fixture.candidates().await.unwrap();
    assert_eq!(candidates.len() as u32, turns);
    let tag_for = |candidate: &super::output::CanonicalAssistantCandidate| ReplayTag {
        request_doc_id: candidate.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: candidate.coordinate.scope,
            turn_index: candidate.coordinate.turn_index,
            attempt: candidate.coordinate.attempt,
        },
    };
    let tags = candidates.iter().map(tag_for).collect::<Vec<_>>();

    let capture_keys = candidates
        .iter()
        .map(|candidate| {
            gents_loop::rendered_request::capture_key(
                NODE_DID,
                SESSION_ID,
                &candidate.request_doc_id,
                &candidate.coordinate.scope.to_string(),
                usize::try_from(candidate.coordinate.turn_index).unwrap(),
                candidate.coordinate.attempt,
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let filter = capture_keys
        .iter()
        .map(|key| format!("\"{}\"", crate::graphql::escape_graphql_string(key)))
        .collect::<Vec<_>>()
        .join(", ");
    let response = fixture
        .node
        .execute(&format!(
            r#"{{ RenderedRequest(filter: {{ capture_key: {{ _in: [{filter}] }} }}) {{ capture_version request_json }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let mut roots = std::collections::BTreeSet::new();
    let mut block_docs = std::collections::BTreeSet::new();
    for row in response.data.unwrap()["RenderedRequest"]
        .as_array()
        .unwrap()
    {
        let version = row["capture_version"].as_u64().unwrap() as u32;
        let record = crate::rendered_request::encoding::decode_capture_record(
            version,
            row["request_json"].as_str().unwrap(),
            crate::rendered_request::CapturePayloadKind::RequestBody,
        )
        .unwrap();
        if let crate::rendered_request::encoding::DecodedRecord::Tree(tree) = &record {
            assert!(
                tree.height <= 1,
                "fixture has at most one internal manifest level"
            );
            roots.insert(tree.root.doc_id.clone());
        }
        for entry in crate::rendered_request::manifest_leaf_entries(fixture.node.as_ref(), record)
            .await
            .unwrap()
        {
            if !roots.contains(&entry.doc_id) {
                block_docs.insert(entry.doc_id);
            }
        }
    }
    assert!(
        block_docs.len() + roots.len() >= turns as usize,
        "the pass has real witnesses to batch"
    );
    let expected_commit_statements = [roots.len(), block_docs.len()]
        .into_iter()
        .map(|count| count.div_ceil(crate::rendered_request::commits::FIELD_COMMIT_ALIAS_BATCH))
        .sum::<usize>();

    let ((batched, commit_statements), capture_queries) =
        crate::session::count_capture_row_queries(
            crate::rendered_request::commits::count_field_commit_statements(
                super::output::resolve_canonical_replay_tags(
                    &fixture.node,
                    fixture.scope(),
                    &fixture.boundary,
                    &tags,
                ),
            ),
        )
        .await;
    let batched = batched.unwrap();
    assert_eq!(
        batched.len(),
        tags.len(),
        "every requested tag answers once, in order"
    );
    assert_eq!(
        capture_queries, 1,
        "one chunked capture-row statement covers every selected turn"
    );
    assert_eq!(
        commit_statements, expected_commit_statements,
        "field-commit witnesses batch across every manifest of the pass"
    );
    for (index, tag) in tags.iter().enumerate() {
        let singleton = super::output::resolve_canonical_replay_tag(
            &fixture.node,
            fixture.scope(),
            &fixture.boundary,
            tag,
        )
        .await
        .unwrap();
        assert_eq!(batched[index].0, *tag);
        assert_eq!(
            batched[index].1, singleton,
            "the batched pass answers exactly the per-tag singleton owner"
        );
    }
}

#[tokio::test]
async fn a_capture_pinning_a_block_that_does_not_hash_drops_only_its_own_turn() {
    let turns = 3u32;
    let corrupt_turn = turns - 1;
    let marker = "corrupt-block-payload-marker";
    let mut fixture = signed_fixture().await;
    fixture
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await
        .unwrap();
    fixture
        .insert_provider_turn(
            1,
            serde_json::json!({
                "model": "claude-test",
                "messages": [
                    {"role": "user", "content": [{"type": "text", "text": "prompt"}]},
                    {"role": "assistant", "content": [
                        {"type": "thinking", "thinking": "think-0", "signature": "sig-0"},
                        {"type": "text", "text": "answer-0"}
                    ]},
                    {"role": "user", "content": [{"type": "text", "text": "x".repeat(16 * 1024)}]}
                ]
            }),
        )
        .await;
    let body = serde_json::json!({
        "model": "claude-test",
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "prompt"}]},
            {"role": "user", "content": [
                {"type": "text", "text": format!("{marker}{}", "z".repeat(16 * 1024))}
            ]}
        ]
    });
    let layout = crate::rendered_request::encoding::capture_block_layout(&body).unwrap();
    let chunks = crate::rendered_request::encoding::chunk_capture_layout(&layout);
    let index = chunks
        .iter()
        .position(|chunk| {
            std::str::from_utf8(&chunk.bytes)
                .expect("canonical block bytes are UTF-8")
                .contains(marker)
        })
        .expect("the marker lands in a block");
    let mut poisoned = chunks[index].bytes.clone();
    let at = std::str::from_utf8(&poisoned)
        .expect("canonical block bytes are UTF-8")
        .find(marker)
        .expect("the marker is in this block");
    poisoned[at] = b'k';
    assert_ne!(
        crate::rendered_request::encoding::block_content_key(&poisoned),
        chunks[index].content_key
    );
    let created = fixture
        .node
        .execute_request(
            defra_node::QueryRequest::new(
                "mutation($input: RenderedRequestBlockMutationInputArg!) { \
                 create_RenderedRequestBlock(input: $input) { _docID } }",
            )
            .with_variables(serde_json::json!({ "input": {
                "content_key": chunks[index].content_key,
                "payload": String::from_utf8(poisoned.clone()).unwrap(),
                "byte_len": chunks[index].bytes.len(),
                "created_at": chrono::Utc::now().to_rfc3339(),
            }})),
        )
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let poisoned_doc =
        crate::graphql::single_mutation_document(&created, "create_RenderedRequestBlock")
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_string();

    fixture.insert_provider_turn(corrupt_turn, body).await;

    let response = fixture
        .node
        .execute(r#"{ RenderedRequest { capture_version request_json } }"#)
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let mut referencing = 0usize;
    for row in response.data.unwrap()["RenderedRequest"]
        .as_array()
        .unwrap()
    {
        let version = row["capture_version"].as_u64().unwrap() as u32;
        let stored = row["request_json"].as_str().unwrap();
        let mut pins_poison = false;
        for kind in [
            crate::rendered_request::CapturePayloadKind::RequestBody,
            crate::rendered_request::CapturePayloadKind::ProvenancePayload,
        ] {
            let record =
                crate::rendered_request::encoding::decode_capture_record(version, stored, kind)
                    .unwrap();
            let entries =
                crate::rendered_request::manifest_leaf_entries(fixture.node.as_ref(), record)
                    .await
                    .unwrap();
            pins_poison |= entries.iter().any(|entry| entry.doc_id == poisoned_doc);
        }
        referencing += pins_poison as usize;
    }
    assert_eq!(
        referencing, 1,
        "only the corrupt turn pins the poisoned block"
    );

    let candidates = fixture
        .candidates()
        .await
        .expect("a corrupt capture must degrade only its own turn");
    assert_eq!(candidates.len() as u32, turns);
    let tags = candidates
        .iter()
        .map(|candidate| ReplayTag {
            request_doc_id: candidate.request_doc_id.clone(),
            source: OutputSource::ProviderTurn {
                scope: candidate.coordinate.scope,
                turn_index: candidate.coordinate.turn_index,
                attempt: candidate.coordinate.attempt,
            },
        })
        .collect::<Vec<_>>();
    let resolved = super::output::resolve_canonical_replay_tags(
        &fixture.node,
        fixture.scope(),
        &fixture.boundary,
        &tags,
    )
    .await
    .expect("a corrupt capture must degrade only its own turn");
    assert_eq!(resolved.len(), tags.len());
    for (tag, evidence) in &resolved {
        let OutputSource::ProviderTurn { turn_index, .. } = tag.source else {
            unreachable!("tags are provider turns");
        };
        if turn_index == corrupt_turn {
            assert!(evidence.is_empty(), "the corrupt turn is non-replayable");
        } else {
            assert_eq!(
                evidence.len(),
                1,
                "the healthy turn {turn_index} keeps its evidence"
            );
            assert!(
                evidence[0].captured.is_some(),
                "the healthy turn {turn_index} still replays its captured body"
            );
        }
    }
}
