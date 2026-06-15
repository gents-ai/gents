use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use codex_app_server_protocol as codex;
use defra_agent::graphql::escape_graphql_string;
use serde::Deserialize;
use serde_json::Value;

use crate::commands::codex_shim::store::query_node_json;
use crate::commands::codex_shim::ShimState;

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub(in crate::commands::codex_shim) struct TokenTotals {
    pub(in crate::commands::codex_shim) input_tokens: i64,
    pub(in crate::commands::codex_shim) output_tokens: i64,
}

impl TokenTotals {
    pub(in crate::commands::codex_shim) fn total(self) -> i64 {
        self.input_tokens + self.output_tokens
    }
}

#[derive(Debug, Default)]
struct RequestUsageAccumulator {
    input_tokens: i64,
    output_tokens: i64,
    has_real_output: bool,
    proxy_output_tokens: i64,
}

#[derive(Debug, Deserialize)]
struct AgentRequestUsageRow {
    request_id: String,
}

#[derive(Debug, Deserialize)]
struct AgentResponseUsageRow {
    request_id: String,
    #[serde(default)]
    token_count: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct InferenceCallUsageRow {
    request_id: String,
    #[serde(default)]
    prompt_tokens: Option<i64>,
    #[serde(default)]
    completion_tokens: Option<i64>,
}

pub(in crate::commands::codex_shim) async fn requests_token_usage(
    state: &ShimState,
    request_ids: &[String],
) -> Result<TokenTotals> {
    let usage = gather_request_usage(state, request_ids).await?;
    Ok(usage
        .into_values()
        .fold(TokenTotals::default(), |mut totals, usage| {
            totals.input_tokens += usage.input_tokens;
            totals.output_tokens += if usage.has_real_output {
                usage.output_tokens
            } else {
                usage.proxy_output_tokens
            };
            totals
        }))
}

pub(in crate::commands::codex_shim) async fn session_token_usage(
    state: &ShimState,
    session_id: &str,
) -> Result<TokenTotals> {
    let request_ids = session_request_ids(state, session_id).await?;
    requests_token_usage(state, &request_ids).await
}

pub(in crate::commands::codex_shim) fn thread_token_usage(
    total: TokenTotals,
    last: TokenTotals,
) -> codex::ThreadTokenUsage {
    codex::ThreadTokenUsage {
        total: token_breakdown(total),
        last: token_breakdown(last),
        model_context_window: None,
    }
}

async fn session_request_ids(state: &ShimState, session_id: &str) -> Result<Vec<String>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(&state.agent_did);
    let escaped_behavior_id = escape_graphql_string(&state.behavior_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    behavior_id: {{ _eq: "{escaped_behavior_id}" }}
                }}
            ) {{
                request_id
            }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    let rows = rows::<AgentRequestUsageRow>(&response, "AgentRequest")
        .context("decoding AgentRequest rows for token usage")?;
    Ok(rows
        .into_iter()
        .map(|row| row.request_id)
        .filter(|request_id| !request_id.trim().is_empty())
        .collect())
}

async fn gather_request_usage(
    state: &ShimState,
    request_ids: &[String],
) -> Result<BTreeMap<String, RequestUsageAccumulator>> {
    let request_ids = request_ids
        .iter()
        .filter(|request_id| !request_id.trim().is_empty())
        .collect::<BTreeSet<_>>();
    if request_ids.is_empty() {
        return Ok(BTreeMap::new());
    }

    let id_list = request_ids
        .iter()
        .map(|request_id| format!(r#""{}""#, escape_graphql_string(request_id)))
        .collect::<Vec<_>>()
        .join(", ");
    let escaped_agent_did = escape_graphql_string(&state.agent_did);
    let escaped_behavior_id = escape_graphql_string(&state.behavior_id);
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{
                    request_id: {{ _in: [{id_list}] }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    behavior_id: {{ _eq: "{escaped_behavior_id}" }}
                }}
            ) {{
                request_id
                token_count
            }}
            InferenceCall(
                filter: {{
                    request_id: {{ _in: [{id_list}] }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    behavior_id: {{ _eq: "{escaped_behavior_id}" }},
                    call_kind: {{ _eq: "inference" }},
                    call_state: {{ _in: ["completed", "failed", "cancelled"] }}
                }}
            ) {{
                request_id
                prompt_tokens
                completion_tokens
            }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    let mut usage = request_ids
        .into_iter()
        .map(|request_id| (request_id.to_string(), RequestUsageAccumulator::default()))
        .collect::<BTreeMap<_, _>>();

    for row in rows::<AgentResponseUsageRow>(&response, "AgentResponse")
        .context("decoding AgentResponse rows for token usage")?
    {
        if let Some(tokens) = row.token_count.and_then(nonnegative_i64) {
            let usage = usage.entry(row.request_id).or_default();
            usage.proxy_output_tokens = usage.proxy_output_tokens.max(tokens);
        }
    }

    for row in rows::<InferenceCallUsageRow>(&response, "InferenceCall")
        .context("decoding InferenceCall rows for token usage")?
    {
        let usage = usage.entry(row.request_id).or_default();
        if let Some(tokens) = row.prompt_tokens.and_then(nonnegative_i64) {
            usage.input_tokens += tokens;
        }
        if let Some(tokens) = row.completion_tokens.and_then(nonnegative_i64) {
            usage.output_tokens += tokens;
            usage.has_real_output = true;
        }
    }

    Ok(usage)
}

fn token_breakdown(totals: TokenTotals) -> codex::TokenUsageBreakdown {
    codex::TokenUsageBreakdown {
        total_tokens: totals.total(),
        input_tokens: totals.input_tokens,
        cached_input_tokens: 0,
        output_tokens: totals.output_tokens,
        reasoning_output_tokens: 0,
    }
}

fn rows<T>(response: &Value, name: &str) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    response
        .pointer(&format!("/data/{name}"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(serde_json::from_value)
        .collect::<serde_json::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn nonnegative_i64(value: i64) -> Option<i64> {
    (value >= 0).then_some(value)
}
