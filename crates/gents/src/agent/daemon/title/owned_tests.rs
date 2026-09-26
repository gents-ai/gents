use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use defra_node::EmbeddedNode;
use futures::{stream, StreamExt};
use gents_protocol::message::ReasoningContent;
use gents_protocol::output::live::reconstruct_audit_prefix;
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::{
    OutputOutcome, OutputSource, OutputWriter, SourceClose, StreamPayload,
};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};

use super::TitleTask;
use crate::agent::completion_retry::CompletionRetryProfileFields;
use crate::backend_provider::BackendProviderKind;
use crate::config::{ResolvedBehavior, SamplingConfig};
use crate::config_client::ConfigAccess;
use crate::identity::{AgentIdentity, KeyIdentity, RuntimePrincipal};
use crate::lean_vocab_test::{
    LeanCanonicalExecutionCase, LeanCanonicalExecutionOperation, LeanCanonicalSource,
    LeanPayloadKind, LeanTerminalSelection,
};
use crate::session::canonical_rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
use crate::tool_surface::BehaviorToolConfig;
use crate::watcher::AgentRequest;

#[derive(Clone)]
struct TitleProvider {
    events: Arc<Vec<RawStreamingChoice<()>>>,
    stall: bool,
    calls: Arc<AtomicUsize>,
    entered: Arc<tokio::sync::Notify>,
}

impl TitleProvider {
    fn new(events: Vec<RawStreamingChoice<()>>, stall: bool) -> Self {
        Self {
            events: Arc::new(events),
            stall,
            calls: Arc::new(AtomicUsize::new(0)),
            entered: Arc::new(tokio::sync::Notify::new()),
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
        self.entered.notify_one();
        let items = self.events.iter().cloned().map(Ok::<_, CompletionError>);
        let inner: rig::streaming::StreamingResult<()> = if self.stall {
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

    fn task(&self, provider: TitleProvider, capture: bool) -> TitleTask<TitleProvider> {
        TitleTask {
            node: self.node.clone(),
            behavior: self.behavior.clone(),
            model: Arc::new(provider),
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
    let case = crate::lean_vocab_test::lean_contract_snapshot().canonical_execution_gate_cases.iter().find(|case| matches!(case, LeanCanonicalExecutionCase::ModelExecution { name: found, .. } if found == name)).expect("Lean title script");
    let LeanCanonicalExecutionCase::ModelExecution {
        operations,
        expected_observations,
        ..
    } = case
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
        let prefix = reconstruct_audit_prefix(
            &observed,
            &fixture.title.doc_id,
            &output_source,
            &writer,
            None,
        )
        .unwrap();
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
                reconstruct_audit_prefix(&observed, &fixture.title.doc_id, &source, &writer, None)
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
