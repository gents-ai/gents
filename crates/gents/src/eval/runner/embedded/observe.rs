//! Request observation for an embedded trial: wait until terminal, read the
//! evidence rows, and classify the outcome the way the stage runner does.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gents_protocol::output::MessageRole;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::transcript::present_message;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config_client::ConfigAccess;
use crate::defra_node::EmbeddedNode;
use crate::graphql::{escape_graphql_string, first_row, graphql_with_transaction_retry};
use crate::run_timeline::{RunTimelineRows, TimelineToolCallRow};

/// Progress and usage callbacks for one `await_terminal_with` poll.
///
/// `on_state` runs when the observed `(lifecycle_state, session_id)` pair
/// changes. `on_interrupt_requested` runs once, immediately before the deadline
/// interrupt. `on_tick` runs after each non-terminal state read, with the time
/// since observation began.
#[async_trait::async_trait]
pub trait ObservationHook: Send + Sync {
    async fn on_state(&self, _state: &str, _session_id: Option<&str>) {}
    async fn on_interrupt_requested(&self) {}
    async fn on_tick(&self, _elapsed: Duration) {}
}

/// Hook that records nothing. [`await_terminal`] uses it.
pub struct NoHook;

#[async_trait::async_trait]
impl ObservationHook for NoHook {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalObservation {
    pub terminal_state: RequestLifecycleState,
    pub session_id: Option<String>,
    pub interrupted_on_deadline: bool,
    pub elapsed: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallEvidence {
    pub tool_name: String,
    pub status: Option<String>,
    pub lifecycle_state: Option<String>,
    pub tool_failure_class: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub args: Value,
    pub result: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferenceCallEvidence {
    pub call_seq: i64,
    pub call_state: Option<String>,
    pub failure_reason: Option<String>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub queued_at: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageEvidence {
    pub role: String,
    pub content: String,
    pub created_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseEvidence {
    pub status: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestEvidence {
    pub messages: Vec<MessageEvidence>,
    pub tool_calls: Vec<ToolCallEvidence>,
    pub inference_calls: Vec<InferenceCallEvidence>,
    pub responses: Vec<ResponseEvidence>,
    pub failure_reason: Option<String>,
}

/// Poll `AgentRequest { lifecycle_state session_id }` until the request is
/// terminal or the deadline-and-grace window ends.
///
/// Every `poll`, read the latest row. When `deadline` elapses while that state
/// is still non-terminal, call [`crate::interrupt::interrupt_request`] exactly
/// once and set `interrupted_on_deadline`. Keep polling until `deadline + grace`.
///
/// Return `Ok` as soon as the state parses to a terminal
/// [`RequestLifecycleState`] (`Completed`, `Failed`, `Superseded`, `Dead`, or
/// `Interrupted`). When the grace expires and the last observed state is still
/// non-terminal, return `Ok` with `terminal_state` set to that state when
/// [`RequestLifecycleState::parse`] yields a terminal state, otherwise
/// `Interrupted`, and `interrupted_on_deadline: true`. This is the library form
/// of today's `nonterminal_after_interrupt` observation: the caller still learns
/// that the deadline fired, and a missing or unparseable row is not unwrapped.
pub async fn await_terminal(
    node: &EmbeddedNode,
    request_id: &str,
    deadline: Duration,
    grace: Duration,
    poll: Duration,
) -> Result<TerminalObservation> {
    await_terminal_with(node, request_id, deadline, grace, poll, &NoHook).await
}

pub async fn await_terminal_with(
    node: &EmbeddedNode,
    request_id: &str,
    deadline: Duration,
    grace: Duration,
    poll: Duration,
    hook: &dyn ObservationHook,
) -> Result<TerminalObservation> {
    let started = std::time::Instant::now();
    let mut interrupted_on_deadline = false;
    let mut last_state = String::new();
    let mut session_id = None;
    let mut last_progress: Option<(String, Option<String>)> = None;
    loop {
        let row = poll_request(node, request_id).await?;
        let observed_state = row
            .as_ref()
            .and_then(|row| row.lifecycle_state.clone())
            .unwrap_or_else(|| "missing".to_string());
        let observed_session = row.as_ref().and_then(|row| row.session_id.clone());
        if let Some(row) = row.as_ref() {
            session_id.clone_from(&row.session_id);
            if let Some(state) = row.lifecycle_state.as_deref() {
                last_state = state.to_string();
            }
        }
        let progress = (observed_state.clone(), observed_session.clone());
        if last_progress.as_ref() != Some(&progress) {
            hook.on_state(&observed_state, observed_session.as_deref())
                .await;
            last_progress = Some(progress);
        }
        if let Some(state) = row.as_ref().and_then(|row| row.lifecycle_state.as_deref()) {
            if let Ok(parsed) = RequestLifecycleState::parse(state) {
                if parsed.is_terminal() {
                    return Ok(TerminalObservation {
                        terminal_state: parsed,
                        session_id,
                        interrupted_on_deadline,
                        elapsed: started.elapsed(),
                    });
                }
            }
        }
        let elapsed = started.elapsed();
        hook.on_tick(elapsed).await;
        if !interrupted_on_deadline && elapsed >= deadline {
            hook.on_interrupt_requested().await;
            crate::interrupt::interrupt_request(node, request_id)
                .await
                .context("interrupt request after evaluation deadline")?;
            interrupted_on_deadline = true;
        }
        if interrupted_on_deadline && elapsed >= deadline.saturating_add(grace) {
            let terminal_state = match RequestLifecycleState::parse(&last_state) {
                Ok(state) if state.is_terminal() => state,
                _ => RequestLifecycleState::Interrupted,
            };
            return Ok(TerminalObservation {
                terminal_state,
                session_id,
                interrupted_on_deadline: true,
                elapsed: started.elapsed(),
            });
        }
        tokio::time::sleep(poll).await;
    }
}

pub async fn collect_request_evidence(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
) -> Result<RequestEvidence> {
    let query = evidence_query(request_id);
    let response = graphql_with_transaction_retry(node, &query, "collect request evidence").await?;
    let data = response
        .data
        .context("collect request evidence returned no data")?;
    let timeline = crate::run_timeline_fetch::load_run_timeline_rows(
        &ConfigAccess::Local(node.clone()),
        request_id,
    )
    .await
    .context("load request run timeline")?;
    Ok(request_evidence_from_sources(&data, &timeline))
}

/// Map the evidence-query data object and the request's run timeline into
/// [`RequestEvidence`]. Tool calls and assistant messages come from the
/// timeline; the request failure reason and inference calls from the query.
pub fn request_evidence_from_sources(data: &Value, timeline: &RunTimelineRows) -> RequestEvidence {
    let mut evidence = request_evidence_from_query_data(data);
    evidence.tool_calls = timeline.tool_calls.iter().map(tool_call_evidence).collect();
    evidence.messages = assistant_messages(timeline);
    evidence
}

/// Map a query data object into [`RequestEvidence`].
///
/// `AgentToolCall` rows, when present, are serialized
/// [`TimelineToolCallRow`]s, the shape the stage runner's observed outcome
/// carries. Missing collections are empty; a row is never unwrapped.
pub fn request_evidence_from_query_data(data: &Value) -> RequestEvidence {
    let requests = rows(data, "AgentRequest");
    let failure_reason = requests
        .first()
        .and_then(|row| field_string(row, "failure_reason"));
    RequestEvidence {
        failure_reason,
        tool_calls: rows(data, "AgentToolCall")
            .iter()
            .filter_map(|row| serde_json::from_value::<TimelineToolCallRow>(row.clone()).ok())
            .map(|row| tool_call_evidence(&row))
            .collect(),
        inference_calls: rows(data, "InferenceCall")
            .iter()
            .map(|row| InferenceCallEvidence {
                call_seq: field_i64(row, "call_seq").unwrap_or(0),
                call_state: field_string(row, "call_state"),
                failure_reason: field_string(row, "failure_reason"),
                prompt_tokens: field_u64(row, "prompt_tokens"),
                completion_tokens: field_u64(row, "completion_tokens"),
                queued_at: field_string(row, "queued_at"),
                started_at: field_string(row, "started_at"),
                ended_at: field_string(row, "ended_at"),
            })
            .collect(),
        responses: Vec::new(),
        messages: Vec::new(),
    }
}

fn tool_call_evidence(row: &TimelineToolCallRow) -> ToolCallEvidence {
    ToolCallEvidence {
        tool_name: row.tool_name.clone(),
        status: Some(row.status.clone()).filter(|status| !status.is_empty()),
        lifecycle_state: row.lifecycle_state.clone(),
        tool_failure_class: row.tool_failure_class.clone(),
        started_at: row.started_at.clone(),
        completed_at: row.completed_at.clone(),
        args: if row.args.is_empty() {
            Value::Null
        } else {
            Value::String(row.args.clone())
        },
        result: row.result.clone().map_or(Value::Null, Value::String),
    }
}

/// The request's own assistant messages in sequence order, presented through
/// the transcript owner.
fn assistant_messages(timeline: &RunTimelineRows) -> Vec<MessageEvidence> {
    let Some(request_doc_id) = timeline.request.doc_id.as_deref() else {
        return Vec::new();
    };
    let mut messages = timeline
        .messages
        .iter()
        .filter(|row| {
            row.request_doc_id.as_deref() == Some(request_doc_id)
                && row.header.role == MessageRole::Assistant
        })
        .collect::<Vec<_>>();
    messages.sort_by_key(|row| row.sequence);
    messages
        .into_iter()
        .map(|row| MessageEvidence {
            role: "assistant".to_string(),
            content: present_message(&row.message).body_markdown,
            created_at: row.timestamp.clone(),
        })
        .collect()
}

/// Same string kinds the stage runner returns today: `"deadline"`, `"tool"`,
/// `"provider"`, `"runtime"`, or `"unknown"`. `None` means the request completed.
pub fn classify_request_outcome(
    terminal_state: RequestLifecycleState,
    interrupted_on_deadline: bool,
    evidence: &RequestEvidence,
) -> Option<&'static str> {
    if interrupted_on_deadline {
        return Some("deadline");
    }
    if terminal_state == RequestLifecycleState::Completed {
        return None;
    }
    let budget_exhausted = evidence.failure_reason.as_deref().is_some_and(|reason| {
        reason.contains("invalid_tool_call_budget_exhausted")
            || reason.contains(crate::agent::loop_stream::REPEATED_TOOL_FAILURE_PREFIX)
    });
    if budget_exhausted {
        return Some("tool");
    }
    let inference_failed = evidence.inference_calls.iter().any(|call| {
        matches!(call.call_state.as_deref(), Some("failed" | "error"))
            || call
                .failure_reason
                .as_deref()
                .is_some_and(|reason| !reason.is_empty())
    });
    let tool_failed = evidence
        .tool_calls
        .iter()
        .any(|call| matches!(call.lifecycle_state.as_deref(), Some("failed")));
    // Failed tool execution can terminate an otherwise healthy provider stream.
    // Co-occurrence does not establish which boundary caused termination.
    if inference_failed && tool_failed {
        return Some("unknown");
    }
    if inference_failed {
        return Some("provider");
    }
    if tool_failed {
        return Some("tool");
    }
    match terminal_state {
        RequestLifecycleState::Failed
        | RequestLifecycleState::Interrupted
        | RequestLifecycleState::Dead
        | RequestLifecycleState::Superseded => Some("runtime"),
        _ => Some("unknown"),
    }
}

#[derive(Debug, Deserialize)]
struct RequestPollRow {
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

async fn poll_request(node: &EmbeddedNode, request_id: &str) -> Result<Option<RequestPollRow>> {
    let response =
        graphql_with_transaction_retry(node, &poll_query(request_id), "await terminal").await?;
    first_row(&response, "AgentRequest")
}

fn poll_query(request_id: &str) -> String {
    let escaped = escape_graphql_string(request_id);
    format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ lifecycle_state session_id }} }}"#
    )
}

/// The request failure reason and its inference calls. Tool calls and
/// messages are read through the run timeline owner.
pub fn evidence_query(request_id: &str) -> String {
    let request_id = escape_graphql_string(request_id);
    format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ failure_reason }}
            InferenceCall(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{
                call_seq call_state failure_reason prompt_tokens completion_tokens queued_at started_at ended_at
            }}
        }}"#
    )
}

/// Usage sample: inference timing and tokens.
pub fn inference_sample_query(request_id: &str) -> String {
    let request_id = escape_graphql_string(request_id);
    format!(
        r#"{{
            InferenceCall(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{
                call_seq call_state failure_reason prompt_tokens completion_tokens queued_at started_at ended_at
            }}
        }}"#
    )
}

fn rows<'a>(data: &'a Value, key: &str) -> &'a [Value] {
    data.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn field_string(row: &Value, key: &str) -> Option<String> {
    match row.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(other) => Some(other.to_string()),
    }
}

fn field_i64(row: &Value, key: &str) -> Option<i64> {
    let value = row.get(key)?;
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|number| i64::try_from(number).ok()))
}

fn field_u64(row: &Value, key: &str) -> Option<u64> {
    let value = row.get(key)?;
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::eval::runner::embedded::EmbeddedHome;
    use gents_protocol::request_lifecycle::RequestLifecycleState;
    use serde_json::Value;

    fn evidence(
        inference_failed: bool,
        tool_failed: bool,
        failure_reason: Option<&str>,
    ) -> RequestEvidence {
        RequestEvidence {
            messages: vec![],
            tool_calls: if tool_failed {
                vec![ToolCallEvidence {
                    tool_name: "t".into(),
                    status: Some("failed".into()),
                    lifecycle_state: Some("failed".into()),
                    tool_failure_class: None,
                    started_at: None,
                    completed_at: None,
                    args: Value::Null,
                    result: Value::Null,
                }]
            } else {
                vec![]
            },
            inference_calls: if inference_failed {
                vec![InferenceCallEvidence {
                    call_seq: 1,
                    call_state: Some("failed".into()),
                    failure_reason: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    queued_at: None,
                    started_at: None,
                    ended_at: None,
                }]
            } else {
                vec![]
            },
            responses: vec![],
            failure_reason: failure_reason.map(str::to_string),
        }
    }

    #[test]
    fn classification_matches_the_stages_table() {
        use RequestLifecycleState as S;
        assert_eq!(
            classify_request_outcome(S::Interrupted, true, &evidence(false, false, None)),
            Some("deadline")
        );
        assert_eq!(
            classify_request_outcome(S::Completed, false, &evidence(false, false, None)),
            None
        );
        assert_eq!(
            classify_request_outcome(
                S::Failed,
                false,
                &evidence(false, false, Some("invalid_tool_call_budget_exhausted"))
            ),
            Some("tool")
        );
        assert_eq!(
            classify_request_outcome(
                S::Failed,
                false,
                &evidence(
                    true,
                    true,
                    Some("invalid_tool_call_budget_exhausted: limit=8, used=8")
                )
            ),
            Some("tool"),
            "budget exhaustion wins when inference and tool also failed"
        );
        assert_eq!(
            classify_request_outcome(
                S::Failed,
                false,
                &evidence(
                    true,
                    true,
                    Some("repeated_tool_failure: tool=config, identical_failures=3")
                )
            ),
            Some("tool"),
            "a repeated identical tool failure is a tool outcome"
        );
        let mut status_only = evidence(false, false, None);
        status_only.tool_calls.push(ToolCallEvidence {
            tool_name: "t".into(),
            status: Some("failed".into()),
            lifecycle_state: Some("completed".into()),
            tool_failure_class: None,
            started_at: None,
            completed_at: None,
            args: Value::Null,
            result: Value::Null,
        });
        assert_eq!(
            classify_request_outcome(S::Failed, false, &status_only),
            Some("runtime"),
            "only the authoritative lifecycle_state marks a tool failed"
        );
        assert_eq!(
            classify_request_outcome(S::Failed, false, &evidence(true, true, None)),
            Some("unknown")
        );
        assert_eq!(
            classify_request_outcome(S::Failed, false, &evidence(true, false, None)),
            Some("provider")
        );
        let mut inference_reason = evidence(false, false, None);
        inference_reason
            .inference_calls
            .push(InferenceCallEvidence {
                call_seq: 1,
                call_state: Some("completed".into()),
                failure_reason: Some("503".into()),
                prompt_tokens: None,
                completion_tokens: None,
                queued_at: None,
                started_at: None,
                ended_at: None,
            });
        assert_eq!(
            classify_request_outcome(S::Failed, false, &inference_reason),
            Some("provider")
        );
        assert_eq!(
            classify_request_outcome(S::Failed, false, &evidence(false, true, None)),
            Some("tool")
        );
        let mut tool_lifecycle = evidence(false, false, None);
        tool_lifecycle.tool_calls.push(ToolCallEvidence {
            tool_name: "t".into(),
            status: None,
            lifecycle_state: Some("failed".into()),
            tool_failure_class: Some("argumentInvalid".into()),
            started_at: None,
            completed_at: None,
            args: Value::Null,
            result: Value::Null,
        });
        assert_eq!(
            classify_request_outcome(S::Failed, false, &tool_lifecycle),
            Some("tool")
        );
        assert_eq!(
            classify_request_outcome(S::Failed, false, &evidence(false, false, None)),
            Some("runtime")
        );
        assert_eq!(
            classify_request_outcome(S::Dead, false, &evidence(false, false, None)),
            Some("runtime")
        );
        assert_eq!(
            classify_request_outcome(S::Superseded, false, &evidence(false, false, None)),
            Some("runtime")
        );
        // Old "mystery" row: a non-runtime, non-completed state is unknown.
        assert_eq!(
            classify_request_outcome(S::Processing, false, &evidence(false, false, None)),
            Some("unknown")
        );
    }

    async fn insert_request(home: &EmbeddedHome, request_id: &str, lifecycle_state: &str) {
        let request_id = crate::graphql::escape_graphql_string(request_id);
        let agent_did = crate::graphql::escape_graphql_string(home.did());
        let lifecycle_state = crate::graphql::escape_graphql_string(lifecycle_state);
        // AgentRequest SDL fields are nillable. Supply the identity and state the
        // observer reads; do not emit an empty list.
        let mutation = format!(
            r#"mutation {{ create_AgentRequest(input: {{ request_id: "{request_id}", agent_did: "{agent_did}", requester_did: "{agent_did}", behavior_id: "observe", session_id: "s-1", content: "x", execution_origin: "interactive", lifecycle_state: "{lifecycle_state}", created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#
        );
        let response = home.node.execute(&mutation).await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
    }

    /// A field a query names that the schema no longer has fails here rather
    /// than in a live run.
    #[tokio::test]
    async fn every_query_observe_issues_is_valid_against_the_home_schema() {
        let home = EmbeddedHome::create_temp("observe-queries").await.unwrap();
        for query in [
            poll_query("req-\"quoted\""),
            evidence_query("req-\"quoted\""),
            inference_sample_query("req-\"quoted\""),
        ] {
            let response = home.node.execute(&query).await;
            assert!(response.errors.is_empty(), "{query}: {:?}", response.errors);
        }
        home.node.shutdown().await;
    }

    /// Every source the evidence reads, mapped out of a real home whose rows
    /// were written by the canonical owners.
    ///
    /// A whole canary run exercises messages and inference calls; it never
    /// produces a tool call or a failed request, and a field read out of the
    /// wrong column would be invisible there. This pins all three vectors and
    /// the failure reason.
    #[tokio::test]
    async fn request_evidence_maps_every_source_it_reads() {
        use crate::lifecycle::RequestTerminalOutcome;
        use crate::streaming::DefraStreamWriter;
        use crate::tool_call_lifecycle::admission_fixture::{
            claimed_signed_request, publish_accepted_on_claimed_request,
        };
        use crate::tool_call_lifecycle::{AwaitMode, CancelPolicy, FailureClass};
        use gents_protocol::message::{AssistantContent, Message, Text};
        use gents_protocol::output::TerminalOutput;

        let home = EmbeddedHome::create_temp("observe-evidence").await.unwrap();
        let did = home.did().to_owned();
        let mut request = claimed_signed_request(
            &home.node,
            "req-evidence",
            "s-evidence",
            home.identity.as_ref(),
            None,
        )
        .await;
        let mut tool = publish_accepted_on_claimed_request(
            home.node.clone(),
            &mut request,
            &did,
            0,
            "fs_read",
            "call-1",
            serde_json::json!({"path": "x"}),
            None,
            AwaitMode::Foreground,
            CancelPolicy::Cascade,
            true,
        )
        .await
        .unwrap();
        tool.fail("denied", FailureClass::ArgumentInvalid)
            .await
            .unwrap();
        let writer = DefraStreamWriter::new(home.node.clone(), &did, Duration::from_millis(1));
        writer
            .start_provider_attempt(
                &request.request().doc_id,
                1,
                0,
                "inference.2".parse().unwrap(),
            )
            .await;
        let answer = writer
            .publish_native_turn(
                &request,
                1,
                0,
                &Message::Assistant {
                    id: Some("final".into()),
                    content: vec![AssistantContent::Text(Text {
                        text: "the answer".into(),
                    })],
                },
            )
            .await
            .unwrap();
        let request_doc_id = escape_graphql_string(&request.request().doc_id);
        let did_literal = escape_graphql_string(&did);
        let response = home
            .node
            .execute(&format!(
                r#"mutation {{ create_InferenceCall(input: {{ request_id: "req-evidence", request_doc_id: "{request_doc_id}", agent_did: "{did_literal}", call_id: "inference-1", call_kind: "inference", attempt: 0, call_seq: 3, call_state: "failed", failure_reason: "HTTP 503", prompt_tokens: 11, completion_tokens: 7, queued_at: "2026-01-01T00:00:00Z", started_at: "2026-01-01T00:00:01Z", ended_at: "2026-01-01T00:00:02Z" }}) {{ _docID }} }}"#
            ))
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        request
            .terminalize_owned(
                RequestTerminalOutcome::Failed,
                TerminalOutput::Message {
                    message_doc_id: answer.message_doc_id,
                },
                Some("invalid_tool_call_budget_exhausted: limit=8, used=8"),
            )
            .await
            .unwrap();

        let evidence = collect_request_evidence(&home.node, "req-evidence")
            .await
            .unwrap();

        assert!(
            evidence
                .failure_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("invalid_tool_call_budget_exhausted")),
            "{:?}",
            evidence.failure_reason
        );

        assert_eq!(evidence.tool_calls.len(), 1, "{:?}", evidence.tool_calls);
        let tool_call = &evidence.tool_calls[0];
        assert_eq!(tool_call.tool_name, "fs_read");
        assert_eq!(tool_call.status.as_deref(), Some("failed"));
        assert_eq!(tool_call.lifecycle_state.as_deref(), Some("failed"));
        assert_eq!(
            tool_call.tool_failure_class.as_deref(),
            Some("argumentInvalid")
        );
        assert_eq!(
            tool_call
                .args
                .as_str()
                .map(|args| serde_json::from_str::<Value>(args).unwrap()),
            Some(serde_json::json!({"path": "x"}))
        );
        assert!(
            tool_call
                .result
                .as_str()
                .is_some_and(|result| result.contains("denied")),
            "{:?}",
            tool_call.result
        );
        for (field, at) in [
            ("started_at", tool_call.started_at.as_deref()),
            ("completed_at", tool_call.completed_at.as_deref()),
        ] {
            assert!(at.is_some_and(|at| !at.is_empty()), "{field}: {at:?}");
        }

        assert_eq!(evidence.inference_calls.len(), 1);
        let call = &evidence.inference_calls[0];
        assert_eq!(
            (call.call_seq, call.prompt_tokens, call.completion_tokens),
            (3, Some(11), Some(7))
        );
        assert_eq!(call.call_state.as_deref(), Some("failed"));
        assert_eq!(call.failure_reason.as_deref(), Some("HTTP 503"));
        assert_eq!(call.queued_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(call.started_at.as_deref(), Some("2026-01-01T00:00:01Z"));
        assert_eq!(call.ended_at.as_deref(), Some("2026-01-01T00:00:02Z"));

        assert!(evidence.responses.is_empty());

        assert_eq!(
            evidence
                .messages
                .iter()
                .map(|message| (message.role.as_str(), message.content.as_str()))
                .collect::<Vec<_>>(),
            [("assistant", ""), ("assistant", "the answer")],
            "the request's assistant messages arrive in sequence order, tool results excluded"
        );
        assert!(
            evidence.messages.iter().all(|message| message
                .created_at
                .as_deref()
                .is_some_and(|at| !at.is_empty())),
            "a message's created_at is its canonical header's: {:?}",
            evidence.messages
        );

        // These are exactly the rows the classifier reads, so the mapping is
        // pinned against what it is for: the tool budget wins over the failed
        // inference call and the failed tool.
        assert_eq!(
            classify_request_outcome(RequestLifecycleState::Failed, false, &evidence),
            Some("tool")
        );
        home.node.shutdown().await;
    }

    #[tokio::test]
    async fn await_terminal_returns_when_the_request_is_already_terminal() {
        let home = EmbeddedHome::create_temp("observe").await.unwrap();
        let request_id = "req-terminal";
        insert_request(&home, request_id, "completed").await;
        let observed = await_terminal(
            &home.node,
            request_id,
            Duration::from_secs(5),
            Duration::from_secs(1),
            Duration::from_millis(50),
        )
        .await
        .unwrap();
        assert_eq!(observed.terminal_state, RequestLifecycleState::Completed);
        assert_eq!(observed.session_id.as_deref(), Some("s-1"));
        assert!(!observed.interrupted_on_deadline);
    }

    #[tokio::test]
    async fn await_terminal_interrupts_on_the_deadline_and_reports_it() {
        let home = EmbeddedHome::create_temp("observe-deadline").await.unwrap();
        insert_request(&home, "req-stuck", "pending").await;
        let observed = await_terminal(
            &home.node,
            "req-stuck",
            Duration::from_millis(200),
            Duration::from_millis(300),
            Duration::from_millis(50),
        )
        .await
        .unwrap();
        assert!(observed.interrupted_on_deadline);
        // No runtime is running, so the observer never flips it; the state stays
        // non-terminal and the grace expires.
        assert_ne!(observed.terminal_state, RequestLifecycleState::Completed);
        let request_id = crate::graphql::escape_graphql_string("req-stuck");
        let query = format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ interrupt_requested_at }} }}"#
        );
        let response = home.node.execute(&query).await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let rendered = serde_json::to_string(&response.data).unwrap();
        assert!(rendered.contains("interrupt_requested_at"), "{rendered}");
    }

    struct DeadlineHook {
        states: std::sync::atomic::AtomicUsize,
        interrupts: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ObservationHook for DeadlineHook {
        async fn on_state(&self, _state: &str, _session_id: Option<&str>) {
            self.states
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }

        async fn on_interrupt_requested(&self) {
            self.interrupts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn await_terminal_with_reports_each_state_and_one_deadline_interrupt() {
        let home = EmbeddedHome::create_temp("observe-hook").await.unwrap();
        insert_request(&home, "req-stuck", "pending").await;
        let hook = DeadlineHook {
            states: std::sync::atomic::AtomicUsize::new(0),
            interrupts: std::sync::atomic::AtomicUsize::new(0),
        };
        let observed = await_terminal_with(
            &home.node,
            "req-stuck",
            Duration::from_millis(200),
            Duration::from_millis(300),
            Duration::from_millis(50),
            &hook,
        )
        .await
        .unwrap();
        assert!(observed.interrupted_on_deadline);
        assert!(
            hook.states.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "on_state was not called"
        );
        assert_eq!(hook.interrupts.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
