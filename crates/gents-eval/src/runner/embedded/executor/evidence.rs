use anyhow::{Context, Result};
use serde_json::Value;

use super::usage::{home_charge, home_rows, total};
use crate::runner::embedded::observe::InferenceCallEvidence;
use crate::runner::executor::StageEvidence;
use crate::{Anchor, TrialUsage};
#[cfg(test)]
use gents::defra_node::Node as EmbeddedNode;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;

pub(super) struct SessionRequest {
    pub(super) request_id: String,
    pub(super) lifecycle_state: String,
}

pub(super) async fn session_requests_with_access(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Vec<SessionRequest>> {
    let session_id = escape_graphql_string(session_id);
    let scope = gents_session::session::public_request_filter(&format!(
        r#"session_id: {{ _eq: "{session_id}" }}"#
    ));
    let query = format!(
        r#"{{ AgentRequest(filter: {{ {scope} }}, order: {{ created_at: ASC }}) {{ request_id lifecycle_state }} }}"#
    );
    let response = access.execute(&query).await?;
    Ok(response
        .get("data")
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .context("trial session query omitted request rows")?
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

#[cfg(test)]
pub(super) async fn home_usage(node: &std::sync::Arc<EmbeddedNode>) -> TrialUsage {
    home_usage_with_access(&ConfigAccess::local(node.clone()))
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(error = %format!("{error:#}"), "eval trial usage could not be collected");
            TrialUsage::default()
        })
}

pub(in crate::runner::embedded) async fn home_usage_with_access(
    access: &ConfigAccess,
) -> Result<TrialUsage> {
    Ok(home_charge(&home_rows(access).await?).usage)
}

pub(in crate::runner::embedded) fn usage_of(calls: &[InferenceCallEvidence]) -> TrialUsage {
    TrialUsage {
        input_tokens: total(calls.iter().map(|call| call.prompt_tokens)),
        output_tokens: total(calls.iter().map(|call| call.completion_tokens)),
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
