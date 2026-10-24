use gents::config_client::GraphqlEndpoint;
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use gents::config_client::ConfigClientError;
use gents_protocol::client_protocol::RequestLifecycleState;
use gents_protocol::row::{decode_node_readiness_snapshot, AgentRequestRow, NodeReadinessRow};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::post_graphql;

type Result<T, E = FleetError> = std::result::Result<T, E>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct FleetSnapshot {
    pub(crate) generated_at: String,
    pub(crate) agents: Vec<FleetAgent>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct FleetAgent {
    pub(crate) node_did: String,
    pub(crate) process_state: String,
    pub(crate) active: i64,
    pub(crate) pending: i64,
    pub(crate) last_seen: String,
}

#[derive(Debug, Deserialize)]
struct FleetEnvelope {
    #[serde(rename = "NodeReadiness", default)]
    readiness: Vec<NodeReadinessRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<AgentRequestRow>,
}

pub(crate) async fn load_fleet_snapshot(
    graphql: &GraphqlEndpoint,
) -> Result<FleetSnapshot, FleetError> {
    let generated_at = Utc::now();
    let response = post_graphql(graphql, &fleet_query()).await?;
    let envelope = decode_fleet_response(response)?;
    Ok(build_fleet_snapshot(generated_at, envelope))
}

fn fleet_query() -> String {
    format!(
        r#"{{
        NodeReadiness(order: {{ node_did: ASC }}) {{
            node_did
            snapshot_json
            updated_at
        }}
        AgentRequest(filter: {{ lifecycle_state: {{ _in: {} }} }}) {{
            request_id
            node_did
            lifecycle_state
        }}
    }}"#,
        RequestLifecycleState::active_runtime_graphql_list(),
    )
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum FleetError {
    #[error(transparent)]
    Graphql(#[from] ConfigClientError),
    #[error("fleet query response missing object data: {response}")]
    MissingData { response: Value },
    #[error("decoding fleet query response")]
    Decode(#[source] serde_json::Error),
}

fn decode_fleet_response(response: Value) -> Result<FleetEnvelope, FleetError> {
    let data = response
        .get("data")
        .filter(|data| data.is_object())
        .cloned()
        .ok_or_else(|| FleetError::MissingData {
            response: response.clone(),
        })?;
    serde_json::from_value(data).map_err(FleetError::Decode)
}

fn build_fleet_snapshot(generated_at: DateTime<Utc>, envelope: FleetEnvelope) -> FleetSnapshot {
    let mut counts = BTreeMap::<String, (i64, i64)>::new();
    for request in &envelope.requests {
        let node_did = request
            .node_did
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_string();
        if node_did.is_empty() {
            continue;
        }
        let entry = counts.entry(node_did).or_default();
        match request.lifecycle_state {
            Some(RequestLifecycleState::Claimed | RequestLifecycleState::Processing) => {
                entry.0 += 1
            }
            Some(RequestLifecycleState::Pending) => entry.1 += 1,
            _ => {}
        }
    }

    let agents = envelope
        .readiness
        .into_iter()
        .filter_map(|readiness| {
            let node_did = readiness.node_did.trim().to_string();
            if node_did.is_empty() {
                return None;
            }
            let (active, pending) = counts.get(&node_did).copied().unwrap_or_default();
            let process_state = decode_node_readiness_snapshot(&readiness, &node_did)
                .map(|snapshot| snapshot.process_state.as_str().to_string())
                .unwrap_or_else(|_| "unknown".to_string());
            Some(FleetAgent {
                process_state,
                last_seen: readiness.updated_at.trim().to_string(),
                node_did,
                active,
                pending,
            })
        })
        .collect();

    FleetSnapshot {
        generated_at: generated_at.to_rfc3339(),
        agents,
    }
}
