use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::client_protocol::RequestLifecycleState;
use gents_protocol::row::{decode_behavior_readiness_snapshot, AgentBehaviorReadinessRow};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::post_graphql;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct FleetSnapshot {
    pub(crate) generated_at: String,
    pub(crate) agents: Vec<FleetAgent>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct FleetAgent {
    pub(crate) agent_did: String,
    pub(crate) process_state: String,
    pub(crate) active: i64,
    pub(crate) pending: i64,
    pub(crate) last_seen: String,
}

#[derive(Debug, Deserialize)]
struct FleetEnvelope {
    #[serde(rename = "AgentBehaviorReadiness", default)]
    readiness: Vec<AgentBehaviorReadinessRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<RequestRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct RequestRow {
    #[serde(default)]
    agent_did: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<RequestLifecycleState>,
}

pub(crate) async fn load_fleet_snapshot(graphql: &str) -> Result<FleetSnapshot> {
    let generated_at = Utc::now();
    let response = post_graphql(graphql, &fleet_query()).await?;
    let envelope = decode_fleet_response(response)?;
    Ok(build_fleet_snapshot(generated_at, envelope))
}

fn fleet_query() -> String {
    format!(
        r#"{{
        AgentBehaviorReadiness(order: {{ agent_did: ASC }}) {{
            agent_did
            snapshot_json
            updated_at
        }}
        AgentRequest(filter: {{ lifecycle_state: {{ _in: {} }} }}) {{
            agent_did
            lifecycle_state
        }}
    }}"#,
        RequestLifecycleState::active_runtime_graphql_list(),
    )
}

fn decode_fleet_response(response: Value) -> Result<FleetEnvelope> {
    let data = response
        .get("data")
        .filter(|data| data.is_object())
        .cloned()
        .with_context(|| format!("fleet query response missing object data: {response}"))?;
    serde_json::from_value(data).context("decoding fleet query response")
}

fn build_fleet_snapshot(generated_at: DateTime<Utc>, envelope: FleetEnvelope) -> FleetSnapshot {
    let mut counts = BTreeMap::<String, (i64, i64)>::new();
    for request in &envelope.requests {
        let agent_did = request
            .agent_did
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_string();
        if agent_did.is_empty() {
            continue;
        }
        let entry = counts.entry(agent_did).or_default();
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
            let agent_did = readiness.agent_did.trim().to_string();
            if agent_did.is_empty() {
                return None;
            }
            let (active, pending) = counts.get(&agent_did).copied().unwrap_or_default();
            let process_state = decode_behavior_readiness_snapshot(&readiness, &agent_did)
                .map(|snapshot| snapshot.process_state.as_str().to_string())
                .unwrap_or_else(|_| "unknown".to_string());
            Some(FleetAgent {
                process_state,
                last_seen: readiness.updated_at.trim().to_string(),
                agent_did,
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
    use gents_protocol::row::{
        BehaviorReadinessEntry, BehaviorReadinessProcessState, BehaviorReadinessSnapshot,
        BehaviorReadinessState, BEHAVIOR_READINESS_FORMAT_VERSION,
    };
    use serde_json::json;

    fn envelope(value: Value) -> FleetEnvelope {
        serde_json::from_value(value).unwrap()
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn readiness(
        agent_did: &str,
        process_state: BehaviorReadinessProcessState,
        updated_at: &str,
    ) -> Value {
        serde_json::to_value(AgentBehaviorReadinessRow {
            agent_did: agent_did.to_string(),
            snapshot_json: serde_json::to_string(&BehaviorReadinessSnapshot {
                format_version: BEHAVIOR_READINESS_FORMAT_VERSION,
                process_state,
                active_generation: 1,
                router_generation: 1,
                default_behavior_id: "default".to_string(),
                behaviors: vec![BehaviorReadinessEntry {
                    behavior_id: "default".to_string(),
                    state: BehaviorReadinessState::Ready,
                    reason: None,
                }],
            })
            .unwrap(),
            updated_at: updated_at.to_string(),
        })
        .unwrap()
    }

    #[test]
    fn reshapes_runtime_rows_with_per_agent_request_counts() {
        let snapshot = build_fleet_snapshot(
            at("2026-06-02T12:00:00Z"),
            envelope(json!({
                "AgentBehaviorReadiness": [
                    readiness("did:a", BehaviorReadinessProcessState::Ready, "2026-06-02T11:59:00Z"),
                    readiness("did:b", BehaviorReadinessProcessState::Recovering, "2026-06-02T11:58:00Z")
                ],
                "AgentRequest": [
                    { "agent_did": "did:a", "lifecycle_state": "processing" },
                    { "agent_did": "did:a", "lifecycle_state": "pending" },
                    { "agent_did": "did:a", "lifecycle_state": "pending" },
                    { "agent_did": "did:b", "lifecycle_state": "claimed" }
                ]
            })),
        );

        assert_eq!(snapshot.agents.len(), 2);

        let a = snapshot
            .agents
            .iter()
            .find(|x| x.agent_did == "did:a")
            .unwrap();
        assert_eq!(a.process_state, "ready");
        assert_eq!(a.active, 1);
        assert_eq!(a.pending, 2);
        assert_eq!(a.last_seen, "2026-06-02T11:59:00Z");

        let b = snapshot
            .agents
            .iter()
            .find(|x| x.agent_did == "did:b")
            .unwrap();
        assert_eq!(b.active, 1);
        assert_eq!(b.pending, 0);
    }

    #[test]
    fn agent_with_no_requests_reports_zero_counts() {
        let snapshot = build_fleet_snapshot(
            at("2026-06-02T12:00:00Z"),
            envelope(json!({
                "AgentBehaviorReadiness": [readiness("did:idle", BehaviorReadinessProcessState::Ready, "x")],
                "AgentRequest": []
            })),
        );
        assert_eq!(snapshot.agents.len(), 1);
        assert_eq!(snapshot.agents[0].active, 0);
        assert_eq!(snapshot.agents[0].pending, 0);
    }
}
