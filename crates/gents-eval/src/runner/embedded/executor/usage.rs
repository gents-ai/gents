//! What a trial is charged for and which of it is the subject's own work
//! (Lean `Runtime.EvalUsage`).

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    CallTotals, FirstRequest, PromptEstimate, SubjectUsage, ToolCallSplit, ToolCount, TrialUsage,
};
use gents_loop::provider_call::admission::CallKind;
use gents_protocol::document_query::{fields, Query, QuerySet, Selection};
use gents_protocol::rendered_request::ContextAccounting;
use gents_protocol::request_admission::RequestPurpose;
use gents_store::config_client::ConfigAccess;

const SCRIPT_TOOL_NAME: &str = "script";

#[derive(Clone, Debug, Default, Deserialize)]
pub(in crate::runner::embedded) struct RequestRow {
    #[serde(rename = "_docID")]
    pub(in crate::runner::embedded) doc_id: String,
    #[serde(default)]
    pub(in crate::runner::embedded) request_id: Option<String>,
    #[serde(default)]
    pub(in crate::runner::embedded) purpose: Option<String>,
    #[serde(default)]
    pub(in crate::runner::embedded) session_id: Option<String>,
    #[serde(default)]
    pub(in crate::runner::embedded) claimed_at: Option<String>,
    #[serde(default)]
    pub(in crate::runner::embedded) terminalized_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(in crate::runner::embedded) struct CallRow {
    #[serde(default)]
    pub(in crate::runner::embedded) request_doc_id: Option<String>,
    #[serde(default)]
    pub(in crate::runner::embedded) call_kind: Option<String>,
    #[serde(default)]
    pub(in crate::runner::embedded) call_seq: Option<i64>,
    #[serde(default)]
    pub(in crate::runner::embedded) started_at: Option<String>,
    #[serde(default)]
    pub(in crate::runner::embedded) ended_at: Option<String>,
    #[serde(default)]
    pub(in crate::runner::embedded) prompt_tokens: Option<u64>,
    #[serde(default)]
    pub(in crate::runner::embedded) completion_tokens: Option<u64>,
    #[serde(default)]
    pub(in crate::runner::embedded) cached_input_tokens: Option<u64>,
    #[serde(default)]
    pub(in crate::runner::embedded) context_accounting_json: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(in crate::runner::embedded) struct ToolRow {
    #[serde(default)]
    pub(in crate::runner::embedded) request_doc_id: Option<String>,
    #[serde(default)]
    pub(in crate::runner::embedded) tool_name: String,
    #[serde(default)]
    pub(in crate::runner::embedded) lifecycle_state: Option<String>,
    #[serde(default)]
    pub(in crate::runner::embedded) started_at: Option<String>,
    #[serde(default)]
    pub(in crate::runner::embedded) completed_at: Option<String>,
    #[serde(default)]
    pub(in crate::runner::embedded) plugin_parent_tool_call_doc_id: Option<String>,
    #[serde(default)]
    pub(in crate::runner::embedded) spawned_by_tool_call_doc_id: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(in crate::runner::embedded) struct HomeRows {
    pub(in crate::runner::embedded) requests: Vec<RequestRow>,
    pub(in crate::runner::embedded) calls: Vec<CallRow>,
    pub(in crate::runner::embedded) tools: Vec<ToolRow>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::runner::embedded) struct HomeCharge {
    pub(in crate::runner::embedded) calls: u64,
    pub(in crate::runner::embedded) usage: TrialUsage,
    pub(in crate::runner::embedded) reported_input_tokens: u64,
    pub(in crate::runner::embedded) reported_output_tokens: u64,
}

pub(in crate::runner::embedded) async fn home_rows(access: &ConfigAccess) -> Result<HomeRows> {
    use fields::{agent_request as request, agent_tool_call as tool, inference_call as call};
    let read = QuerySet::new()
        .plain(&Query::select(
            Selection::doc_id()
                .field(request::REQUEST_ID)
                .field(request::PURPOSE)
                .field(request::SESSION_ID)
                .field(request::CLAIMED_AT)
                .field(request::TERMINALIZED_AT),
        ))
        .plain(&Query::select(
            Selection::new()
                .field(call::REQUEST_DOC_ID)
                .field(call::CALL_KIND)
                .field(call::CALL_SEQ)
                .field(call::STARTED_AT)
                .field(call::ENDED_AT)
                .field(call::PROMPT_TOKENS)
                .field(call::COMPLETION_TOKENS)
                .field(call::CACHED_INPUT_TOKENS)
                .field(call::CONTEXT_ACCOUNTING_JSON),
        ))
        .plain(&Query::select(
            Selection::new()
                .field(tool::REQUEST_DOC_ID)
                .field(tool::TOOL_NAME)
                .field(tool::LIFECYCLE_STATE)
                .field(tool::STARTED_AT)
                .field(tool::COMPLETED_AT)
                .field(tool::PLUGIN_PARENT_TOOL_CALL_DOC_ID)
                .field(tool::SPAWNED_BY_TOOL_CALL_DOC_ID),
        ))
        .render();
    let response = access.read(&read).await?;
    let data = response
        .get("data")
        .context("trial usage read omitted data")?;
    Ok(HomeRows {
        requests: rows(data, "AgentRequest")?,
        calls: rows(data, "InferenceCall")?,
        tools: rows(data, "AgentToolCall")?,
    })
}

fn rows<Row>(data: &Value, collection: &str) -> Result<Vec<Row>>
where
    Row: serde::de::DeserializeOwned,
{
    let rows = data
        .get(collection)
        .filter(|rows| rows.is_array())
        .with_context(|| format!("trial usage read omitted {collection} rows"))?;
    serde_json::from_value(rows.clone())
        .with_context(|| format!("decoding the trial's {collection} rows"))
}

/// Lean `Runtime.EvalUsage.total`.
pub(in crate::runner::embedded) fn total(
    counts: impl IntoIterator<Item = Option<u64>>,
) -> Option<u64> {
    counts
        .into_iter()
        .try_fold(0u64, |sum, count| Some(sum.saturating_add(count?)))
}

/// Lean `Runtime.EvalUsage.charged`.
pub(in crate::runner::embedded) fn charged(
    requests: &[RequestRow],
    request_doc_id: Option<&str>,
) -> bool {
    !requests.iter().any(|request| {
        request_doc_id == Some(request.doc_id.as_str())
            && request.purpose.as_deref() == Some(RequestPurpose::TitleAudit.as_str())
    })
}

/// Lean `Runtime.EvalUsage.homeUsage`.
pub(in crate::runner::embedded) fn home_charge(rows: &HomeRows) -> HomeCharge {
    let calls: Vec<&CallRow> = rows
        .calls
        .iter()
        .filter(|call| charged(&rows.requests, call.request_doc_id.as_deref()))
        .collect();
    let reported = |count: fn(&CallRow) -> Option<u64>| {
        calls
            .iter()
            .filter_map(|call| count(call))
            .fold(0u64, u64::saturating_add)
    };
    HomeCharge {
        calls: calls.len() as u64,
        usage: TrialUsage {
            input_tokens: total(calls.iter().map(|call| call.prompt_tokens)),
            output_tokens: total(calls.iter().map(|call| call.completion_tokens)),
        },
        reported_input_tokens: reported(|call| call.prompt_tokens),
        reported_output_tokens: reported(|call| call.completion_tokens),
    }
}

/// Lean `Runtime.EvalUsage.subjectUsage`; `stage_request_ids` in stage order.
pub(in crate::runner::embedded) fn subject_usage(
    rows: &HomeRows,
    session_id: &str,
    stage_request_ids: &[&str],
) -> Option<SubjectUsage> {
    let requests: Vec<&RequestRow> = rows
        .requests
        .iter()
        .filter(|request| {
            request.purpose.as_deref() == Some(RequestPurpose::PUBLIC.as_str())
                && (request.session_id.as_deref() == Some(session_id)
                    || request
                        .request_id
                        .as_deref()
                        .is_some_and(|id| stage_request_ids.contains(&id)))
        })
        .collect();
    if requests.is_empty() {
        return None;
    }
    let own: BTreeSet<&str> = requests
        .iter()
        .map(|request| request.doc_id.as_str())
        .collect();
    let is_own = |doc_id: &Option<String>| doc_id.as_deref().is_some_and(|id| own.contains(id));
    let calls: Vec<&CallRow> = rows
        .calls
        .iter()
        .filter(|call| is_own(&call.request_doc_id))
        .collect();
    let of_kind = |kind: CallKind| -> Vec<&CallRow> {
        calls
            .iter()
            .copied()
            .filter(|call| call.call_kind.as_deref() == Some(kind.as_str()))
            .collect()
    };
    let turns = of_kind(CallKind::Inference);
    let tools: Vec<&ToolRow> = rows
        .tools
        .iter()
        .filter(|tool| is_own(&tool.request_doc_id) && tool.spawned_by_tool_call_doc_id.is_none())
        .collect();
    let first_doc = stage_request_ids.first().and_then(|first| {
        requests
            .iter()
            .find(|request| request.request_id.as_deref() == Some(*first))
            .map(|request| request.doc_id.as_str())
    });
    Some(SubjectUsage {
        requests: requests.len() as u64,
        first_request: first_doc.and_then(|doc_id| first_request(&turns, doc_id)),
        turns: call_totals(&turns),
        compaction: call_totals(&of_kind(CallKind::Compaction)),
        tool_calls: tool_split(&tools),
        wall_ms: union_ms(requests.iter().map(|request| {
            (
                request.claimed_at.as_deref(),
                request.terminalized_at.as_deref(),
            )
        })),
        model_ms: union_ms(
            turns
                .iter()
                .map(|call| (call.started_at.as_deref(), call.ended_at.as_deref())),
        ),
        tool_ms: union_ms(
            tools
                .iter()
                .filter(|tool| tool.plugin_parent_tool_call_doc_id.is_none())
                .map(|tool| (tool.started_at.as_deref(), tool.completed_at.as_deref())),
        ),
    })
}

fn first_request(turns: &[&CallRow], request_doc_id: &str) -> Option<FirstRequest> {
    let first = turns
        .iter()
        .filter(|call| call.request_doc_id.as_deref() == Some(request_doc_id))
        .min_by_key(|call| call.call_seq)?;
    Some(FirstRequest {
        prompt_tokens: first.prompt_tokens,
        estimate: first
            .context_accounting_json
            .as_deref()
            .and_then(|json| ContextAccounting::parse(json).ok()?.current())
            .map(|accounting| PromptEstimate {
                estimator: accounting.estimator,
                messages: accounting.components.messages as u64,
                documents: accounting.components.documents as u64,
                tool_schemas: accounting.components.tool_schemas as u64,
                additional_parameters: accounting.components.additional_parameters as u64,
                output_schema: accounting.components.output_schema as u64,
            }),
    })
}

fn call_totals(calls: &[&CallRow]) -> CallTotals {
    CallTotals {
        calls: calls.len() as u64,
        input_tokens: total(calls.iter().map(|call| call.prompt_tokens)),
        output_tokens: total(calls.iter().map(|call| call.completion_tokens)),
        cached_input_tokens: total(calls.iter().map(|call| call.cached_input_tokens)),
        unreported: calls
            .iter()
            .filter(|call| call.prompt_tokens.is_none() || call.completion_tokens.is_none())
            .count() as u64,
    }
}

fn tool_split(tools: &[&ToolRow]) -> ToolCallSplit {
    let mut split = ToolCallSplit::default();
    for tool in tools {
        let count: &mut ToolCount = if tool.plugin_parent_tool_call_doc_id.is_some() {
            &mut split.inside_script
        } else if tool.tool_name == SCRIPT_TOOL_NAME {
            &mut split.script
        } else {
            &mut split.direct
        };
        count.calls += 1;
        if matches!(tool.lifecycle_state.as_deref(), Some("failed" | "timedOut")) {
            count.failed += 1;
        }
    }
    split
}

/// Lean `Runtime.EvalUsage.unionLength`.
fn union_ms<'a>(
    intervals: impl Iterator<Item = (Option<&'a str>, Option<&'a str>)>,
) -> Option<u64> {
    let mut spans: Vec<(DateTime<FixedOffset>, DateTime<FixedOffset>)> = Vec::new();
    for (start, end) in intervals {
        let Some(start) = start else {
            continue;
        };
        let start = DateTime::parse_from_rfc3339(start).ok()?;
        let end = DateTime::parse_from_rfc3339(end?).ok()?;
        spans.push((start, end.max(start)));
    }
    spans.sort();
    let mut total = 0i64;
    let mut open: Option<(DateTime<FixedOffset>, DateTime<FixedOffset>)> = None;
    for (start, end) in spans {
        open = match open {
            Some((from, to)) if start <= to => Some((from, to.max(end))),
            Some((from, to)) => {
                total += (to - from).num_milliseconds();
                Some((start, end))
            }
            None => Some((start, end)),
        };
    }
    if let Some((from, to)) = open {
        total += (to - from).num_milliseconds();
    }
    u64::try_from(total).ok()
}

#[cfg(test)]
mod tests;
