use gents_store::config_client::GraphqlEndpoint;
use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{DateTime, Utc};
use gents_protocol::client_protocol::RequestLifecycleState;
use gents_protocol::row::{decode_node_readiness_snapshot, AgentRequestRow, NodeReadinessRow};
use serde::{Deserialize, Serialize};

use gents_store::config_client::ConfigAccess;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetSnapshot {
    pub generated_at: String,
    pub agents: Vec<FleetAgent>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetAgent {
    pub node_did: String,
    pub process_state: String,
    pub active: i64,
    pub pending: i64,
    pub last_seen: String,
}

#[derive(Debug, Deserialize)]
struct FleetEnvelope {
    #[serde(rename = "NodeReadiness", default)]
    readiness: Vec<NodeReadinessRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<AgentRequestRow>,
}

pub async fn load_fleet_snapshot(graphql: &GraphqlEndpoint) -> Result<FleetSnapshot> {
    let generated_at = Utc::now();
    let access = ConfigAccess::Graphql(graphql.clone());
    let envelope = access
        .transact_readonly("views.http.fleet", |txn| {
            Box::pin(async move {
                let nodes = txn
                    .read_view(&gents_documents::views::config::VisibleNodes::new())
                    .await?;
                let mut envelope = FleetEnvelope {
                    readiness: Vec::new(),
                    requests: Vec::new(),
                };
                for node in nodes {
                    let node_did = node.document.node_did;
                    let (readiness, requests) = txn
                        .read_views(&(
                            gents_store::views::runtime::NodeReadinessList::new(&node_did),
                            gents_session::views::requests::RequestRows::new(
                                gents_session::views::transcript::Scope::Node(node_did.clone()),
                            )
                            .active_runtime(),
                        ))
                        .await?;
                    envelope.readiness.extend(readiness);
                    envelope.requests.extend(requests);
                }
                Ok::<_, anyhow::Error>(envelope)
            })
        })
        .await?;
    Ok(build_fleet_snapshot(generated_at, envelope))
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
        // A request parked for its backend, or resuming from it, still holds
        // its session's head: it is active work, not an idle node.
        match request.lifecycle_state {
            Some(state) if state.is_unsettled_execution() => entry.0 += 1,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every unsettled execution is active work: a node whose only request
    /// is parked for its backend does not report zero.
    #[test]
    fn parked_and_resuming_requests_count_as_active() {
        let request = |id: &str, node: &str, state: &str| json!({"request_id": id, "node_did": node, "lifecycle_state": state});
        let readiness = |node: &str| json!({"node_did": node, "snapshot_json": "{}", "updated_at": "2026-11-11T00:00:00Z"});
        let envelope = serde_json::from_value::<FleetEnvelope>(json!({
            "NodeReadiness": [readiness("did:key:parked"), readiness("did:key:busy")],
            "AgentRequest": [
                request("r1", "did:key:parked", "waitingForBackend"),
                request("r2", "did:key:busy", "claimed"),
                request("r3", "did:key:busy", "processing"),
                request("r4", "did:key:busy", "resumingBackend"),
                request("r5", "did:key:busy", "pending"),
            ],
        }))
        .unwrap();
        let snapshot = build_fleet_snapshot(Utc::now(), envelope);
        let counts: Vec<_> = snapshot
            .agents
            .iter()
            .map(|agent| (agent.node_did.as_str(), agent.active, agent.pending))
            .collect();
        assert_eq!(
            counts,
            [("did:key:parked", 1, 0), ("did:key:busy", 3, 1)],
            "{snapshot:?}"
        );
    }
}
