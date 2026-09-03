use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::llm::tool::ToolDefinition;
use crate::llm::tool::{Tool, ToolDyn};
use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::graphql::escape_graphql_string;

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

#[derive(Debug, Deserialize)]
struct ContextEnvelope {
    #[serde(rename = "AgentBehavior", default)]
    behaviors: Vec<BehaviorRow>,
    #[serde(rename = "InferenceProfile", default)]
    profiles: Vec<ProfileRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<RequestRow>,
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
struct BehaviorRow {
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
struct RequestRow {
    #[serde(default)]
    session_id: Option<String>,
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
    agent_did: String,
}

impl ContextBudgetTool {
    pub fn new(node: Arc<EmbeddedNode>, agent_did: impl Into<String>) -> Self {
        Self {
            node,
            agent_did: agent_did.into(),
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
        let snapshot = load_context_budget_snapshot(&self.node, &self.agent_did).await?;
        serde_json::to_string_pretty(&snapshot).map_err(|error| {
            ContextBudgetToolError(anyhow!(
                "failed to serialize context budget output: {error}"
            ))
        })
    }
}

pub fn build_context_budget_tool(
    node: Arc<EmbeddedNode>,
    agent_did: impl Into<String>,
) -> Box<dyn ToolDyn> {
    Box::new(ContextBudgetTool::new(node, agent_did))
}

pub async fn load_context_budget_snapshot(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<ContextBudgetSnapshot> {
    let agent_did = agent_did.trim();
    if agent_did.is_empty() {
        bail!("context_budget tool requires a running agent DID");
    }

    let resp = node.execute(&context_query(agent_did)).await;
    if resp.has_errors() {
        bail!("loading context budget failed: {:?}", resp.errors);
    }
    let envelope: ContextEnvelope = decode(resp.data.as_ref(), "context budget")?;
    let last_request = latest_request_context(&envelope.inference_calls)?;
    let max_tokens = last_request
        .as_ref()
        .map(|request| request.accounting.context_window as i64)
        .or_else(|| max_context_window(&envelope.behaviors, &envelope.profiles));
    let session_ids = distinct_session_ids(&envelope.requests);

    let compactions = if session_ids.is_empty() {
        Vec::new()
    } else {
        let resp = node
            .execute(&compaction_query(agent_did, &session_ids))
            .await;
        if resp.has_errors() {
            bail!("loading context compactions failed: {:?}", resp.errors);
        }
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

fn context_query(agent_did: &str) -> String {
    let agent_did = escape_graphql_string(agent_did);
    format!(
        r#"{{
            AgentBehavior(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}, order: {{ behavior_id: ASC }}) {{
                inference_profile_id
                enabled
            }}
            InferenceProfile(order: {{ profile_id: ASC }}) {{
                profile_id
                context_window
            }}
            AgentRequest(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}, order: {{ created_at: DESC }}, limit: {RECENT_REQUEST_SCAN}) {{
                session_id
            }}
            InferenceCall(
                filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
                    call_kind: {{ _eq: "inference" }}
                }},
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

fn compaction_query(agent_did: &str, session_ids: &[String]) -> String {
    let agent_did = escape_graphql_string(agent_did);
    let list = session_ids
        .iter()
        .map(|id| format!(r#""{}""#, escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"{{
            CompactionEntry(filter: {{ _and: [
                {{ agent_did: {{ _eq: "{agent_did}" }} }},
                {{ session_id: {{ _in: [{list}] }} }}
            ] }}, order: {{ created_at: DESC }}) {{
                created_at
                original_tokens
                compacted_tokens
            }}
            ProviderContextReduction(filter: {{ _and: [
                {{ agent_did: {{ _eq: "{agent_did}" }} }},
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

fn max_context_window(behaviors: &[BehaviorRow], profiles: &[ProfileRow]) -> Option<i64> {
    let profiles = profiles
        .iter()
        .filter_map(|profile| {
            let profile_id = profile.profile_id.trim();
            let context_window = profile.context_window.filter(|value| *value > 0)?;
            (!profile_id.is_empty()).then_some((profile_id, context_window))
        })
        .collect::<BTreeMap<_, _>>();

    behaviors
        .iter()
        .filter(|behavior| behavior.enabled.unwrap_or(true))
        .filter_map(|behavior| behavior.inference_profile_id.as_deref())
        .filter_map(|profile_id| profiles.get(profile_id.trim()).copied())
        .max()
}

fn distinct_session_ids(requests: &[RequestRow]) -> Vec<String> {
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
                    context_window: 1000
                }) { _docID }
            }"#,
            r#"mutation {
                create_AgentBehavior(input: {
                    behavior_id: "behavior-context",
                    agent_did: "did:key:z-context",
                    inference_profile_id: "profile-context",
                    enabled: true
                }) { _docID }
            }"#,
            r#"mutation {
                create_AgentRequest(input: {
                    request_id: "request-context",
                    agent_did: "did:key:z-context",
                    session_id: "session-context",
                    lifecycle_state: "completed",
                    created_at: "2026-06-03T10:00:00Z"
                }) { _docID }
            }"#,
            r#"mutation {
                create_CompactionEntry(input: {
                    compaction_key: "session-context:1",
                    session_id: "session-context",
                    agent_did: "did:key:z-context",
                    sequence: 1,
                    original_tokens: 800,
                    compacted_tokens: 400,
                    created_at: "2026-06-03T10:30:00Z"
                }) { _docID }
            }"#,
            r#"mutation {
                create_ProviderContextReduction(input: {
                    reduction_key: "context-reduction:1",
                    agent_did: "did:key:z-context",
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
                        agent_did: "did:key:z-context",
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

    #[tokio::test]
    async fn context_budget_tool_reports_persisted_context_signal() {
        let node = seeded_node().await;
        let tool = ContextBudgetTool::new(node, "did:key:z-context");

        let output = Tool::call(&tool, ContextBudgetParams {}).await.unwrap();
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
