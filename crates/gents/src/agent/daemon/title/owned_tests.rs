use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use defra_node::EmbeddedNode;
use futures::{stream, StreamExt};
use gents_protocol::message::ReasoningContent;
use gents_protocol::output::extent::inspect_open_source;
use gents_protocol::output::live::reconstruct_dense_prefix;
use gents_protocol::output::reconstruction::{reconstruct_stream, ObservedSegment};
use gents_protocol::output::{
    OutputOutcome, OutputSource, OutputWriter, PayloadRef, SourceClose, StreamPayload,
};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};
use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

use super::{BehaviorDaemon, TitleTask};
use crate::agent::completion_retry::CompletionRetryProfileFields;
use crate::agent::runtime::{run_router_with_watcher, RuntimeAdmissionGate, StartupBarrier};
use crate::backend_provider::BackendProviderKind;
use crate::config::{ResolvedBehavior, SamplingConfig};
use crate::config_client::ConfigAccess;
use crate::hook::{BackgroundExecutionRegistry, BackgroundToolRegistry, FailurePolicy};
use crate::identity::{AgentIdentity, KeyIdentity, RuntimePrincipal};
use crate::lean_vocab_test::{
    LeanCanonicalExecutionCase, LeanCanonicalExecutionOperation, LeanCanonicalSource,
    LeanPayloadKind, LeanTerminalSelection,
};
use crate::prompt::LayeredPromptBuilder;
use crate::runtime_snapshot::ActiveRuntimeSnapshot;
use crate::runtime_status::RuntimeStatusHandle;
use crate::session::canonical_rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
use crate::tool_surface::BehaviorToolConfig;
use crate::watcher::{AgentRequest, DefraWatcher};

#[derive(Clone)]
struct TitleProvider {
    events: Arc<Vec<RawStreamingChoice<()>>>,
    stall: bool,
    calls: Arc<AtomicUsize>,
    entered: Arc<tokio::sync::Notify>,
    before_second: Option<Arc<tokio::sync::Notify>>,
    third_poll: Option<Arc<tokio::sync::Notify>>,
    /// Moves the title request's execution generation before any output is
    /// streamed, so every later auxiliary write is fenced out.
    fence_generation: Option<(Arc<EmbeddedNode>, String, String)>,
    /// Fails every call with this provider error instead of streaming.
    error: Option<String>,
}

#[derive(Default)]
struct TitleWarningCapture(Arc<Mutex<Vec<&'static str>>>);

impl<S: tracing::Subscriber> Layer<S> for TitleWarningCapture {
    fn on_event(&self, event: &tracing::Event<'_>, _: LayerContext<'_, S>) {
        let metadata = event.metadata();
        if metadata.target() == "gents::agent::daemon::title"
            && *metadata.level() == tracing::Level::WARN
        {
            self.0.lock().unwrap().push(metadata.name());
        }
    }
}

impl TitleProvider {
    fn new(events: Vec<RawStreamingChoice<()>>, stall: bool) -> Self {
        Self {
            events: Arc::new(events),
            stall,
            calls: Arc::new(AtomicUsize::new(0)),
            entered: Arc::new(tokio::sync::Notify::new()),
            before_second: None,
            third_poll: None,
            fence_generation: None,
            error: None,
        }
    }
}

#[allow(refining_impl_trait)]
impl CompletionModel for TitleProvider {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &(), _: impl Into<String>) -> Self {
        unreachable!("title tests construct the provider directly")
    }

    async fn completion(
        &self,
        _: CompletionRequest,
    ) -> Result<CompletionResponse<()>, CompletionError> {
        Err(CompletionError::ProviderError("stream only".into()))
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<()>, CompletionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        crate::test_support::capture_scripted_provider_request(&request, "scripted").await?;
        if let Some(error) = &self.error {
            return Err(CompletionError::ProviderError(error.clone()));
        }
        if let Some((node, doc_id, agent_did)) = &self.fence_generation {
            let doc = crate::graphql::escape_graphql_string(doc_id);
            let owner = crate::graphql::escape_graphql_string(agent_did);
            ConfigAccess::Local(node.clone())
                .write(
                    "test.title_fence_generation",
                    &format!(r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }}, agent_did: {{ _eq: "{owner}" }} }}, input: {{ execution_generation: "fenced-title-generation" }}) {{ _docID }} }}"#),
                )
                .await
                .expect("fence title generation");
        }
        self.entered.notify_one();
        let items = self.events.iter().cloned().map(Ok::<_, CompletionError>);
        let inner: rig::streaming::StreamingResult<()> = if let (
            Some(before_second),
            Some(third_poll),
        ) =
            (&self.before_second, &self.third_poll)
        {
            let before_second = before_second.clone();
            let third_poll = third_poll.clone();
            let mut events = items.collect::<Vec<_>>().into_iter();
            let first = events.next().expect("first reasoning event");
            let second = events.next().expect("second reasoning event");
            assert!(events.next().is_none());
            Box::pin(
                stream::once(async move { first })
                    .chain(stream::once(async move {
                        before_second.notified().await;
                        second
                    }))
                    .chain(stream::once(async move {
                        third_poll.notify_one();
                        futures::future::pending::<Result<RawStreamingChoice<()>, CompletionError>>(
                        )
                        .await
                    })),
            )
        } else if self.stall {
            Box::pin(stream::iter(items.collect::<Vec<_>>()).chain(stream::pending()))
        } else {
            Box::pin(stream::iter(items.collect::<Vec<_>>()))
        };
        Ok(StreamingCompletionResponse::stream(inner))
    }
}

struct TitleFixture {
    _directory: tempfile::TempDir,
    node: Arc<EmbeddedNode>,
    behavior: Arc<ResolvedBehavior>,
    identity: Arc<dyn AgentIdentity>,
    parent: AgentRequest,
    title: AgentRequest,
}

impl TitleFixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(directory.path())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let identity: Arc<dyn AgentIdentity> = Arc::new(
            KeyIdentity::load_or_create(directory.path().join("principal.key"), None).unwrap(),
        );
        let principal = Arc::new(RuntimePrincipal {
            agent_did: identity.did().to_owned(),
            identity: identity.clone(),
            default_behavior_id: "general".into(),
            display_name: None,
            enabled: true,
        });
        let behavior = Arc::new(ResolvedBehavior {
            behavior_id: "general".into(),
            principal,
            backend_id: Some("general:backend".into()),
            backend_provider_kind: BackendProviderKind::OpenAiCompatible,
            openai_wire_api: crate::OpenAiWireApi::ChatCompletions,
            backend_endpoint: "http://127.0.0.1:1/v1".into(),
            backend_auth: crate::document_config::BackendAuth::Unauthenticated,
            model_name: "scripted".into(),
            resolved_reasoning_efforts: None,
            context_window: 8192,
            max_output_tokens: 1024,
            max_turns: 2,
            max_turns_provenance: crate::config::MaxTurnsProvenance::Default,
            system_prompt: "system".into(),
            tools: BehaviorToolConfig::meta_only(),
            compaction: None,
            compaction_inference: None,
            max_total_tokens: None,
            stream_batch_ms: 0,
            stream_liveness_timeout: Duration::from_secs(60),
            deadline_duration: Duration::from_secs(120),
            provider_idle_timeout: Duration::from_secs(
                crate::config::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
            ),
            completion_retry: CompletionRetryProfileFields::default(),
            sampling: SamplingConfig::default(),
            skills: Vec::new(),
        });
        crate::test_support::install_test_behavior(&node, identity.did(), &behavior.behavior_id)
            .await;
        let request_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            request_id,
            identity.did(),
            identity.did(),
            &behavior.behavior_id,
            session_id,
            "explain the request",
            "interactive",
            created_at,
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        crate::sign_agent_request_create(identity.as_ref(), &mut create)
            .await
            .unwrap();
        let response = ConfigAccess::Local(node.clone())
            .write("test.title_parent", &create.graphql_mutation().unwrap())
            .await
            .unwrap();
        let parent_doc_id = crate::graphql::created_doc_id(&response, "AgentRequest").unwrap();
        let parent =
            crate::request_admission::load_request_for_admission_test(&node, &parent_doc_id)
                .await
                .unwrap();
        let session = gents_protocol::session::AgentSession {
            session_id: parent.session_id.clone(),
            agent_did: parent.agent_did.clone(),
            requester_did: parent.requester_did.clone(),
            behavior_id: parent.behavior_id.clone(),
            created_at: parent.created_at.clone(),
            closed_at: None,
            title: None,
            tags: vec![],
            provenance: None,
            observation: None,
        };
        let input =
            gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(session).unwrap())
                .unwrap();
        ConfigAccess::Local(node.clone())
            .write(
                "test.title_session",
                &format!("mutation {{ create_AgentSession(input: {input}) {{ _docID }} }}"),
            )
            .await
            .unwrap();
        let title = crate::lifecycle::materialize::write_pending_title_request(
            &node,
            &parent,
            parent.content.clone(),
        )
        .await
        .unwrap();
        Self {
            _directory: directory,
            node,
            behavior,
            identity,
            parent,
            title,
        }
    }

    fn task<M: CompletionModel>(&self, model: M, capture: bool) -> TitleTask<M> {
        TitleTask {
            node: self.node.clone(),
            behavior: self.behavior.clone(),
            provider_family: None,
            model: Arc::new(model),
            verifier: crate::request_admission::AgentRequestAdmissionVerifier::new(
                self.node.clone(),
                self.identity.clone(),
                crate::agent::p2p_reconcile::enrollment_authority_channel().1,
            ),
            capture_factory: capture.then(|| {
                crate::rendered_request::defra_rendered_request_capture_factory(self.node.clone())
            }),
        }
    }

    async fn terminalize_parent(&self) {
        crate::request_admission::terminalize_pending_request_rejection(
            self.node.as_ref(),
            &self.parent.doc_id,
            &self.parent.agent_did,
            "parent ended before title inference",
            "test.title_parent_terminal",
        )
        .await
        .unwrap();
        let doc = crate::graphql::escape_graphql_string(&self.parent.doc_id);
        let response = ConfigAccess::Local(self.node.clone())
            .execute(&format!("{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{doc}\" }} }}, limit: 1) {{ lifecycle_state }} }}"))
            .await
            .unwrap();
        assert_eq!(
            response["data"]["AgentRequest"][0]["lifecycle_state"],
            "failed"
        );
    }

    async fn interrupt_parent(&self) {
        let mut lifecycle = crate::lifecycle::RequestLifecycle::new_with_execution_binding(
            self.node.clone(),
            &self.behavior.behavior_id,
            &self.parent.agent_did,
            self.parent.clone(),
            self.behavior.deadline_duration.as_secs(),
            crate::lifecycle::ExecutionOrigin::Interactive,
            self.behavior.backend_id.clone().unwrap_or_default(),
        );
        assert_eq!(
            lifecycle.claim_with_identity().await.unwrap(),
            crate::lifecycle::ClaimOutcome::Claimed
        );
        crate::interrupt::interrupt_request_by_doc_id(
            self.node.as_ref(),
            &self.parent.doc_id,
            &self.parent.agent_did,
            self.parent.requester_did.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(
            lifecycle
                .terminalize_owned(
                    crate::lifecycle::RequestTerminalOutcome::Interrupted,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    None,
                )
                .await
                .unwrap(),
            crate::lifecycle::TerminalizeResult::Won
        );
        let doc = crate::graphql::escape_graphql_string(&self.parent.doc_id);
        let response = ConfigAccess::Local(self.node.clone())
            .execute(&format!("{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{doc}\" }} }}, limit: 1) {{ lifecycle_state interrupt_requested_at terminal_output }} }}"))
            .await
            .unwrap();
        let row = &response["data"]["AgentRequest"][0];
        assert_eq!(row["lifecycle_state"], "interrupted");
        assert!(row["interrupt_requested_at"].as_str().is_some());
        assert_eq!(
            serde_json::from_value::<gents_protocol::output::TerminalOutput>(
                row["terminal_output"].clone()
            )
            .unwrap(),
            gents_protocol::output::TerminalOutput::NoMessage
        );
    }

    async fn session_message_count(&self) -> usize {
        let session = crate::graphql::escape_graphql_string(&self.parent.session_id);
        let response = ConfigAccess::Local(self.node.clone())
            .execute(&format!("{{ AgentMessage(filter: {{ session_id: {{ _eq: \"{session}\" }} }}) {{ _docID }} }}"))
            .await
            .unwrap();
        response["data"]["AgentMessage"].as_array().unwrap().len()
    }

    async fn output_rows(&self) -> (Vec<crate::session::canonical_rows::OutputSegmentRow>, usize) {
        let doc = crate::graphql::escape_graphql_string(&self.title.doc_id);
        let query = format!("{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: \"{doc}\" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} AgentMessage(filter: {{ request_doc_id: {{ _eq: \"{doc}\" }} }}) {{ _docID }} }}");
        let response = ConfigAccess::Local(self.node.clone())
            .execute(&query)
            .await
            .unwrap();
        let data = &response["data"];
        let rows = data["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .iter()
            .map(decode_output_segment_row)
            .collect::<anyhow::Result<Vec<_>>>()
            .unwrap();
        (rows, data["AgentMessage"].as_array().unwrap().len())
    }

    async fn terminal_row(
        &self,
    ) -> (
        RequestLifecycleState,
        Option<gents_protocol::output::TerminalOutput>,
    ) {
        let doc = crate::graphql::escape_graphql_string(&self.title.doc_id);
        let query = format!("{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{doc}\" }} }}, limit: 1) {{ lifecycle_state terminal_output }} }}");
        let response = ConfigAccess::Local(self.node.clone())
            .execute(&query)
            .await
            .unwrap();
        let row = &response["data"]["AgentRequest"][0];
        (
            serde_json::from_value(row["lifecycle_state"].clone()).unwrap(),
            serde_json::from_value(row["terminal_output"].clone()).unwrap(),
        )
    }
}

fn modeled_title_fields(name: &str) -> (Vec<(StreamPayload, String, u32, u32)>, OutputOutcome) {
    let case = crate::lean_vocab_test::lean_contract_snapshot().canonical_execution_gate_cases.iter().find(|case| matches!(case, LeanCanonicalExecutionCase::NativeExecution { name: found, .. } | LeanCanonicalExecutionCase::ModelExecution { name: found, .. } if found == name)).expect("Lean title script");
    let (LeanCanonicalExecutionCase::NativeExecution {
        operations,
        expected_observations,
        ..
    }
    | LeanCanonicalExecutionCase::ModelExecution {
        operations,
        expected_observations,
        ..
    }) = case
    else {
        unreachable!()
    };
    assert_eq!(operations.len(), expected_observations.len());
    assert!(
        expected_observations.iter().all(|step| step.accepted),
        "{name}: modeled title path changed"
    );
    let final_state = expected_observations.last().unwrap();
    assert_eq!(
        final_state.terminal_selection,
        Some(LeanTerminalSelection::NoMessage)
    );
    assert!(final_state.messages.is_empty());
    let LeanCanonicalExecutionOperation::AppendOutput { record, .. } = &operations[0] else {
        panic!("{name}: first modeled action changed")
    };
    assert!(matches!(
        &record.coordinate.source,
        LeanCanonicalSource::Auxiliary {
            auxiliary_kind: crate::lean_vocab_test::LeanAuxiliaryKind::Title,
            ..
        }
    ));
    let flush = record.flush.as_ref().expect("modeled raw flush");
    let mut cursor = 0usize;
    let fields = flush
        .runs
        .iter()
        .map(|run| {
            let end = cursor + usize::try_from(run.bytes).unwrap();
            let text = String::from_utf8(flush.payload[cursor..end].to_vec()).unwrap();
            cursor = end;
            let declaration = run.declaration.as_ref().expect("modeled declaration");
            let kind = match &declaration.kind {
                LeanPayloadKind::Reasoning => StreamPayload::Reasoning,
                LeanPayloadKind::Signature => StreamPayload::ReasoningSignature,
                LeanPayloadKind::Encrypted => StreamPayload::ReasoningEncrypted,
                LeanPayloadKind::Redacted => StreamPayload::ReasoningRedacted,
                _ => panic!("{name}: title fixture changed payload kind"),
            };
            (
                kind,
                text,
                u32::try_from(declaration.block).unwrap(),
                u32::try_from(declaration.part).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(cursor, flush.payload.len());
    let closure = final_state
        .segments
        .iter()
        .find_map(|record| record.close.as_ref())
        .expect("modeled title closure");
    let crate::lean_vocab_test::LeanCanonicalClosure::Closed { outcome, .. } = closure else {
        panic!("modeled title retracted")
    };
    let outcome = match outcome {
        crate::lean_vocab_test::LeanOutcome::Complete => OutputOutcome::Complete,
        crate::lean_vocab_test::LeanOutcome::Partial => OutputOutcome::Partial,
    };
    (fields, outcome)
}

/// Rig emits one reasoning part per event. The owned accumulator joins these
/// same-ID events into the single multi-part block declared by the Lean raw
/// witness, without altering any received field bytes or native positions.
fn provider_events(
    fields: &[(StreamPayload, String, u32, u32)],
    complete: bool,
) -> Vec<RawStreamingChoice<()>> {
    let mut events = Vec::new();
    let mut index = 0;
    while index < fields.len() {
        let content = match &fields[index].0 {
            StreamPayload::Reasoning => {
                let (signature_kind, signature, _, _) = &fields[index + 1];
                assert_eq!(signature_kind, &StreamPayload::ReasoningSignature);
                index += 2;
                ReasoningContent::Text {
                    text: fields[index - 2].1.clone(),
                    signature: Some(signature.clone()),
                }
            }
            StreamPayload::ReasoningEncrypted => {
                let text = fields[index].1.clone();
                index += 1;
                ReasoningContent::Encrypted(text)
            }
            StreamPayload::ReasoningRedacted => {
                let text = fields[index].1.clone();
                index += 1;
                ReasoningContent::Redacted { data: text }
            }
            other => panic!("unsupported modeled title field {other:?}"),
        };
        events.push(RawStreamingChoice::Reasoning {
            id: None,
            content: crate::llm::rig_compat::to_rig_reasoning_part(&content),
        });
    }
    if complete {
        events.push(RawStreamingChoice::Message("generated-title".into()));
        events.push(RawStreamingChoice::FinalResponse(()));
    }
    events
}

/// A reasoning model that exhausts its turn before any visible title text.
fn reasoning_only_events(
    fields: &[(StreamPayload, String, u32, u32)],
) -> Vec<RawStreamingChoice<()>> {
    let mut events = provider_events(fields, false);
    events.push(RawStreamingChoice::FinalResponse(()));
    events
}

async fn assert_title_audit(
    fixture: &TitleFixture,
    expected: &[(StreamPayload, String, u32, u32)],
    outcome: OutputOutcome,
    attempts: usize,
    complete_text: bool,
) {
    let (rows, message_count) = fixture.output_rows().await;
    assert_eq!(
        message_count, 0,
        "title audit published a transcript header"
    );
    let mut sources = std::collections::HashSet::new();
    for row in &rows {
        let OutputSource::ProviderTurn {
            scope,
            turn_index,
            attempt,
        } = &row.segment.source
        else {
            panic!("title wrote a non-provider source")
        };
        assert_eq!(scope.kind, crate::rendered_request::CaptureScopeKind::Title);
        assert_eq!(*turn_index, 0);
        assert_eq!(row.segment.request_doc_id, fixture.title.doc_id);
        sources.insert((*scope, *turn_index, *attempt));
    }
    assert_eq!(sources.len(), attempts);
    for source in sources {
        let output_source = OutputSource::ProviderTurn {
            scope: source.0,
            turn_index: source.1,
            attempt: source.2,
        };
        let source_rows = rows
            .iter()
            .filter(|row| row.segment.source == output_source)
            .collect::<Vec<_>>();
        let closed = source_rows
            .iter()
            .filter_map(|row| row.segment.close.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(closed.len(), 1, "one exact close per title attempt");
        assert!(
            matches!(closed[0], SourceClose::Closed { outcome: actual, .. } if *actual == outcome)
        );
        let OutputWriter::RequestExecution {
            execution_generation,
        } = &source_rows[0].segment.writer
        else {
            panic!("title audit has no request writer")
        };
        let writer = OutputWriter::RequestExecution {
            execution_generation: execution_generation.clone(),
        };
        let observed = source_rows
            .iter()
            .map(|row| ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            })
            .collect::<Vec<_>>();
        let extent = inspect_open_source(&observed, &fixture.title.doc_id, &output_source, &writer)
            .expect("complete title source extent");
        let SourceClose::Closed {
            segments,
            stream_bytes,
            ..
        } = closed[0]
        else {
            panic!("title source was not closed");
        };
        assert_eq!(
            *segments, extent.segments,
            "title close truncated raw segments"
        );
        assert_eq!(
            *stream_bytes, extent.stream_bytes,
            "title close truncated raw bytes"
        );
        let close_doc_id = source_rows
            .iter()
            .find(|row| row.segment.close.is_some())
            .expect("title close row")
            .doc_id
            .clone();
        let prefix = reconstruct_dense_prefix(
            &observed,
            &fixture.title.doc_id,
            &output_source,
            &writer,
            None,
        )
        .unwrap();
        assert_eq!(prefix.streams, extent.streams);
        for (stream, expected_stream) in extent.streams.iter().enumerate() {
            let sealed = reconstruct_stream(
                &observed,
                &[],
                &[],
                &PayloadRef {
                    close_doc_id: close_doc_id.clone(),
                    stream: u32::try_from(stream).unwrap(),
                },
            )
            .expect("sealed title stream");
            assert_eq!(&sealed, expected_stream);
        }
        assert_eq!(
            prefix.streams.len(),
            expected.len() + usize::from(complete_text)
        );
        for (actual, (kind, text, block, part)) in prefix.streams.iter().zip(expected) {
            assert_eq!(&actual.declaration.payload, kind);
            assert_eq!(&actual.text, text);
            assert_eq!(actual.declaration.block_index, *block);
            assert_eq!(actual.declaration.part_index, *part);
        }
        if complete_text {
            let last = prefix.streams.last().unwrap();
            assert_eq!(last.declaration.payload, StreamPayload::Text);
            assert_eq!(last.text, "generated-title");
        }
    }
}

async fn wait_for_title_streams(fixture: &TitleFixture, minimum: usize, fields: usize) {
    for _ in 0..2_000 {
        let (rows, _) = fixture.output_rows().await;
        let sources = rows
            .iter()
            .filter_map(|row| match &row.segment.source {
                OutputSource::ProviderTurn { scope, attempt, .. }
                    if scope.kind == crate::rendered_request::CaptureScopeKind::Title =>
                {
                    Some((scope.seq, *attempt))
                }
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>();
        let all_received = sources.len() >= minimum
            && sources.iter().all(|(sequence, attempt)| {
                let source = OutputSource::ProviderTurn {
                    scope: crate::rendered_request::CaptureScope {
                        kind: crate::rendered_request::CaptureScopeKind::Title,
                        seq: *sequence,
                    },
                    turn_index: 0,
                    attempt: *attempt,
                };
                let scoped = rows
                    .iter()
                    .filter(|row| row.segment.source == source)
                    .collect::<Vec<_>>();
                let Some(OutputWriter::RequestExecution {
                    execution_generation,
                }) = scoped.first().map(|row| &row.segment.writer)
                else {
                    return false;
                };
                let writer = OutputWriter::RequestExecution {
                    execution_generation: execution_generation.clone(),
                };
                let observed = scoped
                    .iter()
                    .map(|row| ObservedSegment {
                        doc_id: &row.doc_id,
                        segment: &row.segment,
                    })
                    .collect::<Vec<_>>();
                reconstruct_dense_prefix(&observed, &fixture.title.doc_id, &source, &writer, None)
                    .is_ok_and(|prefix| prefix.streams.len() >= fields)
            });
        if all_received {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("title reasoning did not reach the durable output owner");
}

#[tokio::test]
async fn title_reasoning_survives_parent_terminal_under_own_request() {
    let (fields, outcome) = modeled_title_fields("title_reasoning_audit_complete_no_message");
    for parent_state in [
        RequestLifecycleState::Failed,
        RequestLifecycleState::Interrupted,
    ] {
        let fixture = TitleFixture::new().await;
        match parent_state {
            RequestLifecycleState::Failed => fixture.terminalize_parent().await,
            RequestLifecycleState::Interrupted => fixture.interrupt_parent().await,
            _ => unreachable!(),
        }
        let public_head_before = fixture.session_message_count().await;
        let provider = TitleProvider::new(provider_events(&fields, true), false);
        let calls = provider.calls.clone();
        let (_shutdown, rx) = tokio::sync::watch::channel(false);
        fixture
            .task(provider, true)
            .run(fixture.title.clone(), rx)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "parent: {parent_state:?}");
        assert_title_audit(&fixture, &fields, outcome, 1, true).await;
        assert_eq!(
            fixture.session_message_count().await,
            public_head_before,
            "parent: {parent_state:?}"
        );
        assert_eq!(
            fixture.terminal_row().await,
            (
                RequestLifecycleState::Completed,
                Some(gents_protocol::output::TerminalOutput::NoMessage)
            ),
            "parent: {parent_state:?}"
        );
    }
}

#[tokio::test]
async fn title_timeout_drains_each_received_reasoning_attempt_as_partial() {
    let (fields, outcome) = modeled_title_fields("title_live_partial_retains_received_reasoning");
    let fixture = TitleFixture::new().await;
    let provider = TitleProvider::new(provider_events(&fields, false), true);
    let calls = provider.calls.clone();
    let entered = provider.entered.clone();
    let (_shutdown, rx) = tokio::sync::watch::channel(false);
    let task = fixture.task(provider, true);
    let title = fixture.title.clone();
    tokio::time::pause();
    let running = tokio::spawn(async move { task.run(title, rx).await });
    entered.notified().await;
    wait_for_title_streams(&fixture, 1, fields.len()).await;
    tokio::time::advance(Duration::from_secs(10)).await;
    entered.notified().await;
    wait_for_title_streams(&fixture, 2, fields.len()).await;
    tokio::time::advance(Duration::from_secs(10)).await;
    running.await.unwrap().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(outcome, OutputOutcome::Partial);
    assert_title_audit(&fixture, &fields, outcome, 2, false).await;
    assert_eq!(
        fixture.terminal_row().await,
        (
            RequestLifecycleState::Completed,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        )
    );
}

#[tokio::test]
async fn crashed_title_recovers_committed_reasoning_without_publication() {
    let (fields, outcome) = modeled_title_fields("title_expired_unlatched_recovery_no_message");
    let model = crate::lean_vocab_test::lean_contract_snapshot()
        .canonical_execution_gate_cases
        .iter()
        .find(|case| matches!(case, LeanCanonicalExecutionCase::NativeExecution { name, .. } | LeanCanonicalExecutionCase::ModelExecution { name, .. } if name == "title_expired_unlatched_recovery_no_message"))
        .expect("modeled title recovery");
    let (LeanCanonicalExecutionCase::NativeExecution {
        expected_observations,
        ..
    }
    | LeanCanonicalExecutionCase::ModelExecution {
        expected_observations,
        ..
    }) = model
    else {
        unreachable!()
    };
    assert_eq!(
        expected_observations.last().unwrap().request_state,
        "failed"
    );
    assert_eq!(outcome, OutputOutcome::Partial);
    let fixture = TitleFixture::new().await;
    let provider = TitleProvider::new(provider_events(&fields, false), true);
    let entered = provider.entered.clone();
    let (_shutdown, rx) = tokio::sync::watch::channel(false);
    let task = fixture.task(provider, true);
    let title = fixture.title.clone();
    let running = tokio::spawn(async move { task.run(title, rx).await });
    entered.notified().await;
    wait_for_title_streams(&fixture, 1, fields.len()).await;
    let (before, before_messages) = fixture.output_rows().await;
    assert_eq!(before_messages, 0);
    assert!(before.iter().all(|row| row.segment.close.is_none()));
    let before_ids = before
        .iter()
        .map(|row| row.doc_id.clone())
        .collect::<Vec<_>>();

    running.abort();
    assert!(running
        .await
        .expect_err("title task must abort")
        .is_cancelled());
    let (aborted, aborted_messages) = fixture.output_rows().await;
    assert_eq!(aborted_messages, 0);
    assert_eq!(aborted.len(), before.len());
    assert!(aborted.iter().all(|row| row.segment.close.is_none()));
    let physical = crate::graphql::escape_graphql_string(&fixture.title.doc_id);
    let owner = crate::graphql::escape_graphql_string(fixture.identity.did());
    let response = ConfigAccess::Local(fixture.node.clone())
        .execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{physical}" }}, agent_did: {{ _eq: "{owner}" }} }}, limit: 2) {{ execution_generation lifecycle_state }} }}"#))
        .await
        .expect("read title lease owner");
    let rows = response["data"]["AgentRequest"]
        .as_array()
        .expect("title lease rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["lifecycle_state"], "processing");
    let generation = crate::graphql::escape_graphql_string(
        rows[0]["execution_generation"]
            .as_str()
            .expect("title execution generation"),
    );
    let past = crate::graphql::escape_graphql_string(
        &(chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
    );
    let expired = ConfigAccess::Local(fixture.node.clone())
        .write("test.title_expire_own_lease", &format!(r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{physical}" }}, agent_did: {{ _eq: "{owner}" }}, execution_generation: {{ _eq: "{generation}" }}, lifecycle_state: {{ _eq: "processing" }} }}, input: {{ execution_lease_expires_at: "{past}" }}) {{ _docID }} }}"#))
        .await
        .expect("expire exact title generation");
    assert_eq!(
        expired["data"]["update_AgentRequest"][0]["_docID"],
        fixture.title.doc_id
    );

    let first = crate::RequestLifecycle::recover_all(fixture.node.as_ref(), fixture.identity.did())
        .await
        .expect("recover crashed title");
    assert_eq!(first.requests_recovered, 1);
    assert_title_audit(&fixture, &fields, outcome, 1, false).await;
    assert_eq!(
        fixture.terminal_row().await,
        (
            RequestLifecycleState::Failed,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        )
    );
    assert_eq!(fixture.session_message_count().await, 0);
    let (after, _) = fixture.output_rows().await;
    assert_eq!(
        after.len(),
        before_ids.len() + 1,
        "recovery adds one exact closure"
    );
    assert!(before_ids
        .iter()
        .all(|id| after.iter().any(|row| &row.doc_id == id)));

    let replay =
        crate::RequestLifecycle::recover_all(fixture.node.as_ref(), fixture.identity.did())
            .await
            .expect("repeat title recovery");
    assert_eq!(replay.requests_recovered, 0);
    assert_title_audit(&fixture, &fields, outcome, 1, false).await;
    assert_eq!(fixture.output_rows().await.0.len(), after.len());
}

#[tokio::test]
async fn missing_title_capture_fails_before_provider_dispatch() {
    let (fields, _) = modeled_title_fields("title_reasoning_audit_complete_no_message");
    let fixture = TitleFixture::new().await;
    let provider = TitleProvider::new(provider_events(&fields, true), false);
    let calls = provider.calls.clone();
    let (_shutdown, rx) = tokio::sync::watch::channel(false);
    assert!(fixture
        .task(provider, false)
        .run(fixture.title.clone(), rx)
        .await
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(fixture.output_rows().await.0.is_empty());
    assert_eq!(
        fixture.terminal_row().await,
        (
            RequestLifecycleState::Failed,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        )
    );
}

#[tokio::test(flavor = "current_thread")]
async fn title_audit_is_dispatched_once_by_watcher_router_and_daemon() {
    let capture = TitleWarningCapture::default();
    let warnings = capture.0.clone();
    let subscriber = tracing::Dispatch::new(Registry::default().with(capture));
    let _subscriber_guard = tracing::dispatcher::set_default(&subscriber);
    let (fields, outcome) = modeled_title_fields("title_reasoning_audit_complete_no_message");
    for preexisting in [false, true] {
        let mut fixture = TitleFixture::new().await;
        fixture.terminalize_parent().await;
        if !preexisting {
            crate::request_admission::terminalize_pending_request_rejection(
                fixture.node.as_ref(),
                &fixture.title.doc_id,
                &fixture.title.agent_did,
                "retire setup title before live creation",
                "test.title_live_creation_setup",
            )
            .await
            .unwrap();
        }

        let provider = TitleProvider::new(provider_events(&fields, true), false);
        let calls = provider.calls.clone();
        let prompt_builder = LayeredPromptBuilder::for_behavior(
            &fixture.behavior.system_prompt,
            &fixture.behavior.behavior_id,
            &[],
            false,
            &[],
        );
        let preamble = prompt_builder.preamble().to_string();
        let (status_owner, status) = RuntimeStatusHandle::start_with_unbounded_test_clock(
            fixture.node.clone(),
            fixture.identity.did().to_owned(),
        );
        status.initialize_startup("general").await.unwrap();
        status
            .readiness()
            .register_slot("general", 1)
            .await
            .unwrap();
        let (dispatch, receiver) = tokio::sync::mpsc::channel(8);
        let snapshot = Arc::new(ActiveRuntimeSnapshot {
            generation: 1,
            principal: None,
            local_did: String::new(),
            default_behavior_id: "general".into(),
            behaviors: Default::default(),
            tool_surfaces: Default::default(),
            backend_admission_configs: Default::default(),
            unavailable_behaviors: Default::default(),
            active_schedules: Default::default(),
            unavailable_schedules: Default::default(),
            active_event_triggers: Default::default(),
            unavailable_event_triggers: Default::default(),
            active_tasks: Default::default(),
            dispatchers: std::collections::HashMap::from([("general".into(), dispatch)]),
            behavior_executor_capacities: Default::default(),
            behavior_executor_queue_capacities: Default::default(),
        });
        status
            .readiness()
            .publish_snapshot(snapshot.as_ref())
            .await
            .unwrap();
        status
            .set_process_state_durable(crate::agent::ProcessLifecycleState::Ready)
            .await
            .unwrap();
        let (_snapshot_tx, snapshot_rx) = tokio::sync::watch::channel(snapshot);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let mut daemon = BehaviorDaemon::new(
            fixture.node.clone(),
            fixture.behavior.clone(),
            None,
            Arc::new(provider),
            preamble,
            Arc::new(Vec::new()),
            prompt_builder,
            FailurePolicy::default(),
            Some(
                crate::rendered_request::defra_rendered_request_capture_factory(
                    fixture.node.clone(),
                ),
            ),
            BackgroundToolRegistry::default(),
            BackgroundExecutionRegistry::default(),
            Arc::new(StartupBarrier::ready_for_test()),
            status.clone(),
            1,
            crate::request_admission::AgentRequestAdmissionVerifier::new(
                fixture.node.clone(),
                fixture.identity.clone(),
                crate::agent::p2p_reconcile::enrollment_authority_channel().1,
            ),
        )
        .unwrap();
        let daemon_rx = Arc::new(tokio::sync::Mutex::new(receiver));
        let daemon_shutdown = shutdown_rx.clone();
        let gate = RuntimeAdmissionGate::closed();
        gate.open().await;
        let router_gate = gate.clone();
        let router_node = fixture.node.clone();
        let router_did = fixture.identity.did().to_owned();
        let router_task = tokio::spawn(async move {
            run_router_with_watcher(
                router_node.clone(),
                router_did.clone(),
                DefraWatcher::new(router_node, &router_did),
                snapshot_rx,
                shutdown_rx,
                router_gate,
                status,
                None,
            )
            .await
        });

        if !preexisting {
            daemon.spawn_conversation_title_generation(&fixture.parent);
            let old_doc_id = fixture.title.doc_id.clone();
            let session = crate::graphql::escape_graphql_string(&fixture.parent.session_id);
            let purpose =
                crate::graphql::escape_graphql_string(RequestPurpose::TitleAudit.as_str());
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                let response = ConfigAccess::Local(fixture.node.clone())
                    .execute(&format!("{{ AgentRequest(filter: {{ session_id: {{ _eq: \"{session}\" }}, purpose: {{ _eq: \"{purpose}\" }} }}) {{ _docID }} }}"))
                    .await
                    .unwrap();
                let next = response["data"]["AgentRequest"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|row| row["_docID"].as_str())
                    .find(|doc_id| *doc_id != old_doc_id);
                if let Some(doc_id) = next {
                    fixture.title = crate::request_admission::load_request_for_admission_test(
                        &fixture.node,
                        doc_id,
                    )
                    .await
                    .unwrap();
                    break;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "title creator did not persist a request"
                );
                tokio::task::yield_now().await;
            }
        }
        if !preexisting {
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "creator dispatched before daemon"
            );
        }
        let daemon_task = tokio::spawn(async move { daemon.run(daemon_rx, daemon_shutdown).await });

        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if fixture.terminal_row().await.0 == RequestLifecycleState::Completed {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "watcher did not complete title audit"
            );
            tokio::task::yield_now().await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "preexisting={preexisting}");
        assert_title_audit(&fixture, &fields, outcome, 1, true).await;
        assert_eq!(
            fixture.terminal_row().await,
            (
                RequestLifecycleState::Completed,
                Some(gents_protocol::output::TerminalOutput::NoMessage)
            )
        );
        shutdown_tx.send(true).unwrap();
        gate.close().await;
        tokio::time::timeout(Duration::from_secs(5), router_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), daemon_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        status_owner.close().await.unwrap();
        assert!(
            warnings.lock().unwrap().is_empty(),
            "title owner emitted WARN"
        );
    }
}

async fn start_owned_title_daemon(
    fixture: &TitleFixture,
    provider: TitleProvider,
) -> (
    tokio::sync::mpsc::Sender<AgentRequest>,
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
    crate::runtime_status::RuntimeStatusOwner,
) {
    let prompt_builder = LayeredPromptBuilder::for_behavior(
        &fixture.behavior.system_prompt,
        &fixture.behavior.behavior_id,
        &[],
        false,
        &[],
    );
    let preamble = prompt_builder.preamble().to_string();
    let (status_owner, status) = RuntimeStatusHandle::start_with_unbounded_test_clock(
        fixture.node.clone(),
        fixture.identity.did().to_owned(),
    );
    status.initialize_startup("general").await.unwrap();
    status
        .readiness()
        .register_slot("general", 1)
        .await
        .unwrap();
    let (dispatch, receiver) = tokio::sync::mpsc::channel(8);
    let snapshot = ActiveRuntimeSnapshot {
        generation: 1,
        principal: None,
        local_did: String::new(),
        default_behavior_id: "general".into(),
        behaviors: Default::default(),
        tool_surfaces: Default::default(),
        backend_admission_configs: Default::default(),
        unavailable_behaviors: Default::default(),
        active_schedules: Default::default(),
        unavailable_schedules: Default::default(),
        active_event_triggers: Default::default(),
        unavailable_event_triggers: Default::default(),
        active_tasks: Default::default(),
        dispatchers: std::collections::HashMap::from([("general".into(), dispatch.clone())]),
        behavior_executor_capacities: Default::default(),
        behavior_executor_queue_capacities: Default::default(),
    };
    status
        .readiness()
        .publish_snapshot(&snapshot)
        .await
        .unwrap();
    status
        .set_process_state_durable(crate::agent::ProcessLifecycleState::Ready)
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let mut daemon = BehaviorDaemon::new(
        fixture.node.clone(),
        fixture.behavior.clone(),
        None,
        Arc::new(provider),
        preamble,
        Arc::new(Vec::new()),
        prompt_builder,
        FailurePolicy::default(),
        Some(crate::rendered_request::defra_rendered_request_capture_factory(fixture.node.clone())),
        BackgroundToolRegistry::default(),
        BackgroundExecutionRegistry::default(),
        Arc::new(StartupBarrier::ready_for_test()),
        status,
        1,
        crate::request_admission::AgentRequestAdmissionVerifier::new(
            fixture.node.clone(),
            fixture.identity.clone(),
            crate::agent::p2p_reconcile::enrollment_authority_channel().1,
        ),
    )
    .unwrap();
    let task = tokio::spawn(async move {
        daemon
            .run(Arc::new(tokio::sync::Mutex::new(receiver)), shutdown_rx)
            .await
    });
    (dispatch, shutdown_tx, task, status_owner)
}

fn buffered_reasoning_title_provider() -> (
    TitleProvider,
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Notify>,
) {
    let events = ["first", " second"]
        .into_iter()
        .map(|text| RawStreamingChoice::Reasoning {
            id: None,
            content: crate::llm::rig_compat::to_rig_reasoning_part(&ReasoningContent::Text {
                text: text.into(),
                signature: None,
            }),
        })
        .collect();
    let before_second = Arc::new(tokio::sync::Notify::new());
    let third_poll = Arc::new(tokio::sync::Notify::new());
    let mut provider = TitleProvider::new(events, false);
    provider.before_second = Some(before_second.clone());
    provider.third_poll = Some(third_poll.clone());
    (provider, before_second, third_poll)
}

async fn wait_for_first_title_reasoning(fixture: &TitleFixture) {
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let (rows, _) = fixture.output_rows().await;
            if rows.iter().any(|row| !row.segment.runs.is_empty()) {
                assert_eq!(open_title_reasoning(fixture).await, ["first"]);
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("first title reasoning never became durable");
}

async fn open_title_reasoning(fixture: &TitleFixture) -> Vec<String> {
    let (rows, message_count) = fixture.output_rows().await;
    assert_eq!(message_count, 0, "title published a public header");
    let source = rows
        .iter()
        .find_map(|row| match &row.segment.source {
            OutputSource::ProviderTurn { scope, .. }
                if scope.kind == crate::rendered_request::CaptureScopeKind::Title =>
            {
                Some(row.segment.source.clone())
            }
            _ => None,
        })
        .expect("title provider source");
    let writer = rows
        .iter()
        .find(|row| row.segment.source == source)
        .unwrap()
        .segment
        .writer
        .clone();
    let observed = rows
        .iter()
        .filter(|row| row.segment.source == source)
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    reconstruct_dense_prefix(&observed, &fixture.title.doc_id, &source, &writer, None)
        .unwrap()
        .streams
        .into_iter()
        .map(|stream| {
            assert_eq!(stream.declaration.payload, StreamPayload::Reasoning);
            stream.text
        })
        .collect()
}

#[tokio::test]
async fn owned_title_shutdown_drains_buffered_reasoning_before_join() {
    let mut fixture = TitleFixture::new().await;
    Arc::get_mut(&mut fixture.behavior).unwrap().stream_batch_ms = 5_000;
    fixture.terminalize_parent().await;
    let (provider, before_second, third_poll) = buffered_reasoning_title_provider();
    let calls = provider.calls.clone();
    let (dispatch, shutdown, daemon, status_owner) =
        start_owned_title_daemon(&fixture, provider).await;
    dispatch.send(fixture.title.clone()).await.unwrap();
    wait_for_first_title_reasoning(&fixture).await;
    before_second.notify_one();
    tokio::time::timeout(Duration::from_secs(10), third_poll.notified())
        .await
        .expect("title provider did not process both reasoning chunks");
    assert_eq!(open_title_reasoning(&fixture).await, ["first"]);
    assert_eq!(
        fixture.terminal_row().await.0,
        RequestLifecycleState::Processing
    );
    shutdown.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("title daemon did not join its title task")
        .unwrap()
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_title_audit(
        &fixture,
        &[
            (StreamPayload::Reasoning, "first".into(), 0, 0),
            (StreamPayload::Reasoning, " second".into(), 0, 1),
        ],
        OutputOutcome::Partial,
        1,
        false,
    )
    .await;
    assert_eq!(
        fixture.terminal_row().await,
        (
            RequestLifecycleState::Interrupted,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        )
    );
    status_owner.close().await.unwrap();
}

#[tokio::test]
async fn owned_title_shutdown_reports_fenced_partial_without_terminalizing() {
    let mut fixture = TitleFixture::new().await;
    Arc::get_mut(&mut fixture.behavior).unwrap().stream_batch_ms = 5_000;
    fixture.terminalize_parent().await;
    let (provider, before_second, third_poll) = buffered_reasoning_title_provider();
    let calls = provider.calls.clone();
    let (dispatch, shutdown, daemon, status_owner) =
        start_owned_title_daemon(&fixture, provider).await;
    dispatch.send(fixture.title.clone()).await.unwrap();
    wait_for_first_title_reasoning(&fixture).await;
    before_second.notify_one();
    tokio::time::timeout(Duration::from_secs(10), third_poll.notified())
        .await
        .expect("title provider did not process both reasoning chunks");
    assert_eq!(open_title_reasoning(&fixture).await, ["first"]);
    let (before, before_messages) = fixture.output_rows().await;
    assert_eq!(before_messages, 0);
    assert!(before.iter().all(|row| row.segment.close.is_none()));
    let doc = crate::graphql::escape_graphql_string(&fixture.title.doc_id);
    let owner = crate::graphql::escape_graphql_string(&fixture.title.agent_did);
    ConfigAccess::Local(fixture.node.clone())
        .write(
            "test.title_shutdown_fence_generation",
            &format!(r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }}, agent_did: {{ _eq: "{owner}" }} }}, input: {{ execution_generation: "fenced-title-generation" }}) {{ _docID }} }}"#),
        )
        .await
        .unwrap();
    shutdown.send(true).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("title daemon did not join its title task")
        .unwrap()
        .expect_err("fenced partial must surface through daemon join");
    assert!(
        error.is::<super::super::ShutdownDrainFailure>(),
        "{error:#}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.terminal_row().await,
        (RequestLifecycleState::Processing, None)
    );
    let (after, after_messages) = fixture.output_rows().await;
    assert_eq!(after_messages, 0);
    let mut before = before
        .into_iter()
        .map(|row| (row.doc_id, row.segment))
        .collect::<Vec<_>>();
    let mut after = after
        .into_iter()
        .map(|row| (row.doc_id, row.segment))
        .collect::<Vec<_>>();
    before.sort_by(|left, right| left.0.cmp(&right.0));
    after.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(after, before, "failed drain wrote or closed title output");
    status_owner.close().await.unwrap();
}

#[tokio::test]
async fn reasoning_only_title_retains_each_attempt_and_uses_bounded_fallback() {
    let capture = TitleWarningCapture::default();
    let warnings = capture.0.clone();
    let subscriber = tracing::Dispatch::new(Registry::default().with(capture));
    let _subscriber_guard = tracing::dispatcher::set_default(&subscriber);
    let (fields, outcome) = modeled_title_fields("title_live_partial_retains_received_reasoning");
    assert_eq!(outcome, OutputOutcome::Partial);
    let fixture = TitleFixture::new().await;
    let provider = TitleProvider::new(reasoning_only_events(&fields), false);
    let calls = provider.calls.clone();
    let (_shutdown, rx) = tokio::sync::watch::channel(false);
    fixture
        .task(provider, true)
        .run(fixture.title.clone(), rx)
        .await
        .expect("reasoning-only title is an ordinary provider result");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        usize::try_from(super::TITLE_GENERATION_MAX_ATTEMPTS).unwrap()
    );
    assert_title_audit(&fixture, &fields, outcome, 2, false).await;

    let doc = crate::graphql::escape_graphql_string(&fixture.title.doc_id);
    let response = ConfigAccess::Local(fixture.node.clone())
        .execute(&format!("{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{doc}\" }} }}, limit: 1) {{ execution_generation }} }}"))
        .await
        .unwrap();
    let generation = response["data"]["AgentRequest"][0]["execution_generation"]
        .as_str()
        .expect("title execution generation")
        .to_owned();
    let (rows, _) = fixture.output_rows().await;
    assert!(rows.iter().all(|row| matches!(
        &row.segment.writer,
        OutputWriter::RequestExecution { execution_generation } if *execution_generation == generation
    )));

    assert_eq!(
        fixture.terminal_row().await,
        (
            RequestLifecycleState::Completed,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        )
    );
    let session = crate::graphql::escape_graphql_string(&fixture.parent.session_id);
    let response = ConfigAccess::Local(fixture.node.clone())
        .execute(&format!(
            "{{ AgentSession(filter: {{ session_id: {{ _eq: \"{session}\" }} }}) {{ title }} }}"
        ))
        .await
        .unwrap();
    let title = &response["data"]["AgentSession"][0]["title"];
    assert_eq!(
        title["text"],
        super::sanitize_generated_title("", &fixture.parent.content),
        "fallback title: {title}"
    );
    assert_eq!(
        warnings.lock().unwrap().len(),
        usize::try_from(super::TITLE_GENERATION_MAX_ATTEMPTS).unwrap(),
        "only the bounded attempt warnings are emitted"
    );
}

/// #2121 item 5: a title on a usage-limited account records each limited
/// call and falls back to the message title; the turn it names is untouched.
#[tokio::test]
async fn a_usage_limited_title_falls_back_without_failing_the_turn() {
    let fixture = TitleFixture::new().await;
    let backend = fixture.behavior.backend_id.clone().unwrap();
    let registry = crate::admission::AdmissionRegistry::new(fixture.node.clone());
    registry.reconcile(
        1,
        &std::collections::HashMap::from([(
            backend.clone(),
            crate::admission::BackendAdmissionConfig {
                backend_id: backend.clone(),
                max_concurrent: 1,
                max_queue_depth: 1,
                enabled: true,
                probe_status: "healthy".into(),
                measured_unhealthy: false,
                config_fingerprint: backend.clone(),
            },
        )]),
    );
    let resets_at = chrono::Utc::now() + chrono::Duration::hours(2);
    let mut provider = TitleProvider::new(Vec::new(), false);
    provider.error = Some(format!(
        r#"Invalid status code 429 Too Many Requests with message: {{"error":{{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_at":{}}}}}"#,
        resets_at.timestamp()
    ));
    let calls = provider.calls.clone();
    let model = crate::admission::AdmittedCompletionModel::for_test(
        provider,
        registry,
        &format!("{backend}:connection"),
    );
    let parent_state = |node: Arc<EmbeddedNode>, doc: String| async move {
        let doc = crate::graphql::escape_graphql_string(&doc);
        ConfigAccess::Local(node)
            .execute(&format!("{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{doc}\" }} }}, limit: 1) {{ lifecycle_state }} }}"))
            .await
            .unwrap()["data"]["AgentRequest"][0]["lifecycle_state"]
            .clone()
    };
    let parent_before = parent_state(fixture.node.clone(), fixture.parent.doc_id.clone()).await;

    let (_shutdown, rx) = tokio::sync::watch::channel(false);
    fixture
        .task(model, true)
        .run(fixture.title.clone(), rx)
        .await
        .expect("a usage-limited title is an ordinary provider failure");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        usize::try_from(super::TITLE_GENERATION_MAX_ATTEMPTS).unwrap()
    );
    let request_id = crate::graphql::escape_graphql_string(&fixture.title.request_id);
    let response = ConfigAccess::Local(fixture.node.clone())
        .execute(&format!("{{ InferenceCall(filter: {{ request_id: {{ _eq: \"{request_id}\" }} }}) {{ call_kind call_state failure_reason }} }}"))
        .await
        .unwrap();
    let rows = response["data"]["InferenceCall"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    for row in rows {
        assert_eq!(row["call_kind"], "oneoff", "{row}");
        assert_eq!(row["call_state"], "failed", "{row}");
        let reason = row["failure_reason"].as_str().unwrap();
        assert!(
            reason.starts_with("provider usage limit reached (resets at "),
            "{reason}"
        );
        let Some(gents_loop::provider_limit::ProviderLimit::UsageExhausted(limit)) =
            gents_loop::provider_limit::classify_provider_limit(reason, chrono::Utc::now())
        else {
            panic!("recorded reason is not a usage limit: {reason}");
        };
        assert_eq!(
            limit.resets_at.map(|at| at.timestamp()),
            Some(resets_at.timestamp())
        );
    }

    assert_eq!(
        fixture.terminal_row().await,
        (
            RequestLifecycleState::Completed,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        )
    );
    let session = crate::graphql::escape_graphql_string(&fixture.parent.session_id);
    let response = ConfigAccess::Local(fixture.node.clone())
        .execute(&format!(
            "{{ AgentSession(filter: {{ session_id: {{ _eq: \"{session}\" }} }}) {{ title }} }}"
        ))
        .await
        .unwrap();
    assert_eq!(
        response["data"]["AgentSession"][0]["title"]["text"],
        super::sanitize_generated_title("", &fixture.parent.content)
    );
    assert_eq!(
        parent_state(fixture.node.clone(), fixture.parent.doc_id.clone()).await,
        parent_before
    );
}

#[tokio::test]
async fn fenced_title_output_fails_closed_without_retry() {
    let (fields, _) = modeled_title_fields("title_live_partial_retains_received_reasoning");
    let fixture = TitleFixture::new().await;
    let mut provider = TitleProvider::new(reasoning_only_events(&fields), false);
    provider.fence_generation = Some((
        fixture.node.clone(),
        fixture.title.doc_id.clone(),
        fixture.title.agent_did.clone(),
    ));
    let calls = provider.calls.clone();
    let (_shutdown, rx) = tokio::sync::watch::channel(false);
    let error = fixture
        .task(provider, true)
        .run(fixture.title.clone(), rx)
        .await
        .expect_err("a fenced auxiliary writer must fail the title");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "storage failure retried");
    assert!(
        format!("{error:#}").contains("lost its live processing lease"),
        "{error:#}"
    );
    let session = crate::graphql::escape_graphql_string(&fixture.parent.session_id);
    let response = ConfigAccess::Local(fixture.node.clone())
        .execute(&format!(
            "{{ AgentSession(filter: {{ session_id: {{ _eq: \"{session}\" }} }}) {{ title }} }}"
        ))
        .await
        .unwrap();
    assert!(response["data"]["AgentSession"][0]["title"].is_null());
}

#[tokio::test]
async fn title_output_cap_applies_only_when_reasoning_is_disabled_on_the_wire() {
    use crate::config::ReasoningEffort;
    use crate::OpenAiWireApi;
    let fixture = TitleFixture::new().await;
    let configured = Some(1024);
    let capped = Some(super::TITLE_VISIBLE_MAX_TOKENS);
    let mut behavior = (*fixture.behavior).clone();
    let chat = OpenAiWireApi::ChatCompletions;
    let responses = OpenAiWireApi::Responses;
    for (kind, wire, effort, expected) in [
        (
            BackendProviderKind::OpenAiCompatible,
            chat,
            None,
            configured,
        ),
        (
            BackendProviderKind::OpenAiCompatible,
            chat,
            Some(ReasoningEffort::Low),
            configured,
        ),
        (
            BackendProviderKind::OpenAiCompatible,
            chat,
            Some(ReasoningEffort::None),
            capped,
        ),
        (
            BackendProviderKind::OpenAiCompatible,
            responses,
            None,
            configured,
        ),
        (
            BackendProviderKind::OpenAiCompatible,
            responses,
            Some(ReasoningEffort::High),
            configured,
        ),
        (
            BackendProviderKind::OpenAiCompatible,
            responses,
            Some(ReasoningEffort::None),
            capped,
        ),
        (
            BackendProviderKind::OpenRouter,
            chat,
            Some(ReasoningEffort::None),
            capped,
        ),
        (
            BackendProviderKind::ChatGptCodex,
            responses,
            None,
            configured,
        ),
        (
            BackendProviderKind::ChatGptCodex,
            responses,
            Some(ReasoningEffort::None),
            capped,
        ),
        (
            BackendProviderKind::XaiGrokOAuth,
            chat,
            Some(ReasoningEffort::None),
            configured,
        ),
        (
            BackendProviderKind::ClaudeCliSubscription,
            chat,
            Some(ReasoningEffort::None),
            configured,
        ),
    ] {
        behavior.backend_provider_kind = kind;
        behavior.openai_wire_api = wire;
        behavior.sampling.reasoning_effort = effort;
        let config = crate::completion_factory::loop_config(
            &behavior,
            super::title_generation_preamble(),
            0,
            crate::rendered_request::CaptureScopeKind::Title,
        );
        assert_eq!(
            super::title_max_tokens(config.additional_params.as_ref(), configured),
            expected,
            "{kind:?} {wire:?} {effort:?}"
        );
    }

    // A later merge that re-enables thinking wins over the profile's
    // reasoning-off setting, so the visible-title cap no longer applies.
    behavior.backend_provider_kind = BackendProviderKind::OpenAiCompatible;
    behavior.openai_wire_api = chat;
    behavior.sampling.reasoning_effort = Some(ReasoningEffort::None);
    let config = crate::completion_factory::loop_config(
        &behavior,
        super::title_generation_preamble(),
        0,
        crate::rendered_request::CaptureScopeKind::Title,
    );
    let overridden = crate::completion_factory::merge_optional_params(
        config.additional_params,
        Some(serde_json::json!({ "chat_template_kwargs": { "enable_thinking": true } })),
    );
    assert_eq!(
        super::title_max_tokens(overridden.as_ref(), configured),
        configured
    );
}
