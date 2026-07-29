//! Scenario IR for the pairing conformance harness.

use serde::Deserialize;

/// A single-field equality predicate carried by a scenario's `OperatorWrite`.
///
/// Mirrors the production `FilterPredicate`: the scope filter is part of the
/// replicator's identity, so changing it forces a teardown+install of the
/// affected replicator (Lean `PairingReconcile.filter_change_forces_reinstall`).
#[derive(Debug, Clone, Deserialize)]
pub struct ScenarioFilterPredicate {
    pub field: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
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
    PreseedActual {
        node: NodeId,
        #[serde(default)]
        collections: Vec<String>,
        #[serde(default)]
        replicator_addresses: Vec<String>,
        #[serde(default)]
        connected: bool,
    },
    Drop {
        node: NodeId,
    },
    Crash {
        node: NodeId,
    },
    WaitForConvergence,
}

pub type NodeId = String;

#[derive(Debug, Clone, Deserialize)]
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
