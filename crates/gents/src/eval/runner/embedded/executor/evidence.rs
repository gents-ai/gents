use anyhow::{Context, Result};
use serde_json::Value;

use crate::defra_node::EmbeddedNode;
use crate::eval::runner::embedded::observe::{
    request_evidence_from_query_data, InferenceCallEvidence,
};
use crate::eval::runner::executor::StageEvidence;
use crate::eval::{Anchor, TrialUsage};
use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};

pub(super) struct SessionRequest {
    pub(super) request_id: String,
    pub(super) lifecycle_state: String,
}

pub(super) async fn session_requests(
    node: &EmbeddedNode,
    session_id: &str,
) -> Result<Vec<SessionRequest>> {
    let session_id = escape_graphql_string(session_id);
    let scope =
        crate::session::public_request_filter(&format!(r#"session_id: {{ _eq: "{session_id}" }}"#));
    let query = format!(
        r#"{{ AgentRequest(filter: {{ {scope} }}, order: {{ created_at: ASC }}) {{ request_id lifecycle_state }} }}"#
    );
    let response = graphql_with_transaction_retry(node, &query, "eval trial session").await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|row| {
            Some(SessionRequest {
                request_id: row.get("request_id")?.as_str()?.to_string(),
                lifecycle_state: row
                    .get("lifecycle_state")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect())
}

pub(super) async fn home_usage(node: &EmbeddedNode) -> TrialUsage {
    let result = async {
        let response = graphql_with_transaction_retry(
            node,
            "{ InferenceCall { call_seq call_state failure_reason prompt_tokens completion_tokens } }",
            "eval trial total usage",
        ).await?;
        let data = response.data.context("trial usage query omitted data")?;
        anyhow::ensure!(data.get("InferenceCall").is_some_and(Value::is_array), "trial usage query omitted inference calls");
        Ok::<_, anyhow::Error>(usage_of(&request_evidence_from_query_data(&data).inference_calls))
    }.await;
    result.unwrap_or_else(|error| {
        tracing::warn!(error = %format!("{error:#}"), "eval trial usage could not be collected");
        TrialUsage::default()
    })
}

pub(in crate::eval::runner::embedded) fn usage_of<'a>(
    calls: impl IntoIterator<Item = &'a InferenceCallEvidence>,
) -> TrialUsage {
    let mut input_tokens = Some(0u64);
    let mut output_tokens = Some(0u64);
    for call in calls {
        input_tokens = input_tokens
            .zip(call.prompt_tokens)
            .map(|(total, tokens)| total.saturating_add(tokens));
        output_tokens = output_tokens
            .zip(call.completion_tokens)
            .map(|(total, tokens)| total.saturating_add(tokens));
    }
    TrialUsage {
        input_tokens,
        output_tokens,
    }
}

pub(super) fn anchor(stages: &[StageEvidence]) -> Anchor {
    Anchor {
        terminal_states: stages
            .iter()
            .filter_map(|stage| stage.terminal_state)
            .collect(),
        requests: count(
            stages
                .iter()
                .filter(|stage| stage.request_id.is_some())
                .count(),
        ),
        inference_calls: count(
            stages
                .iter()
                .map(|stage| stage.inference_calls.len())
                .sum::<usize>(),
        ),
    }
}

fn count(total: usize) -> u32 {
    u32::try_from(total).unwrap_or(u32::MAX)
}
