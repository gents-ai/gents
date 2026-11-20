use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::Utc;
use gents_protocol::row::NodeReadinessRow;
use gents_store::config_client::{post_graphql, ConfigAccess, GraphqlEndpoint};
use gents_store::graphql::escape_graphql_string;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::http::liveness::{
    compute_request_liveness_summary, owns_liveness_row, with_active_native_executors,
    LivenessActivityRow, LivenessRequestRow, LivenessToolCallRow, RuntimeLivenessSnapshot,
};

const LIVENESS_ACTIVITY_CHUNK: usize = 32;
pub const LIVENESS_ACTIVITY_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);

#[derive(Debug, Serialize)]
pub struct MetricsQueryData {
    pub node_runtimes: Vec<MetricsRuntimeRow>,
    pub node_readiness: Vec<NodeReadinessRow>,
    pub inference_backends: Vec<MetricsBackendRow>,
    pub liveness: RuntimeLivenessSnapshot,
}

#[derive(Debug, Deserialize)]
pub(super) struct MetricsQueryEnvelope {
    #[serde(rename = "NodeRuntime", default)]
    node_runtimes: Vec<MetricsRuntimeRow>,
    #[serde(rename = "NodeReadiness", default)]
    node_readiness: Vec<NodeReadinessRow>,
    #[serde(rename = "InferenceBackendMetadata", default)]
    pub(super) inference_backends: Vec<MetricsBackendRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<LivenessRequestRow>,
    #[serde(rename = "AgentToolCall", default)]
    tool_calls: Vec<LivenessToolCallRow>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MetricsRuntimeRow {
    pub node_did: String,
    #[serde(default)]
    pub reconcile_phase: String,
    #[serde(default)]
    pub last_reconcile_result: String,
    #[serde(default)]
    pub last_reconcile_completed_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MetricsBackendRow {
    pub backend_id: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub max_concurrent: Option<i64>,
    #[serde(default)]
    pub max_queue_depth: Option<i64>,
    #[serde(default, deserialize_with = "null_string_as_unknown")]
    pub probe_status: String,
    pub last_probe: Option<String>,
}

pub(super) fn null_string_as_unknown<'de, D>(
    deserializer: D,
) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_else(|| "unknown".to_string()))
}

pub struct MetricsCoreData {
    envelope: MetricsQueryEnvelope,
    local_node_did: String,
}

pub async fn load_metrics_query_data(
    graphql: &GraphqlEndpoint,
    local_node_did: &str,
) -> Result<MetricsQueryData> {
    let core = load_metrics_core_data(graphql, local_node_did).await?;
    Ok(core
        .with_liveness_activity(
            graphql,
            tokio::time::Instant::now() + LIVENESS_ACTIVITY_BUDGET,
        )
        .await)
}

pub(super) async fn load_scrape_metrics_core_data(
    graphql: &GraphqlEndpoint,
    local_node_did: &str,
) -> Result<MetricsCoreData> {
    let response = post_graphql(
        graphql,
        r#"{
            NodeRuntime {
                node_did
                reconcile_phase
                last_reconcile_result
                last_reconcile_completed_at
            }
            NodeReadiness {
                node_did
                snapshot_json
                updated_at
            }
            InferenceBackendMetadata {
                _docID
                backend_id
                node_did
                enabled
                max_concurrent
                max_queue_depth
            }
            InferenceBackendObservation { node_did backend_id metadata_doc_id probe_status last_probe }
            AgentRequest(filter: {
                lifecycle_state: { _eq: "processing" }
            }) {
                _docID
                request_id
                node_did
                claimed_at
                deadline
                request_hop
                caused_by_parent_request_id
                caused_by_trigger_kind
            }
            AgentToolCall(filter: {
                lifecycle_state: { _eq: "running" }
            }) {
                request_id
                node_did
                tool_call_id
                tool_name
                started_at
                deadline_at
                await_mode
            }
        }"#,
    )
    .await?;
    let mut data = response
        .get("data")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    attach_scrape_backend_observations(&mut data);
    let envelope: MetricsQueryEnvelope =
        serde_json::from_value(data).context("decoding runtime HTTP query response")?;
    Ok(MetricsCoreData {
        envelope,
        local_node_did: local_node_did.to_string(),
    })
}

fn attach_scrape_backend_observations(data: &mut Value) {
    let observations = data
        .get("InferenceBackendObservation")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let Some(backends) = data
        .get_mut("InferenceBackendMetadata")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for backend in backends {
        let observation = match (
            backend["node_did"].as_str(),
            backend["backend_id"].as_str(),
            backend["_docID"].as_str(),
        ) {
            (Some(node), Some(id), Some(doc)) => {
                gents_documents::inference_backend::observation_binding::bind_rows(
                    node,
                    id,
                    doc,
                    &observations,
                )
                .ok()
                .flatten()
                .cloned()
            }
            _ => None,
        };
        if let Some(object) = backend.as_object_mut() {
            for field in ["probe_status", "last_probe"] {
                let value = observation
                    .as_ref()
                    .and_then(|observation| observation.get(field))
                    .cloned()
                    .unwrap_or(Value::Null);
                object.insert(field.to_owned(), value);
            }
        }
    }
}

pub async fn load_metrics_core_data(
    graphql: &GraphqlEndpoint,
    local_node_did: &str,
) -> Result<MetricsCoreData> {
    let access = ConfigAccess::Graphql(graphql.clone());
    let envelope = access
        .transact_readonly("views.http.metrics_core", |txn| {
            Box::pin(async move {
                let (runtimes, readiness, requests, tools) = txn
                    .read_views(&(
                        gents_store::views::runtime::NodeRuntimeList::new(local_node_did),
                        gents_store::views::runtime::NodeReadinessList::new(local_node_did),
                        gents_session::views::requests::RequestRows::new(
                            gents_session::views::transcript::Scope::Node(
                                local_node_did.to_owned(),
                            ),
                        )
                        .processing(),
                        gents_session::views::transcript::ToolCallRows::running(
                            gents_session::views::transcript::Scope::Node(
                                local_node_did.to_owned(),
                            ),
                        ),
                    ))
                    .await?;
                let backends = gents_inference::views::backends::read_inventory_in_txn(
                    txn,
                    &gents_inference::views::backends::BackendList::new(local_node_did),
                )
                .await?;
                Ok::<_, anyhow::Error>(MetricsQueryEnvelope {
                    node_runtimes: runtimes
                        .into_iter()
                        .map(|row| MetricsRuntimeRow {
                            node_did: row.node_did,
                            reconcile_phase: row.reconcile_phase.unwrap_or_default(),
                            last_reconcile_result: row.last_reconcile_result.unwrap_or_default(),
                            last_reconcile_completed_at: row
                                .last_reconcile_completed_at
                                .unwrap_or_default(),
                        })
                        .collect(),
                    node_readiness: readiness,
                    inference_backends: backends
                        .into_iter()
                        .map(|row| MetricsBackendRow {
                            backend_id: row.metadata.backend_id,
                            enabled: row.metadata.enabled,
                            max_concurrent: row.metadata.max_concurrent,
                            max_queue_depth: row.metadata.max_queue_depth,
                            probe_status: row
                                .observation
                                .as_ref()
                                .and_then(|row| row.probe_status.clone())
                                .unwrap_or_else(|| "unknown".to_owned()),
                            last_probe: row.observation.and_then(|row| row.last_probe),
                        })
                        .collect(),
                    requests,
                    tool_calls: tools
                        .into_iter()
                        .map(|row| LivenessToolCallRow {
                            request_id: row.request_id.unwrap_or_default(),
                            node_did: row.node_did.unwrap_or_default(),
                            tool_call_id: row.tool_call_id.unwrap_or_default(),
                            tool_name: row.tool_name.unwrap_or_default(),
                            started_at: row.started_at,
                            deadline_at: row.deadline_at,
                            await_mode: row.await_mode,
                        })
                        .collect(),
                })
            })
        })
        .await?;
    Ok(MetricsCoreData {
        envelope,
        local_node_did: local_node_did.to_owned(),
    })
}

#[derive(Clone, Copy)]
pub(super) enum ActivityReader {
    Client,
    Scrape,
}

impl MetricsCoreData {
    pub async fn with_liveness_activity(
        self,
        graphql: &GraphqlEndpoint,
        deadline: tokio::time::Instant,
    ) -> MetricsQueryData {
        self.with_activity_reader(graphql, deadline, ActivityReader::Client)
            .await
    }

    pub(super) async fn with_activity_reader(
        self,
        graphql: &GraphqlEndpoint,
        deadline: tokio::time::Instant,
        reader: ActivityReader,
    ) -> MetricsQueryData {
        let Self {
            envelope,
            local_node_did,
        } = self;
        let deadline = deadline.min(tokio::time::Instant::now() + LIVENESS_ACTIVITY_BUDGET);
        let activity = load_liveness_activity(
            graphql,
            &local_node_did,
            &envelope.requests,
            deadline,
            reader,
        )
        .await;
        let liveness = compute_request_liveness_summary(
            Utc::now(),
            &local_node_did,
            envelope.requests,
            envelope.tool_calls,
            activity,
        );
        MetricsQueryData {
            node_runtimes: envelope.node_runtimes,
            node_readiness: envelope.node_readiness,
            inference_backends: envelope.inference_backends,
            liveness,
        }
    }
}

async fn load_liveness_activity(
    graphql: &GraphqlEndpoint,
    local_node_did: &str,
    requests: &[LivenessRequestRow],
    deadline: tokio::time::Instant,
    reader: ActivityReader,
) -> Vec<LivenessActivityRow> {
    let request_doc_ids = requests
        .iter()
        .filter(|row| {
            owns_liveness_row(
                local_node_did.trim(),
                row.node_did.as_deref().unwrap_or_default(),
            )
        })
        .filter_map(|row| {
            let doc_id = row.doc_id.as_deref().map(str::trim)?;
            let node_did = row.node_did.as_deref().map(str::trim)?;
            (!doc_id.is_empty() && !node_did.is_empty()).then_some((doc_id, node_did))
        })
        .collect::<Vec<_>>();
    let mut activity = Vec::new();
    let mut chunks = request_doc_ids.chunks(LIVENESS_ACTIVITY_CHUNK);
    while let Some(chunk) = chunks.next() {
        let read = async {
            match reader {
                ActivityReader::Client => load_liveness_activity_chunk(graphql, chunk).await,
                ActivityReader::Scrape => load_scrape_liveness_activity_chunk(graphql, chunk).await,
            }
        };
        match tokio::time::timeout_at(deadline, read).await {
            Ok(Ok(rows)) => activity.extend(rows),
            Ok(Err(error)) => tracing::warn!(
                request_count = chunk.len(),
                error = format!("{error:#}"),
                "liveness activity read failed; progress falls back to claimed_at"
            ),
            Err(_) => {
                let request_count = chunk.len() + chunks.map(<[_]>::len).sum::<usize>();
                tracing::warn!(
                    request_count,
                    budget_ms = LIVENESS_ACTIVITY_BUDGET.as_millis() as u64,
                    "liveness activity budget exhausted; progress falls back to claimed_at"
                );
                break;
            }
        }
    }
    activity
}

async fn load_liveness_activity_chunk(
    graphql: &GraphqlEndpoint,
    requests: &[(&str, &str)],
) -> Result<Vec<LivenessActivityRow>> {
    let mut tools = Vec::with_capacity(requests.len() * 2);
    let mut calls = Vec::with_capacity(requests.len() * 2);
    for (request_doc_id, node_did) in requests {
        for completed in [false, true] {
            let mut view = gents_session::views::transcript::ToolCallRows::new(
                gents_session::views::transcript::Scope::Node((*node_did).to_owned()),
            );
            view.request_doc_ids = Some(vec![(*request_doc_id).to_owned()]);
            view.started_only = !completed;
            view.completed_only = completed;
            view.order = if completed {
                gents_session::views::transcript::ToolOrder::Completed
            } else {
                gents_session::views::transcript::ToolOrder::Started
            };
            view.direction = gents_protocol::document_query::Direction::Desc;
            view.limit = Some(1);
            tools.push(view);
        }
        for edge in [
            gents_inference::views::activity::ActivityEdge::Started,
            gents_inference::views::activity::ActivityEdge::Ended,
        ] {
            calls.push(
                gents_inference::views::activity::InferenceCalls::new(*node_did)
                    .latest_activity(*request_doc_id, edge),
            );
        }
    }
    let access = ConfigAccess::Graphql(graphql.clone());
    access
        .transact_readonly("views.http.liveness_activity", |txn| {
            let tools = &tools;
            let calls = &calls;
            Box::pin(async move {
                let tools = txn.read_views(tools).await?;
                let calls = txn.read_views(calls).await?;
                let mut activity = tools
                    .into_iter()
                    .flatten()
                    .map(|row| LivenessActivityRow {
                        request_doc_id: row.request_doc_id,
                        node_did: row.node_did,
                        started_at: row.started_at,
                        completed_at: row.completed_at,
                        ended_at: None,
                    })
                    .collect::<Vec<_>>();
                activity.extend(calls.into_iter().flatten().map(|row| LivenessActivityRow {
                    request_doc_id: row.request_doc_id,
                    node_did: Some(row.node_did),
                    started_at: row.started_at,
                    completed_at: None,
                    ended_at: row.ended_at,
                }));
                Ok::<_, anyhow::Error>(activity)
            })
        })
        .await
}

async fn load_scrape_liveness_activity_chunk(
    graphql: &GraphqlEndpoint,
    requests: &[(&str, &str)],
) -> Result<Vec<LivenessActivityRow>> {
    let response = post_graphql(graphql, &scrape_liveness_activity_query(requests)).await?;
    let data = response
        .get("data")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    let pages: BTreeMap<String, Vec<LivenessActivityRow>> =
        serde_json::from_value(data).context("decoding liveness activity response")?;
    Ok(pages.into_values().flatten().collect())
}

fn scrape_liveness_activity_query(requests: &[(&str, &str)]) -> String {
    let mut selections = String::new();
    for (index, (request_doc_id, node_did)) in requests.iter().enumerate() {
        let request_doc_id = escape_graphql_string(request_doc_id);
        let node_did = escape_graphql_string(node_did);
        for (alias, collection, field, fields) in [
            (
                "ts",
                "AgentToolCall",
                "started_at",
                "started_at completed_at",
            ),
            (
                "tc",
                "AgentToolCall",
                "completed_at",
                "started_at completed_at",
            ),
            ("is", "InferenceCall", "started_at", "started_at ended_at"),
            ("ie", "InferenceCall", "ended_at", "started_at ended_at"),
        ] {
            selections.push_str(&format!(
                r#"
            {alias}{index}: {collection}(
                filter: {{
                    request_doc_id: {{ _eq: "{request_doc_id}" }},
                    node_did: {{ _eq: "{node_did}" }},
                    {field}: {{ _ne: null }}
                }},
                order: {{ {field}: DESC }},
                limit: 1
            ) {{
                request_doc_id
                node_did
                {fields}
            }}"#
            ));
        }
    }
    format!("{{{selections}\n}}")
}

pub fn with_local_native_executors(mut data: MetricsQueryData) -> MetricsQueryData {
    data.liveness = with_active_native_executors(
        data.liveness,
        gents_session::exec::status::active_native_executors(),
    );
    data
}
