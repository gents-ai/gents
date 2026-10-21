use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::llm::tool::ToolDefinition;
use crate::llm::tool::{Tool, ToolDyn};
use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::row::AgentRequestRow;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};

pub const CONTEXT_BUDGET_TOOL_NAME: &str = "context_budget";

const RECENT_REQUEST_SCAN: usize = 200;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContextBudgetParams {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextBudgetSnapshot {
    pub max_tokens: Option<i64>,
    pub current_estimate: Option<i64>,
    pub utilization_percent: Option<f64>,
    pub compaction_count: i64,
    pub last_compacted_at: Option<String>,
    pub sessions_considered: i64,
    pub request_scan_limit: i64,
    pub last_request: Option<LastRequestContextSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LastRequestContextSnapshot {
    pub request_id: String,
    pub call_id: String,
    pub call_sequence: i64,
    pub queued_at: Option<String>,
    pub accounting: gents_protocol::rendered_request::ContextAccounting,
}

/// Identity the snapshot is scoped to. Every persisted row the tool reads must
/// belong to this node, this agent and this session; a node-wide scan would
/// attribute other agents' (and other sessions') provider dispatches to the
/// caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBudgetScope {
    pub node_did: String,
    pub agent_id: String,
    pub session_id: String,
}

impl ContextBudgetScope {
    /// Resolve the scope from the in-flight tool call. Fails closed when the
    /// runtime did not install an agent or session identity.
    fn from_tool_runtime(node_did: &str) -> Result<Self> {
        let runtime = crate::tool_call_lifecycle::runtime::current_tool_runtime_context();
        let agent_id = runtime
            .as_ref()
            .and_then(|runtime| runtime.agent_id.clone())
            .filter(|value| !value.trim().is_empty())
            .context("context_budget tool requires the calling agent identity")?;
        let session_id = runtime
            .as_ref()
            .and_then(|runtime| runtime.session_id.clone())
            .filter(|value| !value.trim().is_empty())
            .context("context_budget tool requires the calling session identity")?;
        let node_did = runtime
            .as_ref()
            .and_then(|runtime| runtime.node_did.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| node_did.to_string());
        Ok(Self {
            node_did,
            agent_id,
            session_id,
        })
    }
}

#[derive(Debug, Deserialize)]
struct ContextEnvelope {
    #[serde(rename = "Agent", default)]
    agents: Vec<AgentRow>,
    #[serde(rename = "InferenceProfile", default)]
    profiles: Vec<ProfileRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<AgentRequestRow>,
}

#[derive(Debug, Deserialize)]
struct InferenceCallEnvelope {
    #[serde(rename = "InferenceCall", default)]
    inference_calls: Vec<InferenceCallRow>,
}

#[derive(Debug, Deserialize)]
struct CompactionEnvelope {
    #[serde(rename = "CompactionEntry", default)]
    compactions: Vec<CompactionRow>,
    #[serde(rename = "ProviderContextReduction", default)]
    provider_context_reductions: Vec<CompactionRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct AgentRow {
    #[serde(default)]
    inference_profile_id: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProfileRow {
    #[serde(default)]
    profile_id: String,
    #[serde(default)]
    context_window: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct CompactionRow {
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    original_tokens: Option<i64>,
    #[serde(default)]
    compacted_tokens: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct InferenceCallRow {
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    call_id: String,
    #[serde(default)]
    call_seq: i64,
    #[serde(default)]
    queued_at: Option<String>,
    #[serde(default)]
    context_accounting_json: Option<String>,
}

#[derive(Debug)]
pub struct ContextBudgetToolError(anyhow::Error);

impl std::fmt::Display for ContextBudgetToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for ContextBudgetToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.root_cause())
    }
}

impl From<anyhow::Error> for ContextBudgetToolError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

#[derive(Clone)]
pub struct ContextBudgetTool {
    node: Arc<EmbeddedNode>,
    node_did: String,
}

impl ContextBudgetTool {
    pub fn new(node: Arc<EmbeddedNode>, node_did: impl Into<String>) -> Self {
        Self {
            node,
            node_did: node_did.into(),
        }
    }
}

impl Tool for ContextBudgetTool {
    const NAME: &'static str = CONTEXT_BUDGET_TOOL_NAME;

    type Error = ContextBudgetToolError;
    type Args = ContextBudgetParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Report this agent's exact latest persisted provider-dispatch context \
                accounting: component token estimates, context window, compaction threshold, \
                decision/reason, utilization, and compaction history. Counts both session-prefix \
                and per-turn provider-context reductions."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {}
            }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        let scope = ContextBudgetScope::from_tool_runtime(&self.node_did)?;
        let snapshot = load_context_budget_snapshot(&self.node, &scope).await?;
        crate::tool_output::render(
            &snapshot,
            &[
                "current_estimate",
                "max_tokens",
                "utilization_percent",
                "last_request",
                "compaction_count",
                "last_compacted_at",
            ],
        )
        .map_err(|error| {
            ContextBudgetToolError(anyhow!(
                "failed to serialize context budget output: {error}"
            ))
        })
    }
}

pub fn build_context_budget_tool(
    node: Arc<EmbeddedNode>,
    node_did: impl Into<String>,
) -> Box<dyn ToolDyn> {
    Box::new(ContextBudgetTool::new(node, node_did))
}

pub async fn load_context_budget_snapshot(
    node: &EmbeddedNode,
    scope: &ContextBudgetScope,
) -> Result<ContextBudgetSnapshot> {
    let node_did = scope.node_did.trim();
    if node_did.is_empty() {
        bail!("context_budget tool requires a running node DID");
    }
    let agent_id = scope.agent_id.trim();
    let session_id = scope.session_id.trim();
    if agent_id.is_empty() || session_id.is_empty() {
        bail!("context_budget tool requires the calling agent and session identity");
    }

    let resp = graphql_with_transaction_retry(
        node,
        &context_query(node_did, agent_id, session_id),
        "loading context budget",
    )
    .await?;
    let envelope: ContextEnvelope = decode(resp.data.as_ref(), "context budget")?;
    let request_ids = distinct_request_ids(&envelope.requests);
    let last_request = if request_ids.is_empty() {
        None
    } else {
        let resp = graphql_with_transaction_retry(
            node,
            &inference_call_query(node_did, agent_id, &request_ids),
            "loading context budget inference calls",
        )
        .await?;
        let calls: InferenceCallEnvelope =
            decode(resp.data.as_ref(), "context budget inference calls")?;
        latest_request_context(&calls.inference_calls)?
    };
    let max_tokens = last_request
        .as_ref()
        .map(|request| request.accounting.context_window as i64)
        .or_else(|| max_context_window(&envelope.agents, &envelope.profiles));
    let session_ids = distinct_session_ids(&envelope.requests);

    let compactions = if session_ids.is_empty() {
        Vec::new()
    } else {
        let resp = graphql_with_transaction_retry(
            node,
            &compaction_query(node_did, &session_ids),
            "loading context compactions",
        )
        .await?;
        let envelope: CompactionEnvelope = decode(resp.data.as_ref(), "context compactions")?;
        envelope
            .compactions
            .into_iter()
            .chain(envelope.provider_context_reductions)
            .collect()
    };

    Ok(build_snapshot(
        max_tokens,
        session_ids.len() as i64,
        compactions,
        last_request,
    ))
}

fn context_query(node_did: &str, agent_id: &str, session_id: &str) -> String {
    let node_did = escape_graphql_string(node_did);
    let agent_id = escape_graphql_string(agent_id);
    let session_id = escape_graphql_string(session_id);
    format!(
        r#"{{
            Agent(filter: {{ _and: [
                {{ node_did: {{ _eq: "{node_did}" }} }},
                {{ agent_id: {{ _eq: "{agent_id}" }} }}
            ] }}, order: {{ agent_id: ASC }}) {{
                inference_profile_id
                enabled
            }}
            InferenceProfile(filter: {{ node_did: {{ _eq: "{node_did}" }} }}, order: {{ profile_id: ASC }}) {{
                profile_id
                context_window
            }}
            AgentRequest(filter: {{ _and: [
                {{ node_did: {{ _eq: "{node_did}" }} }},
                {{ session_id: {{ _eq: "{session_id}" }} }}
            ] }}, order: {{ created_at: DESC }}, limit: {RECENT_REQUEST_SCAN}) {{
                request_id
                session_id
            }}
        }}"#
    )
}

fn inference_call_query(node_did: &str, agent_id: &str, request_ids: &[String]) -> String {
    let node_did = escape_graphql_string(node_did);
    let agent_id = escape_graphql_string(agent_id);
    let list = request_ids
        .iter()
        .map(|id| format!(r#""{}""#, escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"{{
            InferenceCall(
                filter: {{ _and: [
                    {{ node_did: {{ _eq: "{node_did}" }} }},
                    {{ agent_id: {{ _eq: "{agent_id}" }} }},
                    {{ call_kind: {{ _eq: "inference" }} }},
                    {{ request_id: {{ _in: [{list}] }} }}
                ] }},
                order: {{ queued_at: DESC }},
                limit: {RECENT_REQUEST_SCAN}
            ) {{
                request_id
                call_id
                call_seq
                queued_at
                context_accounting_json
            }}
        }}"#
    )
}

fn compaction_query(node_did: &str, session_ids: &[String]) -> String {
    let node_did = escape_graphql_string(node_did);
    let list = session_ids
        .iter()
        .map(|id| format!(r#""{}""#, escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"{{
            CompactionEntry(filter: {{ _and: [
                {{ node_did: {{ _eq: "{node_did}" }} }},
                {{ session_id: {{ _in: [{list}] }} }}
            ] }}, order: {{ created_at: DESC }}) {{
                created_at
                original_tokens
                compacted_tokens
            }}
            ProviderContextReduction(filter: {{ _and: [
                {{ node_did: {{ _eq: "{node_did}" }} }},
                {{ session_id: {{ _in: [{list}] }} }}
            ] }}, order: {{ created_at: DESC }}) {{
                created_at
                original_tokens
                compacted_tokens
            }}
        }}"#
    )
}

fn decode<T: serde::de::DeserializeOwned>(data: Option<&Value>, label: &str) -> Result<T> {
    let data = data
        .filter(|data| data.is_object())
        .cloned()
        .with_context(|| format!("{label} query response missing object data"))?;
    serde_json::from_value(data).with_context(|| format!("decoding {label} query response"))
}

fn max_context_window(agents: &[AgentRow], profiles: &[ProfileRow]) -> Option<i64> {
    let profiles = profiles
        .iter()
        .filter_map(|profile| {
            let profile_id = profile.profile_id.trim();
            let context_window = profile.context_window.filter(|value| *value > 0)?;
            (!profile_id.is_empty()).then_some((profile_id, context_window))
        })
        .collect::<BTreeMap<_, _>>();

    agents
        .iter()
        .filter(|agent| agent.enabled.unwrap_or(true))
        .filter_map(|agent| agent.inference_profile_id.as_deref())
        .filter_map(|profile_id| profiles.get(profile_id.trim()).copied())
        .max()
}

fn distinct_request_ids(requests: &[AgentRequestRow]) -> Vec<String> {
    requests
        .iter()
        .filter_map(|request| {
            let request_id = request.request_id.trim();
            (!request_id.is_empty()).then(|| request_id.to_string())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn distinct_session_ids(requests: &[AgentRequestRow]) -> Vec<String> {
    requests
        .iter()
        .filter_map(|request| {
            let session_id = request.session_id.as_deref().unwrap_or_default().trim();
            (!session_id.is_empty()).then(|| session_id.to_string())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn build_snapshot(
    max_tokens: Option<i64>,
    sessions_considered: i64,
    compactions: Vec<CompactionRow>,
    last_request: Option<LastRequestContextSnapshot>,
) -> ContextBudgetSnapshot {
    let compaction_count = compactions.len() as i64;
    let latest = compactions
        .iter()
        .max_by(|a, b| a.created_at.cmp(&b.created_at));
    let current_estimate = last_request
        .as_ref()
        .map(|request| request.accounting.estimated_input_tokens as i64)
        .or_else(|| latest.and_then(|entry| entry.compacted_tokens))
        .or_else(|| latest.and_then(|entry| entry.original_tokens));
    let utilization_percent = match (current_estimate, max_tokens) {
        (Some(current), Some(max)) if max > 0 => Some((current as f64 / max as f64) * 100.0),
        _ => None,
    };

    ContextBudgetSnapshot {
        max_tokens,
        current_estimate,
        utilization_percent,
        compaction_count,
        last_compacted_at: latest.and_then(|entry| entry.created_at.clone()),
        sessions_considered,
        request_scan_limit: RECENT_REQUEST_SCAN as i64,
        last_request,
    }
}

fn latest_request_context(rows: &[InferenceCallRow]) -> Result<Option<LastRequestContextSnapshot>> {
    let mut candidates = rows
        .iter()
        .filter_map(|row| {
            row.context_accounting_json
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(|encoded| (row, encoded))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left, _), (right, _)| {
        left.queued_at
            .cmp(&right.queued_at)
            .then_with(|| left.call_seq.cmp(&right.call_seq))
            .then_with(|| left.call_id.cmp(&right.call_id))
    });
    let Some((row, encoded)) = candidates.pop() else {
        return Ok(None);
    };
    let accounting = serde_json::from_str(encoded).with_context(|| {
        format!(
            "decoding context accounting for InferenceCall {}",
            row.call_id
        )
    })?;
    Ok(Some(LastRequestContextSnapshot {
        request_id: row.request_id.clone(),
        call_id: row.call_id.clone(),
        call_sequence: row.call_seq,
        queued_at: row.queued_at.clone(),
        accounting,
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::llm::tool::Tool;

    use super::*;

    async fn seeded_node() -> Arc<EmbeddedNode> {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

        for mutation in [
            r#"mutation {
                create_InferenceProfile(input: {
                    profile_id: "profile-context",
                    node_did: "did:key:z-context",
                    context_window: 1000
                }) { _docID }
            }"#,
            r#"mutation {
                create_Agent(input: {
                    agent_id: "agent-context",
                    node_did: "did:key:z-context",
                    inference_profile_id: "profile-context",
                    enabled: true
                }) { _docID }
            }"#,
            r#"mutation {
                create_AgentRequest(input: {
                    request_id: "request-context",
                    purpose: "normal",
                    node_did: "did:key:z-context",
                    session_id: "session-context",
                    lifecycle_state: "completed",
                    created_at: "2026-06-03T10:00:00Z"
                }) { _docID }
            }"#,
            r#"mutation {
                create_CompactionEntry(input: {
                    compaction_key: "session-context:1",
                    session_id: "session-context",
                    node_did: "did:key:z-context",
                    sequence: 1,
                    original_tokens: 800,
                    compacted_tokens: 400,
                    created_at: "2026-06-03T10:30:00Z"
                }) { _docID }
            }"#,
            r#"mutation {
                create_ProviderContextReduction(input: {
                    reduction_key: "context-reduction:1",
                    node_did: "did:key:z-context",
                    requester_did: null,
                    session_id: "session-context",
                    request_id: "request-context",
                    request_doc_id: "request-doc-context",
                    request_commit_cid: "request-cid",
                    reduction_index: 1,
                    turn_index: 0,
                    parent_reduction_key: null,
                    producer_call_id: null,
                    producer_call_seq: null,
                    source_boundary_json: "null",
                    compacted_prefix_json: "null",
                    retained_suffix_json: "null",
                    pair_closed: true,
                    checkpoint_messages_json: "null",
                    replay_associations_json: "{\"required\":[],\"prefix_sources\":[],\"retained_sources\":[]}",
                    summary: "provider checkpoint",
                    messages_compacted: 1,
                    original_tokens: 700,
                    compacted_tokens: 300,
                    created_at: "2026-06-03T10:45:00Z"
                }) { _docID }
            }"#,
        ] {
            let response = node.execute(mutation).await;
            assert!(!response.has_errors(), "seed failed: {:?}", response.errors);
        }

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
            context_window: 1_000,
            compaction_threshold_basis_points: 8_000,
            compaction_threshold_tokens: 800,
            configured_max_output_tokens: Some(100),
            effective_max_output_tokens: Some(100),
            compaction_reason:
                gents_protocol::rendered_request::ContextCompactionReason::BelowThreshold,
            pre_compaction_input_tokens: None,
        };
        let accounting = escape_graphql_string(&serde_json::to_string(&accounting).unwrap());
        let response = node
            .execute(&format!(
                r#"mutation {{
                    create_InferenceCall(input: {{
                        call_id: "context-call",
                        request_id: "request-context",
                        request_doc_id: "request-context-doc",
                        node_did: "did:key:z-context",
                        agent_id: "agent-context",
                        call_kind: "inference",
                        call_seq: 1,
                        queued_at: "2026-06-03T11:00:00Z",
                        context_accounting_json: "{accounting}"
                    }}) {{ _docID }}
                }}"#
            ))
            .await;
        assert!(!response.has_errors(), "seed failed: {:?}", response.errors);

        node
    }

    async fn call_as(tool: &ContextBudgetTool, agent_id: &str, session_id: &str) -> String {
        crate::tool_call_lifecycle::runtime::scope_tool_request_identity(
            None,
            Some("did:key:z-context".into()),
            Some(agent_id.into()),
            Some("caller-request".into()),
            crate::tool_call_lifecycle::runtime::scope_request_tool_execution_with_session(
                None,
                tokio_util::sync::CancellationToken::new(),
                None,
                None,
                Some(session_id.into()),
                async { Tool::call(tool, ContextBudgetParams {}).await.unwrap() },
            ),
        )
        .await
    }

    /// Seed a newer, larger provider dispatch for a different agent on the
    /// same node (the reported Codex background-completion shape), plus a
    /// newer dispatch by the same agent in a different session.
    async fn seed_foreign_dispatches(node: &EmbeddedNode) {
        for (agent_id, session_id, request_id, call_id, queued_at) in [
            (
                "agent-codex",
                "session-codex",
                "request-codex",
                "codex-call",
                "2026-06-03T12:00:00Z",
            ),
            (
                "agent-context",
                "session-other",
                "request-other",
                "other-call",
                "2026-06-03T12:30:00Z",
            ),
        ] {
            let response = node
                .execute(&format!(
                    r#"mutation {{
                        create_AgentRequest(input: {{
                            request_id: "{request_id}",
                            purpose: "normal",
                            node_did: "did:key:z-context",
                            session_id: "{session_id}",
                            lifecycle_state: "completed",
                            created_at: "{queued_at}"
                        }}) {{ _docID }}
                    }}"#
                ))
                .await;
            assert!(!response.has_errors(), "seed failed: {:?}", response.errors);
            let response = node
                .execute(&format!(
                    r#"mutation {{
                        create_CompactionEntry(input: {{
                            compaction_key: "{session_id}:1",
                            session_id: "{session_id}",
                            node_did: "did:key:z-context",
                            sequence: 1,
                            original_tokens: 9000,
                            compacted_tokens: 9000,
                            created_at: "{queued_at}"
                        }}) {{ _docID }}
                    }}"#
                ))
                .await;
            assert!(!response.has_errors(), "seed failed: {:?}", response.errors);
            let accounting = gents_protocol::rendered_request::ContextAccounting {
                accounting_version: gents_protocol::rendered_request::CONTEXT_ACCOUNTING_VERSION,
                turn_index: 0,
                attempt: 0,
                estimator: "foreign".to_string(),
                components: gents_protocol::rendered_request::ContextInputComponents {
                    messages: 59_000,
                    documents: 0,
                    tool_schemas: 0,
                    additional_parameters: 0,
                    output_schema: 0,
                },
                estimated_input_tokens: 59_000,
                context_window: 272_000,
                compaction_threshold_basis_points: 7_500,
                compaction_threshold_tokens: 204_000,
                configured_max_output_tokens: None,
                effective_max_output_tokens: None,
                compaction_reason:
                    gents_protocol::rendered_request::ContextCompactionReason::BelowThreshold,
                pre_compaction_input_tokens: None,
            };
            let accounting = escape_graphql_string(&serde_json::to_string(&accounting).unwrap());
            let response = node
                .execute(&format!(
                    r#"mutation {{
                        create_InferenceCall(input: {{
                            call_id: "{call_id}",
                            request_id: "{request_id}",
                            request_doc_id: "{request_id}-doc",
                            node_did: "did:key:z-context",
                            agent_id: "{agent_id}",
                            call_kind: "inference",
                            call_seq: 1,
                            queued_at: "{queued_at}",
                            context_accounting_json: "{accounting}"
                        }}) {{ _docID }}
                    }}"#
                ))
                .await;
            assert!(!response.has_errors(), "seed failed: {:?}", response.errors);
        }
    }

    #[tokio::test]
    async fn context_budget_tool_ignores_other_agents_and_sessions_on_the_node() {
        let node = seeded_node().await;
        seed_foreign_dispatches(node.as_ref()).await;
        let tool = ContextBudgetTool::new(node, "did:key:z-context");

        let output = call_as(&tool, "agent-context", "session-context").await;
        let parsed: ContextBudgetSnapshot = serde_json::from_str(&output).unwrap();

        let last = parsed.last_request.expect("own request accounting");
        assert_eq!(last.call_id, "context-call");
        assert_eq!(last.request_id, "request-context");
        assert_eq!(parsed.max_tokens, Some(1000));
        assert_eq!(parsed.current_estimate, Some(650));
        assert_eq!(parsed.sessions_considered, 1);
        assert_eq!(parsed.compaction_count, 2);

        // The other agent sees only its own dispatch, never the caller's.
        let output = call_as(&tool, "agent-codex", "session-codex").await;
        let parsed: ContextBudgetSnapshot = serde_json::from_str(&output).unwrap();
        assert_eq!(
            parsed.last_request.expect("codex accounting").call_id,
            "codex-call"
        );
        assert_eq!(parsed.compaction_count, 1);

        // A different agent inside the caller's session gets no foreign accounting.
        let output = call_as(&tool, "agent-codex", "session-context").await;
        let parsed: ContextBudgetSnapshot = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed.last_request, None);
    }

    #[tokio::test]
    async fn fallback_context_window_does_not_use_another_nodes_profile() {
        let node = seeded_node().await;
        let tool = ContextBudgetTool::new(Arc::clone(&node), "did:key:z-context");
        let output = call_as(&tool, "agent-context", "session-without-accounting").await;
        let own: ContextBudgetSnapshot = serde_json::from_str(&output).unwrap();
        assert_eq!(own.max_tokens, Some(1000));

        for mutation in [
            r#"mutation {
                create_InferenceProfile(input: {
                    profile_id: "foreign-profile",
                    node_did: "did:key:z-foreign",
                    context_window: 9000
                }) { _docID }
            }"#,
            r#"mutation {
                update_Agent(filter: { agent_id: { _eq: "agent-context" } },
                    input: { inference_profile_id: "foreign-profile" }) { _docID }
            }"#,
        ] {
            let response = node.execute(mutation).await;
            assert!(!response.has_errors(), "seed failed: {:?}", response.errors);
        }
        let output = call_as(&tool, "agent-context", "session-without-accounting").await;
        let scoped: ContextBudgetSnapshot = serde_json::from_str(&output).unwrap();
        assert_eq!(scoped.max_tokens, None);
    }

    #[tokio::test]
    async fn context_budget_tool_fails_closed_without_caller_identity() {
        let node = seeded_node().await;
        let tool = ContextBudgetTool::new(node, "did:key:z-context");

        let error = Tool::call(&tool, ContextBudgetParams {})
            .await
            .expect_err("unscoped call must not fall back to a node-wide scan");
        assert!(
            error.to_string().contains("calling agent identity"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn context_budget_tool_reports_persisted_context_signal() {
        let node = seeded_node().await;
        let tool = ContextBudgetTool::new(node, "did:key:z-context");

        let output = call_as(&tool, "agent-context", "session-context").await;
        let parsed: ContextBudgetSnapshot = serde_json::from_str(&output).unwrap();

        assert_eq!(parsed.max_tokens, Some(1000));
        assert_eq!(parsed.current_estimate, Some(650));
        assert_eq!(parsed.utilization_percent, Some(65.0));
        assert_eq!(parsed.compaction_count, 2);
        assert_eq!(
            parsed.last_compacted_at.as_deref(),
            Some("2026-06-03T10:45:00Z")
        );
        assert_eq!(parsed.sessions_considered, 1);
        let last = parsed.last_request.expect("exact request accounting");
        assert_eq!(last.request_id, "request-context");
        assert_eq!(last.accounting.components.tool_schemas, 100);
        assert_eq!(last.accounting.compaction_threshold_tokens, 800);
    }
}
