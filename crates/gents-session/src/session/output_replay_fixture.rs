use super::provider_context_reduction::capture_source_boundary;
use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, ReasoningPart, SegmentRun,
    SourceClose, StreamDeclaration, StreamPayload, TranscriptMessage,
};
use gents_protocol::rendered_request::RenderedRequestSource;
use gents_store::identity::NodeIdentity;

use super::output::replay_candidates::load_canonical_assistant_candidates;
use super::output::CanonicalReplayScope;

use gents_protocol::output::rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::transcript::SourceBoundary;

use gents_loop::claude_messages_body::{ReplayIssuer, ReplayWire};

#[derive(Clone, Copy)]
pub enum CaptureStorage {
    Exact,
    Corrupt,
    MissingDeltaBase,
}

#[derive(Clone)]
pub struct CapturedRequest {
    pub issuer: ReplayIssuer,
    pub wire: ReplayWire,
    pub body: serde_json::Value,
}

pub const SESSION_ID: &str = "replay-session";

pub const REQUEST_ID: &str = "replay-request";

pub struct ReplayFixture {
    pub node: Arc<EmbeddedNode>,
    pub _db: gents_inference::test_support::TestDb,
    pub user: gents_store::identity::KeyIdentity,
    pub user_access: gents_store::config_client::ConfigAccess,
    pub agent: Arc<dyn gents_store::identity::NodeIdentity>,
    pub access: gents_store::config_client::ConfigAccess,
    pub node_did: String,
    pub requester_did: String,
    pub request_doc_id: String,
    pub request_commit_cid: String,
    pub header_doc_id: String,
    pub header: TranscriptMessage,
    pub boundary: SourceBoundary,
    pub scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
}

pub async fn fixture_write(
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

pub async fn fixture_query(
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
    pub fn scope(&self) -> CanonicalReplayScope<'_> {
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

    pub async fn candidates(&self) -> anyhow::Result<Vec<super::output::CanonicalAssistantCandidate>> {
        load_canonical_assistant_candidates(&self.access, self.scope(), &self.boundary).await
    }

    pub async fn insert_capture(&self, source: RenderedRequestSource) -> Option<CapturedRequest> {
        self.insert_capture_stored(source, CaptureStorage::Exact)
            .await
    }

    pub async fn insert_capture_stored(
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

    pub async fn insert_capture_row(
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

    pub async fn insert_provider_turn(&mut self, turn_index: u32, body: serde_json::Value) {
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

    pub async fn insert_authored_assistant(&mut self) {
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

pub async fn fixture() -> ReplayFixture {
    fixture_with_provider_payload(false).await
}

pub async fn signed_fixture() -> ReplayFixture {
    fixture_with_provider_payload(true).await
}

pub async fn fixture_with_provider_payload(signed: bool) -> ReplayFixture {
    fixture_with_provider_payload_and_scope(
        signed,
        gents_protocol::rendered_request::CaptureScopeKind::Inference,
    )
    .await
}

pub async fn fixture_with_provider_payload_and_scope(
    signed: bool,
    scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
) -> ReplayFixture {
    fixture_with_provider_payload_and_scope_and_tool(signed, scope_kind, true).await
}

pub async fn fixture_with_provider_payload_and_scope_and_tool(
    signed: bool,
    scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
    with_tool: bool,
) -> ReplayFixture {
    fixture_with_provider_payload_options(signed, scope_kind, with_tool, false).await
}

pub async fn fixture_with_provider_payload_options(
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
