use rapidhash::RapidHashMap;
use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents::config_client::ConfigAccess;
use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use gents::session::origin::load_session;
use gents_protocol::row::AgentRequestRow;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{effective_context_window_tokens, nonempty};
use crate::caused_sessions::{
    load_caused_session, load_caused_sessions, load_direct_caused_sessions, CausedSession,
    SessionScope,
};

pub(crate) const SUBAGENT_GET_METHOD: &str = "x.ai/subagent/get";
pub(crate) const SUBAGENT_LIST_RUNNING_METHOD: &str = "x.ai/subagent/list_running";
pub(crate) const SUBAGENT_CANCEL_METHOD: &str = "x.ai/subagent/cancel";

#[derive(Debug, Clone, PartialEq)]
pub(super) struct SubagentSpawnedUpdate {
    pub subagent_id: String,
    pub parent_session_id: String,
    pub parent_prompt_id: Option<String>,
    pub child_session_id: String,
    pub subagent_type: String,
    pub description: String,
    pub workflow_run_id: Option<String>,
    pub context_normalized: bool,
}

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

#[derive(Debug, Clone, PartialEq)]
pub(super) struct SubagentFinishedUpdate {
    pub subagent_id: String,
    pub child_session_id: String,
    pub status: SubagentFinishStatus,
    pub error: Option<String>,
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

    fn of(request: &AgentRequestRow) -> Option<Self> {
        match request.lifecycle_state.map(|state| state.as_str()) {
            Some("completed") => Some(Self::Completed),
            Some("interrupted") => Some(Self::Cancelled),
            Some("failed" | "dead" | "superseded") => Some(Self::Failed),
            _ => None,
        }
    }
}

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
                if let Some(run_id) = update.workflow_run_id.as_deref() {
                    payload["workflow_run_id"] = json!(run_id);
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
                payload
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
struct HeadActivity {
    tokens_used: u64,
    tools: Vec<(Option<String>, Option<String>)>,
    terminalized_at: Option<String>,
}

#[derive(Debug, Default)]
struct Activity {
    heads: RapidHashMap<String, HeadActivity>,
    cause_sequences: RapidHashMap<String, i64>,
    /// Cause tool-call doc id to the `tool_call_id` of the plugin job it pins.
    job_runs: RapidHashMap<String, String>,
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
}

#[derive(Deserialize)]
struct CauseRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    #[serde(default)]
    message_sequence: Option<i64>,
    #[serde(default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    job_pin: Option<Value>,
}

pub(crate) async fn caused_by_request(
    node: &Arc<EmbeddedNode>,
    request: &AgentRequestRow,
) -> Result<Vec<CausedSession>> {
    load_direct_caused_sessions(node, request).await
}

pub(super) async fn project_caused_sessions(
    node: &Arc<EmbeddedNode>,
    parent: &AgentRequestRow,
    parent_prompt_id: Option<&str>,
    context_window_tokens: u64,
    delivered_runs: &BTreeSet<String>,
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
        let job_run = session
            .first
            .caused_by_parent_tool_call_doc_id
            .as_deref()
            .and_then(|doc_id| activity.job_runs.get(doc_id));
        let description = match job_run {
            Some(_) => spawn_title(node, session)
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, session_id = %id,
                        "job child title unreadable; using the first prompt");
                    None
                })
                .unwrap_or_else(|| description(session)),
            None => description(session),
        };
        updates.push(SubagentUpdate::Spawned(SubagentSpawnedUpdate {
            subagent_id: id.clone(),
            parent_session_id: session.caused_by_scope.session_id.clone(),
            parent_prompt_id: parent_prompt_id.map(ToOwned::to_owned),
            child_session_id: id.clone(),
            subagent_type: session.agent_id.clone(),
            description,
            workflow_run_id: job_run.filter(|run| delivered_runs.contains(*run)).cloned(),
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
                tool_calls: count(head.tools.len()),
                turns: 1,
                duration_ms: elapsed_millis(
                    session.latest.created_at.as_deref(),
                    head.terminalized_at.as_deref(),
                ),
                tokens_used: head.tokens_used,
                will_wake: job_run.is_none()
                    && session.first.caused_by_parent_tool_call_doc_id.is_some(),
            })),
        }
        chronology.push(sequence);
    }
    Ok(SubagentProjection {
        updates,
        chronology,
    })
}

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
            node_did: principal.to_owned(),
            session_id: session.clone(),
            requester_did: Some(principal.to_owned()),
        })
        .collect::<Vec<_>>();
    if method == SUBAGENT_LIST_RUNNING_METHOD {
        let running = load_caused_sessions(&node, &roots)
            .await?
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
    let Some(session) = load_caused_session(&node, id, |scope| roots.contains(scope)).await? else {
        return Ok(if method == SUBAGENT_CANCEL_METHOD {
            json!({"subagentId": id, "cancelled": false, "outcome": {"kind": "not_found"}})
        } else {
            json!({"snapshot": null})
        });
    };
    if method == SUBAGENT_CANCEL_METHOD {
        let caller = roots
            .iter()
            .find(|root| root.session_id == session.root_session_id)
            .context("caused session resolved outside the connection's sessions")?;
        return cancel(&node, id, caller, &session, context_window).await;
    }
    let activity = load_activity(&node, std::slice::from_ref(&session), None).await?;
    Ok(json!({"snapshot": snapshot(&node, &session, &activity, context_window).await?}))
}

async fn cancel(
    node: &Arc<EmbeddedNode>,
    id: &str,
    caller: &SessionScope,
    session: &CausedSession,
    context_window: u64,
) -> Result<Value> {
    if let Some(refusal) =
        gents::session::message::interrupt_refusal(node, caller, &session.scope).await?
    {
        anyhow::bail!(refusal);
    }
    if gents::session::message::interrupt_session(node, &session.scope)
        .await?
        .is_some()
    {
        return Ok(json!({"subagentId": id, "cancelled": true, "outcome": {"kind": "cancelled"}}));
    }
    let activity = load_activity(node, std::slice::from_ref(session), None).await?;
    let snapshot = snapshot(node, session, &activity, context_window).await?;
    let status = snapshot["status"]
        .as_str()
        .unwrap_or("completed")
        .to_owned();
    Ok(json!({"subagentId": id, "cancelled": false,
        "outcome": {"kind": "already_finished", "status": status}}))
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
        .unwrap_or_else(
            || match latest.lifecycle_state.map(|state| state.as_str()) {
                Some("pending" | "claimed") => "initializing",
                _ => "running",
            },
        );
    let duration = if latest.is_terminal() || started == 0 {
        progress.duration_ms
    } else {
        (chrono::Utc::now().timestamp_millis().max(0) as u64).saturating_sub(started)
    };
    let mut value = json!({
        "subagentId": session.scope.session_id,
        "parentSessionId": session.caused_by_scope.session_id,
        "childSessionId": session.scope.session_id,
        "subagentType": session.agent_id,
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
            object.insert("output".into(), json!(final_output(node, latest).await?));
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

async fn final_output(node: &Arc<EmbeddedNode>, request: &AgentRequestRow) -> Result<String> {
    let request_doc_id = request
        .doc_id
        .as_deref()
        .context("terminal caused request omitted physical identity")?;
    match gents::load_caused_request_terminal(node, request_doc_id).await? {
        Some(gents::tool_call_lifecycle::CausedRequestTerminal::Completed { output }) => Ok(output),
        Some(_) => anyhow::bail!("caused request did not complete"),
        None => anyhow::bail!("completed caused request has no replicated terminal output yet"),
    }
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
                r#"{{node_did: {{_eq: "{}"}}, request_doc_id: {{_eq: "{}"}}}}"#,
                escape_graphql_string(&scope.node_did),
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
                    &scope.node_did,
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
                r#"causes: AgentToolCall(docIDs: [{ids}], filter: {{ request_doc_id: {{_eq: "{}"}} }}) {{ _docID message_sequence tool_call_id job_pin }}"#,
                escape_graphql_string(parent)
            )
        })
        .unwrap_or_default();
    let query = format!(
        r#"{{
            InferenceCall(filter: {{_or: [{usage_scopes}], call_kind: {{_eq: "inference"}}, call_state: {{_in: ["completed", "failed", "cancelled"]}}}}) {{ request_doc_id prompt_tokens completion_tokens }}
            AgentToolCall(filter: {{_or: [{tool_scopes}]}}) {{ request_doc_id tool_name lifecycle_state }}
            AgentRequest(docIDs: [{head_ids}]) {{ _docID terminalized_at }}
            {causes}
        }}"#
    );
    let response =
        graphql_with_transaction_retry(node, &query, "grok caused session activity").await?;
    let mut activity = Activity::default();
    for (_, doc_id) in &heads {
        activity
            .heads
            .insert(doc_id.clone(), HeadActivity::default());
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
        }
    }
    if !causes.is_empty() {
        for row in decode_rows::<CauseRow>(&response, "causes")? {
            if let (Some(_), Some(call)) = (row.job_pin, row.tool_call_id) {
                activity.job_runs.insert(row.doc_id.clone(), call);
            }
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

fn decode_rows<T>(response: &defra_node::QueryResponse, field: &str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
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
    for name in head
        .tools
        .iter()
        .filter_map(|(name, _)| name.as_deref().and_then(nonempty))
    {
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

/// The title the job gave its child session when it spawned it.
async fn spawn_title(node: &Arc<EmbeddedNode>, session: &CausedSession) -> Result<Option<String>> {
    let access = ConfigAccess::local(node.clone());
    let loaded = load_session(
        &access,
        &session.scope.session_id,
        session.scope.requester_did.as_deref(),
    )
    .await?;
    Ok(loaded
        .and_then(|loaded| loaded.title)
        .map(|title| title.text))
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

    #[tokio::test]
    async fn one_call_that_started_two_sessions_spawns_one_subagent_each() {
        let dir = tempfile::tempdir().unwrap();
        let node = Arc::new(
            crate::shared::test_support::node_builder()
                .data_path(dir.path().join("node"))
                .build()
                .await
                .unwrap(),
        );
        gents::store::schema::ensure_runtime_schemas(node.as_ref())
            .await
            .unwrap();
        let did = crate::caused_sessions::fixtures::FIXTURE_DID;
        let doc_id =
            crate::caused_sessions::fixtures::seed_one_call_two_sessions(&node, did, "parent")
                .await;
        let parent = request(json!({
            "_docID": doc_id, "request_id": "parent-request", "session_id": "parent",
            "node_did": did, "requester_did": did,
        }));
        let mut started = caused_by_request(&node, &parent)
            .await
            .unwrap()
            .into_iter()
            .map(|session| session.scope.session_id)
            .collect::<Vec<_>>();
        started.sort();
        assert_eq!(started, ["child-a", "child-b"]);
        node.shutdown().await;
    }

    const DID: &str = crate::caused_sessions::fixtures::FIXTURE_DID;

    async fn embedded_node(dir: &tempfile::TempDir) -> Arc<EmbeddedNode> {
        let node = Arc::new(
            crate::shared::test_support::node_builder()
                .data_path(dir.path().join("node"))
                .build()
                .await
                .unwrap(),
        );
        gents::store::schema::ensure_runtime_schemas(node.as_ref())
            .await
            .unwrap();
        node
    }

    async fn create(node: &EmbeddedNode, collection: &str, input: &str) -> String {
        let response = node
            .execute(&format!(
                "mutation {{ create_{collection}(input: {{ {input} }}) {{ _docID }} }}"
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        gents::graphql::single_mutation_document(&response, &format!("create_{collection}"))
            .unwrap()
            .and_then(|document| document["_docID"].as_str().map(str::to_owned))
            .expect("created doc id")
    }

    /// A parent request whose `workflow` call (tool-call id `call-run`) started
    /// the session `child`, titled `Audit the repo`, with a completed request.
    async fn seed_job_child(node: &EmbeddedNode) -> AgentRequestRow {
        seed_child(
            node,
            r#", job_pin: { script_id: "audit", handle: "audit-1" }"#,
        )
        .await
    }

    /// Like `seed_job_child`, with `pin` as the call row's extra fields: empty
    /// seeds a plain `agent.new` call.
    async fn seed_child(node: &EmbeddedNode, pin: &str) -> AgentRequestRow {
        let parent = create(
            node,
            "AgentRequest",
            &format!(
                r#"purpose: "normal", request_id: "parent-request", node_did: "{DID}", requester_did: "{DID}", agent_id: "worker", session_id: "parent", content: "run it", lifecycle_state: "completed", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "2026-09-26T00:00:00Z", retry_count: 0, max_retries: 3"#
            ),
        )
        .await;
        let call = create(
            node,
            "AgentToolCall",
            &format!(
                r#"tool_call_key: "parent:call-run", request_id: "parent-request", request_doc_id: "{parent}", session_id: "parent", node_did: "{DID}", requester_did: "{DID}", tool_call_id: "call-run", tool_name: "workflow", lifecycle_state: "completed", message_sequence: 3{pin}"#
            ),
        )
        .await;
        let opening = create(
            node,
            "AgentRequest",
            &format!(
                r#"purpose: "normal", request_id: "child-request", node_did: "{DID}", requester_did: "{DID}", agent_id: "worker", session_id: "child", caused_by_parent_request_id: "parent-request", caused_by_parent_request_doc_id: "{parent}", caused_by_parent_tool_call_id: "call-run", caused_by_parent_tool_call_doc_id: "{call}", content: "work", lifecycle_state: "completed", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "2026-09-26T00:00:01Z", retry_count: 0, max_retries: 3"#
            ),
        )
        .await;
        let mut child = crate::caused_sessions::fixtures::fixture_session(DID, "child");
        child.title = Some(gents_protocol::session::SessionTitle {
            text: "Audit the repo".into(),
            source: gents_protocol::session::SessionTitleSource::Task,
        });
        crate::caused_sessions::fixtures::create_started_session(
            node,
            child,
            gents_protocol::session::SessionReference {
                node_did: DID.into(),
                session_id: "parent".into(),
                requester_did: Some(DID.into()),
            },
            &parent,
            &opening,
        )
        .await;
        request(json!({
            "_docID": parent, "request_id": "parent-request", "session_id": "parent",
            "node_did": DID, "requester_did": DID,
        }))
    }

    async fn project(
        node: &Arc<EmbeddedNode>,
        parent: &AgentRequestRow,
        delivered: &[&str],
    ) -> SubagentProjection {
        let delivered = delivered.iter().map(|run| (*run).to_owned()).collect();
        project_caused_sessions(node, parent, None, 100, &delivered)
            .await
            .unwrap()
    }

    fn spawned(projection: &SubagentProjection) -> &SubagentSpawnedUpdate {
        match &projection.updates[0] {
            SubagentUpdate::Spawned(update) => update,
            other => panic!("expected spawned, got {other:?}"),
        }
    }

    fn finished(projection: &SubagentProjection) -> &SubagentFinishedUpdate {
        match &projection.updates[1] {
            SubagentUpdate::Finished(update) => update,
            other => panic!("expected finished, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn job_child_card_carries_run_id_after_first_frame() {
        let dir = tempfile::tempdir().unwrap();
        let node = embedded_node(&dir).await;
        let parent = seed_job_child(&node).await;
        let projection = project(&node, &parent, &["call-run"]).await;
        assert_eq!(
            spawned(&projection).workflow_run_id.as_deref(),
            Some("call-run")
        );
        assert_eq!(
            projection.updates[0].to_payload()["workflow_run_id"],
            "call-run"
        );
        node.shutdown().await;
    }

    #[tokio::test]
    async fn undelivered_or_abandoned_run_omits_run_id() {
        let dir = tempfile::tempdir().unwrap();
        let node = embedded_node(&dir).await;
        let parent = seed_job_child(&node).await;
        for delivered in [&[][..], &["call-other"][..]] {
            let projection = project(&node, &parent, delivered).await;
            let card = spawned(&projection);
            assert_eq!(card.workflow_run_id, None, "{delivered:?}");
            assert_eq!(card.subagent_id, "child");
            assert!(projection.updates[0]
                .to_payload()
                .get("workflow_run_id")
                .is_none());
        }
        node.shutdown().await;
    }

    #[tokio::test]
    async fn abandoned_run_omits_run_id() {
        use crate::commands::grok_shim::projection::{BoundModelContext, ProjectionEngine};
        let dir = tempfile::tempdir().unwrap();
        let node = embedded_node(&dir).await;
        let parent = seed_job_child(&node).await;
        let engine = ProjectionEngine::new(
            node.clone(),
            BoundModelContext::new("model".into(), "Model".into(), 1000),
        );
        // A tick that delivered the run, then one that abandoned it: the cursor
        // reports the run's tool-call id in the first and drops it in the second.
        engine.record_job_runs("parent", &BTreeSet::from(["call-run".to_owned()]));
        let delivered: Vec<_> = engine.delivered_job_runs().into_iter().collect();
        let delivered: Vec<&str> = delivered.iter().map(String::as_str).collect();
        assert_eq!(
            spawned(&project(&node, &parent, &delivered).await)
                .workflow_run_id
                .as_deref(),
            Some("call-run")
        );
        engine.record_job_runs("parent", &BTreeSet::new());
        assert!(engine.delivered_job_runs().is_empty());
        let projection =
            project_caused_sessions(&node, &parent, None, 100, &engine.delivered_job_runs())
                .await
                .unwrap();
        assert_eq!(spawned(&projection).workflow_run_id, None);
        node.shutdown().await;
    }

    #[tokio::test]
    async fn job_child_uses_spawn_title_and_will_wake_false() {
        let dir = tempfile::tempdir().unwrap();
        let node = embedded_node(&dir).await;
        let parent = seed_job_child(&node).await;
        let projection = project(&node, &parent, &["call-run"]).await;
        assert_eq!(spawned(&projection).description, "Audit the repo");
        assert!(!finished(&projection).will_wake);
        node.shutdown().await;
    }

    #[tokio::test]
    async fn agent_new_children_project_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let node = embedded_node(&dir).await;
        let parent = seed_child(&node, "").await;
        let projection = project(&node, &parent, &["call-run"]).await;
        assert_eq!(
            (
                spawned(&projection).workflow_run_id.as_deref(),
                spawned(&projection).description.as_str()
            ),
            (None, "work")
        );
        assert!(finished(&projection).will_wake);
        assert_eq!(projection.updates.len(), 2);
        node.shutdown().await;
    }

    #[tokio::test]
    async fn unreadable_job_child_title_falls_back_to_the_first_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let node = embedded_node(&dir).await;
        let parent = seed_job_child(&node).await;
        // The same label on another node makes the requester-scoped read ambiguous.
        create(
            &node,
            "AgentSession",
            &format!(
                r#"session_id: "child", node_did: "did:test:other", requester_did: "{DID}", agent_id: "worker", created_at: "2026-09-26T00:00:02Z", title: {{ text: "Duplicate", source: "task" }}, provenance: {{ parent_request_doc_id: "{}" }}"#,
                parent.doc_id.as_deref().unwrap()
            ),
        )
        .await;
        let projection = project(&node, &parent, &["call-run"]).await;
        assert_eq!(spawned(&projection).description, "work");
        assert_eq!(
            spawned(&projection).workflow_run_id.as_deref(),
            Some("call-run")
        );
        node.shutdown().await;
    }

    #[tokio::test]
    async fn generated_plugin_job_lineage_cases_drive_grok_cards() {
        let snapshot: Value = gents_lean_contract::load_contract_snapshot().unwrap();
        for case in snapshot["plugin_job_lineage_cases"].as_array().unwrap() {
            let mut case = case.clone();
            let queries = case["queries"].as_array().unwrap().clone();
            let mut causes: Vec<&str> = queries
                .iter()
                .map(|query| query["cause_request_doc_id"].as_str().unwrap())
                .collect();
            causes.dedup();
            // A cause outside the case's rows is the calling request: it opens
            // the calling session, ahead of the rows that name it.
            let rows = case["rows"].as_array_mut().unwrap();
            for cause in causes.iter().rev() {
                if !rows.iter().any(|row| row["_docID"] == *cause) {
                    rows.insert(
                        0,
                        json!({
                            "created_at": 1, "_docID": cause, "session_id": "parent",
                            "is_public": true, "caused_by_parent_request_doc_id": null,
                            "caused_by_parent_tool_call_doc_id": null,
                            "opening_request_doc_id": null,
                        }),
                    );
                }
            }
            let dir = tempfile::tempdir().unwrap();
            let node = embedded_node(&dir).await;
            let ids = crate::caused_sessions::fixtures::seed_lineage_case(&node, &case, DID).await;
            for cause in causes {
                let parent = request(json!({
                    "_docID": ids[cause], "request_id": cause, "session_id": "parent",
                    "node_did": DID, "requester_did": DID,
                }));
                let mut expected: Vec<(String, Option<String>)> = queries
                    .iter()
                    .filter(|query| query["cause_request_doc_id"] == cause)
                    .filter(|query| query["listed_start"] == true)
                    .filter_map(|query| {
                        let opening = query["expected_opening_doc_id"].as_str()?;
                        let session = query["session_id"].as_str().unwrap();
                        Some((session.to_owned(), Some(ids[opening].clone())))
                    })
                    .collect();
                expected.sort();
                let mut opened: Vec<_> = caused_by_request(&node, &parent)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|session| (session.scope.session_id, session.first.doc_id))
                    .collect();
                opened.sort();
                assert_eq!(opened, expected, "{}: {cause}", case["name"]);
                let mut cards: Vec<String> = project(&node, &parent, &[])
                    .await
                    .updates
                    .iter()
                    .filter_map(|update| match update {
                        SubagentUpdate::Spawned(card) => Some(card.child_session_id.clone()),
                        _ => None,
                    })
                    .collect();
                cards.sort();
                let sessions: Vec<String> =
                    expected.into_iter().map(|(session, _)| session).collect();
                assert_eq!(cards, sessions, "{}: {cause}", case["name"]);
            }
            node.shutdown().await;
        }
    }
}
