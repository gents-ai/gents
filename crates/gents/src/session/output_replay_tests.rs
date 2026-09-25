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
use super::output::{load_canonical_assistant_candidates, CanonicalReplayScope};
use crate::provider_context_reduction::{capture_source_boundary, SourceBoundary};

use gents_loop::claude_messages_body::{
    restore_historical_reasoning_suffix, ReplayCheckpoint, ReplayIssuer, ReplayTag, ReplayWire,
    TaggedAssistantRow,
};
use gents_loop::loop_stream::ReplayProjectionContext;

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
        load_canonical_assistant_candidates(&self.node, self.scope(), &self.boundary).await
    }

    async fn insert_capture(
        &self,
        source: RenderedRequestSource,
    ) -> Option<ReplayProjectionContext> {
        use crate::rendered_request::{
            build_rendered_completion_request, AssemblyBuildPath, AssemblyTrace,
            DefraRenderedRequestSink, RenderedRequestComponents, RenderedRequestContext,
        };

        let capture_scope = format!("{}.1", self.scope_kind);
        let (uri, family, body, wire) = match source {
            RenderedRequestSource::ClaudeCliSubscription => (
                crate::claude_messages::MESSAGES_URI,
                gents_loop::backend_provider::BackendProviderKind::ClaudeCliSubscription.as_str(),
                serde_json::json!({
                    "model": "claude-test", "system": [{"type":"text","text":"system"}],
                    "messages": [{"role":"user","content":[{"type":"text","text":"prompt"}]}],
                    "tools": [], "stream": true,
                }),
                Some(ReplayWire::ClaudeMessages),
            ),
            RenderedRequestSource::OpenAiChatCompletions => (
                "https://api.openai.com/v1/chat/completions",
                gents_loop::backend_provider::BackendProviderKind::OpenAiCompatible.as_str(),
                serde_json::json!({"model":"test-model","messages":[{"role":"user","content":"prompt"}]}),
                None,
            ),
            RenderedRequestSource::OpenAiResponses => (
                "https://api.openai.com/v1/responses",
                gents_loop::backend_provider::BackendProviderKind::OpenAiCompatible.as_str(),
                serde_json::json!({"model":"test-model","input":[{"role":"user","content":"prompt"}]}),
                Some(ReplayWire::Responses),
            ),
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
                agent_did: AGENT_DID.into(),
                requester_did: String::new(),
                behavior_id: "general".into(),
                session_id: SESSION_ID.into(),
                model_name: "test-model".into(),
                provider_family: Some(family.into()),
            },
            &capture_scope,
            source,
            Some(endpoint),
            Some(route_path_sha256),
            0,
            0,
            trace,
            RenderedRequestComponents::from_provider_body(body.clone(), source),
            None,
        )
        .expect("canonical capture fixture");
        DefraRenderedRequestSink::new(self.node.clone())
            .capture(rendered)
            .await
            .expect("durable capture owner");
        wire.map(|wire| ReplayProjectionContext { issuer, wire, body })
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
        &projection,
    )
    .await
    .unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(
        evidence[0].origin,
        gents_loop::claude_messages_body::ReplayOrigin::AcceptedProvider
    );
    assert_eq!(evidence[0].reasoning.len(), 2);

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
        &projection,
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
        &projection,
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
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    let created = node
        .execute(&format!(
            r#"mutation {{ create_AgentRequest(input: {{
                request_id: "{}", purpose: "normal", agent_did: "{}",
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
    let checkpoint = ReplayCheckpoint {
        required: vec![],
        prefix_rows: vec![],
        retained: vec![TaggedAssistantRow {
            source: Some(tag.clone()),
            physical_header: Some(candidate.header_doc_id.clone()),
            block_indices: (0..content.len()).collect(),
            id: id.clone(),
            content: content.clone(),
        }],
        retired: vec![],
    };
    let evidence = super::output::resolve_canonical_replay_tag(
        &fixture.node,
        fixture.scope(),
        &fixture.boundary,
        &tag,
        &projection,
    )
    .await
    .unwrap();
    assert_eq!(evidence.len(), 1);
    assert!(evidence[0].prefix_compatible);
    assert_eq!(evidence[0].reasoning.len(), 2);
    let restored = restore_historical_reasoning_suffix(
        &checkpoint,
        &projection.issuer,
        projection.wire,
        |_| evidence.clone(),
    );
    assert_eq!(restored[0].content, content);
    assert_eq!(restored[0].block_indices, vec![0, 1, 2, 3]);

    let mut changed = projection.clone();
    changed.body["system"] = serde_json::json!([{"type":"text","text":"changed system"}]);
    let rejected = super::output::resolve_canonical_replay_tag(
        &fixture.node,
        fixture.scope(),
        &fixture.boundary,
        &tag,
        &changed,
    )
    .await
    .unwrap();
    assert_eq!(rejected.len(), 1);
    assert!(!rejected[0].prefix_compatible);
    let stripped = restore_historical_reasoning_suffix(
        &checkpoint,
        &projection.issuer,
        projection.wire,
        |_| rejected.clone(),
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
    let projection = ReplayProjectionContext {
        issuer: ReplayIssuer {
            family: gents_loop::backend_provider::BackendProviderKind::ClaudeCliSubscription
                .as_str()
                .into(),
            endpoint: "not-a-captured-route".into(),
        },
        wire: ReplayWire::ClaudeMessages,
        body: serde_json::json!({"messages":[]}),
    };
    assert!(super::output::resolve_canonical_replay_tag(
        &missing.node,
        missing.scope(),
        &missing.boundary,
        &tag,
        &projection,
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
        &projection,
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
        &projection,
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
    use gents_loop::loop_stream::{narrow_tagged_history, provider_view_tagged, LoopReplayInput};
    use gents_loop::provider_input::ProviderInputProfile;
    use gents_protocol::message::AssistantContent;
    use gents_protocol::rendered_request::CaptureScopeKind;

    // This is a complete, physically published provider assistant, not a
    // body-only lookalike. Its OneShot close is invalid for the Inference
    // resolver, so historical reasoning cannot acquire that provenance.
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
    let mut projected = provider_view_tagged(ProviderInputProfile::ClaudeMessages, tagged).unwrap();
    narrow_tagged_history(
        ProviderInputProfile::ClaudeMessages,
        &mut projected,
        &mut replay,
        None,
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
        .execute_request_with_retry(
            defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&twin).unwrap()),
            defra_node::ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    assert!(observed.candidates().await.is_err());
}
