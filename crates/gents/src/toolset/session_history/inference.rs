use std::collections::BTreeSet;

use anyhow::{Context, Result};

use super::{InferenceDetailRow, SessionTokenUsage};

pub(super) const INFERENCE_DETAIL_FIELDS: &str = "request_doc_id request_id call_id call_seq queued_at started_at ended_at prompt_tokens call_kind completion_tokens cached_input_tokens cache_creation_input_tokens reasoning_output_tokens provider_total_tokens usage_completeness context_accounting_json";

pub(super) fn inference_turns(rows: &[InferenceDetailRow]) -> Result<Option<u64>> {
    let mut turns = BTreeSet::new();
    for row in rows {
        match row.call_kind.as_deref() {
            Some("inference") => {}
            Some("compaction" | "oneoff") => continue,
            _ => return Ok(None),
        }
        let Some(request_doc_id) = row.request_doc_id.as_deref().filter(|id| !id.is_empty()) else {
            return Ok(None);
        };
        let Some(encoded) = row
            .context_accounting_json
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        else {
            return Ok(None);
        };
        let accounting: gents_protocol::rendered_request::ContextAccounting =
            serde_json::from_str(encoded)
                .with_context(|| format!("decoding turn coordinate for call {}", row.call_id))?;
        turns.insert((request_doc_id, accounting.turn_index));
    }
    Ok(Some(turns.len() as u64))
}

pub(super) fn aggregate_inference_usage(rows: &[InferenceDetailRow]) -> Result<SessionTokenUsage> {
    let api_duration_ms = rows.iter().try_fold(0_u64, |total, row| {
        let started = chrono::DateTime::parse_from_rfc3339(row.started_at.as_deref()?).ok()?;
        let ended = chrono::DateTime::parse_from_rfc3339(row.ended_at.as_deref()?).ok()?;
        if ended < started {
            return None;
        }
        let duration =
            u64::try_from(ended.signed_duration_since(started).num_milliseconds()).ok()?;
        total.checked_add(duration)
    });
    let observations = rows
        .iter()
        .map(|row| row.usage.observation())
        .collect::<Result<Vec<_>, _>>();
    let calls_with_usage = rows
        .iter()
        .filter(|row| {
            row.usage.observation().ok().is_some_and(|usage| {
                usage.completeness() == gents_protocol::usage::UsageCompleteness::Complete
            })
        })
        .count() as i64;
    let summary = observations
        .and_then(gents_protocol::usage::TokenUsageSummary::from_observations)
        .ok();
    let calls = i64::try_from(rows.len()).context("too many inference calls")?;
    Ok(SessionTokenUsage {
        model_calls: calls,
        calls_with_usage,
        calls_missing_usage: calls - calls_with_usage,
        input_tokens: summary.and_then(|value| value.input.observed()),
        cached_input_tokens: summary.and_then(|value| value.cache_read.observed()),
        cache_creation_input_tokens: summary.and_then(|value| value.cache_write.observed()),
        reasoning_output_tokens: summary.and_then(|value| value.reasoning.observed()),
        provider_total_tokens: summary.and_then(|value| value.provider_total.observed()),
        fresh_input_tokens: summary.and_then(|value| value.fresh_input()),
        output_tokens: summary.and_then(|value| value.output.observed()),
        charged_tokens: summary.and_then(|value| value.charged_total()),
        api_duration_ms,
        incomplete: summary.is_none() || calls_with_usage != calls,
    })
}

pub(super) fn latest_context_from_detail_rows(
    rows: &[InferenceDetailRow],
) -> Result<Option<super::super::context_budget::LastRequestContextSnapshot>> {
    let Some(row) = rows
        .iter()
        .filter(|row| row.call_kind.as_deref() == Some("inference"))
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
    Ok(Some(
        super::super::context_budget::LastRequestContextSnapshot {
            request_id: row.request_id.clone(),
            call_id: row.call_id.clone(),
            call_sequence: row.call_seq,
            queued_at: row.queued_at.clone(),
            accounting: serde_json::from_str(encoded)
                .with_context(|| format!("decoding context accounting for call {}", row.call_id))?,
        },
    ))
}
