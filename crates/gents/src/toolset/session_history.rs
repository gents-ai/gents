use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use crate::llm::tool::ToolDefinition;
use crate::llm::tool::{Tool, ToolDyn};
use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::graphql::escape_graphql_string;

pub const SESSION_HISTORY_TOOL_NAME: &str = "sessions";

const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 1000;
const REQUEST_SCAN_LIMIT: usize = 5000;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionHistoryParams {
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionHistorySnapshot {
    pub agent_did: String,
    pub limit: usize,
    pub request_scan_limit: usize,
    pub sessions: Vec<SessionHistoryRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionHistoryRow {
    pub session_id: String,
    pub agent_name: Option<String>,
    pub behavior_id: Option<String>,
    pub status: Option<String>,
    pub created_at: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub latest_request_id: Option<String>,
    pub latest_request_status: Option<String>,
    pub latest_request_lifecycle_state: Option<String>,
    pub latest_request_created_at: Option<String>,
    pub request_count: i64,
    pub message_count: i64,
    pub latest_message_at: Option<String>,
    pub compaction_count: i64,
    pub last_compacted_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RequestScanEnvelope {
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<RequestRow>,
}

#[derive(Debug, Deserialize)]
struct SessionDetailEnvelope {
    #[serde(rename = "AgentSession", default)]
    sessions: Vec<SessionRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<RequestRow>,
    #[serde(rename = "AgentMessage", default)]
    messages: Vec<MessageRow>,
    #[serde(rename = "CompactionEntry", default)]
    compactions: Vec<CompactionRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionRow {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    agent_name: Option<String>,
    #[serde(default)]
    behavior_id: Option<String>,
    #[serde(default)]
    started: Option<String>,
    #[serde(default)]
    ended: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RequestRow {
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    behavior_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct MessageRow {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CompactionRow {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
}

#[derive(Debug)]
pub struct SessionHistoryToolError(anyhow::Error);

impl std::fmt::Display for SessionHistoryToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for SessionHistoryToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.root_cause())
    }
}

impl From<anyhow::Error> for SessionHistoryToolError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

#[derive(Clone)]
pub struct SessionHistoryTool {
    node: Arc<EmbeddedNode>,
    agent_did: String,
}

impl SessionHistoryTool {
    pub fn new(node: Arc<EmbeddedNode>, agent_did: impl Into<String>) -> Self {
        Self {
            node,
            agent_did: agent_did.into(),
        }
    }
}

impl Tool for SessionHistoryTool {
    const NAME: &'static str = SESSION_HISTORY_TOOL_NAME;

    type Error = SessionHistoryToolError;
    type Args = SessionHistoryParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "List this agent's recent persisted sessions, including session status, \
                latest request state, request/message counts, and compaction activity."
                .to_string(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list"],
                        "description": "Action to run. Defaults to list."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_LIMIT,
                        "description": "Maximum number of recent sessions to return."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        validate_action(args.action.as_deref())?;
        let snapshot =
            load_session_history_snapshot(&self.node, &self.agent_did, args.limit).await?;
        serde_json::to_string_pretty(&snapshot).map_err(|error| {
            SessionHistoryToolError(anyhow!(
                "failed to serialize session history output: {error}"
            ))
        })
    }
}

pub fn build_session_history_tool(
    node: Arc<EmbeddedNode>,
    agent_did: impl Into<String>,
) -> Box<dyn ToolDyn> {
    Box::new(SessionHistoryTool::new(node, agent_did))
}

pub async fn load_session_history_snapshot(
    node: &EmbeddedNode,
    agent_did: &str,
    limit: Option<usize>,
) -> Result<SessionHistorySnapshot> {
    let agent_did = agent_did.trim();
    if agent_did.is_empty() {
        bail!("sessions tool requires a running agent DID");
    }

    let limit = clamp_limit(limit);
    let resp = node.execute(&request_scan_query(agent_did)).await;
    if resp.has_errors() {
        bail!(
            "loading session history request scan failed: {:?}",
            resp.errors
        );
    }
    let envelope: RequestScanEnvelope = decode(resp.data.as_ref(), "session history request scan")?;
    let session_ids = recent_session_ids(&envelope.requests, limit);

    let sessions = if session_ids.is_empty() {
        Vec::new()
    } else {
        let resp = node
            .execute(&session_detail_query(agent_did, &session_ids))
            .await;
        if resp.has_errors() {
            bail!("loading session history details failed: {:?}", resp.errors);
        }
        let envelope: SessionDetailEnvelope =
            decode(resp.data.as_ref(), "session history details")?;
        build_session_rows(&session_ids, envelope)
    };

    Ok(SessionHistorySnapshot {
        agent_did: agent_did.to_string(),
        limit,
        request_scan_limit: REQUEST_SCAN_LIMIT,
        sessions,
    })
}

fn validate_action(action: Option<&str>) -> Result<()> {
    match action.map(str::trim).filter(|action| !action.is_empty()) {
        None | Some("list") => Ok(()),
        Some(other) => bail!("unsupported sessions action '{other}'; supported action: list"),
    }
}

fn clamp_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

fn request_scan_query(agent_did: &str) -> String {
    let agent_did = escape_graphql_string(agent_did);
    format!(
        r#"{{
            AgentRequest(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}, order: {{ created_at: DESC }}, limit: {REQUEST_SCAN_LIMIT}) {{
                session_id
            }}
        }}"#
    )
}

fn session_detail_query(agent_did: &str, session_ids: &[String]) -> String {
    let agent_did = escape_graphql_string(agent_did);
    let list = quoted_graphql_list(session_ids);
    format!(
        r#"{{
            AgentSession(filter: {{ session_id: {{ _in: [{list}] }} }}) {{
                session_id
                agent_name
                behavior_id
                started
                ended
                status
            }}
            AgentRequest(
                filter: {{ _and: [
                    {{ agent_did: {{ _eq: "{agent_did}" }} }},
                    {{ session_id: {{ _in: [{list}] }} }}
                ] }},
                order: {{ created_at: DESC }}
            ) {{
                request_id
                session_id
                behavior_id
                status
                lifecycle_state
                created_at
            }}
            AgentMessage(filter: {{ session_id: {{ _in: [{list}] }} }}) {{
                session_id
                timestamp
            }}
            CompactionEntry(filter: {{ session_id: {{ _in: [{list}] }} }}) {{
                session_id
                created_at
            }}
        }}"#
    )
}

fn quoted_graphql_list(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn decode<T: serde::de::DeserializeOwned>(data: Option<&Value>, label: &str) -> Result<T> {
    let data = data
        .filter(|data| data.is_object())
        .cloned()
        .with_context(|| format!("{label} query response missing object data"))?;
    serde_json::from_value(data).with_context(|| format!("decoding {label} query response"))
}

fn recent_session_ids(requests: &[RequestRow], limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for request in requests {
        let Some(session_id) = clean(request.session_id.as_ref()) else {
            continue;
        };
        if seen.insert(session_id.clone()) {
            ids.push(session_id);
            if ids.len() >= limit {
                break;
            }
        }
    }
    ids
}

fn build_session_rows(
    session_ids: &[String],
    envelope: SessionDetailEnvelope,
) -> Vec<SessionHistoryRow> {
    let SessionDetailEnvelope {
        sessions,
        requests,
        messages,
        compactions,
    } = envelope;
    let sessions_by_id = sessions
        .into_iter()
        .filter_map(|session| clean(Some(&session.session_id)).map(|id| (id, session)))
        .collect::<BTreeMap<_, _>>();
    let session_id_set = session_ids.iter().cloned().collect::<HashSet<_>>();
    let aggregates = aggregate_by_session(requests, messages, compactions, &session_id_set);

    session_ids
        .iter()
        .map(|session_id| {
            let session = sessions_by_id.get(session_id);
            let aggregate = aggregates.get(session_id);
            let latest_request = aggregate.and_then(|aggregate| aggregate.latest_request.as_ref());
            let started_at = session.and_then(|row| clean(row.started.as_ref()));
            let latest_request_created_at =
                latest_request.and_then(|row| clean(row.created_at.as_ref()));

            SessionHistoryRow {
                session_id: session_id.clone(),
                agent_name: session.and_then(|row| clean(row.agent_name.as_ref())),
                behavior_id: session
                    .and_then(|row| clean(row.behavior_id.as_ref()))
                    .or_else(|| latest_request.and_then(|row| clean(row.behavior_id.as_ref()))),
                status: session
                    .and_then(|row| clean(row.status.as_ref()))
                    .or_else(|| latest_request.and_then(|row| clean(row.status.as_ref()))),
                created_at: started_at
                    .clone()
                    .or_else(|| latest_request_created_at.clone()),
                started_at,
                ended_at: session.and_then(|row| clean(row.ended.as_ref())),
                latest_request_id: latest_request.and_then(|row| clean(row.request_id.as_ref())),
                latest_request_status: latest_request.and_then(|row| clean(row.status.as_ref())),
                latest_request_lifecycle_state: latest_request
                    .and_then(|row| clean(row.lifecycle_state.as_ref())),
                latest_request_created_at,
                request_count: aggregate
                    .map(|aggregate| aggregate.request_count)
                    .unwrap_or(0),
                message_count: aggregate
                    .map(|aggregate| aggregate.message_count)
                    .unwrap_or(0),
                latest_message_at: aggregate
                    .and_then(|aggregate| aggregate.latest_message_at.clone()),
                compaction_count: aggregate
                    .map(|aggregate| aggregate.compaction_count)
                    .unwrap_or(0),
                last_compacted_at: aggregate
                    .and_then(|aggregate| aggregate.last_compacted_at.clone()),
            }
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
struct SessionAggregate {
    request_count: i64,
    latest_request: Option<RequestRow>,
    message_count: i64,
    latest_message_at: Option<String>,
    compaction_count: i64,
    last_compacted_at: Option<String>,
}

fn aggregate_by_session(
    requests: Vec<RequestRow>,
    messages: Vec<MessageRow>,
    compactions: Vec<CompactionRow>,
    session_id_set: &HashSet<String>,
) -> BTreeMap<String, SessionAggregate> {
    let mut aggregates = BTreeMap::<String, SessionAggregate>::new();

    for request in requests {
        let Some(session_id) = clean(request.session_id.as_ref()) else {
            continue;
        };
        if !session_id_set.contains(&session_id) {
            continue;
        }
        let aggregate = aggregates.entry(session_id).or_default();
        aggregate.request_count += 1;
        if aggregate.latest_request.is_none()
            || should_replace_latest(
                aggregate
                    .latest_request
                    .as_ref()
                    .and_then(|row| clean(row.created_at.as_ref()))
                    .as_deref(),
                clean(request.created_at.as_ref()).as_deref(),
            )
        {
            aggregate.latest_request = Some(request);
        }
    }

    for message in messages {
        let Some(session_id) = clean(message.session_id.as_ref()) else {
            continue;
        };
        if !session_id_set.contains(&session_id) {
            continue;
        }
        let aggregate = aggregates.entry(session_id).or_default();
        aggregate.message_count += 1;
        update_latest(&mut aggregate.latest_message_at, message.timestamp.as_ref());
    }

    for compaction in compactions {
        let Some(session_id) = clean(compaction.session_id.as_ref()) else {
            continue;
        };
        if !session_id_set.contains(&session_id) {
            continue;
        }
        let aggregate = aggregates.entry(session_id).or_default();
        aggregate.compaction_count += 1;
        update_latest(
            &mut aggregate.last_compacted_at,
            compaction.created_at.as_ref(),
        );
    }

    aggregates
}

fn update_latest(current: &mut Option<String>, candidate: Option<&String>) {
    let Some(candidate) = clean(candidate) else {
        return;
    };
    if should_replace_latest(current.as_deref(), Some(candidate.as_str())) {
        *current = Some(candidate);
    }
}

fn should_replace_latest(current: Option<&str>, candidate: Option<&str>) -> bool {
    match (current, candidate) {
        (None, Some(_)) => true,
        (Some(current), Some(candidate)) => candidate > current,
        _ => false,
    }
}

fn clean(value: Option<&String>) -> Option<String> {
    value
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::llm::tool::Tool;

    use super::*;

    #[test]
    fn clamp_limit_honors_large_requests_up_to_the_backstop() {
        // Unset → default.
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        // Floor: a zero/garbage request is raised to 1, never 0.
        assert_eq!(clamp_limit(Some(0)), 1);
        // SP3 de-cap: a large requested count is honored (was clamped at 50).
        assert_eq!(clamp_limit(Some(750)), 750);
        // Only the backstop caps it.
        assert_eq!(clamp_limit(Some(MAX_LIMIT + 5_000)), MAX_LIMIT);
        // The scan budget must be able to surface MAX_LIMIT distinct sessions.
        const {
            assert!(
                REQUEST_SCAN_LIMIT >= MAX_LIMIT,
                "REQUEST_SCAN_LIMIT must stay >= MAX_LIMIT or the cap is unreachable"
            );
        }
    }

    async fn seeded_node() -> Arc<EmbeddedNode> {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

        for mutation in [
            r#"mutation {
                create_AgentSession(input: {
                    session_id: "session-a",
                    agent_name: "OpenAI Agent",
                    behavior_id: "behavior-a",
                    started: "2026-06-03T09:55:00Z",
                    status: "open"
                }) { _docID }
            }"#,
            r#"mutation {
                create_AgentSession(input: {
                    session_id: "session-b",
                    agent_name: "OpenAI Agent",
                    behavior_id: "behavior-b",
                    started: "2026-06-03T10:55:00Z",
                    status: "open"
                }) { _docID }
            }"#,
            r#"mutation {
                create_AgentRequest(input: {
                    request_id: "request-a-old",
                    agent_did: "did:key:z-sessions",
                    behavior_id: "behavior-a",
                    session_id: "session-a",
                    status: "completed",
                    lifecycle_state: "completed",
                    created_at: "2026-06-03T10:00:00Z"
                }) { _docID }
            }"#,
            r#"mutation {
                create_AgentRequest(input: {
                    request_id: "request-a-new",
                    agent_did: "did:key:z-sessions",
                    behavior_id: "behavior-a",
                    session_id: "session-a",
                    status: "processing",
                    lifecycle_state: "processing",
                    created_at: "2026-06-03T10:05:00Z"
                }) { _docID }
            }"#,
            r#"mutation {
                create_AgentRequest(input: {
                    request_id: "request-b-new",
                    agent_did: "did:key:z-sessions",
                    behavior_id: "behavior-b",
                    session_id: "session-b",
                    status: "completed",
                    lifecycle_state: "completed",
                    created_at: "2026-06-03T11:00:00Z"
                }) { _docID }
            }"#,
            r#"mutation {
                create_AgentRequest(input: {
                    request_id: "request-other-agent",
                    agent_did: "did:key:z-other",
                    behavior_id: "behavior-c",
                    session_id: "session-c",
                    status: "completed",
                    lifecycle_state: "completed",
                    created_at: "2026-06-03T12:00:00Z"
                }) { _docID }
            }"#,
            r#"mutation {
                create_AgentMessage(input: {
                    message_key: "session-a:1",
                    session_id: "session-a",
                    sequence: 1,
                    role: "user",
                    content: "hello",
                    timestamp: "2026-06-03T10:01:00Z"
                }) { _docID }
            }"#,
            r#"mutation {
                create_AgentMessage(input: {
                    message_key: "session-a:2",
                    session_id: "session-a",
                    sequence: 2,
                    role: "assistant",
                    content: "hi",
                    timestamp: "2026-06-03T10:06:00Z"
                }) { _docID }
            }"#,
            r#"mutation {
                create_CompactionEntry(input: {
                    compaction_key: "session-a:1",
                    session_id: "session-a",
                    sequence: 1,
                    original_tokens: 800,
                    compacted_tokens: 400,
                    created_at: "2026-06-03T10:07:00Z"
                }) { _docID }
            }"#,
        ] {
            let response = node.execute(mutation).await;
            assert!(!response.has_errors(), "seed failed: {:?}", response.errors);
        }

        node
    }

    #[tokio::test]
    async fn session_history_snapshot_reports_recent_agent_sessions() {
        let node = seeded_node().await;

        let snapshot = load_session_history_snapshot(&node, "did:key:z-sessions", Some(2))
            .await
            .unwrap();

        assert_eq!(snapshot.agent_did, "did:key:z-sessions");
        assert_eq!(snapshot.limit, 2);
        assert_eq!(snapshot.request_scan_limit, REQUEST_SCAN_LIMIT);
        assert_eq!(
            snapshot
                .sessions
                .iter()
                .map(|session| session.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["session-b", "session-a"]
        );

        let session_a = snapshot
            .sessions
            .iter()
            .find(|session| session.session_id == "session-a")
            .unwrap();
        assert_eq!(session_a.agent_name.as_deref(), Some("OpenAI Agent"));
        assert_eq!(session_a.behavior_id.as_deref(), Some("behavior-a"));
        assert_eq!(session_a.status.as_deref(), Some("open"));
        assert_eq!(
            session_a.created_at.as_deref(),
            Some("2026-06-03T09:55:00Z")
        );
        assert_eq!(
            session_a.latest_request_id.as_deref(),
            Some("request-a-new")
        );
        assert_eq!(
            session_a.latest_request_lifecycle_state.as_deref(),
            Some("processing")
        );
        assert_eq!(session_a.request_count, 2);
        assert_eq!(session_a.message_count, 2);
        assert_eq!(
            session_a.latest_message_at.as_deref(),
            Some("2026-06-03T10:06:00Z")
        );
        assert_eq!(session_a.compaction_count, 1);
        assert_eq!(
            session_a.last_compacted_at.as_deref(),
            Some("2026-06-03T10:07:00Z")
        );
    }

    #[tokio::test]
    async fn sessions_tool_serializes_limited_history() {
        let node = seeded_node().await;
        let tool = SessionHistoryTool::new(node, "did:key:z-sessions");

        let output = Tool::call(
            &tool,
            SessionHistoryParams {
                action: Some("list".to_string()),
                limit: Some(1),
            },
        )
        .await
        .unwrap();
        let parsed: SessionHistorySnapshot = serde_json::from_str(&output).unwrap();

        assert_eq!(parsed.limit, 1);
        assert_eq!(parsed.sessions.len(), 1);
        assert_eq!(parsed.sessions[0].session_id, "session-b");
    }

    #[tokio::test]
    async fn sessions_tool_rejects_unsupported_action() {
        let node = seeded_node().await;
        let tool = SessionHistoryTool::new(node, "did:key:z-sessions");

        let error = Tool::call(
            &tool,
            SessionHistoryParams {
                action: Some("read".to_string()),
                limit: None,
            },
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("unsupported sessions action"));
    }
}
