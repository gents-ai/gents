//! A finished trial's provider transcripts, read from its retained home. Each
//! stage request yields the exact bodies it sent its provider and the
//! assistant messages it recorded, rendered as that provider's history renders
//! them, with each turn's reasoning, usage, tool calls and context reductions
//! behind them.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use futures::future::{FutureExt, LocalBoxFuture};
use gents_documents::InferenceBackend;
use gents_loop::provider_call::admission::CallKind;
use gents_loop::provider_input::ProviderInputCounter;
use gents_loop::rendered_request::RenderedRequestComponents;
use gents_observe::observe::adapter_projection::{redact_conversation, ProjectionRedactionMode};
use gents_observe::observe::run_timeline::events::TimelineRenderedRequestEvent;
use gents_observe::observe::run_timeline::rendered::{
    rendered_request_event, rendered_request_tiebreak,
};
use gents_observe::observe::run_timeline::rows::{
    RunTimelineRows, TimelineInferenceCallRow, TimelineMessageRow, TimelineRenderedRequestRow,
};
use gents_observe::observe::run_timeline_fetch::{
    load_settled_run_timeline_rows, RUN_TIMELINE_SETTLE_BOUND,
};
use gents_protocol::output::{MessageRole, OutputSource};
use gents_protocol::rendered_request::{
    CaptureScopeKind, HistoryStubbing, ParsedProvenance, ProvenanceManifest, RenderedRequestSource,
};
use gents_protocol::transcript::present_message;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use serde::Serialize;
use serde_json::Value;

use super::inspect::read_retained_home;
use crate::runner::TrialLocator;

/// Body fields holding the conversation in each provider wire: `messages`
/// (Claude's `system` beside it), or Responses' `input` and `instructions`.
const CONVERSATION_FIELDS: &[&str] = &["messages", "system", "input", "instructions"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptCalls {
    /// Each stage's last inference call, whose body holds the whole
    /// conversation the stage ended on.
    Final,
    /// Every provider call each stage made, compaction calls included.
    All,
}

#[derive(Clone, Copy, Debug)]
pub struct TranscriptOptions {
    pub calls: TranscriptCalls,
    pub redaction: ProjectionRedactionMode,
}

#[derive(Debug, Serialize)]
pub struct TrialTranscript {
    /// Every inference call the trial home recorded, title and compaction
    /// calls included, summed as the run report sums them.
    pub usage: TokenUsage,
    pub stages: Vec<StageTranscript>,
}

/// A total is absent once any call it sums left that count unreported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct StageTranscript {
    pub request_id: String,
    pub session_id: Option<String>,
    /// The agent that served the request: the subject, or one it delegated to.
    pub agent_did: Option<String>,
    pub lifecycle_state: Option<String>,
    /// This request's inference calls of every kind.
    pub inference_calls: usize,
    /// Its own completion loop's calls.
    pub model_turns: usize,
    pub usage: TokenUsage,
    pub tool_calls: ToolCallCounts,
    pub context: ContextReductions,
    /// Requests this one caused: a background tool's completion, which
    /// continues this session, or a delegated agent's request in its own.
    pub child_requests: Vec<ChildRequest>,
    pub calls: Vec<ProviderCall>,
    /// Every assistant message the request recorded, in session sequence order.
    pub outputs: Vec<AssistantOutput>,
    /// The last assistant message the request recorded.
    pub final_output: Option<AssistantOutput>,
}

#[derive(Debug, Default, Serialize)]
pub struct ToolCallCounts {
    pub total: usize,
    pub by_tool: BTreeMap<String, usize>,
    pub by_state: BTreeMap<String, usize>,
}

#[derive(Debug, Default, Serialize)]
pub struct ContextReductions {
    /// Whether the last inference body was assembled over a compaction or a
    /// provider-context reduction instead of the verbatim session history.
    pub final_call_reduced: bool,
    /// Earlier tool outputs the last inference body carries as stubs, which
    /// is neither a compaction nor a reduction.
    pub final_call_history_stubbing: Option<HistoryStubbing>,
    pub compaction_calls: usize,
    pub compactions: Vec<Reduction>,
    pub provider_context_reductions: Vec<Reduction>,
}

#[derive(Debug, Serialize)]
pub struct Reduction {
    pub key: String,
    pub messages_compacted: i64,
    pub original_tokens: i64,
    pub compacted_tokens: i64,
    pub created_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChildRequest {
    pub request_id: String,
    pub session_id: Option<String>,
    pub lifecycle_state: Option<String>,
    pub caused_by_tool_call_id: Option<String>,
    pub transcript: Option<Box<StageTranscript>>,
    /// Why the child yielded no transcript; its parent's stays exported.
    pub transcript_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProviderCall {
    pub capture_scope: Option<String>,
    pub turn_index: Option<i64>,
    pub attempt: Option<i64>,
    pub call_id: Option<String>,
    pub call_seq: Option<i64>,
    pub call_state: Option<String>,
    pub model_name: Option<String>,
    pub source: Option<String>,
    pub created_at: Option<String>,
    pub reduction_keys: Vec<String>,
    pub history_stubbing: Option<HistoryStubbing>,
    pub usage: Option<TokenUsage>,
    /// Temperature, top_p, maximum tokens and reasoning, read from the body.
    pub sampling: Value,
    /// The exact body sent to the provider.
    pub request: Value,
    /// The assistant message this provider call produced, when it published one.
    pub output: Option<AssistantOutput>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AssistantOutput {
    pub sequence: i64,
    pub capture_scope: Option<String>,
    pub turn_index: Option<i64>,
    pub attempt: Option<i64>,
    pub call_id: Option<String>,
    pub call_seq: Option<i64>,
    /// The message as the provider's wire renders it in any later request's
    /// history: one chat-completions message, or the Responses items it spans.
    pub wire: Vec<Value>,
    /// Reasoning the provider returned; chat-completions history never
    /// replays it, so `wire` omits it there.
    pub reasoning: Option<String>,
}

/// Reads each stage request's transcript from a finished trial's retained
/// home, as the held evaluator who created the trial.
pub async fn export_retained_transcript(
    runtime: &tokio::runtime::Handle,
    runs_dir: &Path,
    locator: &TrialLocator,
    evaluator: &dyn gents_store::identity::NodeIdentity,
    stage_request_ids: Vec<String>,
    options: TranscriptOptions,
) -> Result<TrialTranscript> {
    read_retained_home(
        runtime,
        runs_dir,
        locator,
        evaluator,
        move |access, node_did| async move {
            let backends = trial_backends(&access, &node_did).await?;
            let mut stages = Vec::with_capacity(stage_request_ids.len());
            for request_id in &stage_request_ids {
                stages.push(request_transcript(&access, request_id, &backends, options).await?);
            }
            Ok(TrialTranscript {
                usage: TokenUsage::sum(&trial_inference_calls(&access).await?),
                stages,
            })
        },
    )
    .await
    .context("exporting retained trial transcripts")
}

/// One request's transcript, with every request it caused exported in turn.
fn request_transcript<'a>(
    access: &'a ConfigAccess,
    request_id: &'a str,
    backends: &'a [InferenceBackend],
    options: TranscriptOptions,
) -> LocalBoxFuture<'a, Result<StageTranscript>> {
    async move {
        let rows = load_settled_run_timeline_rows(access, request_id, RUN_TIMELINE_SETTLE_BOUND)
            .await
            .with_context(|| format!("loading request {request_id}"))?;
        let mut transcript = stage_transcript(access, &rows, backends, options)
            .await
            .with_context(|| format!("exporting request {request_id}"))?;
        for child in &mut transcript.child_requests {
            let child_id = child.request_id.clone();
            match request_transcript(access, &child_id, backends, options).await {
                Ok(exported) => child.transcript = Some(Box::new(exported)),
                Err(error) => child.transcript_error = Some(format!("{error:#}")),
            }
        }
        Ok(transcript)
    }
    .boxed_local()
}

async fn stage_transcript(
    access: &ConfigAccess,
    rows: &RunTimelineRows,
    backends: &[InferenceBackend],
    options: TranscriptOptions,
) -> Result<StageTranscript> {
    let request = &rows.request;
    let doc_id = request
        .doc_id
        .as_deref()
        .context("stage request has no physical document ID")?;
    let own = |row_doc_id: Option<&str>| row_doc_id == Some(doc_id);
    let calls = rows
        .inference_calls
        .iter()
        .filter(|call| own(call.request_doc_id.as_deref()))
        .collect::<Vec<_>>();
    let mut captures = rows
        .rendered_requests
        .iter()
        .filter(|row| own(row.request_doc_id.as_deref()))
        .map(|row| (row, rendered_request_event(row)))
        .collect::<Vec<_>>();
    captures.sort_by_cached_key(|(_, event)| (event.call_seq, rendered_request_tiebreak(event)));
    let is_kind = |event: &TimelineRenderedRequestEvent, kind: CaptureScopeKind| {
        event.scope_kind.as_deref() == Some(kind.as_str())
    };
    let final_capture = last_inference_capture(&captures);
    let (outputs, outputs_by_coord) =
        stage_assistant_outputs(rows, doc_id, &captures, &calls, backends, options.redaction)?;
    let selected = match options.calls {
        TranscriptCalls::Final => final_capture.into_iter().collect::<Vec<_>>(),
        TranscriptCalls::All => captures.iter().collect(),
    };
    let mut exported = Vec::with_capacity(selected.len());
    for (row, event) in selected {
        let output = capture_coordinate(event)
            .and_then(|coord| outputs_by_coord.get(&coord).copied())
            .map(|index| outputs[index].clone());
        exported.push(provider_call(access, row, event, &calls, output, options.redaction).await?);
    }
    let final_output = outputs.last().cloned();
    Ok(StageTranscript {
        request_id: request.request_id.clone(),
        session_id: request.session_id.clone(),
        agent_did: request.agent_did.clone(),
        lifecycle_state: request
            .lifecycle_state
            .map(|state| state.as_str().to_owned()),
        inference_calls: calls.len(),
        model_turns: calls
            .iter()
            .filter(|call| call.call_kind == CallKind::Inference.as_str())
            .count(),
        usage: TokenUsage::sum(calls.iter().copied()),
        tool_calls: tool_call_counts(rows, doc_id),
        context: ContextReductions {
            final_call_reduced: final_capture
                .is_some_and(|(_, event)| !event.reduction_keys.is_empty()),
            final_call_history_stubbing: final_capture.and_then(|(row, _)| history_stubbing(row)),
            compaction_calls: captures
                .iter()
                .filter(|(_, event)| {
                    is_kind(event, CaptureScopeKind::Compaction)
                        || is_kind(event, CaptureScopeKind::CompactionFallback)
                })
                .count(),
            compactions: rows
                .compactions
                .iter()
                .filter(|row| own(row.request_doc_id.as_deref()))
                .map(|row| Reduction {
                    key: row.compaction_key.clone(),
                    messages_compacted: row.messages_compacted,
                    original_tokens: row.original_tokens,
                    compacted_tokens: row.compacted_tokens,
                    created_at: row.created_at.clone(),
                })
                .collect(),
            provider_context_reductions: rows
                .provider_context_reductions
                .iter()
                .filter(|row| row.request_doc_id == doc_id)
                .map(|row| Reduction {
                    key: row.reduction_key.clone(),
                    messages_compacted: row.messages_compacted,
                    original_tokens: row.original_tokens,
                    compacted_tokens: row.compacted_tokens,
                    created_at: row.created_at.clone(),
                })
                .collect(),
        },
        child_requests: rows
            .requests
            .iter()
            .filter(|child| own(child.caused_by_parent_request_doc_id.as_deref()))
            .map(|child| ChildRequest {
                request_id: child.request_id.clone(),
                session_id: child.session_id.clone(),
                lifecycle_state: child.lifecycle_state.map(|state| state.as_str().to_owned()),
                caused_by_tool_call_id: child.caused_by_parent_tool_call_id.clone(),
                transcript: None,
                transcript_error: None,
            })
            .collect(),
        calls: exported,
        outputs,
        final_output,
    })
}

async fn provider_call(
    access: &ConfigAccess,
    row: &TimelineRenderedRequestRow,
    event: &TimelineRenderedRequestEvent,
    calls: &[&TimelineInferenceCallRow],
    output: Option<AssistantOutput>,
    redaction: ProjectionRedactionMode,
) -> Result<ProviderCall> {
    let body = capture_body(access, row).await?;
    let sampling = match capture_source(row) {
        Ok(source) => {
            RenderedRequestComponents::from_provider_body(body.clone(), source).sampling_json
        }
        Err(_) => Value::Null,
    };
    let call = event
        .call_id
        .as_deref()
        .and_then(|call_id| calls.iter().find(|call| call.call_id == call_id));
    Ok(ProviderCall {
        capture_scope: event.capture_scope.clone(),
        turn_index: event.turn_index,
        attempt: event.attempt,
        call_id: event.call_id.clone(),
        call_seq: event.call_seq,
        call_state: call.map(|call| call.call_state.clone()),
        model_name: event.model_name.clone(),
        source: event.source.clone(),
        created_at: event.created_at.clone(),
        reduction_keys: event.reduction_keys.clone(),
        history_stubbing: history_stubbing(row),
        usage: call.map(|call| TokenUsage::of(call)),
        sampling,
        request: redact_body(body, redaction),
        output,
    })
}

/// The decoded body of one capture; the timeline never selects `request_json`.
async fn capture_body(access: &ConfigAccess, row: &TimelineRenderedRequestRow) -> Result<Value> {
    let doc_id = row
        .doc_id
        .as_deref()
        .context("capture has no physical document ID")?;
    let version = row
        .capture_version
        .and_then(|version| u32::try_from(version).ok())
        .context("capture has no supported numeric capture_version")?;
    let response = access
        .execute(&format!(
            r#"{{ RenderedRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ request_json }} }}"#,
            escape_graphql_string(doc_id)
        ))
        .await?;
    let rows = response
        .pointer("/data/RenderedRequest")
        .and_then(Value::as_array)
        .context("capture body query omitted its rows")?;
    let [stored] = rows.as_slice() else {
        anyhow::bail!("capture {doc_id} did not resolve uniquely");
    };
    let stored = stored
        .get("request_json")
        .and_then(Value::as_str)
        .with_context(|| format!("capture {doc_id} has no stored request_json"))?;
    gents_inference::rendered_request::decode_capture_json(
        access,
        version,
        stored,
        gents_inference::rendered_request::CapturePayloadKind::RequestBody,
    )
    .await
    .with_context(|| format!("decoding capture {doc_id}"))
}

fn history_stubbing(row: &TimelineRenderedRequestRow) -> Option<HistoryStubbing> {
    match ProvenanceManifest::parse(row.provenance_json.as_deref()?) {
        Ok(ParsedProvenance::Manifest(manifest)) => manifest
            .assembly_trace
            .context_accounting
            .map(|accounting| accounting.history_stubbing),
        _ => None,
    }
}

fn capture_source(row: &TimelineRenderedRequestRow) -> Result<RenderedRequestSource> {
    let source = row.source.clone().context("capture records no source")?;
    serde_json::from_value(Value::String(source)).context("capture records an unknown source")
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TurnCoordinate {
    capture_scope: String,
    turn_index: i64,
    attempt: i64,
}

fn last_inference_capture<'a>(
    captures: &'a [(&'a TimelineRenderedRequestRow, TimelineRenderedRequestEvent)],
) -> Option<&'a (&'a TimelineRenderedRequestRow, TimelineRenderedRequestEvent)> {
    captures.iter().rev().find(|(_, event)| {
        event.scope_kind.as_deref() == Some(CaptureScopeKind::Inference.as_str())
    })
}

fn capture_coordinate(event: &TimelineRenderedRequestEvent) -> Option<TurnCoordinate> {
    Some(TurnCoordinate {
        capture_scope: event.capture_scope.clone()?,
        turn_index: event.turn_index?,
        attempt: event.attempt?,
    })
}

fn message_turn_coordinate(message: &TimelineMessageRow, doc_id: &str) -> Option<TurnCoordinate> {
    let rest = message
        .header
        .message_key
        .strip_prefix("provider:")
        .or_else(|| message.header.message_key.strip_prefix("request-recovery:"))?;
    let encoded_source = rest.strip_prefix(doc_id)?.strip_prefix(':')?;
    let OutputSource::ProviderTurn {
        scope,
        turn_index,
        attempt,
    } = serde_json::from_str::<OutputSource>(encoded_source).ok()?
    else {
        return None;
    };
    Some(TurnCoordinate {
        capture_scope: scope.to_string(),
        turn_index: i64::from(turn_index),
        attempt: i64::from(attempt),
    })
}

fn assistant_messages<'a>(rows: &'a RunTimelineRows, doc_id: &str) -> Vec<&'a TimelineMessageRow> {
    let mut messages = rows
        .messages
        .iter()
        .filter(|message| {
            message.request_doc_id.as_deref() == Some(doc_id)
                && message.header.role == MessageRole::Assistant
        })
        .collect::<Vec<_>>();
    messages.sort_by(|left, right| {
        left.sequence
            .cmp(&right.sequence)
            .then_with(|| left.header.message_key.cmp(&right.header.message_key))
    });
    messages
}

fn stage_assistant_outputs(
    rows: &RunTimelineRows,
    doc_id: &str,
    captures: &[(&TimelineRenderedRequestRow, TimelineRenderedRequestEvent)],
    calls: &[&TimelineInferenceCallRow],
    backends: &[InferenceBackend],
    redaction: ProjectionRedactionMode,
) -> Result<(Vec<AssistantOutput>, BTreeMap<TurnCoordinate, usize>)> {
    let mut captures_by_coord = BTreeMap::new();
    for (row, event) in captures {
        if let Some(coord) = capture_coordinate(event) {
            captures_by_coord.insert(coord, (*row, event));
        }
    }
    let final_capture = last_inference_capture(captures).map(|(row, event)| (*row, event));
    let messages = assistant_messages(rows, doc_id);
    let mut outputs = Vec::with_capacity(messages.len());
    let mut outputs_by_coord = BTreeMap::new();
    for message in messages {
        let coord = message_turn_coordinate(message, doc_id);
        let matched = coord
            .as_ref()
            .and_then(|coord| captures_by_coord.get(coord).copied());
        let Some((capture_row, _)) = matched.or(final_capture) else {
            continue;
        };
        let matched_event = matched.map(|(_, event)| event);
        let call_id = matched_event.and_then(|event| event.call_id.as_deref());
        let output = assistant_output(
            message,
            capture_row,
            matched_event,
            coord.as_ref(),
            wire_counter(calls, backends, capture_row, call_id)?,
            redaction,
        )?;
        let index = outputs.len();
        outputs.push(output);
        if let Some(coord) = coord {
            outputs_by_coord.insert(coord, index);
        }
    }
    Ok((outputs, outputs_by_coord))
}

/// The counter the request's own loop projected its history with: the
/// backend its inference calls ran on, at the model the capture names.
fn wire_counter(
    calls: &[&TimelineInferenceCallRow],
    backends: &[InferenceBackend],
    capture: &TimelineRenderedRequestRow,
    call_id: Option<&str>,
) -> Result<ProviderInputCounter> {
    let backend_id = call_id
        .and_then(|id| {
            calls
                .iter()
                .find(|call| call.call_id == id)
                .and_then(|call| call.backend_id.as_deref())
        })
        .or_else(|| {
            calls
                .iter()
                .rev()
                .filter(|call| call.call_kind == CallKind::Inference.as_str())
                .find_map(|call| call.backend_id.as_deref())
        })
        .context("the stage's inference calls name no backend")?;
    let backend = backends
        .iter()
        .find(|backend| backend.backend_id == backend_id)
        .with_context(|| format!("the trial's backend {backend_id} is not readable"))?;
    let fields = gents_inference::backends::registry::backend_fields(backend);
    Ok(ProviderInputCounter::new(
        fields.backend_provider_kind,
        fields.openai_wire_api,
        capture.model_name.clone().unwrap_or_default(),
    ))
}

fn assistant_output(
    message: &TimelineMessageRow,
    capture: &TimelineRenderedRequestRow,
    matched_event: Option<&TimelineRenderedRequestEvent>,
    coord: Option<&TurnCoordinate>,
    counter: ProviderInputCounter,
    redaction: ProjectionRedactionMode,
) -> Result<AssistantOutput> {
    let body = counter.project_message_body(std::slice::from_ref(&message.message))?;
    let wire = body
        .get(capture_source(capture)?.messages_field())
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(AssistantOutput {
        sequence: message.sequence,
        capture_scope: matched_event
            .and_then(|event| event.capture_scope.clone())
            .or_else(|| coord.map(|coord| coord.capture_scope.clone())),
        turn_index: matched_event
            .and_then(|event| event.turn_index)
            .or_else(|| coord.map(|coord| coord.turn_index)),
        attempt: matched_event
            .and_then(|event| event.attempt)
            .or_else(|| coord.map(|coord| coord.attempt)),
        call_id: matched_event.and_then(|event| event.call_id.clone()),
        call_seq: matched_event.and_then(|event| event.call_seq),
        wire: wire
            .into_iter()
            .map(|item| redact_conversation(item, redaction))
            .collect(),
        reasoning: present_message(&message.message)
            .reasoning_markdown
            .map(
                |text| match redact_conversation(Value::String(text), redaction) {
                    Value::String(text) => text,
                    other => other.to_string(),
                },
            ),
    })
}

fn redact_body(mut body: Value, redaction: ProjectionRedactionMode) -> Value {
    if let Some(object) = body.as_object_mut() {
        for field in CONVERSATION_FIELDS {
            if let Some(value) = object.get_mut(*field) {
                *value = redact_conversation(value.take(), redaction);
            }
        }
    }
    body
}

fn tool_call_counts(rows: &RunTimelineRows, doc_id: &str) -> ToolCallCounts {
    let mut counts = ToolCallCounts::default();
    for call in rows
        .tool_calls
        .iter()
        .filter(|call| call.request_doc_id.as_deref() == Some(doc_id))
    {
        counts.total += 1;
        *counts.by_tool.entry(call.tool_name.clone()).or_default() += 1;
        *counts
            .by_state
            .entry(call.lifecycle_state.clone().unwrap_or_default())
            .or_default() += 1;
    }
    counts
}

async fn trial_backends(access: &ConfigAccess, node_did: &str) -> Result<Vec<InferenceBackend>> {
    access
        .transact_readonly("eval.transcript.backends", |txn| {
            Box::pin(
                async move { gents_documents::list_inference_backends_in_txn(txn, node_did).await },
            )
        })
        .await
}

async fn trial_inference_calls(access: &ConfigAccess) -> Result<Vec<TimelineInferenceCallRow>> {
    let response = access
        .execute(
            "{ InferenceCall { call_id request_id request_doc_id call_kind call_state prompt_tokens \
             completion_tokens cached_input_tokens cache_creation_input_tokens reasoning_output_tokens } }",
        )
        .await?;
    let rows = response
        .pointer("/data/InferenceCall")
        .cloned()
        .context("trial usage query omitted inference calls")?;
    let mut calls: Vec<TimelineInferenceCallRow> =
        serde_json::from_value(rows).context("decoding the trial's inference calls")?;
    let requests = super::executor::usage::home_rows(access).await?.requests;
    calls.retain(|call| super::executor::usage::charged(&requests, call.request_doc_id.as_deref()));
    Ok(calls)
}

impl TokenUsage {
    fn of(call: &TimelineInferenceCallRow) -> Self {
        let count = |value: Option<i64>| value.and_then(|value| u64::try_from(value).ok());
        Self {
            input_tokens: count(call.prompt_tokens),
            output_tokens: count(call.completion_tokens),
            reasoning_output_tokens: count(call.reasoning_output_tokens),
            cached_input_tokens: count(call.cached_input_tokens),
            cache_creation_input_tokens: count(call.cache_creation_input_tokens),
        }
    }

    fn sum<'a>(calls: impl IntoIterator<Item = &'a TimelineInferenceCallRow>) -> Self {
        let add = |total: Option<u64>, count: Option<u64>| {
            total
                .zip(count)
                .map(|(total, count)| total.saturating_add(count))
        };
        calls.into_iter().map(Self::of).fold(
            Self {
                input_tokens: Some(0),
                output_tokens: Some(0),
                reasoning_output_tokens: Some(0),
                cached_input_tokens: Some(0),
                cache_creation_input_tokens: Some(0),
            },
            |total, call| Self {
                input_tokens: add(total.input_tokens, call.input_tokens),
                output_tokens: add(total.output_tokens, call.output_tokens),
                reasoning_output_tokens: add(
                    total.reasoning_output_tokens,
                    call.reasoning_output_tokens,
                ),
                cached_input_tokens: add(total.cached_input_tokens, call.cached_input_tokens),
                cache_creation_input_tokens: add(
                    total.cache_creation_input_tokens,
                    call.cache_creation_input_tokens,
                ),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::message::Message;
    use gents_protocol::output::{MessagePublication, OutputOutcome, TranscriptMessage};
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

    fn assistant_row(doc_id: &str, message_key: String, sequence: i64) -> TimelineMessageRow {
        TimelineMessageRow {
            doc_id: Some(format!("msg-{sequence}")),
            session_id: "session-1".into(),
            request_doc_id: Some(doc_id.into()),
            sequence,
            timestamp: Some("2026-01-01T00:00:00Z".into()),
            node_did: Some("did:key:node".into()),
            header: TranscriptMessage {
                message_key,
                session_id: "session-1".into(),
                node_did: "did:key:node".into(),
                requester_did: None,
                request_doc_id: Some(doc_id.into()),
                publication: MessagePublication::RequestExecution {
                    execution_generation: "gen-1".into(),
                },
                outcome: OutputOutcome::Complete,
                sequence: u32::try_from(sequence).unwrap(),
                role: MessageRole::Assistant,
                native_id: None,
                native_request_id: None,
                blocks: Vec::new(),
                created_at: "2026-01-01T00:00:00Z".into(),
            },
            message: Message::assistant("ok"),
        }
    }

    #[test]
    fn message_turn_coordinate_decodes_provider_and_recovery_keys() {
        let doc_id = "bae-req-1";
        let source = OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: 1,
            },
            turn_index: 2,
            attempt: 1,
        };
        let encoded = serde_json::to_string(&source).unwrap();
        let expected = TurnCoordinate {
            capture_scope: "inference.1".into(),
            turn_index: 2,
            attempt: 1,
        };

        let provider = assistant_row(doc_id, format!("provider:{doc_id}:{encoded}"), 3);
        assert_eq!(
            message_turn_coordinate(&provider, doc_id),
            Some(expected.clone())
        );

        let recovery = assistant_row(doc_id, format!("request-recovery:{doc_id}:{encoded}"), 4);
        assert_eq!(message_turn_coordinate(&recovery, doc_id), Some(expected));

        let other_request = assistant_row(doc_id, format!("provider:bae-other:{encoded}"), 5);
        assert_eq!(message_turn_coordinate(&other_request, doc_id), None);

        let authored = assistant_row(doc_id, format!("authored:{doc_id}:prompt"), 6);
        assert_eq!(message_turn_coordinate(&authored, doc_id), None);
    }
}
