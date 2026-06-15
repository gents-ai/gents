//! Scenario IR for the pairing conformance harness.

use serde::{Deserialize, Serialize};

/// A single-field equality predicate carried by a scenario's `OperatorWrite`.
///
/// Mirrors the production `FilterPredicate`: the scope filter is part of the
/// replicator's identity, so changing it forces a teardown+install of the
/// affected replicator (Lean `PairingReconcile.filter_change_forces_reinstall`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioFilterPredicate {
    pub field: String,
    pub value: String,
}

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
        /// Optional per-collection scope filter applied to this pairing's
        /// replicators. Absent (the common case) leaves the pairing unfiltered;
        /// changing it on an already-converged pairing reinstalls the
        /// replicator. Backward-compatible: existing fixtures omit it.
        #[serde(default)]
        replicator_filter: std::collections::BTreeMap<String, ScenarioFilterPredicate>,
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
                    replicator_filter: Default::default(),
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
