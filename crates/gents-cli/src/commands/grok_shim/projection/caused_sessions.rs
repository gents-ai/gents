//! Grok pager subagent lifecycle projected from caused sessions.
//!
//! A pager subagent is a session whose first request was caused by a request
//! of the projected session (`caused_by_parent_request_doc_id`). Its
//! `subagent_spawned` / `subagent_progress` / `subagent_finished` payloads are
//! routed by `childSessionId` on the originating session's channel and
//! describe the caused session's latest request.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use gents_protocol::output::TerminalOutput;
use gents_protocol::row::AgentRequestRow;
use gents_protocol::transcript::present_message;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{effective_context_window_tokens, nonempty};
use crate::caused_sessions::{load_caused_sessions, CausedSession, SessionScope};

/// Ext request methods routed to this leaf by the ACP service.
pub(crate) const SUBAGENT_GET_METHOD: &str = "x.ai/subagent/get";
pub(crate) const SUBAGENT_LIST_RUNNING_METHOD: &str = "x.ai/subagent/list_running";

/// Shape of a `subagent_spawned` update payload (Grok pager
/// `extensions::notification::SubagentSpawned`).
///
/// The optional wire fields (`effective_context_source`, `capability_mode`,
/// `persona`, `role`, `model`, `resumed_from`, `workflow_run_id`) stay absent:
/// no durable Gents document carries them. `context_normalized` is always
/// true because every projected window passes through
/// `effective_context_window_tokens`.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SubagentSpawnedUpdate {
    pub subagent_id: String,
    pub parent_session_id: String,
    pub parent_prompt_id: Option<String>,
    pub child_session_id: String,
    pub subagent_type: String,
    pub description: String,
    pub context_normalized: bool,
}

/// Shape of a `subagent_progress` update payload.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SubagentProgressUpdate {
    pub subagent_id: String,
    pub parent_session_id: String,
    pub child_session_id: String,
    pub duration_ms: u64,
    pub turn_count: u32,
    pub tool_call_count: u32,
    pub tokens_used: u64,
    pub context_window_tokens: u64,
    pub context_usage_pct: u8,
    pub tools_used: Vec<String>,
    pub error_count: u32,
}

/// Shape of a terminal `subagent_finished` update payload. The pager routes
/// the finish by the subagent id alone, so it carries no parent session.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SubagentFinishedUpdate {
    pub subagent_id: String,
    pub child_session_id: String,
    pub status: SubagentFinishStatus,
    pub error: Option<String>,
    pub output: Option<String>,
    pub tool_calls: u32,
    pub turns: u32,
    pub duration_ms: u64,
    pub tokens_used: u64,
    pub will_wake: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SubagentFinishStatus {
    Completed,
    Failed,
    Cancelled,
}

impl SubagentFinishStatus {
    fn wire_name(self) -> &'static str {
        match self {
            SubagentFinishStatus::Completed => "completed",
            SubagentFinishStatus::Failed => "failed",
            SubagentFinishStatus::Cancelled => "cancelled",
        }
    }

    /// Only the canonical request lifecycle owns terminality; an interrupt
    /// marker is a request to stop, not evidence that execution stopped.
    fn of(request: &AgentRequestRow) -> Option<Self> {
        match request.lifecycle_state.map(|state| state.as_str()) {
            Some("completed") => Some(Self::Completed),
            Some("interrupted") => Some(Self::Cancelled),
            Some("failed" | "dead" | "superseded") => Some(Self::Failed),
            _ => None,
        }
    }
}

/// Projection events for one originating request, aligned 1:1 with the
/// durable chronology key of the tool call that caused each session.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SubagentProjection {
    pub updates: Vec<SubagentUpdate>,
    pub chronology: Vec<Option<i64>>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum SubagentUpdate {
    Spawned(SubagentSpawnedUpdate),
    Progress(SubagentProgressUpdate),
    Finished(SubagentFinishedUpdate),
}

impl SubagentUpdate {
    pub fn session_update_kind(&self) -> &'static str {
        match self {
            SubagentUpdate::Spawned(_) => "subagent_spawned",
            SubagentUpdate::Progress(_) => "subagent_progress",
            SubagentUpdate::Finished(_) => "subagent_finished",
        }
    }

    pub fn subagent_id(&self) -> &str {
        match self {
            SubagentUpdate::Spawned(update) => &update.subagent_id,
            SubagentUpdate::Progress(update) => &update.subagent_id,
            SubagentUpdate::Finished(update) => &update.subagent_id,
        }
    }

    /// The `sessionUpdate` discriminator keeps its camelCase enum tag; every
    /// inner DTO field renders with its snake_case key.
    pub fn to_payload(&self) -> Value {
        match self {
            SubagentUpdate::Spawned(update) => {
                let mut payload = json!({
                    "sessionUpdate": "subagent_spawned",
                    "subagent_id": update.subagent_id,
                    "parent_session_id": update.parent_session_id,
                    "child_session_id": update.child_session_id,
                    "subagent_type": update.subagent_type,
                    "description": update.description,
                    "context_normalized": update.context_normalized,
                });
                if let Some(prompt_id) = update.parent_prompt_id.as_deref() {
                    payload["parent_prompt_id"] = json!(prompt_id);
                }
                payload
            }
            SubagentUpdate::Progress(update) => json!({
                "sessionUpdate": "subagent_progress",
                "subagent_id": update.subagent_id,
                "parent_session_id": update.parent_session_id,
                "child_session_id": update.child_session_id,
                "duration_ms": update.duration_ms,
                "turn_count": update.turn_count,
                "tool_call_count": update.tool_call_count,
                "tokens_used": update.tokens_used,
                "context_window_tokens": update.context_window_tokens,
                "context_usage_pct": update.context_usage_pct,
                "tools_used": update.tools_used,
                "error_count": update.error_count,
            }),
            SubagentUpdate::Finished(update) => {
                let mut payload = json!({
                    "sessionUpdate": "subagent_finished",
                    "subagent_id": update.subagent_id,
                    "child_session_id": update.child_session_id,
                    "status": update.status.wire_name(),
                    "tool_calls": update.tool_calls,
                    "turns": update.turns,
                    "duration_ms": update.duration_ms,
                    "tokens_used": update.tokens_used,
                    "will_wake": update.will_wake,
                });
                if let Some(error) = update.error.as_deref() {
                    payload["error"] = json!(error);
                }
                if let Some(output) = update.output.as_deref() {
                    payload["output"] = json!(output);
                }
                payload
            }
        }
    }
}

/// Durable activity of one caused session's latest request.
#[derive(Debug, Clone, Default)]
struct HeadActivity {
    tokens_used: u64,
    tools: Vec<(Option<String>, Option<String>)>,
    terminalized_at: Option<String>,
    terminal_output: Option<TerminalOutput>,
}

#[derive(Debug, Default)]
struct Activity {
    heads: HashMap<String, HeadActivity>,
    cause_sequences: HashMap<String, i64>,
}

#[derive(Deserialize)]
struct UsageRow {
    request_doc_id: String,
    #[serde(default)]
    prompt_tokens: Option<i64>,
    #[serde(default)]
    completion_tokens: Option<i64>,
}

#[derive(Deserialize)]
struct ToolRow {
    request_doc_id: String,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
}

#[derive(Deserialize)]
struct HeadRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    #[serde(default)]
    terminalized_at: Option<String>,
    #[serde(default)]
    terminal_output: Option<TerminalOutput>,
}

#[derive(Deserialize)]
struct CauseRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    #[serde(default)]
    message_sequence: Option<i64>,
}

fn request_scope(request: &AgentRequestRow) -> Result<SessionScope> {
    Ok(SessionScope {
        agent_did: request
            .agent_did
            .clone()
            .context("projected request omitted agent_did")?,
        session_id: request
            .session_id
            .clone()
            .context("projected request omitted session_id")?,
        requester_did: request.requester_did.clone(),
    })
}

/// Sessions whose first request was caused by this exact physical request.
pub(crate) async fn caused_by_request(
    node: &EmbeddedNode,
    request: &AgentRequestRow,
) -> Result<Vec<CausedSession>> {
    let doc_id = request
        .doc_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .context("caused-session projection requires a physical request")?;
    Ok(load_caused_sessions(node, &[request_scope(request)?])
        .await?
        .into_iter()
        .filter(|session| {
            session.depth == 1
                && session.first.caused_by_parent_request_doc_id.as_deref() == Some(doc_id)
        })
        .collect())
}

/// Project the pager subagent lifecycle for the sessions caused by one
/// request. Returns one `spawned` per session plus `progress` while its
/// latest request runs or `finished` once it is terminal.
pub(super) async fn project_caused_sessions(
    node: &Arc<EmbeddedNode>,
    parent: &AgentRequestRow,
    parent_prompt_id: Option<&str>,
    context_window_tokens: u64,
) -> Result<SubagentProjection> {
    let sessions = caused_by_request(node, parent).await?;
    let activity = load_activity(node, &sessions, parent.doc_id.as_deref()).await?;
    let mut updates = Vec::new();
    let mut chronology = Vec::new();
    for session in &sessions {
        let sequence = session
            .first
            .caused_by_parent_tool_call_doc_id
            .as_deref()
            .and_then(|doc_id| activity.cause_sequences.get(doc_id))
            .copied();
        let head = activity.head(session);
        let id = session.scope.session_id.clone();
        updates.push(SubagentUpdate::Spawned(SubagentSpawnedUpdate {
            subagent_id: id.clone(),
            parent_session_id: session.caused_by_scope.session_id.clone(),
            parent_prompt_id: parent_prompt_id.map(ToOwned::to_owned),
            child_session_id: id.clone(),
            subagent_type: session.behavior_id.clone(),
            description: description(session),
            context_normalized: true,
        }));
        chronology.push(sequence);
        match SubagentFinishStatus::of(&session.latest) {
            None => updates.push(SubagentUpdate::Progress(progress_update(
                session,
                &head,
                context_window_tokens,
            ))),
            Some(status) => updates.push(SubagentUpdate::Finished(SubagentFinishedUpdate {
                subagent_id: id.clone(),
                child_session_id: id,
                status,
                error: session
                    .latest
                    .failure_reason
                    .as_deref()
                    .and_then(nonempty)
                    .map(ToOwned::to_owned),
                output: None,
                tool_calls: count(head.tools.len()),
                turns: 1,
                duration_ms: elapsed_millis(
                    session.latest.created_at.as_deref(),
                    head.terminalized_at.as_deref(),
                ),
                tokens_used: head.tokens_used,
                will_wake: session.first.caused_by_parent_tool_call_doc_id.is_some(),
            })),
        }
        chronology.push(sequence);
    }
    Ok(SubagentProjection {
        updates,
        chronology,
    })
}

/// Answer `x.ai/subagent/get` and `x.ai/subagent/list_running` from the
/// sessions caused by this connection's sessions. `sessions` comes only from
/// the connection's validated registry; the caller identity is agent ==
/// requester, matching normal shim submission.
pub(crate) async fn handle(
    node: Arc<EmbeddedNode>,
    principal: &str,
    sessions: &[String],
    method: &str,
    params: &Value,
    context_window: u64,
) -> Result<Value> {
    let roots = sessions
        .iter()
        .map(|session| SessionScope {
            agent_did: principal.to_owned(),
            session_id: session.clone(),
            requester_did: Some(principal.to_owned()),
        })
        .collect::<Vec<_>>();
    let caused = load_caused_sessions(&node, &roots).await?;
    if method == SUBAGENT_LIST_RUNNING_METHOD {
        let running = caused
            .into_iter()
            .filter(|session| !session.latest.is_terminal())
            .collect::<Vec<_>>();
        let activity = load_activity(&node, &running, None).await?;
        let mut subagents = Vec::new();
        for session in &running {
            let mut snapshot = snapshot(&node, session, &activity, context_window).await?;
            if snapshot["status"] == "running" {
                snapshot
                    .as_object_mut()
                    .expect("snapshot object")
                    .remove("status");
                subagents.push(snapshot);
            }
        }
        return Ok(json!({"subagents": subagents}));
    }
    let id = params["subagentId"]
        .as_str()
        .context("subagentId required")?;
    let Some(session) = caused
        .into_iter()
        .find(|session| session.scope.session_id == id)
    else {
        return Ok(json!({"snapshot": null}));
    };
    let activity = load_activity(&node, std::slice::from_ref(&session), None).await?;
    Ok(json!({"snapshot": snapshot(&node, &session, &activity, context_window).await?}))
}

async fn snapshot(
    node: &Arc<EmbeddedNode>,
    session: &CausedSession,
    activity: &Activity,
    context_window: u64,
) -> Result<Value> {
    let head = activity.head(session);
    let latest = &session.latest;
    let progress = progress_update(session, &head, context_window);
    let started = latest
        .created_at
        .as_deref()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.timestamp_millis().max(0) as u64)
        .unwrap_or(0);
    let status = SubagentFinishStatus::of(latest)
        .map(SubagentFinishStatus::wire_name)
        .unwrap_or_else(|| match latest.lifecycle_state.map(|state| state.as_str()) {
            Some("pending" | "claimed") => "initializing",
            _ => "running",
        });
    let duration = if latest.is_terminal() || started == 0 {
        progress.duration_ms
    } else {
        (chrono::Utc::now().timestamp_millis().max(0) as u64).saturating_sub(started)
    };
    let mut value = json!({
        "subagentId": session.scope.session_id,
        "parentSessionId": session.caused_by_scope.session_id,
        "childSessionId": session.scope.session_id,
        "subagentType": session.behavior_id,
        "description": description(session),
        "startedAtEpochMs": started,
        "durationMs": duration,
        "status": status,
    });
    let object = value.as_object_mut().expect("snapshot object");
    match status {
        "running" => {
            object.insert("turnCount".into(), json!(progress.turn_count));
            object.insert("toolCallCount".into(), json!(progress.tool_call_count));
            object.insert("tokensUsed".into(), json!(progress.tokens_used));
            object.insert(
                "contextWindowTokens".into(),
                json!(progress.context_window_tokens),
            );
            object.insert("contextUsagePct".into(), json!(progress.context_usage_pct));
            object.insert("toolsUsed".into(), json!(progress.tools_used));
            object.insert("errorCount".into(), json!(progress.error_count));
        }
        "completed" => {
            object.insert(
                "output".into(),
                json!(final_output(node, latest, head.terminal_output.as_ref()).await?),
            );
            object.insert("toolCalls".into(), json!(progress.tool_call_count));
            object.insert("turns".into(), json!(progress.turn_count));
        }
        "failed" => {
            object.insert(
                "failureError".into(),
                json!(latest
                    .failure_reason
                    .as_deref()
                    .and_then(nonempty)
                    .unwrap_or("subagent failed")),
            );
        }
        "cancelled" => {
            if let Some(reason) = latest.failure_reason.as_deref().and_then(nonempty) {
                object.insert("cancelReason".into(), json!(reason));
            }
        }
        _ => {}
    }
    Ok(value)
}

async fn final_output(
    node: &Arc<EmbeddedNode>,
    request: &AgentRequestRow,
    terminal_output: Option<&TerminalOutput>,
) -> Result<String> {
    let Some(selection) = terminal_output else {
        anyhow::bail!("terminal caused request omitted canonical terminal output");
    };
    let TerminalOutput::Message { message_doc_id } = selection else {
        return Ok(String::new());
    };
    let request_doc_id = request
        .doc_id
        .as_deref()
        .context("terminal caused request omitted physical identity")?;
    let agent_did = request
        .agent_did
        .as_deref()
        .context("terminal caused request omitted agent_did")?;
    let access = gents::ConfigAccess::Local(node.clone());
    let (header, message) = gents::session::load_canonical_message(
        &access,
        message_doc_id,
        agent_did,
        request.requester_did.as_deref(),
    )
    .await
    .context("resolving exact canonical caused-session terminal output")?;
    anyhow::ensure!(
        Some(header.session_id.as_str()) == request.session_id.as_deref()
            && header.request_doc_id.as_deref() == Some(request_doc_id)
            && header.role == gents_protocol::output::MessageRole::Assistant,
        "canonical caused-session terminal output crossed exact request scope"
    );
    Ok(present_message(&message).body_markdown)
}

impl Activity {
    fn head(&self, session: &CausedSession) -> HeadActivity {
        session
            .latest
            .doc_id
            .as_deref()
            .and_then(|doc_id| self.heads.get(doc_id))
            .cloned()
            .unwrap_or_default()
    }
}

/// One read of the latest requests' usage, tools and terminal facts, plus
/// the transcript positions of the tool calls in `parent_doc_id` that caused
/// each session. `InferenceCall` has no requester/session columns, so it is
/// scoped by principal and physical request.
async fn load_activity(
    node: &EmbeddedNode,
    sessions: &[CausedSession],
    parent_doc_id: Option<&str>,
) -> Result<Activity> {
    let heads = sessions
        .iter()
        .filter_map(|session| {
            Some((
                session.scope.clone(),
                session.latest.doc_id.clone().filter(|id| !id.is_empty())?,
            ))
        })
        .collect::<Vec<_>>();
    if heads.is_empty() {
        return Ok(Activity::default());
    }
    let usage_scopes = heads
        .iter()
        .map(|(scope, doc_id)| {
            format!(
                r#"{{agent_did: {{_eq: "{}"}}, request_doc_id: {{_eq: "{}"}}}}"#,
                escape_graphql_string(&scope.agent_did),
                escape_graphql_string(doc_id)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let tool_scopes = heads
        .iter()
        .map(|(scope, doc_id)| {
            format!(
                r#"{{{}, request_doc_id: {{_eq: "{}"}}}}"#,
                gents::session::session_scope_filter(
                    &scope.agent_did,
                    &scope.session_id,
                    scope.requester_did.as_deref()
                ),
                escape_graphql_string(doc_id)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let head_ids = quoted_list(heads.iter().map(|(_, doc_id)| doc_id.as_str()));
    let cause_ids = quoted_list(
        sessions
            .iter()
            .filter_map(|session| session.first.caused_by_parent_tool_call_doc_id.as_deref()),
    );
    let causes = parent_doc_id
        .filter(|_| !cause_ids.is_empty())
        .map(|parent| {
            let ids = &cause_ids;
            format!(
                r#"causes: AgentToolCall(filter: {{_docID: {{_in: [{ids}]}}, request_doc_id: {{_eq: "{}"}}}}) {{ _docID message_sequence }}"#,
                escape_graphql_string(parent)
            )
        })
        .unwrap_or_default();
    let query = format!(
        r#"{{
            InferenceCall(filter: {{_or: [{usage_scopes}], call_kind: {{_eq: "inference"}}, call_state: {{_in: ["completed", "failed", "cancelled"]}}}}) {{ request_doc_id prompt_tokens completion_tokens }}
            AgentToolCall(filter: {{_or: [{tool_scopes}]}}) {{ request_doc_id tool_name lifecycle_state }}
            AgentRequest(filter: {{_docID: {{_in: [{head_ids}]}}}}) {{ _docID terminalized_at terminal_output }}
            {causes}
        }}"#
    );
    let response = graphql_with_transaction_retry(node, &query, "grok caused session activity").await?;
    let mut activity = Activity::default();
    for (_, doc_id) in &heads {
        activity.heads.insert(doc_id.clone(), HeadActivity::default());
    }
    for row in decode_rows::<UsageRow>(&response, "InferenceCall")? {
        if let Some(head) = activity.heads.get_mut(&row.request_doc_id) {
            for tokens in [row.prompt_tokens, row.completion_tokens] {
                if let Some(tokens) = tokens.and_then(|value| u64::try_from(value).ok()) {
                    head.tokens_used = head.tokens_used.saturating_add(tokens);
                }
            }
        }
    }
    for row in decode_rows::<ToolRow>(&response, "AgentToolCall")? {
        if let Some(head) = activity.heads.get_mut(&row.request_doc_id) {
            head.tools.push((row.tool_name, row.lifecycle_state));
        }
    }
    for row in decode_rows::<HeadRow>(&response, "AgentRequest")? {
        if let Some(head) = activity.heads.get_mut(&row.doc_id) {
            head.terminalized_at = row.terminalized_at;
            head.terminal_output = row.terminal_output;
        }
    }
    if !causes.is_empty() {
        for row in decode_rows::<CauseRow>(&response, "causes")? {
            if let Some(sequence) = row.message_sequence {
                activity.cause_sequences.insert(row.doc_id, sequence);
            }
        }
    }
    Ok(activity)
}

fn quoted_list<'a>(ids: impl IntoIterator<Item = &'a str>) -> String {
    ids.into_iter()
        .map(|id| format!("\"{}\"", escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(",")
}

fn decode_rows<T: DeserializeOwned>(
    response: &defra_node::QueryResponse,
    field: &str,
) -> Result<Vec<T>> {
    response
        .data
        .as_ref()
        .and_then(|data| data.get(field))
        .and_then(Value::as_array)
        .with_context(|| format!("caused session {field} query returned no rows array"))?
        .iter()
        .cloned()
        .map(|row| serde_json::from_value(row).with_context(|| format!("decoding {field}")))
        .collect()
}

fn progress_update(
    session: &CausedSession,
    head: &HeadActivity,
    context_window_tokens: u64,
) -> SubagentProgressUpdate {
    let context_window_tokens = effective_context_window_tokens(context_window_tokens);
    let mut tools_used = Vec::<String>::new();
    for name in head.tools.iter().filter_map(|(name, _)| name.as_deref().and_then(nonempty)) {
        if !tools_used.iter().any(|seen| seen == name) {
            tools_used.push(name.to_string());
        }
    }
    let failed_tools = head
        .tools
        .iter()
        .filter(|(_, state)| state.as_deref() == Some("failed"))
        .count();
    let request_failure = usize::from(
        session
            .latest
            .failure_reason
            .as_deref()
            .and_then(nonempty)
            .is_some(),
    );
    SubagentProgressUpdate {
        subagent_id: session.scope.session_id.clone(),
        parent_session_id: session.caused_by_scope.session_id.clone(),
        child_session_id: session.scope.session_id.clone(),
        duration_ms: elapsed_millis(
            session.latest.created_at.as_deref(),
            head.terminalized_at.as_deref(),
        ),
        turn_count: 1,
        tool_call_count: count(head.tools.len()),
        tokens_used: head.tokens_used,
        context_window_tokens,
        context_usage_pct: context_usage_pct(head.tokens_used, context_window_tokens),
        tools_used,
        error_count: count(failed_tools + request_failure),
    }
}

fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn description(session: &CausedSession) -> String {
    const MAX_DESCRIPTION_CHARS: usize = 120;
    session
        .first
        .content
        .as_deref()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(MAX_DESCRIPTION_CHARS)
        .collect()
}

fn context_usage_pct(tokens_used: u64, context_window_tokens: u64) -> u8 {
    if context_window_tokens == 0 {
        return 0;
    }
    u8::try_from(
        tokens_used
            .saturating_mul(100)
            .saturating_div(context_window_tokens),
    )
    .unwrap_or(100)
}

fn elapsed_millis(started_at: Option<&str>, ended_at: Option<&str>) -> u64 {
    let (Some(started), Some(ended)) = (started_at, ended_at) else {
        return 0;
    };
    let (Ok(started), Ok(ended)) = (
        chrono::DateTime::parse_from_rfc3339(started),
        chrono::DateTime::parse_from_rfc3339(ended),
    ) else {
        return 0;
    };
    ended
        .signed_duration_since(started)
        .num_milliseconds()
        .max(0)
        .try_into()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(value: Value) -> AgentRequestRow {
        serde_json::from_value(value).unwrap()
    }

    fn session(lifecycle_state: &str) -> CausedSession {
        let first = request(json!({
            "_docID": "doc-child", "request_id": "child", "session_id": "child-session",
            "agent_did": "did:child", "requester_did": "did:parent", "behavior_id": "worker",
            "content": "  inspect the logs  ", "lifecycle_state": lifecycle_state,
            "created_at": "2026-09-26T00:00:00Z",
            "caused_by_parent_request_doc_id": "doc-parent",
            "caused_by_parent_tool_call_doc_id": "doc-call",
        }));
        CausedSession {
            scope: SessionScope {
                agent_did: "did:child".into(),
                session_id: "child-session".into(),
                requester_did: Some("did:parent".into()),
            },
            behavior_id: "worker".into(),
            root_session_id: "parent-session".into(),
            caused_by_scope: SessionScope {
                agent_did: "did:parent".into(),
                session_id: "parent-session".into(),
                requester_did: Some("did:parent".into()),
            },
            depth: 1,
            latest: first.clone(),
            first,
        }
    }

    #[test]
    fn finish_status_follows_the_canonical_lifecycle() {
        for (state, expected) in [
            ("completed", Some(SubagentFinishStatus::Completed)),
            ("interrupted", Some(SubagentFinishStatus::Cancelled)),
            ("failed", Some(SubagentFinishStatus::Failed)),
            ("dead", Some(SubagentFinishStatus::Failed)),
            ("superseded", Some(SubagentFinishStatus::Failed)),
            ("pending", None),
            ("processing", None),
        ] {
            assert_eq!(SubagentFinishStatus::of(&session(state).latest), expected, "{state}");
        }
    }

    #[test]
    fn progress_routes_by_the_caused_session() {
        let head = HeadActivity {
            tokens_used: 50,
            tools: vec![
                (Some("bash".into()), Some("failed".into())),
                (Some("bash".into()), Some("completed".into())),
                (Some("read".into()), None),
            ],
            ..HeadActivity::default()
        };
        let progress = progress_update(&session("processing"), &head, 100);
        assert_eq!(progress.subagent_id, "child-session");
        assert_eq!(progress.child_session_id, "child-session");
        assert_eq!(progress.parent_session_id, "parent-session");
        assert_eq!(progress.tool_call_count, 3);
        assert_eq!(progress.tools_used, ["bash", "read"]);
        assert_eq!(progress.error_count, 1);
        assert_eq!(progress.context_usage_pct, 50);
        assert_eq!(description(&session("processing")), "inspect the logs");
    }

    #[test]
    fn payloads_keep_the_pager_wire_shape() {
        let spawned = SubagentUpdate::Spawned(SubagentSpawnedUpdate {
            subagent_id: "child-session".into(),
            parent_session_id: "parent-session".into(),
            parent_prompt_id: Some("prompt-1".into()),
            child_session_id: "child-session".into(),
            subagent_type: "worker".into(),
            description: "inspect".into(),
            context_normalized: true,
        })
        .to_payload();
        assert_eq!(spawned["sessionUpdate"], "subagent_spawned");
        assert_eq!(spawned["parent_prompt_id"], "prompt-1");
        let finished = SubagentUpdate::Finished(SubagentFinishedUpdate {
            subagent_id: "child-session".into(),
            child_session_id: "child-session".into(),
            status: SubagentFinishStatus::Failed,
            error: Some("boom".into()),
            output: None,
            tool_calls: 0,
            turns: 1,
            duration_ms: 0,
            tokens_used: 0,
            will_wake: true,
        })
        .to_payload();
        assert_eq!(finished["status"], "failed");
        assert_eq!(finished["error"], "boom");
        assert!(finished.get("output").is_none());
        assert!(finished.get("parent_session_id").is_none());
    }
}
