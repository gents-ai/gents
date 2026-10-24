use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use gents_codex_protocol as codex;
use gents_protocol::row::AgentRequestRow;
use gents_protocol::usage::{TokenUsage, TokenUsageColumns, TokenUsageSummary};
use serde::Deserialize;
use serde_json::Value;

use crate::commands::codex_shim::store::query_node_json;
use crate::commands::codex_shim::ShimState;

use super::CodexThreadRecord;

#[derive(Clone, Copy)]
struct UsageScope<'a> {
    node_did: &'a str,
    agent_id: &'a str,
    requester_did: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub(in crate::commands::codex_shim) struct TokenTotals(pub Option<TokenUsageSummary>);

impl TokenTotals {
    fn from_observations<I>(observations: I) -> Self
    where
        I: IntoIterator<Item = TokenUsage>,
    {
        Self(TokenUsageSummary::from_observations(observations).ok())
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(in crate::commands::codex_shim) struct InferenceUsageObservation {
    pub(in crate::commands::codex_shim) call_id: String,
    pub(in crate::commands::codex_shim) totals: TokenTotals,
}

#[derive(Debug, Deserialize, serde::Serialize, PartialEq, Eq)]
struct InferenceCallUsageRow {
    call_id: String,
    request_id: String,
    #[serde(default)]
    call_seq: Option<i64>,
    #[serde(flatten)]
    usage: TokenUsageColumns,
}

async fn requests_token_usage_scoped(
    state: &ShimState,
    request_ids: &[String],
    scope: UsageScope<'_>,
) -> Result<TokenTotals> {
    let usage = gather_request_usage(state, request_ids, scope).await?;
    let Ok(usage) = coherent_usage(usage) else {
        return Ok(TokenTotals::default());
    };
    let observations = usage
        .into_iter()
        .map(|row| row.usage.observation())
        .collect::<Result<Vec<_>>>();
    Ok(observations
        .map(TokenTotals::from_observations)
        .unwrap_or_default())
}

pub(in crate::commands::codex_shim) async fn submitted_token_usage(
    state: &ShimState,
    request: &crate::SubmittedRequest,
    turn_request_ids: Option<&[String]>,
) -> Result<TokenTotals> {
    let scope = UsageScope {
        node_did: &request.node_did,
        requester_did: request.requester_did.as_deref(),
        agent_id: request
            .agent_id
            .as_deref()
            .context("committed request missing agent")?,
    };
    if let Some(ids) = turn_request_ids {
        latest_requests_token_usage_scoped(state, ids, scope).await
    } else {
        let ids = session_request_ids(state, &request.session_id, scope).await?;
        requests_token_usage_scoped(state, &ids, scope).await
    }
}

pub(in crate::commands::codex_shim) fn latest_inference_usage_observation(
    rows: &[Value],
) -> Option<InferenceUsageObservation> {
    rows.iter()
        .filter(|row| row.get("call_kind").and_then(Value::as_str) == Some("inference"))
        .filter(|row| {
            matches!(
                row.get("call_state").and_then(Value::as_str),
                Some("failed" | "completed" | "cancelled")
            )
        })
        .filter_map(|row| {
            let call_id = row.get("call_id")?.as_str()?.to_string();
            let usage: TokenUsageColumns = serde_json::from_value(row.clone()).ok()?;
            Some((
                row.get("call_seq")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                InferenceUsageObservation {
                    call_id,
                    totals: usage
                        .observation()
                        .map(|usage| TokenTotals::from_observations([usage]))
                        .unwrap_or_default(),
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
    let total = requests_token_usage_scoped(state, &request_ids, scope).await?;
    let last = latest_requests_token_usage_scoped(state, &request_ids, scope).await?;
    Ok((total, last))
}

pub(in crate::commands::codex_shim) fn thread_token_usage(
    total: TokenTotals,
    last: TokenTotals,
    model_context_window: i64,
) -> Option<codex::ThreadTokenUsage> {
    Some(codex::ThreadTokenUsage {
        total: token_breakdown(total)?,
        last: token_breakdown(last)?,
        model_context_window: Some(model_context_window),
    })
}

async fn session_request_ids(
    state: &ShimState,
    session_id: &str,
    scope: UsageScope<'_>,
) -> Result<Vec<String>> {
    let session_scope =
        gents::session::session_scope_filter(scope.node_did, session_id, scope.requester_did);
    let session_scope = gents::session::public_request_filter(&session_scope);
    let escaped_agent_id = escape_graphql_string(scope.agent_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    {session_scope},
                    agent_id: {{ _eq: "{escaped_agent_id}" }}
                }},
                order: {{ created_at: ASC }}
            ) {{
                request_id
            }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    let rows = rows::<AgentRequestRow>(&response, "AgentRequest")
        .context("decoding AgentRequest rows for token usage")?;
    Ok(rows
        .into_iter()
        .map(|row| row.request_id)
        .filter(|request_id| !request_id.trim().is_empty())
        .collect())
}

async fn scoped_request_doc_ids(
    state: &ShimState,
    ids: Vec<&str>,
    scope: UsageScope<'_>,
) -> Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let logical = ids
        .iter()
        .map(|id| format!("\"{}\"", escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(",");
    let owner = escape_graphql_string(scope.node_did);
    let agent = escape_graphql_string(scope.agent_id);
    let requester = scope
        .requester_did
        .map(|did| format!("\"{}\"", escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    let scope = gents::session::public_request_filter(&format!(
        r#"request_id:{{_in:[{logical}]}},node_did:{{_eq:"{owner}"}},agent_id:{{_eq:"{agent}"}},requester_did:{{_eq:{requester}}}"#
    ));
    let response = query_node_json(
        &state.node,
        &format!(r#"{{AgentRequest(filter:{{{scope}}}){{_docID request_id}}}}"#),
    )
    .await?;
    let mut resolved = BTreeMap::new();
    for row in rows::<Value>(&response, "AgentRequest")? {
        let id = row["request_id"]
            .as_str()
            .context("usage request missing logical ID")?;
        let physical = row["_docID"]
            .as_str()
            .context("usage request missing physical ID")?;
        anyhow::ensure!(
            resolved
                .insert(id.to_owned(), physical.to_owned())
                .is_none(),
            "ambiguous scoped usage request"
        );
    }
    anyhow::ensure!(
        ids.iter().all(|id| resolved.contains_key(*id)),
        "usage request is absent from exact requester scope"
    );
    Ok(resolved.into_values().collect())
}

async fn gather_request_usage(
    state: &ShimState,
    request_ids: &[String],
    scope: UsageScope<'_>,
) -> Result<Vec<InferenceCallUsageRow>> {
    let request_ids = request_ids
        .iter()
        .filter(|request_id| !request_id.trim().is_empty())
        .collect::<BTreeSet<_>>();
    if request_ids.is_empty() {
        return Ok(Vec::new());
    }

    let physical_ids = scoped_request_doc_ids(
        state,
        request_ids.iter().map(|id| id.as_str()).collect(),
        scope,
    )
    .await?;
    let id_list = physical_ids
        .iter()
        .map(|request_id| format!(r#""{}""#, escape_graphql_string(request_id)))
        .collect::<Vec<_>>()
        .join(", ");
    let escaped_node_did = escape_graphql_string(scope.node_did);
    let escaped_agent_id = escape_graphql_string(scope.agent_id);
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{
                    request_doc_id: {{ _in: [{id_list}] }},
                    node_did: {{ _eq: "{escaped_node_did}" }},
                    agent_id: {{ _eq: "{escaped_agent_id}" }},
                    call_kind: {{ _in: ["inference", "compaction", "oneoff"] }},
                    call_state: {{ _in: ["completed", "failed", "cancelled"] }}
                }}
            ) {{
                call_id
                request_id
                call_seq
                prompt_tokens
                completion_tokens
                cached_input_tokens
                cache_creation_input_tokens
                reasoning_output_tokens
                provider_total_tokens
                usage_completeness
            }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    rows::<InferenceCallUsageRow>(&response, "InferenceCall")
        .context("decoding InferenceCall rows for token usage")
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
    let physical_ids =
        scoped_request_doc_ids(state, positions.keys().copied().collect(), scope).await?;
    let id_list = physical_ids
        .iter()
        .map(|request_id| format!(r#""{}""#, escape_graphql_string(request_id)))
        .collect::<Vec<_>>()
        .join(", ");
    let escaped_node_did = escape_graphql_string(scope.node_did);
    let escaped_agent_id = escape_graphql_string(scope.agent_id);
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{
                    request_doc_id: {{ _in: [{id_list}] }},
                    node_did: {{ _eq: "{escaped_node_did}" }},
                    agent_id: {{ _eq: "{escaped_agent_id}" }},
                    call_kind: {{ _eq: "inference" }},
                    call_state: {{ _in: ["completed", "failed", "cancelled"] }}
                }}
            ) {{
                call_id
                request_id
                call_seq
                prompt_tokens
                completion_tokens
                cached_input_tokens
                cache_creation_input_tokens
                reasoning_output_tokens
                provider_total_tokens
                usage_completeness
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
    let Ok(rows) = coherent_usage(rows) else {
        return TokenTotals::default();
    };
    rows.into_iter()
        .filter_map(|row| {
            positions
                .get(row.request_id.as_str())
                .map(|position| ((*position, row.call_seq.unwrap_or_default()), row.usage))
        })
        .max_by_key(|(ordering, _)| *ordering)
        .and_then(|(_, usage)| usage.observation().ok())
        .map(|usage| TokenTotals::from_observations([usage]))
        .unwrap_or_default()
}

fn coherent_usage(rows: Vec<InferenceCallUsageRow>) -> Result<Vec<InferenceCallUsageRow>> {
    let mut unique = BTreeMap::<String, InferenceCallUsageRow>::new();
    for row in rows {
        anyhow::ensure!(!row.call_id.is_empty(), "usage row has no call identity");
        if let Some(existing) = unique.get(&row.call_id) {
            anyhow::ensure!(existing == &row, "conflicting usage for one call identity");
        } else {
            unique.insert(row.call_id.clone(), row);
        }
    }
    Ok(unique.into_values().collect())
}

fn root_usage_scope(state: &ShimState) -> UsageScope<'_> {
    UsageScope {
        node_did: state.node_did.as_ref(),
        agent_id: state.agent_id.as_ref(),
        requester_did: Some(state.local_requester_did()),
    }
}

fn record_usage_scope<'a>(state: &'a ShimState, record: &'a CodexThreadRecord) -> UsageScope<'a> {
    record
        .caused
        .as_ref()
        .map(|link| UsageScope {
            node_did: &link.node_did,
            agent_id: &link.agent_id,
            requester_did: link.requester_did.as_deref(),
        })
        .unwrap_or_else(|| root_usage_scope(state))
}

/// Codex requires integer breakdowns. Do not publish a complete-looking update
/// when a call or a breakdown was unreported; the durable ledger retains the
/// partial observation for clients that can represent missing counters.
fn token_breakdown(totals: TokenTotals) -> Option<codex::TokenUsageBreakdown> {
    let summary = totals.0?;
    if summary.cache_read.reported != summary.calls || summary.reasoning.reported != summary.calls {
        return None;
    }
    Some(codex::TokenUsageBreakdown {
        total_tokens: i64::try_from(summary.charged_total()?).ok()?,
        input_tokens: i64::try_from(summary.input.known).ok()?,
        cached_input_tokens: i64::try_from(summary.cache_read.known).ok()?,
        output_tokens: i64::try_from(summary.output.known).ok()?,
        reasoning_output_tokens: i64::try_from(summary.reasoning.known).ok()?,
    })
}

fn rows<T>(response: &Value, name: &str) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    response
        .pointer(&format!("/data/{name}"))
        .and_then(Value::as_array)
        .cloned()
        .with_context(|| format!("usage query omitted {name} rows"))?
        .into_iter()
        .map(serde_json::from_value)
        .collect::<serde_json::Result<Vec<_>>>()
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(request: &str, sequence: i64, input: i64, output: Option<i64>) -> InferenceCallUsageRow {
        serde_json::from_value(json!({"call_id":format!("{request}-{sequence}"),"request_id":request,"call_seq":sequence,"prompt_tokens":input,"completion_tokens":output,"cached_input_tokens":20,"reasoning_output_tokens":2,"usage_completeness":"complete"})).unwrap()
    }

    #[test]
    fn latest_usage_selects_newest_request_then_newest_call() {
        let rows = vec![
            row("request-2", 1, 300, Some(20)),
            row("request-1", 9, 900, Some(90)),
            row("request-2", 2, 350, Some(25)),
        ];
        let positions = BTreeMap::from([("request-1", 0), ("request-2", 1)]);
        let totals = token_breakdown(latest_usage_from_rows(rows, &positions)).unwrap();
        assert_eq!(totals.input_tokens, 350);
        assert_eq!(totals.output_tokens, 25);
        assert_eq!(totals.cached_input_tokens, 20);
        assert_eq!(totals.reasoning_output_tokens, 2);
        assert_eq!(totals.total_tokens, 375);
    }

    #[test]
    fn latest_missing_usage_does_not_reuse_an_older_complete_call() {
        let positions = BTreeMap::from([("request", 0)]);
        let rows = vec![
            row("request", 1, 300, Some(20)),
            row("request", 2, 350, None),
        ];
        assert!(token_breakdown(latest_usage_from_rows(rows, &positions)).is_none());
    }

    #[test]
    fn live_context_observation_uses_latest_terminal_inference_call() {
        let mut first = serde_json::to_value(row("request", 1, 200, Some(10))).unwrap();
        first.as_object_mut().unwrap().extend(
            json!({"call_id":"first","call_kind":"inference","call_state":"completed"})
                .as_object()
                .unwrap()
                .clone(),
        );
        let mut latest = first.clone();
        latest["call_id"] = json!("latest");
        latest["call_seq"] = json!(2);
        latest["usage_completeness"] = json!("missing");
        latest["prompt_tokens"] = Value::Null;
        latest["completion_tokens"] = Value::Null;
        let observation = latest_inference_usage_observation(&[first, latest]).unwrap();
        assert_eq!(observation.call_id, "latest");
        assert!(token_breakdown(observation.totals).is_none());
    }
}
