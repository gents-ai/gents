use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use codex_app_server_protocol as codex;
use defra_agent::graphql::escape_graphql_string;
use serde::Deserialize;
use serde_json::Value;

use crate::commands::codex_shim::store::query_node_json;
use crate::commands::codex_shim::ShimState;

use super::CodexThreadRecord;

#[derive(Clone, Copy)]
struct UsageScope<'a> {
    agent_did: &'a str,
    behavior_id: &'a str,
}

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

#[derive(Debug, Clone, Eq, PartialEq)]
pub(in crate::commands::codex_shim) struct InferenceUsageObservation {
    pub(in crate::commands::codex_shim) call_id: String,
    pub(in crate::commands::codex_shim) totals: TokenTotals,
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
    call_seq: Option<i64>,
    #[serde(default)]
    prompt_tokens: Option<i64>,
    #[serde(default)]
    completion_tokens: Option<i64>,
}

pub(in crate::commands::codex_shim) async fn requests_token_usage(
    state: &ShimState,
    request_ids: &[String],
) -> Result<TokenTotals> {
    let usage = gather_request_usage(state, request_ids, root_usage_scope(state)).await?;
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
    let scope = root_usage_scope(state);
    let request_ids = session_request_ids(state, session_id, scope).await?;
    requests_token_usage(state, &request_ids).await
}

pub(in crate::commands::codex_shim) async fn latest_requests_token_usage(
    state: &ShimState,
    request_ids: &[String],
) -> Result<TokenTotals> {
    latest_requests_token_usage_scoped(state, request_ids, root_usage_scope(state)).await
}

pub(in crate::commands::codex_shim) fn latest_inference_usage_observation(
    rows: &[Value],
) -> Option<InferenceUsageObservation> {
    rows.iter()
        .filter(|row| row.get("call_kind").and_then(Value::as_str) == Some("inference"))
        .filter(|row| {
            matches!(
                row.get("call_state").and_then(Value::as_str),
                Some("completed" | "failed" | "cancelled")
            )
        })
        .filter_map(|row| {
            let call_id = row.get("call_id")?.as_str()?.to_string();
            let prompt_tokens = row.get("prompt_tokens").and_then(Value::as_i64);
            let completion_tokens = row.get("completion_tokens").and_then(Value::as_i64);
            if prompt_tokens.is_none() && completion_tokens.is_none() {
                return None;
            }
            Some((
                row.get("call_seq")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                InferenceUsageObservation {
                    call_id,
                    totals: TokenTotals {
                        input_tokens: prompt_tokens.and_then(nonnegative_i64).unwrap_or_default(),
                        output_tokens: completion_tokens
                            .and_then(nonnegative_i64)
                            .unwrap_or_default(),
                    },
                },
            ))
        })
        .max_by_key(|(call_seq, _)| *call_seq)
        .map(|(_, observation)| observation)
}

pub(in crate::commands::codex_shim) async fn thread_record_token_usage(
    state: &ShimState,
    record: &CodexThreadRecord,
) -> Result<(TokenTotals, TokenTotals)> {
    let scope = record_usage_scope(state, record);
    let request_ids = session_request_ids(state, &record.session_id, scope).await?;
    let total = gather_request_usage(state, &request_ids, scope)
        .await?
        .into_values()
        .fold(TokenTotals::default(), |mut totals, usage| {
            totals.input_tokens += usage.input_tokens;
            totals.output_tokens += if usage.has_real_output {
                usage.output_tokens
            } else {
                usage.proxy_output_tokens
            };
            totals
        });
    let last = latest_requests_token_usage_scoped(state, &request_ids, scope).await?;
    Ok((total, last))
}

pub(in crate::commands::codex_shim) fn thread_token_usage(
    total: TokenTotals,
    last: TokenTotals,
    model_context_window: i64,
) -> codex::ThreadTokenUsage {
    codex::ThreadTokenUsage {
        total: token_breakdown(total),
        last: token_breakdown(last),
        model_context_window: Some(model_context_window),
    }
}

async fn session_request_ids(
    state: &ShimState,
    session_id: &str,
    scope: UsageScope<'_>,
) -> Result<Vec<String>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(scope.agent_did);
    let escaped_behavior_id = escape_graphql_string(scope.behavior_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    behavior_id: {{ _eq: "{escaped_behavior_id}" }}
                }},
                order: {{ created_at: ASC }}
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
    scope: UsageScope<'_>,
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
    let escaped_agent_did = escape_graphql_string(scope.agent_did);
    let escaped_behavior_id = escape_graphql_string(scope.behavior_id);
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
                call_seq
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

async fn latest_requests_token_usage_scoped(
    state: &ShimState,
    request_ids: &[String],
    scope: UsageScope<'_>,
) -> Result<TokenTotals> {
    let positions = request_ids
        .iter()
        .enumerate()
        .filter(|(_, request_id)| !request_id.trim().is_empty())
        .map(|(index, request_id)| (request_id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    if positions.is_empty() {
        return Ok(TokenTotals::default());
    }
    let id_list = positions
        .keys()
        .map(|request_id| format!(r#""{}""#, escape_graphql_string(request_id)))
        .collect::<Vec<_>>()
        .join(", ");
    let escaped_agent_did = escape_graphql_string(scope.agent_did);
    let escaped_behavior_id = escape_graphql_string(scope.behavior_id);
    let query = format!(
        r#"{{
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
                call_seq
                prompt_tokens
                completion_tokens
            }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    let rows = rows::<InferenceCallUsageRow>(&response, "InferenceCall")
        .context("decoding latest InferenceCall row for context usage")?;
    Ok(latest_usage_from_rows(rows, &positions))
}

fn latest_usage_from_rows(
    rows: Vec<InferenceCallUsageRow>,
    positions: &BTreeMap<&str, usize>,
) -> TokenTotals {
    let mut latest = None::<((usize, i64), TokenTotals)>;
    for row in rows {
        let Some(request_position) = positions.get(row.request_id.as_str()).copied() else {
            continue;
        };
        if row.prompt_tokens.is_none() && row.completion_tokens.is_none() {
            continue;
        }
        let ordering = (request_position, row.call_seq.unwrap_or_default());
        let totals = TokenTotals {
            input_tokens: row
                .prompt_tokens
                .and_then(nonnegative_i64)
                .unwrap_or_default(),
            output_tokens: row
                .completion_tokens
                .and_then(nonnegative_i64)
                .unwrap_or_default(),
        };
        if latest
            .as_ref()
            .is_none_or(|(latest_ordering, _)| ordering > *latest_ordering)
        {
            latest = Some((ordering, totals));
        }
    }
    latest.map(|(_, totals)| totals).unwrap_or_default()
}

fn root_usage_scope(state: &ShimState) -> UsageScope<'_> {
    UsageScope {
        agent_did: state.agent_did.as_ref(),
        behavior_id: state.behavior_id.as_ref(),
    }
}

fn record_usage_scope<'a>(state: &'a ShimState, record: &'a CodexThreadRecord) -> UsageScope<'a> {
    record
        .subagent
        .as_ref()
        .map(|link| UsageScope {
            agent_did: &link.agent_did,
            behavior_id: &link.behavior_id,
        })
        .unwrap_or_else(|| root_usage_scope(state))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_usage_keeps_cumulative_and_current_context_distinct() {
        let usage = thread_token_usage(
            TokenTotals {
                input_tokens: 850,
                output_tokens: 150,
            },
            TokenTotals {
                input_tokens: 300,
                output_tokens: 20,
            },
            1_000,
        );

        assert_eq!(usage.total.total_tokens, 1_000);
        assert_eq!(usage.last.total_tokens, 320);
        assert_eq!(usage.model_context_window, Some(1_000));
    }

    #[test]
    fn latest_usage_selects_newest_request_then_newest_call() {
        let rows = vec![
            InferenceCallUsageRow {
                request_id: "request-2".to_string(),
                call_seq: Some(1),
                prompt_tokens: Some(300),
                completion_tokens: Some(20),
            },
            InferenceCallUsageRow {
                request_id: "request-1".to_string(),
                call_seq: Some(9),
                prompt_tokens: Some(900),
                completion_tokens: Some(90),
            },
            InferenceCallUsageRow {
                request_id: "request-2".to_string(),
                call_seq: Some(2),
                prompt_tokens: Some(350),
                completion_tokens: Some(25),
            },
        ];
        let positions = BTreeMap::from([("request-1", 0), ("request-2", 1)]);

        assert_eq!(
            latest_usage_from_rows(rows, &positions),
            TokenTotals {
                input_tokens: 350,
                output_tokens: 25,
            }
        );
    }

    #[test]
    fn live_context_observation_uses_latest_terminal_inference_call() {
        let rows = vec![
            serde_json::json!({
                "call_id": "compact",
                "call_kind": "compaction",
                "call_state": "completed",
                "call_seq": 3,
                "prompt_tokens": 999,
                "completion_tokens": 999,
            }),
            serde_json::json!({
                "call_id": "inference-1",
                "call_kind": "inference",
                "call_state": "completed",
                "call_seq": 1,
                "prompt_tokens": 200,
                "completion_tokens": 10,
            }),
            serde_json::json!({
                "call_id": "inference-2",
                "call_kind": "inference",
                "call_state": "completed",
                "call_seq": 2,
                "prompt_tokens": 300,
                "completion_tokens": 20,
            }),
        ];

        assert_eq!(
            latest_inference_usage_observation(&rows),
            Some(InferenceUsageObservation {
                call_id: "inference-2".to_string(),
                totals: TokenTotals {
                    input_tokens: 300,
                    output_tokens: 20,
                },
            })
        );
    }
}
