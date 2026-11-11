use super::provider_context_reduction::capture_source_boundary;
use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use gents_protocol::message::Message;
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, ReasoningPart, SegmentRun,
    SourceClose, StreamDeclaration, StreamPayload, TranscriptMessage,
};
use gents_protocol::rendered_request::RenderedRequestSource;
use gents_store::identity::NodeIdentity;

use super::output::replay_candidates::load_canonical_assistant_candidates;
use super::output::replay_request::ReplayRequestReader;
use super::output::CanonicalReplayScope;

use gents_protocol::output::rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::transcript::SourceBoundary;

use gents_loop::claude_messages_body::{
    replay_stage, select_replay, ReplayIssuer, ReplayTag, ReplayWire, TaggedAssistantRow,
};
use gents_loop::provider_input::replay_frontier::flatten;

#[derive(Clone, Copy, Debug)]
enum RequestFault {
    Missing,
    RowsReadFails,
    CommitsReadFails,
}

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

const SESSION_ID: &str = "replay-session";

const REQUEST_ID: &str = "replay-request";

struct ReplayFixture {
    node: Arc<EmbeddedNode>,
    _db: gents_inference::test_support::TestDb,
    user: gents_store::identity::KeyIdentity,
    user_access: gents_store::config_client::ConfigAccess,
    agent: Arc<dyn gents_store::identity::NodeIdentity>,
    access: gents_store::config_client::ConfigAccess,
    node_did: String,
    requester_did: String,
    request_doc_id: String,
    request_commit_cid: String,
    header_doc_id: String,
    header: TranscriptMessage,
    boundary: SourceBoundary,
    scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
}

async fn fixture_write(
    access: &gents_store::config_client::ConfigAccess,
    mutation: &str,
    variables: &serde_json::Value,
) -> Result<serde_json::Value, gents_store::config_client::ConfigClientError> {
    access
        .transact_write("test.replay_fixture", |txn| {
            Box::pin(async move { txn.execute_with_variables(mutation, variables).await })
        })
        .await
}

async fn fixture_query(
    access: &gents_store::config_client::ConfigAccess,
    document: &str,
) -> query::QueryResponse {
    let value = if document.trim_start().starts_with("mutation") {
        access.write("test.replay_query", document).await
    } else {
        access.execute(document).await
    }
    .unwrap();
    serde_json::from_value(value).unwrap()
}

impl ReplayFixture {
    fn scope(&self) -> CanonicalReplayScope<'_> {
        CanonicalReplayScope {
            node_did: &self.node_did,
            requester_did: Some(&self.requester_did),
            session_id: SESSION_ID,
            request_id: REQUEST_ID,
            request_doc_id: &self.request_doc_id,
            request_commit_cid: &self.request_commit_cid,
            expected_scope_kind: self.scope_kind,
        }
    }

    async fn candidates(&self) -> anyhow::Result<Vec<super::output::CanonicalAssistantCandidate>> {
        load_canonical_assistant_candidates(&self.access, self.scope(), &self.boundary).await
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
        use gents_inference::rendered_request::{
            build_rendered_completion_request, AssemblyBuildPath, AssemblyTrace,
            DefraRenderedRequestSink, RenderedRequestComponents, RenderedRequestContext,
        };

        let capture_scope = format!("{}.1", self.scope_kind);
        let uri = match source {
            RenderedRequestSource::ClaudeCliSubscription => {
                gents_inference::clients::claude_messages::MESSAGES_URI
            }
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
        let route_path_sha256 = gents_loop::rendered_request::transport::provider_route_path_sha256(
            destination.path(),
            destination.query(),
        );
        let issuer = gents_loop::rendered_request::transport::replay_issuer_for_destination(
            family,
            &destination,
        )
        .expect("fixture route names a replay issuer");
        let trace = AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, Vec::new());
        let rendered = build_rendered_completion_request(
            &RenderedRequestContext {
                request_doc_id: self.request_doc_id.clone(),
                request_commit_cid: self.request_commit_cid.clone(),
                request_id: REQUEST_ID.into(),
                node_did: self.node_did.clone(),
                requester_did: self.requester_did.clone(),
                agent_did: self.agent.did().to_owned(),
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
        let sink =
            DefraRenderedRequestSink::new_managed(self.node.clone(), self.agent.as_ref()).unwrap();
        match storage {
            CaptureStorage::Exact => sink.capture(rendered).await.expect("durable capture owner"),
            CaptureStorage::Corrupt => sink
                .create_stored_for_test(&rendered, "not a capture record")
                .await
                .expect("corrupt capture row"),
            CaptureStorage::MissingDeltaBase => {
                use gents_inference::rendered_request::encoding::{
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
                        node_did: self.node_did.clone(),
                        requester_did: self.requester_did.clone(),
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
            node_did: self.node_did.clone(),
            requester_did: Some(self.requester_did.clone()),
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
        let created = fixture_write(
            &self.access,
            CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
            &output_segment_create_variables(&segment).unwrap(),
        )
        .await
        .unwrap();
        let close_doc_id =
            gents_store::graphql::created_doc_id(&created, "AgentOutputSegment").unwrap();
        let header = TranscriptMessage {
            native_request_id: None,
            message_key: format!(
                "provider:{}:{}",
                self.request_doc_id,
                serde_json::to_string(&source).unwrap()
            ),
            session_id: SESSION_ID.into(),
            node_did: self.node_did.clone(),
            requester_did: Some(self.requester_did.clone()),
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
        fixture_write(
            &self.access,
            CREATE_AGENT_MESSAGE_MUTATION,
            &transcript_message_create_variables(&header).unwrap(),
        )
        .await
        .unwrap();
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
            &self.access,
            SESSION_ID,
            &self.node_did,
            Some(&self.requester_did),
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
            node_did: self.node_did.clone(),
            requester_did: Some(self.requester_did.clone()),
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
        let created = fixture_write(
            &self.access,
            CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
            &output_segment_create_variables(&segment).unwrap(),
        )
        .await
        .unwrap();
        let close_doc_id =
            gents_store::graphql::created_doc_id(&created, "AgentOutputSegment").unwrap();
        let header = TranscriptMessage {
            native_request_id: None,
            message_key: gents_protocol::output::rows::authored_message_key(
                &self.request_doc_id,
                "assistant-note",
            ),
            session_id: SESSION_ID.into(),
            node_did: self.node_did.clone(),
            requester_did: Some(self.requester_did.clone()),
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
        fixture_write(
            &self.access,
            CREATE_AGENT_MESSAGE_MUTATION,
            &transcript_message_create_variables(&header).unwrap(),
        )
        .await
        .unwrap();
        self.boundary = capture_source_boundary(
            &self.access,
            SESSION_ID,
            &self.node_did,
            Some(&self.requester_did),
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
    let evidence = super::output::replay_tags::resolve_canonical_replay_tag(
        &fixture.access,
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
    let batched = super::output::replay_tags::resolve_canonical_replay_tags(
        &fixture.access,
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
    let without_scope = super::output::replay_tags::resolve_canonical_replay_tag(
        &fixture.access,
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
    fixture_with_provider_payload_options(signed, scope_kind, with_tool, false).await
}

async fn fixture_with_provider_payload_options(
    signed: bool,
    scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
    with_tool: bool,
    recovered: bool,
) -> ReplayFixture {
    use crate::request::lifecycle::{RequestIdentity, RequestSigner, RequestSpec};
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    let (db, user, user_access, binding) =
        gents_inference::test_support::signed_test_node("canonical-replay").await;
    let node = db.node.clone();
    let node_did = db.node_identity.did().to_owned();
    let requester_did = user.did().to_owned();
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
    let agent_did = ::identity::Did::new(agent.did()).unwrap();
    let admin = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        ::identity::Did::new(&node_did).unwrap(),
    );
    for permission in [
        gents_store::config_client::NodePermission::CollectionGet,
        gents_store::config_client::NodePermission::DocumentRead,
        gents_store::config_client::NodePermission::DocumentUpdate,
        gents_store::config_client::NodePermission::DocumentDelete,
        gents_store::config_client::NodePermission::DacRelationAdd,
        gents_store::config_client::NodePermission::DacRelationDelete,
    ] {
        admin
            .grant_node_permission(&agent_did, permission)
            .await
            .unwrap();
    }
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        agent_did,
        agent.as_ref(),
    )
    .unwrap();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let create = crate::request::lifecycle::build_signed_client_request(
        &user_access,
        RequestSpec::new(
            RequestPurpose::Normal,
            RequestIdentity {
                request_id: REQUEST_ID.into(),
                node_did: node_did.clone(),
                requester_did: Some(requester_did.clone()),
                agent_did: agent.did().to_owned(),
                session_id: SESSION_ID.into(),
                content: "prompt".into(),
                execution_origin: crate::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: now.clone(),
            },
            AgentRequestAdmissionRecord::client(&requester_did),
        ),
        RequestSigner::Identity(&user),
    )
    .await
    .unwrap();
    crate::request::lifecycle::submit_client_request(&user_access, &create)
        .await
        .unwrap();
    let response = user_access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{_docID}} }}"#,
            gents_store::graphql::escape_graphql_string(REQUEST_ID),
        ))
        .await
        .unwrap();
    let request_doc_id = response["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    let request_commit_cid = access
        .transact_readonly("test.replay_request_commit", |txn| {
            let id = &request_doc_id;
            Box::pin(async move {
                gents_store::graphql::newest_document_composite_commit_in_txn(
                    txn,
                    id,
                    "test.replay_request_commit",
                )
                .await
            })
        })
        .await
        .unwrap()
        .unwrap()
        .cid;

    let source = OutputSource::ProviderTurn {
        scope: format!("{scope_kind}.1").parse().unwrap(),
        turn_index: 0,
        attempt: 0,
    };
    let (runs, payload, stream_bytes) = if recovered {
        let declaration = |stream, bytes, block_index, payload| SegmentRun {
            stream,
            bytes,
            declaration: Some(StreamDeclaration {
                block_index,
                part_index: 0,
                payload,
            }),
        };
        (
            vec![
                declaration(0, 1, 0, StreamPayload::Reasoning),
                declaration(1, 1, 0, StreamPayload::ReasoningSignature),
                declaration(2, 0, 0, StreamPayload::ReasoningStop),
                declaration(3, 1, 1, StreamPayload::Text),
                declaration(4, 1, 2, StreamPayload::Reasoning),
            ],
            "ASTU".into(),
            vec![1, 1, 0, 1, 1],
        )
    } else if signed {
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
        node_did: node_did.clone(),
        requester_did: Some(requester_did.clone()),
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
            outcome: if recovered {
                OutputOutcome::Partial
            } else {
                OutputOutcome::Complete
            },
            segments: 1,
            stream_bytes,
        }),
        created_at: now.clone(),
    };
    let created = fixture_write(
        &access,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        &output_segment_create_variables(&segment).unwrap(),
    )
    .await
    .unwrap();
    let close_doc_id =
        gents_store::graphql::created_doc_id(&created, "AgentOutputSegment").unwrap();
    let header = TranscriptMessage {
        native_request_id: None,
        message_key: format!(
            "provider:{}:{}",
            request_doc_id,
            serde_json::to_string(&source).unwrap()
        ),
        session_id: SESSION_ID.into(),
        node_did: node_did.clone(),
        requester_did: Some(requester_did.clone()),
        request_doc_id: Some(request_doc_id.clone()),
        publication: if recovered {
            MessagePublication::RequestRecovery {
                execution_generation: "recovery".into(),
            }
        } else {
            MessagePublication::RequestExecution {
                execution_generation: "generation".into(),
            }
        },
        outcome: if recovered {
            OutputOutcome::Partial
        } else {
            OutputOutcome::Complete
        },
        sequence: 1,
        role: MessageRole::Assistant,
        native_id: (signed && !recovered).then(|| "provider-id".into()),
        blocks: if recovered {
            vec![
                MessageBlock::Reasoning {
                    id: None,
                    parts: vec![ReasoningPart::Text {
                        text: PayloadRef {
                            close_doc_id: close_doc_id.clone(),
                            stream: 0,
                        },
                        signature: Some("S".into()),
                    }],
                },
                MessageBlock::Text {
                    text: PresentedPayload {
                        output: PayloadRef {
                            close_doc_id: close_doc_id.clone(),
                            stream: 3,
                        },
                        presentation: PayloadPresentation::Full,
                    },
                },
            ]
        } else if signed {
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
    let created = fixture_write(
        &access,
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_create_variables(&header).unwrap(),
    )
    .await
    .unwrap();
    let header_doc_id = gents_store::graphql::created_doc_id(&created, "AgentMessage").unwrap();
    let boundary = capture_source_boundary(
        &access,
        SESSION_ID,
        &node_did,
        Some(&requester_did),
        &request_doc_id,
        &request_commit_cid,
    )
    .await
    .unwrap();
    ReplayFixture {
        node,
        _db: db,
        user,
        user_access,
        agent,
        access,
        node_did,
        requester_did,
        request_doc_id,
        request_commit_cid,
        header_doc_id,
        header,
        boundary,
        scope_kind,
    }
}

#[tokio::test]
async fn recovered_stopped_reasoning_replays_only_with_its_authenticated_prefix() {
    use gents_protocol::message::AssistantContent;
    let fixture = fixture_with_provider_payload_options(
        true,
        gents_protocol::rendered_request::CaptureScopeKind::Inference,
        false,
        true,
    )
    .await;
    let capture = fixture
        .insert_capture(RenderedRequestSource::ClaudeCliSubscription)
        .await
        .unwrap();
    let candidate = fixture.candidates().await.unwrap().remove(0);
    assert!(!candidate.complete);
    let tag = ReplayTag {
        request_doc_id: fixture.request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: candidate.coordinate.scope,
            turn_index: candidate.coordinate.turn_index,
            attempt: candidate.coordinate.attempt,
        },
    };
    let Message::Assistant { id, content, .. } = candidate.message.clone() else {
        panic!("expected assistant")
    };
    assert_eq!(content.len(), 2);
    assert!(matches!(content[0], AssistantContent::Reasoning(_)));
    assert!(matches!(content[1], AssistantContent::Text(_)));
    let rows = vec![TaggedAssistantRow {
        source: Some(tag.clone()),
        physical_header: Some(candidate.header_doc_id),
        block_indices: candidate.block_indices,
        id,
        content: content.clone(),
    }];
    let evidence = super::output::replay_tags::resolve_canonical_replay_tag(
        &fixture.access,
        fixture.scope(),
        &fixture.boundary,
        &tag,
    )
    .await
    .unwrap();
    assert_eq!(evidence.len(), 1);
    let resolve = |_: &ReplayTag| evidence.clone();
    let stage = replay_stage(&rows, &capture.issuer, capture.wire, resolve);
    assert_eq!(stage, rows);
    let next_body = |system: &str| {
        let mut body = capture.body.clone();
        body["system"] = serde_json::json!([{"type": "text", "text": system}]);
        body["messages"].as_array_mut().unwrap().push(serde_json::json!({
            "role": "assistant", "content": gents_loop::claude_messages_body::encode_assistant_content(&stage[0].content).unwrap(),
        }));
        flatten(&body, capture.wire).unwrap()
    };
    assert_eq!(
        select_replay(
            &rows,
            &capture.issuer,
            capture.wire,
            resolve,
            &next_body("system")
        ),
        rows
    );
    let stripped = select_replay(
        &rows,
        &capture.issuer,
        capture.wire,
        resolve,
        &next_body("changed"),
    );
    assert!(stripped[0]
        .content
        .iter()
        .all(|block| !matches!(block, AssistantContent::Reasoning(_))));
}

/// Lean `claude_reasoning_only_is_not_replayed`: a recovered turn that kept
/// only its stopped reasoning, with no ordinary content, is no replay base on
/// the Claude wire, even beside its authenticated capture.
#[tokio::test]
async fn recovered_reasoning_without_text_is_not_replayed_on_the_claude_wire() {
    use gents_protocol::message::AssistantContent;
    let fixture = fixture_with_provider_payload_options(
        true,
        gents_protocol::rendered_request::CaptureScopeKind::Inference,
        false,
        true,
    )
    .await;
    let capture = fixture
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
    let Message::Assistant { id, content, .. } = candidate.message.clone() else {
        panic!("expected assistant")
    };
    assert!(matches!(content[0], AssistantContent::Reasoning(_)));
    let rows = vec![TaggedAssistantRow {
        source: Some(tag.clone()),
        physical_header: Some(candidate.header_doc_id),
        block_indices: candidate.block_indices[..1].to_vec(),
        id,
        content: content[..1].to_vec(),
    }];
    let evidence = super::output::replay_tags::resolve_canonical_replay_tag(
        &fixture.access,
        fixture.scope(),
        &fixture.boundary,
        &tag,
    )
    .await
    .unwrap();
    let resolve = |_: &ReplayTag| evidence.clone();
    let no_reasoning = |replayed: &[TaggedAssistantRow]| {
        replayed.iter().all(|row| {
            row.content
                .iter()
                .all(|block| !matches!(block, AssistantContent::Reasoning(_)))
        })
    };
    let stage = replay_stage(&rows, &capture.issuer, capture.wire, resolve);
    assert!(no_reasoning(&stage), "{stage:?}");
    let body = flatten(&capture.body, capture.wire).unwrap();
    let selected = select_replay(&rows, &capture.issuer, capture.wire, resolve, &body);
    assert!(no_reasoning(&selected), "{selected:?}");
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
    let Message::Assistant { id, content, .. } = candidate.message.clone() else {
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
    let evidence = super::output::replay_tags::resolve_canonical_replay_tag(
        &fixture.access,
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
    assert!(super::output::replay_tags::resolve_canonical_replay_tag(
        &missing.access,
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
    let foreign_evidence = super::output::replay_tags::resolve_canonical_replay_tag(
        &foreign.access,
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
    let later_deadline = gents_store::graphql::escape_graphql_string(
        &(chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(),
    );
    let updated = fixture_query(&observed.access, &format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_lease_expires_at: "{later_deadline}" }}) {{ _docID }} }}"#,
            gents_store::graphql::escape_graphql_string(&observed.request_doc_id),
        ))
        .await;
    assert!(!updated.has_errors(), "{:?}", updated.errors);
    let newest = observed
        .access
        .transact_readonly("test.replay_later_commit", |txn| {
            Box::pin(
                gents_store::graphql::newest_document_composite_commit_in_txn(
                    txn,
                    &observed.request_doc_id,
                    "test.replay_later_commit",
                ),
            )
        })
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
    let evidence = super::output::replay_tags::resolve_canonical_replay_tag(
        &claude.access,
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
async fn replay_rejects_forged_high_water_and_storage_rejects_sequence_twins() {
    let observed = fixture().await;
    let mut forged = observed.boundary.clone();
    forged.canonical_through.as_mut().unwrap().commit_cid = "not-a-header-commit".into();
    let bad_high_water =
        load_canonical_assistant_candidates(&observed.access, observed.scope(), &forged)
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
        load_canonical_assistant_candidates(&observed.access, forged_scope, &forged_request)
            .await
            .is_err()
    );

    let mut twin = observed.header.clone();
    twin.message_key.push_str(":twin");
    let error = fixture_write(
        &observed.access,
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_create_variables(&twin).unwrap(),
    )
    .await
    .expect_err("a sequence slot cannot hold a second header");
    assert!(gents_protocol::error_chain::ErrorChain(&error)
        .to_string()
        .contains("unique index"));
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
        let evidence = super::output::replay_tags::resolve_canonical_replay_tag(
            &fixture.access,
            fixture.scope(),
            &fixture.boundary,
            &tag,
        )
        .await
        .expect("an unverifiable capture must not fail the request");
        assert!(evidence.is_empty());
        let Message::Assistant { id, content, .. } = candidate.message.clone() else {
            panic!("canonical fixture must reconstruct an assistant");
        };
        let rows = vec![TaggedAssistantRow {
            source: Some(tag),
            physical_header: Some(candidate.header_doc_id.clone()),
            block_indices: (0..content.len()).collect(),
            id,
            content,
        }];
        let issuer = gents_inference::llm::backend_client::claude_subscription_replay_issuer()
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

struct FaultingRequests<'a> {
    access: &'a gents_store::config_client::ConfigAccess,
    request_doc_id: &'a str,
    fault: Option<RequestFault>,
}

struct CountingRequests<'a> {
    access: &'a gents_store::config_client::ConfigAccess,
    rows: std::sync::atomic::AtomicUsize,
    commits: std::sync::atomic::AtomicUsize,
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
    let access = fixture.access.clone();
    access
        .transact_write("test.replay.duplicate_header", |txn| {
            let duplicate = duplicate.clone();
            Box::pin(async move {
                txn.execute_with_variables(
                    CREATE_AGENT_MESSAGE_MUTATION,
                    &transcript_message_create_variables(&duplicate)?,
                )
                .await
                .map_err(anyhow::Error::from)
            })
        })
        .await
        .unwrap();
    fixture.boundary = capture_source_boundary(
        &fixture.access,
        SESSION_ID,
        &fixture.node_did,
        Some(&fixture.requester_did),
        &fixture.request_doc_id,
        &fixture.request_commit_cid,
    )
    .await
    .unwrap();
    let requests = CountingRequests {
        access: &fixture.access,
        rows: std::sync::atomic::AtomicUsize::new(0),
        commits: std::sync::atomic::AtomicUsize::new(0),
    };
    for resolution in 1..=2 {
        let (candidates, segment_scans) = super::count_request_output_scans(
            super::output::replay_candidates::load_canonical_assistant_candidates_with(
                &fixture.access,
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
    let selected = super::output::replay_candidates::load_canonical_assistant_candidates_with(
        &fixture.access,
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

async fn load_with_fault(
    fixture: &ReplayFixture,
    scope: CanonicalReplayScope<'_>,
    boundary: &SourceBoundary,
    fault: Option<RequestFault>,
    selected_tags: Option<&[ReplayTag]>,
) -> anyhow::Result<Vec<super::output::CanonicalAssistantCandidate>> {
    let requests = FaultingRequests {
        access: &fixture.access,
        request_doc_id: &fixture.request_doc_id,
        fault,
    };
    super::output::replay_candidates::load_canonical_assistant_candidates_with(
        &fixture.access,
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
    use crate::request::lifecycle::{RequestIdentity, RequestSigner, RequestSpec};
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    let next = crate::request::lifecycle::build_signed_client_request(
        &fixture.user_access,
        RequestSpec::new(
            RequestPurpose::Normal,
            RequestIdentity {
                request_id: "replay-request-next".into(),
                node_did: fixture.node_did.clone(),
                requester_did: Some(fixture.requester_did.clone()),
                agent_did: fixture.agent.did().to_owned(),
                session_id: SESSION_ID.into(),
                content: "next".into(),
                execution_origin: crate::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            AgentRequestAdmissionRecord::client(&fixture.requester_did),
        ),
        RequestSigner::Identity(&fixture.user),
    )
    .await
    .unwrap();
    crate::request::lifecycle::submit_client_request(&fixture.user_access, &next)
        .await
        .unwrap();
    let response = fixture
        .user_access
        .execute(r#"{ AgentRequest(filter: {request_id: {_eq: "replay-request-next"}}) {_docID} }"#)
        .await
        .unwrap();
    let current_doc_id = response["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    let current_commit_cid = fixture
        .access
        .transact_readonly("test.replay_next_commit", |txn| {
            let id = &current_doc_id;
            Box::pin(async move {
                gents_store::graphql::newest_document_composite_commit_in_txn(
                    txn,
                    id,
                    "test.replay_next_commit",
                )
                .await
            })
        })
        .await
        .unwrap()
        .unwrap()
        .cid;
    let boundary = capture_source_boundary(
        &fixture.access,
        SESSION_ID,
        &fixture.node_did,
        Some(&fixture.requester_did),
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
            "{fault:?} is not a replay violation: {error:#}"
        );
    }
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
                &fixture.node_did,
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
        .map(|key| format!("\"{}\"", gents_store::graphql::escape_graphql_string(key)))
        .collect::<Vec<_>>()
        .join(", ");
    let response = fixture_query(&fixture.access, &format!(
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
        let record = gents_inference::rendered_request::encoding::decode_capture_record(
            version,
            row["request_json"].as_str().unwrap(),
            gents_inference::rendered_request::CapturePayloadKind::RequestBody,
        )
        .unwrap();
        if let gents_inference::rendered_request::encoding::DecodedRecord::Tree(tree) = &record {
            assert!(
                tree.height <= 1,
                "fixture has at most one internal manifest level"
            );
            roots.insert(tree.root.doc_id.clone());
        }
        for entry in
            gents_inference::rendered_request::manifest_leaf_entries(&fixture.access, record)
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
        .map(|count| {
            count.div_ceil(gents_inference::rendered_request::commits::FIELD_COMMIT_ALIAS_BATCH)
        })
        .sum::<usize>();

    let ((batched, commit_statements), capture_queries) =
        super::count_capture_row_queries(
            gents_inference::rendered_request::commits::count_field_commit_statements(
                super::output::replay_tags::resolve_canonical_replay_tags(
                    &fixture.access,
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
        let singleton = super::output::replay_tags::resolve_canonical_replay_tag(
            &fixture.access,
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
    let layout = gents_inference::rendered_request::encoding::capture_block_layout(&body).unwrap();
    let chunks = gents_inference::rendered_request::encoding::chunk_capture_layout(&layout);
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
        gents_inference::rendered_request::encoding::block_content_key(&poisoned),
        chunks[index].content_key
    );
    // Capture blocks are deduplicated within their session scope, so the
    // poisoned block must occupy the same scope the sink resolves for the turn.
    let session_doc_id = {
        let response = fixture
            .access
            .execute(&format!(
                r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                gents_store::graphql::escape_graphql_string(&fixture.node_did),
                gents_store::graphql::escape_graphql_string(SESSION_ID),
            ))
            .await
            .unwrap();
        let rows = response["data"]["AgentSession"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            1,
            "the replay session scopes its capture blocks"
        );
        rows[0]["_docID"].as_str().unwrap().to_owned()
    };
    let created = fixture_write(
        &fixture.access,
        "mutation($input: RenderedRequestBlockMutationInputArg!) { create_RenderedRequestBlock(input: $input) { _docID } }",
        &serde_json::json!({"input": {
            "session_doc_id": session_doc_id,
            "agent_did": fixture.agent.did(),
            "content_key": chunks[index].content_key,
            "payload": String::from_utf8(poisoned.clone()).unwrap(),
            "byte_len": chunks[index].bytes.len(),
            "created_at": chrono::Utc::now().to_rfc3339(),
        }}),
    ).await.unwrap();
    let poisoned_doc =
        gents_store::graphql::created_doc_id(&created, "RenderedRequestBlock").unwrap();

    fixture.insert_provider_turn(corrupt_turn, body).await;

    let response = fixture_query(
        &fixture.access,
        r#"{ RenderedRequest { capture_version request_json } }"#,
    )
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
            gents_inference::rendered_request::CapturePayloadKind::RequestBody,
            gents_inference::rendered_request::CapturePayloadKind::ProvenancePayload,
        ] {
            let record = gents_inference::rendered_request::encoding::decode_capture_record(
                version, stored, kind,
            )
            .unwrap();
            let entries =
                gents_inference::rendered_request::manifest_leaf_entries(&fixture.access, record)
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
    let resolved = super::output::replay_tags::resolve_canonical_replay_tags(
        &fixture.access,
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

#[async_trait::async_trait]
impl ReplayRequestReader for CountingRequests<'_> {
    async fn request_facts(
        &self,
        ids: &[String],
    ) -> anyhow::Result<
        std::collections::BTreeMap<String, super::output::replay_request::ReplayRequestFacts>,
    > {
        self.commits
            .fetch_add(ids.len(), std::sync::atomic::Ordering::Relaxed);
        self.rows
            .fetch_add(ids.len(), std::sync::atomic::Ordering::Relaxed);
        self.access.request_facts(ids).await
    }
}

#[async_trait::async_trait]
impl ReplayRequestReader for FaultingRequests<'_> {
    async fn request_facts(
        &self,
        request_doc_ids: &[String],
    ) -> anyhow::Result<
        std::collections::BTreeMap<String, super::output::replay_request::ReplayRequestFacts>,
    > {
        if request_doc_ids.iter().any(|id| id == self.request_doc_id) {
            match self.fault {
                Some(RequestFault::RowsReadFails) => anyhow::bail!("injected AgentRequest read"),
                Some(RequestFault::CommitsReadFails) => {
                    anyhow::bail!("injected composite commit read")
                }
                _ => {}
            }
        }
        let mut facts = self.access.request_facts(request_doc_ids).await?;
        if matches!(self.fault, Some(RequestFault::Missing)) {
            if let Some(request) = facts.get_mut(self.request_doc_id) {
                request.rows.clear();
            }
        }
        Ok(facts)
    }
}
