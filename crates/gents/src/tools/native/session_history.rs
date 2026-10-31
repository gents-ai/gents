use std::collections::BTreeMap;
use std::sync::Arc;

mod context_calls;
pub use context_calls::load_latest_session_context_calls;
mod context_details;
mod discovery;
pub use context_details::{load_session_context_details, SessionContextDetails};
mod inference;
mod investigation;
pub use investigation::load_session_investigation;
use investigation::{
    AgentDetailRow, CompactionConfigDetailRow, ContextDetailRow, ProfileDetailRow,
    ToolCallDetailRow,
};
mod observation;
pub use observation::{
    load_pinned_request_context_observation, load_request_context_observation,
    load_session_inference_observation, load_session_request_doc_ids,
};
mod snapshot;
pub use snapshot::{load_session_history_snapshot, load_session_history_snapshot_with_access};

use crate::inference::llm::tool::ToolDefinition;
use crate::inference::llm::tool::{Tool, ToolDyn};
use anyhow::{anyhow, Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::store::graphql::escape_graphql_string;

pub const SESSION_HISTORY_TOOL_NAME: &str = "sessions";

const ACTIONS: [&str; 7] = [
    "help",
    "list",
    "count",
    "search",
    "get",
    "transcript",
    "output",
];

pub(crate) fn is_current_session(
    caller_owner: &str,
    caller_session: &str,
    listed_owner: &str,
    listed_session: &str,
) -> bool {
    caller_owner == listed_owner && caller_session == listed_session
}

const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 1000;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHistoryParams {
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub filter: Option<discovery::SessionFilter>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub details: bool,
    #[serde(default)]
    pub call_id: Option<String>,
    #[serde(default)]
    pub tool_call_doc_id: Option<String>,
    #[serde(default)]
    pub offset_bytes: Option<usize>,
    #[serde(default)]
    pub expected_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionHistorySnapshot {
    pub node_did: String,
    pub limit: usize,
    pub sessions: Vec<SessionHistoryRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionHistoryRow {
    pub session_id: String,
    #[serde(default)]
    pub is_current: bool,
    pub agent_id: Option<String>,
    pub title: Option<gents_protocol::session::SessionTitle>,
    pub tags: Vec<String>,
    pub created_at: Option<String>,
    pub closed_at: Option<String>,
    pub latest_request_id: Option<String>,
    pub latest_request_lifecycle_state: Option<RequestLifecycleState>,
    pub latest_request_created_at: Option<String>,
    pub request_count: i64,
    pub message_count: i64,
    pub latest_message_at: Option<String>,
    pub compaction_count: i64,
    pub last_compacted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionInvestigationSnapshot {
    pub node_did: String,
    pub session_id: String,
    pub requests: Vec<SessionRequestEvent>,
    pub tool_calls: SessionToolCallStats,
    pub token_usage: SessionTokenUsage,
    pub parent_inclusive_audit_usage: Vec<super::ParentAuditUsageObservation>,
    pub compactions: Vec<SessionCompactionEvent>,
    pub latest_context: Option<super::context_budget::LastRequestContextSnapshot>,
    pub compaction_strategy: crate::inference::compaction::CompactionStrategy,
    pub compaction_threshold: Option<f64>,
    pub context_window: Option<i64>,
    pub parent_request_ids: Vec<String>,
    pub child_session_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRequestEvent {
    pub request_id: String,
    pub lifecycle_state: Option<String>,
    pub created_at: Option<String>,
    pub terminalized_at: Option<String>,
    pub failure_reason: Option<String>,
    pub deadline: Option<String>,
    pub retry_count: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionToolCallStats {
    pub total: i64,
    pub by_tool: BTreeMap<String, i64>,
    pub by_status: BTreeMap<String, i64>,
    pub total_latency_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionTokenUsage {
    pub model_calls: i64,
    pub calls_with_usage: i64,
    pub calls_missing_usage: i64,
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub provider_total_tokens: Option<u64>,
    pub fresh_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub charged_tokens: Option<u64>,
    #[serde(default)]
    pub api_duration_ms: Option<u64>,
    pub incomplete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionInferenceObservation {
    pub token_usage: SessionTokenUsage,
    pub inference_turns: Option<u64>,
    pub latest_context: Option<super::context_budget::LastRequestContextSnapshot>,
    pub latest_completion_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestContextObservation {
    pub context: super::context_budget::LastRequestContextSnapshot,
    pub completion_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionCompactionEvent {
    pub scope: String,
    pub created_at: Option<String>,
    pub messages_compacted: Option<i64>,
    pub original_tokens: Option<i64>,
    pub compacted_tokens: Option<i64>,
    pub compacted_through_sequence: Option<i64>,
    pub request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InvestigationEnvelope {
    #[serde(rename = "AgentSession", default)]
    sessions: Vec<SessionRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<AgentRequestRow>,
    #[serde(rename = "AgentToolCall", default)]
    tool_calls: Vec<ToolCallDetailRow>,
    #[serde(rename = "CompactionEntry", default)]
    compactions: Vec<CompactionRow>,
    #[serde(rename = "ProviderContextReduction", default)]
    provider_reductions: Vec<CompactionRow>,
    #[serde(rename = "Agent", default)]
    agents: Vec<AgentDetailRow>,
    #[serde(rename = "AgentContext", default)]
    contexts: Vec<ContextDetailRow>,
    #[serde(rename = "CompactionConfig", default)]
    compactions_config: Vec<CompactionConfigDetailRow>,
    #[serde(rename = "InferenceProfile", default)]
    profiles: Vec<ProfileDetailRow>,
}

#[derive(Debug, Deserialize)]
struct InvestigationCallsEnvelope {
    #[serde(rename = "InferenceCall", default)]
    inference_calls: Vec<InferenceDetailRow>,
    #[serde(rename = "AgentRequest", default)]
    child_requests: Vec<AgentRequestRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionRow {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    title: Option<gents_protocol::session::SessionTitle>,
    #[serde(default, deserialize_with = "deserialize_null_tags")]
    tags: Vec<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    closed_at: Option<String>,
    #[serde(default)]
    observation: Option<gents_protocol::session::SessionObservation>,
}

#[derive(Debug, Clone, Deserialize)]
struct CompactionRow {
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    messages_compacted: Option<i64>,
    #[serde(default)]
    original_tokens: Option<i64>,
    #[serde(default)]
    compacted_tokens: Option<i64>,
    #[serde(default)]
    compacted_through_sequence: Option<i64>,
    #[serde(default)]
    request_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct InferenceDetailRow {
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    ended_at: Option<String>,
    #[serde(default)]
    request_doc_id: Option<String>,
    #[serde(default)]
    call_kind: Option<String>,
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    call_id: String,
    #[serde(default)]
    call_seq: i64,
    #[serde(default)]
    queued_at: Option<String>,
    #[serde(flatten)]
    usage: gents_protocol::usage::TokenUsageColumns,
    #[serde(default)]
    context_accounting_json: Option<String>,
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
    node_did: String,
}

impl SessionHistoryTool {
    pub fn new(node: Arc<EmbeddedNode>, node_did: impl Into<String>) -> Self {
        Self {
            node,
            node_did: node_did.into(),
        }
    }
}

impl Tool for SessionHistoryTool {
    const NAME: &'static str = SESSION_HISTORY_TOOL_NAME;

    type Error = SessionHistoryToolError;
    type Args = SessionHistoryParams;
    type Output = String;

    /// Anthropic Messages rejects `oneOf`, `allOf` and `anyOf` at the top
    /// level of `input_schema`, so per-action requirements are enforced by
    /// `call` with a recovery hint rather than by conditional schema clauses.
    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Discover, count and search sessions readable through DefraDB ACP; inspect canonical transcript evidence; recover a tool call's full output by call_id. Use help for available actions and help topic for action syntax.".into(),
            parameters: json!({
                "type":"object", "additionalProperties":false,
                "properties": {
                    "action":{"type":"string","enum":ACTIONS,"description":"Defaults to list. Search requires query; get and transcript require session_id; output requires call_id or tool_call_doc_id."},
                    "topic":{"type":"string","enum":&ACTIONS[1..],"description":"Action to explain when action is help."},
                    "limit":{"type":"integer","minimum":1,"maximum":100},
                    "session_id":{"type":"string","minLength":1,"description":"Session ID returned by list or search; required for get and transcript. Output defaults to the current session."},
                    "filter":{"type":"object","additionalProperties":false,"properties":{
                        "agent_id":{"type":"string"},"status":{"type":"string","enum":["open","closed"]},
                        "tag":{"type":"string"},"created_after":{"type":"string"},"created_before":{"type":"string"},"text":{"type":"string","description":"Title or session ID substring only; use query to search transcript text."}
                    }},
                    "cursor":{"type":"string","description":"Previous next_cursor with the same action and filters."},"query":{"type":"string","minLength":1,"description":"Required for search: literal case-insensitive substring of title, ID or transcript."},"details":{"type":"boolean","description":"Include accounting and timeline for get."},
                    "call_id":{"type":"string","minLength":1,"description":"Output: the call_id of a tool result or [tool: …] stub."},
                    "tool_call_doc_id":{"type":"string","minLength":1,"description":"Output: exact tool call document, when a call_id is ambiguous."},
                    "offset_bytes":{"type":"integer","minimum":0,"description":"Output: from the previous next_call."},
                    "expected_hash":{"type":"string","description":"Output: from the previous next_call."}
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let action = args.action.as_deref().unwrap_or("list").trim();
        if !ACTIONS.contains(&action) {
            return Err(anyhow!(
                "unsupported sessions action; next call: sessions {{\"action\":\"help\"}}"
            )
            .into());
        }
        let value = discovery::run(self, &args, action).await.map_err(|error| {
            if format!("{error:#}").contains("next call:") {
                error
            } else {
                anyhow!("{error:#}; next call: sessions {{\"action\":\"help\"}}")
            }
        })?;
        Ok(if action == "output" {
            serde_json::to_string(&crate::tool_output::Ordered::reading_order(
                value,
                discovery::OUTPUT_ORDER,
            ))
        } else {
            serde_json::to_string(&discovery::AnswerFirst(&value))
        }
        .context("serialize sessions result")?)
    }
}

pub fn build_session_history_tool(
    node: Arc<EmbeddedNode>,
    node_did: impl Into<String>,
) -> Box<dyn ToolDyn> {
    Box::new(SessionHistoryTool::new(node, node_did))
}

fn clamp_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

fn quoted_graphql_list(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn decode<T>(data: Option<&Value>, label: &str) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let data = data
        .filter(|data| data.is_object())
        .cloned()
        .with_context(|| format!("{label} query response missing object data"))?;
    serde_json::from_value(data).with_context(|| format!("decoding {label} query response"))
}

fn clean(value: Option<&String>) -> Option<String> {
    value
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn deserialize_null_tags<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Vec<String>>::deserialize(deserializer)?.unwrap_or_default())
}

#[cfg(test)]
mod tests;
