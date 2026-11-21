use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use gents_protocol::usage::TokenUsageColumns;
use gents_store::config_client::{post_graphql, GraphqlEndpoint};
use serde::Deserialize;
use serde_json::Value;

use super::exposition::rfc3339_timestamp;
use super::inference::{InferenceMetricsQueryData, INFERENCE_METRICS_WINDOW_SECS};
use super::store::null_string_as_unknown;

const INFERENCE_METRICS_PAGE_SIZE: usize = 500;

#[derive(Debug, Deserialize)]
pub(super) struct InferenceMetricsPageData {
    #[serde(rename = "Node", default)]
    nodes: Vec<InferenceNodeRow>,
    #[serde(rename = "Agent", default)]
    agents: Vec<InferenceAgentRow>,
    #[serde(rename = "InferenceCall", default)]
    pub(super) calls: Vec<InferenceCallMetricRow>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct InferenceNodeRow {
    #[serde(default)]
    pub(super) node_did: String,
    #[serde(default)]
    pub(super) display_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct InferenceAgentRow {
    #[serde(default)]
    pub(super) agent_did: String,
    #[serde(default)]
    pub(super) node_did: String,
    #[serde(default)]
    pub(super) display_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct InferenceCallMetricRow {
    #[serde(default)]
    pub(super) node_did: String,
    #[serde(default)]
    pub(super) agent_did: String,
    #[serde(default)]
    pub(super) backend_id: String,
    #[serde(default)]
    pub(super) call_state: String,
    #[serde(default)]
    pub(super) ended_at: String,
    #[serde(default, deserialize_with = "null_string_as_unknown")]
    pub(super) model_name: String,
    #[serde(default)]
    pub(super) call_kind: String,
    #[serde(flatten)]
    pub(super) usage: TokenUsageColumns,
}

pub(super) async fn load_inference_metrics_query_data(
    graphql: &GraphqlEndpoint,
) -> Result<InferenceMetricsQueryData> {
    let window_started_at = Utc::now() - Duration::seconds(INFERENCE_METRICS_WINDOW_SECS);
    let mut result = InferenceMetricsQueryData {
        nodes: Vec::new(),
        agents: Vec::new(),
        calls: Vec::new(),
        window_seconds: INFERENCE_METRICS_WINDOW_SECS,
    };
    let mut offset = 0usize;

    loop {
        let query = inference_metrics_query(
            INFERENCE_METRICS_PAGE_SIZE,
            offset,
            offset == 0,
            &window_started_at,
        );
        let page = load_inference_metrics_page(graphql, &query).await?;
        if offset == 0 {
            result.nodes = page.nodes;
            result.agents = page.agents;
        }

        let page_len = page.calls.len();
        let (recent_calls, reached_window_start) =
            retain_windowed_inference_calls(page.calls, &window_started_at);
        result.calls.extend(recent_calls);

        if page_len < INFERENCE_METRICS_PAGE_SIZE || reached_window_start {
            break;
        }
        offset += INFERENCE_METRICS_PAGE_SIZE;
    }

    Ok(result)
}

async fn load_inference_metrics_page(
    graphql: &GraphqlEndpoint,
    query: &str,
) -> Result<InferenceMetricsPageData> {
    let response = post_graphql(graphql, query).await?;
    let data = response
        .get("data")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    serde_json::from_value(data).context("decoding inference metrics query response")
}

/// `ended_at` is stored as RFC 3339 UTC text. The bound keeps whole seconds
/// and no offset, so it sorts before every timestamp inside the window; rows
/// are still checked exactly by `retain_windowed_inference_calls`.
pub(super) fn inference_metrics_ended_lower_bound(window_started_at: &DateTime<Utc>) -> String {
    window_started_at.format("%Y-%m-%dT%H:%M:%S").to_string()
}

pub(super) fn inference_metrics_query(
    limit: usize,
    offset: usize,
    include_metadata: bool,
    window_started_at: &DateTime<Utc>,
) -> String {
    let ended_after = inference_metrics_ended_lower_bound(window_started_at);
    let metadata = if include_metadata {
        r#"
        Node {
            node_did
            display_name
        }
        Agent {
            agent_did: agent_did
            node_did
            display_name
        }

"#
    } else {
        ""
    };

    format!(
        r#"{{
        {metadata}
        InferenceCall(
            filter: {{
                call_kind: {{ _in: ["inference", "compaction", "scheduled", "oneoff"] }},
                call_state: {{ _in: ["completed", "failed", "cancelled"] }},
                ended_at: {{ _ge: "{ended_after}" }}
            }},
            order: [{{ ended_at: DESC }}, {{ call_id: DESC }}],
            limit: {limit},
            offset: {offset}
        ) {{
            node_did
            agent_did
            backend_id
            call_state
            ended_at
            model_name
            call_kind
            prompt_tokens
            completion_tokens
            cached_input_tokens
            cache_creation_input_tokens
            reasoning_output_tokens
            provider_total_tokens
            usage_completeness
        }}
    }}"#
    )
}

fn retain_windowed_inference_calls(
    calls: Vec<InferenceCallMetricRow>,
    window_started_at: &DateTime<Utc>,
) -> (Vec<InferenceCallMetricRow>, bool) {
    let mut recent = Vec::new();
    let mut reached_window_start = false;
    for call in calls {
        let Some(ended_at) = rfc3339_timestamp(&call.ended_at) else {
            reached_window_start = true;
            continue;
        };
        if ended_at < *window_started_at {
            reached_window_start = true;
        } else {
            recent.push(call);
        }
    }
    (recent, reached_window_start)
}
