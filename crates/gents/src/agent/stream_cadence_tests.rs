//! Segment cadence of streamed reasoning through the real processor and
//! auxiliary sink, on a simulated batch clock, with the persisted bytes
//! reconstructed afterwards.

use std::path::PathBuf;

use super::*;
use crate::session::canonical_rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
use gents_loop::provider_audit::{
    AuxiliaryOutputEvent, AuxiliaryOutputObservation, ClaudeAuditEvent, ClaudeBlockKind,
    ProviderAuditObservation,
};
use gents_loop::stream_processor::AssistantTurnAccumulator;
use gents_protocol::output::live::reconstruct_dense_prefix;
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::{OutputSource, OutputWriter, StreamPayload};
use gents_protocol::rendered_request::CaptureScope;

const SECONDS: u32 = 20;
const SIGNATURE: &str = "signature-bytes";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Stream {
    Plain,
    ClaudeAudit,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum End {
    Complete,
    Cancel,
}

struct Persisted {
    flushes: usize,
    reasoning: String,
    signature: String,
}

fn fragment(index: u32) -> String {
    format!("step {index} of the plan; ")
}

fn expected_reasoning(chunks_per_second: u32) -> String {
    (0..SECONDS * chunks_per_second).map(fragment).collect()
}

fn audit_item(
    scope: CaptureScope,
    event: ClaudeAuditEvent,
) -> Result<LoopStreamItem, crate::error::LoopStreamError> {
    Ok(LoopStreamItem::ProviderAudit(ProviderAuditObservation {
        capture_scope: scope,
        turn: 0,
        attempt: 0,
        event,
    }))
}

async fn fixture(
    name: &str,
) -> (
    Arc<defra_node::EmbeddedNode>,
    PathBuf,
    DefraSessionHook,
    RequestLifecycle,
    DefraStreamWriter,
) {
    let data_path =
        std::env::temp_dir().join(format!("stream-cadence-{name}-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        crate::test_support::node_builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_runtime_schemas(&node).await.unwrap();
    let hook =
        DefraSessionHook::with_identity(node.clone(), "did:test:test", FailurePolicy::default());
    assert!(matches!(
        hook.on_completion_call(&user_text_message(name), &[]).await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.unwrap();
    let request_id = uuid::Uuid::new_v4().to_string();
    let request_doc_id = create_pending_request(&node, &request_id, &session_id).await;
    hook.set_active_request_lineage(Some(request_id.clone()), None)
        .await
        .unwrap();
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::seconds(60)))
        .await;
    let request = fixture_agent_request(request_doc_id, &request_id, &session_id, name);
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "test-agent",
        "did:test:test",
        request,
        30,
        ExecutionOrigin::Interactive,
        "test-backend",
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let writer = DefraStreamWriter::new(
        node.clone(),
        "did:test:test",
        Duration::from_millis(crate::config::DEFAULT_STREAM_BATCH_MS),
    );
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    (node, data_path, hook, lifecycle, writer)
}

async fn persisted(
    node: &defra_node::EmbeddedNode,
    request_doc_id: &str,
    source: &OutputSource,
    writer: &OutputWriter,
) -> Persisted {
    let result = node
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
            crate::graphql::escape_graphql_string(request_doc_id)
        ))
        .await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    let records = result.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| decode_output_segment_row(row).unwrap())
        .collect::<Vec<_>>();
    let flushes = records
        .iter()
        .filter(|row| row.segment.source == *source && row.segment.ordinal.is_some())
        .count();
    let facts = records
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let prefix = reconstruct_dense_prefix(&facts, request_doc_id, source, writer, None).unwrap();
    let text = |payload: StreamPayload| {
        prefix
            .streams
            .iter()
            .filter(|stream| stream.declaration.payload == payload)
            .map(|stream| stream.text.as_str())
            .collect::<String>()
    };
    Persisted {
        flushes,
        reasoning: text(StreamPayload::Reasoning),
        signature: text(StreamPayload::ReasoningSignature),
    }
}

/// Streams reasoning through the owned processor the way the inference loop
/// does: each chunk is processed, then a due batch is flushed.
async fn processor_stream(stream: Stream, end: End, chunks_per_second: u32) -> Persisted {
    let (node, data_path, hook, mut lifecycle, writer) =
        fixture(&format!("processor-{stream:?}-{end:?}")).await;
    let doc_id = lifecycle.request().doc_id.clone();
    let generation = lifecycle.execution_generation().unwrap().to_owned();
    let scope: CaptureScope = "inference.1".parse().unwrap();
    let profile = match stream {
        Stream::Plain => ProviderInputProfile::OpenAiChatCompletions,
        Stream::ClaudeAudit => ProviderInputProfile::ClaudeMessages,
    };
    let mut processor = StreamProcessor::new(&hook, &writer, &mut lifecycle, &doc_id, profile);
    processor
        .process_item(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 0,
            attempt: 0,
            capture_scope: scope,
        }))
        .await
        .unwrap();
    if stream == Stream::ClaudeAudit {
        let start = ClaudeAuditEvent::BlockStart {
            index: 0,
            kind: ClaudeBlockKind::Thinking,
        };
        processor
            .process_item(audit_item(scope, start))
            .await
            .unwrap();
    }
    let step = Duration::from_secs(1) / chunks_per_second;
    for index in 0..SECONDS * chunks_per_second {
        let item = match stream {
            Stream::Plain => Ok(LoopStreamItem::ReasoningDelta {
                id: None,
                reasoning: fragment(index),
            }),
            Stream::ClaudeAudit => audit_item(
                scope,
                ClaudeAuditEvent::ThinkingText {
                    index: 0,
                    fragment: fragment(index),
                },
            ),
        };
        processor.process_item(item).await.unwrap();
        writer.advance_batch_clock(&doc_id, step).await;
        let due = processor.next_flush_deadline().await;
        if due.is_some_and(|due| due <= tokio::time::Instant::now()) {
            processor.flush_pending().await.unwrap();
        }
    }
    if stream == Stream::ClaudeAudit {
        let signature = ClaudeAuditEvent::Signature {
            index: 0,
            fragment: SIGNATURE.into(),
        };
        processor
            .process_item(audit_item(scope, signature))
            .await
            .unwrap();
    }
    match end {
        End::Complete => {
            let message = processor.assistant_turn.clone().take_message().unwrap();
            processor
                .process_item(Ok(LoopStreamItem::ProviderTurnReady {
                    turn: 0,
                    attempt: 0,
                    message,
                }))
                .await
                .unwrap();
        }
        End::Cancel => {
            processor
                .persist_partial_turn("cancel reasoning stream")
                .await
                .unwrap();
        }
    }
    drop(processor);
    let source = OutputSource::ProviderTurn {
        scope,
        turn_index: 0,
        attempt: 0,
    };
    let output_writer = OutputWriter::RequestExecution {
        execution_generation: generation,
    };
    let result = persisted(&node, &doc_id, &source, &output_writer).await;
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(data_path);
    result
}

/// Streams Claude thinking audits through the auxiliary sink, driving its
/// deadline the way the auxiliary loop does.
async fn auxiliary_stream(end: End, chunks_per_second: u32) -> Persisted {
    let (node, data_path, _hook, lifecycle, _writer) = fixture(&format!("auxiliary-{end:?}")).await;
    let request = lifecycle.request().clone();
    let generation = lifecycle.execution_generation().unwrap().to_owned();
    let scope: CaptureScope = "compaction.1".parse().unwrap();
    let auxiliary_writer = Arc::new(DefraStreamWriter::new(
        node.clone(),
        "did:test:test",
        Duration::from_millis(crate::config::DEFAULT_STREAM_BATCH_MS),
    ));
    let sink = crate::streaming::auxiliary::sink_with_writer(
        Arc::clone(&auxiliary_writer),
        request.clone(),
        generation.clone(),
        ProviderInputProfile::ClaudeMessages,
    );
    let observe = |event| {
        (sink.observe)(AuxiliaryOutputObservation {
            capture_scope: scope,
            turn: 0,
            attempt: 0,
            event,
        })
    };
    let mut expected = AssistantTurnAccumulator::default();
    let mut audit = |event: ClaudeAuditEvent| {
        expected.apply_provider_audit(&event).unwrap();
        AuxiliaryOutputEvent::Audit(event)
    };
    observe(AuxiliaryOutputEvent::AttemptStarted).await.unwrap();
    observe(audit(ClaudeAuditEvent::BlockStart {
        index: 0,
        kind: ClaudeBlockKind::Thinking,
    }))
    .await
    .unwrap();
    let step = Duration::from_secs(1) / chunks_per_second;
    for index in 0..SECONDS * chunks_per_second {
        observe(audit(ClaudeAuditEvent::ThinkingText {
            index: 0,
            fragment: fragment(index),
        }))
        .await
        .unwrap();
        auxiliary_writer
            .advance_batch_clock(&request.doc_id, step)
            .await;
        let due = (sink.next_flush_deadline)(scope, 0, 0).await.unwrap();
        if due.is_some_and(|due| due <= tokio::time::Instant::now()) {
            (sink.flush_pending)(scope, 0, 0).await.unwrap();
        }
    }
    observe(audit(ClaudeAuditEvent::Signature {
        index: 0,
        fragment: SIGNATURE.into(),
    }))
    .await
    .unwrap();
    let close = match end {
        End::Complete => AuxiliaryOutputEvent::TurnReady {
            message: expected.take_message().unwrap(),
        },
        End::Cancel => AuxiliaryOutputEvent::ClosePartial,
    };
    observe(close).await.unwrap();
    let source = OutputSource::ProviderTurn {
        scope,
        turn_index: 0,
        attempt: 0,
    };
    let output_writer = OutputWriter::RequestExecution {
        execution_generation: generation,
    };
    let result = persisted(&node, &request.doc_id, &source, &output_writer).await;
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(data_path);
    result
}

/// The first reasoning bytes flush at once, later reasoning once per five
/// batch intervals, and the close carries the remainder.
fn assert_reasoning_cadence(case: &str, persisted: &Persisted, chunks_per_second: u32) {
    let batches = (SECONDS / crate::streaming::REASONING_BATCH_FACTOR) as usize;
    assert!(
        (batches - 1..=batches + 2).contains(&persisted.flushes),
        "{case}: a {SECONDS}s reasoning stream appended {} segments, expected {}..={}",
        persisted.flushes,
        batches - 1,
        batches + 2,
    );
    assert_eq!(
        persisted.reasoning,
        expected_reasoning(chunks_per_second),
        "{case}: persisted reasoning lost or reordered bytes"
    );
}

#[tokio::test]
async fn plain_reasoning_stream_flushes_per_reasoning_batch_and_keeps_every_byte() {
    for end in [End::Complete, End::Cancel] {
        let persisted = processor_stream(Stream::Plain, end, 50).await;
        assert_reasoning_cadence(&format!("plain {end:?}"), &persisted, 50);
    }
    let finer = processor_stream(Stream::Plain, End::Complete, 250).await;
    assert_reasoning_cadence("plain at 250 chunks/s", &finer, 250);
}

#[tokio::test]
async fn claude_thinking_audits_flush_per_reasoning_batch_and_keep_every_byte() {
    for end in [End::Complete, End::Cancel] {
        let persisted = processor_stream(Stream::ClaudeAudit, end, 50).await;
        let case = format!("claude {end:?}");
        assert_reasoning_cadence(&case, &persisted, 50);
        assert_eq!(
            persisted.signature, SIGNATURE,
            "{case}: signature bytes lost"
        );
    }
}

#[tokio::test]
async fn auxiliary_thinking_audits_flush_per_reasoning_batch_and_keep_every_byte() {
    for end in [End::Complete, End::Cancel] {
        let persisted = auxiliary_stream(end, 50).await;
        let case = format!("auxiliary {end:?}");
        assert_reasoning_cadence(&case, &persisted, 50);
        assert_eq!(
            persisted.signature, SIGNATURE,
            "{case}: signature bytes lost"
        );
    }
}
