use std::collections::{BTreeMap, BTreeSet, HashSet};
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
    #[serde(default)]
    pub session_id: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionInvestigationSnapshot {
    pub agent_did: String,
    pub session_id: String,
    pub requests: Vec<SessionRequestEvent>,
    pub tool_calls: SessionToolCallStats,
    pub token_usage: SessionTokenUsage,
    pub compactions: Vec<SessionCompactionEvent>,
    pub latest_context: Option<super::context_budget::LastRequestContextSnapshot>,
    pub compaction_strategy: Option<String>,
    pub compaction_threshold: Option<f64>,
    pub context_window: Option<i64>,
    pub parent_request_ids: Vec<String>,
    pub child_session_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRequestEvent {
    pub request_id: String,
    pub status: Option<String>,
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
    pub fresh_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub charged_tokens: Option<u64>,
    pub incomplete: bool,
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

#[derive(Debug, Deserialize)]
struct InvestigationEnvelope {
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<RequestRow>,
    #[serde(rename = "AgentToolCall", default)]
    tool_calls: Vec<ToolCallDetailRow>,
    #[serde(rename = "CompactionEntry", default)]
    compactions: Vec<CompactionRow>,
    #[serde(rename = "ProviderContextReduction", default)]
    provider_reductions: Vec<CompactionRow>,
    #[serde(rename = "AgentBehavior", default)]
    behaviors: Vec<BehaviorDetailRow>,
    #[serde(rename = "InferenceProfile", default)]
    profiles: Vec<ProfileDetailRow>,
}

#[derive(Debug, Deserialize)]
struct InvestigationCallsEnvelope {
    #[serde(rename = "InferenceCall", default)]
    inference_calls: Vec<InferenceDetailRow>,
    #[serde(rename = "AgentRequest", default)]
    child_requests: Vec<RequestRow>,
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
    #[serde(default, rename = "_docID")]
    doc_id: Option<String>,
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
    #[serde(default)]
    terminalized_at: Option<String>,
    #[serde(default)]
    failure_reason: Option<String>,
    #[serde(default)]
    deadline: Option<String>,
    #[serde(default)]
    retry_count: Option<i64>,
    #[serde(default)]
    caused_by_parent_request_id: Option<String>,
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
struct ToolCallDetailRow {
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    latency_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct InferenceDetailRow {
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    call_id: String,
    #[serde(default)]
    call_seq: i64,
    #[serde(default)]
    queued_at: Option<String>,
    #[serde(default)]
    prompt_tokens: Option<i64>,
    #[serde(default)]
    completion_tokens: Option<i64>,
    #[serde(default)]
    cached_input_tokens: Option<i64>,
    #[serde(default)]
    context_accounting_json: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct BehaviorDetailRow {
    #[serde(default)]
    behavior_id: String,
    #[serde(default)]
    inference_profile_id: Option<String>,
    #[serde(default)]
    compaction_strategy: Option<String>,
    #[serde(default)]
    compaction_threshold: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProfileDetailRow {
    #[serde(default)]
    profile_id: String,
    #[serde(default)]
    context_window: Option<i64>,
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
            description:
                "List this agent's recent persisted sessions or investigate one session's \
                request timeline, tool aggregates, provider token usage, exact latest context \
                accounting, compaction events, and subagent linkage."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "get"],
                        "description": "Action to run. Defaults to list."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_LIMIT,
                        "description": "Maximum number of recent sessions to return."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Required for get; the session to investigate."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let action = validate_action(args.action.as_deref())?;
        let output = match action {
            SessionHistoryAction::List => serde_json::to_value(
                load_session_history_snapshot(&self.node, &self.agent_did, args.limit).await?,
            ),
            SessionHistoryAction::Get => {
                let session_id = args
                    .session_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| anyhow!("sessions get requires session_id"))?;
                serde_json::to_value(
                    load_session_investigation(&self.node, &self.agent_did, session_id).await?,
                )
            }
        };
        let output = output.map_err(anyhow::Error::from)?;
        serde_json::to_string_pretty(&output).map_err(|error| {
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

pub async fn load_session_investigation(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
) -> Result<SessionInvestigationSnapshot> {
    let agent_did = agent_did.trim();
    let session_id = session_id.trim();
    if agent_did.is_empty() || session_id.is_empty() {
        bail!("sessions get requires a running agent DID and non-empty session_id");
    }

    let response = node
        .execute(&session_investigation_query(agent_did, session_id))
        .await;
    if response.has_errors() {
        bail!(
            "loading session investigation failed: {:?}",
            response.errors
        );
    }
    let envelope: InvestigationEnvelope = decode(response.data.as_ref(), "session investigation")?;
    if envelope.requests.is_empty() {
        bail!("session '{session_id}' has no requests owned by this agent");
    }

    let request_doc_ids = envelope
        .requests
        .iter()
        .filter_map(|request| clean(request.doc_id.as_ref()))
        .collect::<Vec<_>>();
    if request_doc_ids.len() != envelope.requests.len() {
        bail!("session investigation contains a request without a physical document ID");
    }
    let response = node
        .execute(&session_investigation_calls_query(
            agent_did,
            &request_doc_ids,
        ))
        .await;
    if response.has_errors() {
        bail!(
            "loading session investigation calls failed: {:?}",
            response.errors
        );
    }
    let calls: InvestigationCallsEnvelope =
        decode(response.data.as_ref(), "session investigation calls")?;

    Ok(build_session_investigation(
        agent_did, session_id, envelope, calls,
    )?)
}

fn session_investigation_query(agent_did: &str, session_id: &str) -> String {
    let agent_did = escape_graphql_string(agent_did);
    let session_id = escape_graphql_string(session_id);
    format!(
        r#"{{
            AgentRequest(
                filter: {{ _and: [
                    {{ agent_did: {{ _eq: "{agent_did}" }} }},
                    {{ session_id: {{ _eq: "{session_id}" }} }}
                ] }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                request_id
                session_id
                behavior_id
                status
                lifecycle_state
                created_at
                terminalized_at
                failure_reason
                deadline
                retry_count
                caused_by_parent_request_id
            }}
            AgentToolCall(
                filter: {{ _and: [
                    {{ agent_did: {{ _eq: "{agent_did}" }} }},
                    {{ session_id: {{ _eq: "{session_id}" }} }}
                ] }}
            ) {{
                tool_name
                lifecycle_state
                status
                latency_ms
            }}
            CompactionEntry(
                filter: {{ _and: [
                    {{ agent_did: {{ _eq: "{agent_did}" }} }},
                    {{ session_id: {{ _eq: "{session_id}" }} }}
                ] }},
                order: {{ sequence: ASC }}
            ) {{
                request_id
                created_at
                messages_compacted
                compacted_through_sequence
                original_tokens
                compacted_tokens
            }}
            ProviderContextReduction(
                filter: {{ _and: [
                    {{ agent_did: {{ _eq: "{agent_did}" }} }},
                    {{ session_id: {{ _eq: "{session_id}" }} }}
                ] }},
                order: {{ created_at: ASC }}
            ) {{
                request_id
                created_at
                messages_compacted
                original_tokens
                compacted_tokens
            }}
            AgentBehavior(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{
                behavior_id
                inference_profile_id
                compaction_strategy
                compaction_threshold
            }}
            InferenceProfile {{
                profile_id
                context_window
            }}
        }}"#
    )
}

fn session_investigation_calls_query(agent_did: &str, request_doc_ids: &[String]) -> String {
    let agent_did = escape_graphql_string(agent_did);
    let request_doc_ids = quoted_graphql_list(request_doc_ids);
    format!(
        r#"{{
            InferenceCall(
                filter: {{ _and: [
                    {{ agent_did: {{ _eq: "{agent_did}" }} }},
                    {{ request_doc_id: {{ _in: [{request_doc_ids}] }} }}
                ] }},
                order: {{ queued_at: ASC }}
            ) {{
                request_id
                call_id
                call_seq
                queued_at
                prompt_tokens
                completion_tokens
                cached_input_tokens
                context_accounting_json
            }}
            AgentRequest(
                filter: {{ caused_by_parent_request_doc_id: {{ _in: [{request_doc_ids}] }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                request_id
                session_id
                created_at
            }}
        }}"#
    )
}

fn build_session_investigation(
    agent_did: &str,
    session_id: &str,
    envelope: InvestigationEnvelope,
    calls: InvestigationCallsEnvelope,
) -> Result<SessionInvestigationSnapshot> {
    let latest_behavior_id = envelope
        .requests
        .iter()
        .rev()
        .find_map(|request| clean(request.behavior_id.as_ref()));
    let behavior = latest_behavior_id.as_deref().and_then(|behavior_id| {
        envelope
            .behaviors
            .iter()
            .find(|behavior| behavior.behavior_id == behavior_id)
    });
    let profile = behavior
        .and_then(|behavior| behavior.inference_profile_id.as_deref())
        .and_then(|profile_id| {
            envelope
                .profiles
                .iter()
                .find(|profile| profile.profile_id == profile_id)
        });
    let parent_request_ids = envelope
        .requests
        .iter()
        .filter_map(|request| clean(request.caused_by_parent_request_id.as_ref()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let requests = envelope
        .requests
        .iter()
        .map(|request| SessionRequestEvent {
            request_id: clean(request.request_id.as_ref()).unwrap_or_default(),
            status: clean(request.status.as_ref()),
            lifecycle_state: clean(request.lifecycle_state.as_ref()),
            created_at: clean(request.created_at.as_ref()),
            terminalized_at: clean(request.terminalized_at.as_ref()),
            failure_reason: clean(request.failure_reason.as_ref()),
            deadline: clean(request.deadline.as_ref()),
            retry_count: request.retry_count,
        })
        .collect();

    let mut tool_calls = SessionToolCallStats::default();
    for call in envelope.tool_calls {
        tool_calls.total += 1;
        let tool = clean(call.tool_name.as_ref()).unwrap_or_else(|| "unknown".to_string());
        *tool_calls.by_tool.entry(tool).or_default() += 1;
        let status = clean(call.lifecycle_state.as_ref())
            .or_else(|| clean(call.status.as_ref()))
            .unwrap_or_else(|| "unknown".to_string());
        *tool_calls.by_status.entry(status).or_default() += 1;
        tool_calls.total_latency_ms = tool_calls
            .total_latency_ms
            .saturating_add(call.latency_ms.unwrap_or_default().max(0));
    }

    let model_calls = calls.inference_calls.len() as i64;
    let valid_usage_rows = calls
        .inference_calls
        .iter()
        .filter(|call| match (call.prompt_tokens, call.completion_tokens) {
            (Some(prompt), Some(completion)) if prompt >= 0 && completion >= 0 => call
                .cached_input_tokens
                .is_some_and(|cached| cached >= 0 && cached <= prompt),
            _ => false,
        })
        .collect::<Vec<_>>();
    let calls_with_usage = valid_usage_rows.len() as i64;
    let calls_missing_usage = model_calls.saturating_sub(calls_with_usage);
    let (input_tokens, output_tokens, cached_input_tokens) =
        crate::provider_usage::sum_persisted_usage_columns(valid_usage_rows.iter().map(|call| {
            (
                call.prompt_tokens,
                call.completion_tokens,
                call.cached_input_tokens,
            )
        }));
    let fresh_input_tokens =
        input_tokens.map(|input| input.saturating_sub(cached_input_tokens.unwrap_or_default()));
    let charged_tokens = if calls_missing_usage == 0 {
        Some(crate::provider_usage::sum_charged_from_persisted_parts(
            valid_usage_rows
                .iter()
                .map(|call| (call.prompt_tokens, call.completion_tokens)),
        )?)
    } else {
        None
    };
    let token_usage = SessionTokenUsage {
        model_calls,
        calls_with_usage,
        calls_missing_usage,
        input_tokens,
        cached_input_tokens,
        fresh_input_tokens,
        output_tokens,
        charged_tokens,
        incomplete: calls_missing_usage > 0,
    };

    let latest_context = latest_context_from_detail_rows(&calls.inference_calls)?;
    let mut compactions = envelope
        .compactions
        .into_iter()
        .map(|row| compaction_event("session_prefix", row))
        .chain(
            envelope
                .provider_reductions
                .into_iter()
                .map(|row| compaction_event("provider_context", row)),
        )
        .collect::<Vec<_>>();
    compactions.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    let child_session_ids = calls
        .child_requests
        .iter()
        .filter_map(|request| clean(request.session_id.as_ref()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    Ok(SessionInvestigationSnapshot {
        agent_did: agent_did.to_string(),
        session_id: session_id.to_string(),
        requests,
        tool_calls,
        token_usage,
        compactions,
        latest_context: latest_context.clone(),
        compaction_strategy: behavior
            .and_then(|behavior| clean(behavior.compaction_strategy.as_ref())),
        compaction_threshold: latest_context
            .as_ref()
            .map(|context| {
                f64::from(
                    u32::try_from(context.accounting.compaction_threshold_basis_points)
                        .unwrap_or(u32::MAX),
                ) / 10_000.0
            })
            .or_else(|| behavior.and_then(|behavior| behavior.compaction_threshold)),
        context_window: latest_context
            .as_ref()
            .and_then(|context| i64::try_from(context.accounting.context_window).ok())
            .or_else(|| profile.and_then(|profile| profile.context_window)),
        parent_request_ids,
        child_session_ids,
    })
}

fn compaction_event(scope: &str, row: CompactionRow) -> SessionCompactionEvent {
    SessionCompactionEvent {
        scope: scope.to_string(),
        created_at: clean(row.created_at.as_ref()),
        messages_compacted: row.messages_compacted,
        original_tokens: row.original_tokens,
        compacted_tokens: row.compacted_tokens,
        compacted_through_sequence: row.compacted_through_sequence,
        request_id: clean(row.request_id.as_ref()),
    }
}

fn latest_context_from_detail_rows(
    rows: &[InferenceDetailRow],
) -> Result<Option<super::context_budget::LastRequestContextSnapshot>> {
    let Some(row) = rows
        .iter()
        .filter(|row| {
            row.context_accounting_json
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
        })
        .max_by(|left, right| {
            left.queued_at
                .cmp(&right.queued_at)
                .then_with(|| left.call_seq.cmp(&right.call_seq))
                .then_with(|| left.call_id.cmp(&right.call_id))
        })
    else {
        return Ok(None);
    };
    let encoded = row.context_accounting_json.as_deref().unwrap_or_default();
    Ok(Some(super::context_budget::LastRequestContextSnapshot {
        request_id: row.request_id.clone(),
        call_id: row.call_id.clone(),
        call_sequence: row.call_seq,
        queued_at: row.queued_at.clone(),
        accounting: serde_json::from_str(encoded)
            .with_context(|| format!("decoding context accounting for call {}", row.call_id))?,
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionHistoryAction {
    List,
    Get,
}

fn validate_action(action: Option<&str>) -> Result<SessionHistoryAction> {
    match action.map(str::trim).filter(|action| !action.is_empty()) {
        None | Some("list") => Ok(SessionHistoryAction::List),
        Some("get") => Ok(SessionHistoryAction::Get),
        Some(other) => bail!("unsupported sessions action '{other}'; supported actions: list, get"),
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
            AgentSession(filter: {{ _and: [
                {{ agent_did: {{ _eq: "{agent_did}" }} }},
                {{ session_id: {{ _in: [{list}] }} }}
            ] }}) {{
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
            AgentMessage(filter: {{ _and: [
                {{ agent_did: {{ _eq: "{agent_did}" }} }},
                {{ session_id: {{ _in: [{list}] }} }}
            ] }}) {{
                session_id
                timestamp
            }}
            CompactionEntry(filter: {{ _and: [
                {{ agent_did: {{ _eq: "{agent_did}" }} }},
                {{ session_id: {{ _in: [{list}] }} }}
            ] }}) {{
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
                create_InferenceProfile(input: {
                    profile_id: "profile-a",
                    context_window: 20000
                }) { _docID }
            }"#,
            r#"mutation {
                create_AgentBehavior(input: {
                    behavior_id: "behavior-a",
                    agent_did: "did:key:z-sessions",
                    inference_profile_id: "profile-a",
                    compaction_strategy: "StripThenSummarize",
                    compaction_threshold: 0.9,
                    enabled: true
                }) { _docID }
            }"#,
            r#"mutation {
                create_AgentSession(input: {
                    session_id: "session-a",
                    agent_did: "did:key:z-sessions",
                    agent_name: "OpenAI Agent",
                    behavior_id: "behavior-a",
                    started: "2026-06-03T09:55:00Z",
                    status: "open"
                }) { _docID }
            }"#,
            r#"mutation {
                create_AgentSession(input: {
                    session_id: "session-b",
                    agent_did: "did:key:z-sessions",
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
                    agent_did: "did:key:z-sessions",
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
                    agent_did: "did:key:z-sessions",
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
                    agent_did: "did:key:z-sessions",
                    sequence: 1,
                    original_tokens: 800,
                    compacted_tokens: 400,
                    created_at: "2026-06-03T10:07:00Z"
                }) { _docID }
            }"#,
            r#"mutation {
                create_AgentToolCall(input: {
                    tool_call_key: "session-a:tool:1",
                    request_id: "request-a-new",
                    session_id: "session-a",
                    agent_did: "did:key:z-sessions",
                    tool_name: "read_file",
                    status: "completed",
                    lifecycle_state: "completed",
                    latency_ms: 25
                }) { _docID }
            }"#,
            r#"mutation {
                create_CompactionEntry(input: {
                    compaction_key: "session-a:foreign",
                    session_id: "session-a",
                    agent_did: "did:key:z-other",
                    sequence: 99,
                    original_tokens: 9999,
                    compacted_tokens: 9000,
                    created_at: "2026-06-03T10:08:00Z"
                }) { _docID }
            }"#,
        ] {
            let response = node.execute(mutation).await;
            assert!(!response.has_errors(), "seed failed: {:?}", response.errors);
        }

        let response = node
            .execute(
                r#"{
                    AgentRequest(filter: {request_id: {_eq: "request-a-new"}}) { _docID }
                }"#,
            )
            .await;
        assert!(
            !response.has_errors(),
            "request lookup failed: {:?}",
            response.errors
        );
        let request_doc_id = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("_docID"))
            .and_then(Value::as_str)
            .expect("request-a-new document ID");
        let accounting = gents_protocol::rendered_request::ContextAccounting {
            accounting_version: gents_protocol::rendered_request::CONTEXT_ACCOUNTING_VERSION,
            turn_index: 2,
            attempt: 0,
            estimator: "serialized_json_bytes_div_4_v1".to_string(),
            components: gents_protocol::rendered_request::ContextInputComponents {
                messages: 500,
                documents: 25,
                tool_schemas: 100,
                additional_parameters: 20,
                output_schema: 5,
            },
            estimated_input_tokens: 650,
            context_window: 10_000,
            compaction_threshold_basis_points: 5_700,
            compaction_threshold_tokens: 5_700,
            configured_max_output_tokens: Some(1_000),
            effective_max_output_tokens: Some(1_000),
            compaction_reason:
                gents_protocol::rendered_request::ContextCompactionReason::BelowThreshold,
            pre_compaction_input_tokens: None,
        };
        let accounting = escape_graphql_string(&serde_json::to_string(&accounting).unwrap());
        let mutation = format!(
            r#"mutation {{
            create_InferenceCall(input: {{
                call_id: "session-a:call:1"
                request_id: "request-a-new"
                request_doc_id: "{request_doc_id}"
                agent_did: "did:key:z-sessions"
                call_kind: "inference"
                call_seq: 1
                queued_at: "2026-06-03T10:05:30Z"
                prompt_tokens: 100
                completion_tokens: 20
                cached_input_tokens: 40
                context_accounting_json: "{accounting}"
            }}) {{ _docID }}
        }}"#,
            request_doc_id = escape_graphql_string(request_doc_id)
        );
        let response = node.execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "inference seed failed: {:?}",
            response.errors
        );

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
                session_id: None,
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
    async fn sessions_get_reports_timeline_tools_usage_and_compactions() {
        let node = seeded_node().await;
        let tool = SessionHistoryTool::new(node, "did:key:z-sessions");

        let output = Tool::call(
            &tool,
            SessionHistoryParams {
                action: Some("get".to_string()),
                limit: None,
                session_id: Some("session-a".to_string()),
            },
        )
        .await
        .unwrap();
        let parsed: SessionInvestigationSnapshot = serde_json::from_str(&output).unwrap();

        assert_eq!(parsed.session_id, "session-a");
        assert_eq!(parsed.requests.len(), 2);
        assert_eq!(parsed.tool_calls.total, 1);
        assert_eq!(parsed.tool_calls.by_tool.get("read_file"), Some(&1));
        assert_eq!(parsed.token_usage.model_calls, 1);
        assert_eq!(parsed.token_usage.input_tokens, Some(100));
        assert_eq!(parsed.token_usage.cached_input_tokens, Some(40));
        assert_eq!(parsed.token_usage.fresh_input_tokens, Some(60));
        assert_eq!(parsed.token_usage.output_tokens, Some(20));
        assert!(!parsed.token_usage.incomplete);
        assert_eq!(parsed.compactions.len(), 1);
        assert_eq!(parsed.compactions[0].scope, "session_prefix");
        assert_eq!(parsed.context_window, Some(10_000));
        assert_eq!(parsed.compaction_threshold, Some(0.57));
        assert_eq!(
            parsed
                .latest_context
                .as_ref()
                .map(|context| context.accounting.estimated_input_tokens),
            Some(650)
        );
    }

    #[tokio::test]
    async fn sessions_get_fails_usage_totals_closed_for_partial_rows() {
        let node = seeded_node().await;
        let response = node
            .execute(r#"{ AgentRequest(filter: {request_id: {_eq: "request-a-new"}}) { _docID } }"#)
            .await;
        let request_doc_id = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("_docID"))
            .and_then(Value::as_str)
            .unwrap();
        let mutation = format!(
            r#"mutation {{ create_InferenceCall(input: {{
                call_id: "session-a:partial"
                request_id: "request-a-new"
                request_doc_id: "{}"
                agent_did: "did:key:z-sessions"
                call_kind: "inference"
                call_seq: 2
                queued_at: "2026-06-03T10:05:31Z"
                prompt_tokens: 999
            }}) {{ _docID }} }}"#,
            escape_graphql_string(request_doc_id)
        );
        let response = node.execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "partial usage seed: {:?}",
            response.errors
        );
        let mutation = format!(
            r#"mutation {{ create_InferenceCall(input: {{
                call_id: "session-a:invalid-cache"
                request_id: "request-a-new"
                request_doc_id: "{}"
                agent_did: "did:key:z-sessions"
                call_kind: "inference"
                call_seq: 3
                queued_at: "2026-06-03T10:05:32Z"
                prompt_tokens: 100
                completion_tokens: 10
                cached_input_tokens: 101
            }}) {{ _docID }} }}"#,
            escape_graphql_string(request_doc_id)
        );
        let response = node.execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "invalid cache seed: {:?}",
            response.errors
        );

        let snapshot = load_session_investigation(&node, "did:key:z-sessions", "session-a")
            .await
            .unwrap();
        assert_eq!(snapshot.token_usage.model_calls, 3);
        assert_eq!(snapshot.token_usage.calls_with_usage, 1);
        assert_eq!(snapshot.token_usage.input_tokens, Some(100));
        assert_eq!(snapshot.token_usage.output_tokens, Some(20));
        assert_eq!(snapshot.token_usage.charged_tokens, None);
        assert!(snapshot.token_usage.incomplete);
    }

    #[tokio::test]
    async fn sessions_get_does_not_join_duplicate_logical_request_ids() {
        let node = seeded_node().await;
        let response = node
            .execute(
                r#"mutation {
                    create_AgentRequest(input: {
                        request_id: "request-a-new"
                        agent_did: "did:key:z-sessions"
                        behavior_id: "behavior-b"
                        session_id: "session-b"
                        status: "completed"
                        created_at: "2026-06-03T11:05:00Z"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(
            !response.has_errors(),
            "duplicate-label request: {:?}",
            response.errors
        );
        let response = node
            .execute(
                r#"{
                    AgentRequest(filter: {
                        request_id: {_eq: "request-a-new"},
                        session_id: {_eq: "session-b"}
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(
            !response.has_errors(),
            "duplicate-label request lookup: {:?}",
            response.errors
        );
        let outsider_doc_id = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("_docID"))
            .and_then(Value::as_str)
            .expect("outsider request document ID");
        let mutation = format!(
            r#"mutation {{ create_InferenceCall(input: {{
                call_id: "session-b:duplicate-label-call"
                request_id: "request-a-new"
                request_doc_id: "{}"
                agent_did: "did:key:z-sessions"
                call_kind: "inference"
                call_seq: 1
                queued_at: "2026-06-03T11:05:01Z"
                prompt_tokens: 900
                completion_tokens: 90
            }}) {{ _docID }} }}"#,
            escape_graphql_string(outsider_doc_id)
        );
        let response = node.execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "outsider inference: {:?}",
            response.errors
        );

        let snapshot = load_session_investigation(&node, "did:key:z-sessions", "session-a")
            .await
            .unwrap();
        assert_eq!(snapshot.token_usage.model_calls, 1);
        assert_eq!(snapshot.token_usage.input_tokens, Some(100));
        assert_eq!(snapshot.token_usage.charged_tokens, Some(120));
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
                session_id: None,
            },
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("unsupported sessions action"));
    }
}
