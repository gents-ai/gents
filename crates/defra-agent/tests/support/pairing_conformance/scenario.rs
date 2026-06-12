//! Scenario IR for the pairing conformance harness.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "PascalCase")]
pub enum Action {
    OperatorWrite {
        node: NodeId,
        peer: NodeId,
        #[serde(default)]
        collections: Vec<String>,
        #[serde(default)]
        replicator_addresses: Vec<String>,
    },
    OperatorDelete {
        node: NodeId,
        peer: NodeId,
    },
    Reconcile {
        node: NodeId,
    },
    ReadFailure {
        node: NodeId,
    },
    PreseedActual {
        node: NodeId,
        #[serde(default)]
        collections: Vec<String>,
        #[serde(default)]
        replicator_addresses: Vec<String>,
        #[serde(default)]
        connected: bool,
    },
    PeerDisconnected {
        node: NodeId,
    },
    Drop {
        node: NodeId,
    },
    Crash {
        node: NodeId,
    },
    WaitForConvergence {
        timeout_secs: u64,
    },
}

pub type NodeId = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub actions: Vec<Action>,
}

impl Scenario {
    pub fn from_json_file(path: &std::path::Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)?;
        let scenario = serde_json::from_slice(&bytes)?;
        Ok(scenario)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_minimal_scenario() {
        let scenario = Scenario {
            name: "smoke".into(),
            actions: vec![
                Action::OperatorWrite {
                    node: "A".into(),
                    peer: "B".into(),
                    collections: vec!["c1".into()],
                    replicator_addresses: vec!["/ip4/127.0.0.1/tcp/4101/p2p/p1".into()],
                },
                Action::ReadFailure { node: "A".into() },
                Action::WaitForConvergence { timeout_secs: 10 },
            ],
        };
        let json = serde_json::to_string(&scenario).unwrap();
        let parsed: Scenario = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.actions.len(), 3);
    }
}
