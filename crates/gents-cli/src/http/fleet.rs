//! `/fleet` surfacing: a per-`agent_did` reshape of data already loaded for
//! `/metrics` and `/fleet/slots` — each agent's `process_state`, active
//! (processing) and pending request counts, and `last_seen`
//! (`AgentRuntime.updated_at`). No new data; just a fleet-oriented projection.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
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
    #[serde(rename = "AgentRuntime", default)]
    runtimes: Vec<RuntimeRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<RequestRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct RuntimeRow {
    #[serde(default)]
    agent_did: String,
    #[serde(default)]
    process_state: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RequestRow {
    #[serde(default)]
    agent_did: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

pub(crate) async fn load_fleet_snapshot(graphql: &str) -> Result<FleetSnapshot> {
    let generated_at = Utc::now();
    let response = post_graphql(graphql, fleet_query()).await?;
    let envelope = decode_fleet_response(response)?;
    Ok(build_fleet_snapshot(generated_at, envelope))
}

fn fleet_query() -> &'static str {
    r#"{
        AgentRuntime(order: { agent_did: ASC }) {
            agent_did
            process_state
            updated_at
        }
        AgentRequest(filter: { status: { _in: ["pending", "processing"] } }) {
            agent_did
            status
        }
    }"#
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
    // (active, pending) per agent_did.
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
        match request.status.as_deref().map(str::trim) {
            Some("processing") => entry.0 += 1,
            Some("pending") => entry.1 += 1,
            _ => {}
        }
    }

    let agents = envelope
        .runtimes
        .into_iter()
        .filter_map(|runtime| {
            let agent_did = runtime.agent_did.trim().to_string();
            if agent_did.is_empty() {
                return None;
            }
            let (active, pending) = counts.get(&agent_did).copied().unwrap_or_default();
            Some(FleetAgent {
                process_state: runtime.process_state.unwrap_or_default().trim().to_string(),
                last_seen: runtime.updated_at.unwrap_or_default().trim().to_string(),
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
    use serde_json::json;

    fn envelope(value: Value) -> FleetEnvelope {
        serde_json::from_value(value).unwrap()
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn reshapes_runtime_rows_with_per_agent_request_counts() {
        let snapshot = build_fleet_snapshot(
            at("2026-06-02T12:00:00Z"),
            envelope(json!({
                "AgentRuntime": [
                    { "agent_did": "did:a", "process_state": "ready", "updated_at": "2026-06-02T11:59:00Z" },
                    { "agent_did": "did:b", "process_state": "recovering", "updated_at": "2026-06-02T11:58:00Z" }
                ],
                "AgentRequest": [
                    { "agent_did": "did:a", "status": "processing" },
                    { "agent_did": "did:a", "status": "pending" },
                    { "agent_did": "did:a", "status": "pending" },
                    { "agent_did": "did:b", "status": "processing" }
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
                "AgentRuntime": [{ "agent_did": "did:idle", "process_state": "ready", "updated_at": "x" }],
                "AgentRequest": []
            })),
        );
        assert_eq!(snapshot.agents.len(), 1);
        assert_eq!(snapshot.agents[0].active, 0);
        assert_eq!(snapshot.agents[0].pending, 0);
    }
}
