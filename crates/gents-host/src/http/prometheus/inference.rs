use std::collections::BTreeMap;

use gents_protocol::usage::TokenUsageSummary;

use super::exposition::{push_metric_prelude, push_metric_sample};
use super::inference_query::{InferenceAgentRow, InferenceCallMetricRow, InferenceNodeRow};

pub(super) const INFERENCE_METRICS_WINDOW_SECS: i64 = 5 * 60;

#[derive(Debug)]
pub(super) struct InferenceMetricsQueryData {
    pub(super) nodes: Vec<InferenceNodeRow>,
    pub(super) agents: Vec<InferenceAgentRow>,
    pub(super) calls: Vec<InferenceCallMetricRow>,
    pub(super) window_seconds: i64,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct InferenceRequestMetricKey {
    pub(super) agent: String,
    pub(super) node_did: String,
    pub(super) backend_id: String,
    pub(super) model: String,
    pub(super) status: String,
    pub(super) call_kind: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct InferenceTokenMetricKey {
    pub(super) agent: String,
    pub(super) node_did: String,
    pub(super) backend_id: String,
    pub(super) model: String,
    pub(super) call_kind: String,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct InferenceMetricFamilies {
    pub(super) request_totals: BTreeMap<InferenceRequestMetricKey, i64>,
    pub(super) usage: BTreeMap<InferenceTokenMetricKey, Option<TokenUsageSummary>>,
}

pub(super) fn render_inference_metrics(lines: &mut Vec<String>, data: &InferenceMetricsQueryData) {
    let families = build_inference_metric_families(data);

    push_metric_prelude(
        lines,
        "gents_inference_metrics_window_seconds",
        "Trailing scrape window, in seconds, used for inference request and token gauges.",
    );
    push_metric_sample(
        lines,
        "gents_inference_metrics_window_seconds",
        &[],
        data.window_seconds,
    );

    push_metric_prelude(
        lines,
        "gents_inference_requests_window_count",
        "Terminal inference calls ended inside the trailing scrape window, grouped by agent, backend, model, and terminal status; this gauge is not cumulative.",
    );
    for (key, total) in families.request_totals {
        push_metric_sample(
            lines,
            "gents_inference_requests_window_count",
            &[
                ("agent", key.agent),
                ("node_did", key.node_did),
                ("backend_id", key.backend_id),
                ("model", key.model),
                ("status", key.status),
                ("call_kind", key.call_kind),
            ],
            total,
        );
    }

    type Counter = fn(&TokenUsageSummary) -> Option<u64>;
    let counters: &[(&str, &str, Counter)] = &[
        (
            "prompt_tokens",
            "Reported input tokens, including cache reads and writes.",
            |s| s.input.observed(),
        ),
        (
            "completion_tokens",
            "Reported output tokens, including reasoning.",
            |s| s.output.observed(),
        ),
        (
            "cached_input_tokens",
            "Reported cache read tokens, a subset of input.",
            |s| s.cache_read.observed(),
        ),
        (
            "cache_creation_input_tokens",
            "Reported cache write tokens, a subset of input.",
            |s| s.cache_write.observed(),
        ),
        (
            "reasoning_output_tokens",
            "Reported reasoning tokens, a subset of output.",
            |s| s.reasoning.observed(),
        ),
        (
            "provider_total_tokens",
            "Provider-reported total evidence, not an additional charge.",
            |s| s.provider_total.observed(),
        ),
        (
            "incomplete_usage",
            "Calls without complete valid provider input and output usage.",
            |s| Some(s.incomplete_calls),
        ),
        (
            "input_reported_calls",
            "Calls reporting input tokens.",
            |s| Some(s.input.reported),
        ),
        (
            "output_reported_calls",
            "Calls reporting output tokens.",
            |s| Some(s.output.reported),
        ),
        (
            "cache_read_reported_calls",
            "Calls reporting cache read tokens.",
            |s| Some(s.cache_read.reported),
        ),
        (
            "cache_write_reported_calls",
            "Calls reporting cache write tokens.",
            |s| Some(s.cache_write.reported),
        ),
        (
            "reasoning_reported_calls",
            "Calls reporting reasoning tokens.",
            |s| Some(s.reasoning.reported),
        ),
        (
            "provider_total_reported_calls",
            "Calls reporting provider total tokens.",
            |s| Some(s.provider_total.reported),
        ),
    ];
    for (counter, help, value) in counters {
        let name = format!("gents_inference_{counter}_window_sum");
        push_metric_prelude(lines, &name, help);
        for (key, summary) in &families.usage {
            let Some(amount) = summary.as_ref().and_then(value) else {
                continue;
            };
            push_metric_sample(
                lines,
                &name,
                &[
                    ("agent", key.agent.clone()),
                    ("node_did", key.node_did.clone()),
                    ("backend_id", key.backend_id.clone()),
                    ("model", key.model.clone()),
                    ("call_kind", key.call_kind.clone()),
                ],
                amount,
            );
        }
    }
}

pub(super) fn build_inference_metric_families(
    data: &InferenceMetricsQueryData,
) -> InferenceMetricFamilies {
    let nodes = data
        .nodes
        .iter()
        .filter_map(|principal| {
            let node_did = clean_metric_label(&principal.node_did)?;
            Some((node_did, clean_metric_label(&principal.display_name)))
        })
        .collect::<BTreeMap<_, _>>();
    let mut families = InferenceMetricFamilies::default();
    let mut observations = BTreeMap::<InferenceTokenMetricKey, Vec<_>>::new();
    for call in &data.calls {
        if !is_terminal_inference_status(&call.call_state) {
            continue;
        }

        let agent_did = clean_metric_label(&call.agent_did);
        let call_node_did = clean_metric_label(&call.node_did);
        let agent_config = agent_did.as_deref().and_then(|agent_did| {
            data.agents.iter().find(|agent_config| {
                clean_metric_label(&agent_config.agent_did).as_deref() == Some(agent_did)
                    && call_node_did
                        .as_deref()
                        .is_none_or(|node_did| agent_config.node_did == node_did)
            })
        });
        let node_did = call_node_did
            .or_else(|| {
                agent_config.and_then(|agent_config| clean_metric_label(&agent_config.node_did))
            })
            .unwrap_or_else(|| "unknown".to_string());
        let agent = nodes
            .get(&node_did)
            .cloned()
            .flatten()
            .or_else(|| {
                agent_config.and_then(|agent_config| clean_metric_label(&agent_config.display_name))
            })
            .unwrap_or_else(|| node_did.clone());
        let backend_id =
            clean_metric_label(&call.backend_id).unwrap_or_else(|| "unknown".to_string());
        let model = clean_metric_label(&call.model_name).unwrap_or_else(|| "unknown".to_string());
        let call_kind =
            clean_metric_label(&call.call_kind).unwrap_or_else(|| "unknown".to_string());
        let status = clean_metric_label(&call.call_state).unwrap_or_else(|| "unknown".to_string());

        let request_key = InferenceRequestMetricKey {
            agent: agent.clone(),
            node_did: node_did.clone(),
            backend_id: backend_id.clone(),
            model: model.clone(),
            status,
            call_kind: call_kind.clone(),
        };
        *families.request_totals.entry(request_key).or_default() += 1;

        let token_key = InferenceTokenMetricKey {
            agent,
            node_did,
            backend_id,
            model,
            call_kind,
        };
        observations
            .entry(token_key)
            .or_default()
            .push(call.usage.observation().unwrap_or_default());
    }

    families.usage = observations
        .into_iter()
        .map(|(key, values)| (key, TokenUsageSummary::from_observations(values).ok()))
        .collect();
    families
}

fn is_terminal_inference_status(status: &str) -> bool {
    matches!(status.trim(), "failed" | "completed" | "cancelled")
}

fn clean_metric_label(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}
