//! Pure desired-vs-actual diff for pairing reconcile.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::templates::PairingFilters;

/// Operator-set desired pairing for one peer.
///
/// `replicator_filter` is the per-pairing scope filter resolved from the
/// pairing's scope template (empty == unfiltered). It is part of the
/// *replicator identity*: every replicator in this pairing carries this
/// per-collection filter map, so a changed map makes the `(address, filters)`
/// identity distinct and forces a teardown+install — mirroring the Lean
/// `PairingReconcile.ReplicatorId = (address, ReplicatorFilter)` and
/// `filter_change_forces_reinstall`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingDesired {
    pub collections: BTreeSet<String>,
    pub replicator_addresses: BTreeSet<String>,
    #[serde(default)]
    pub replicator_collections: BTreeSet<String>,
    #[serde(default)]
    pub replicator_filter: PairingFilters,
    #[serde(default)]
    pub template_ids: BTreeSet<String>,
}

impl PairingDesired {
    pub fn has_wiring(&self) -> bool {
        !self.collections.is_empty() || !self.replicator_addresses.is_empty()
    }

    pub fn effective_replicator_collections(&self) -> &BTreeSet<String> {
        if self.replicator_collections.is_empty() {
            &self.collections
        } else {
            &self.replicator_collections
        }
    }

    pub fn uses_subagent_template(&self) -> bool {
        self.template_ids
            .iter()
            .any(|template| template.starts_with("subagent-"))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingActual {
    pub collections: BTreeSet<String>,
    pub replicator_addresses: BTreeSet<String>,
    #[serde(default)]
    pub replicator_collections: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingApplied {
    pub collections: BTreeSet<String>,
    pub replicator_addresses: BTreeSet<String>,
    #[serde(default)]
    pub replicator_filter: PairingFilters,
}

impl PairingApplied {
    pub fn is_empty(&self) -> bool {
        self.collections.is_empty() && self.replicator_addresses.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOp {
    InstallCollection(String),
    TeardownCollection(String),
    InstallReplicator(String),
    TeardownReplicator(String),
}

pub fn compute_pairing_diff(desired: &PairingDesired, actual: &PairingActual) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    for c in desired.collections.difference(&actual.collections) {
        ops.push(DiffOp::InstallCollection(c.clone()));
    }
    for c in actual.collections.difference(&desired.collections) {
        ops.push(DiffOp::TeardownCollection(c.clone()));
    }
    for r in desired
        .replicator_addresses
        .difference(&actual.replicator_addresses)
    {
        ops.push(DiffOp::InstallReplicator(r.clone()));
    }
    for r in actual
        .replicator_addresses
        .difference(&desired.replicator_addresses)
    {
        ops.push(DiffOp::TeardownReplicator(r.clone()));
    }
    ops
}

pub fn compute_owned_pairing_diff(
    desired: &PairingDesired,
    actual: &PairingActual,
    applied: &PairingApplied,
) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    for c in desired.collections.difference(&actual.collections) {
        ops.push(DiffOp::InstallCollection(c.clone()));
    }
    for c in actual
        .collections
        .intersection(&applied.collections)
        .filter(|c| !desired.collections.contains(*c))
    {
        ops.push(DiffOp::TeardownCollection(c.clone()));
    }
    // Replicator identity is (address, filter, collections). A managed
    // replicator whose desired filter differs from the applied filter, or
    // whose remotely-observed collection set differs from the desired set, is
    // a *distinct* identity (Lean `filter_change_distinct_identity`,
    // `collections_change_distinct_identity`): tear down the old identity and
    // install the new one, even though the address is unchanged. The
    // collections comparison fences the layer-order race where a replicator
    // installed from the data-plane layer alone silently kept its narrow
    // collection set after the control-plane layer merged in.
    let filter_changed = desired.replicator_filter != applied.replicator_filter;
    let desired_replicator_collections = desired.effective_replicator_collections();
    for r in desired
        .replicator_addresses
        .difference(&actual.replicator_addresses)
    {
        ops.push(DiffOp::InstallReplicator(r.clone()));
    }
    for r in actual
        .replicator_addresses
        .intersection(&applied.replicator_addresses)
        .filter(|r| desired.replicator_addresses.contains(*r))
    {
        let collections_changed = actual
            .replicator_collections
            .get(r)
            .is_some_and(|carried| carried != desired_replicator_collections);
        if filter_changed || collections_changed {
            ops.push(DiffOp::TeardownReplicator(r.clone()));
            ops.push(DiffOp::InstallReplicator(r.clone()));
        }
    }
    for r in actual
        .replicator_addresses
        .intersection(&applied.replicator_addresses)
        .filter(|r| !desired.replicator_addresses.contains(*r))
    {
        ops.push(DiffOp::TeardownReplicator(r.clone()));
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(xs: &[&str]) -> BTreeSet<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn empty_inputs_yield_no_ops() {
        let ops = compute_pairing_diff(&PairingDesired::default(), &PairingActual::default());
        assert!(ops.is_empty());
    }

    #[test]
    fn missing_collection_yields_install() {
        let desired = PairingDesired {
            collections: s(&["c1"]),
            ..Default::default()
        };
        let actual = PairingActual::default();
        assert_eq!(
            compute_pairing_diff(&desired, &actual),
            vec![DiffOp::InstallCollection("c1".into())]
        );
    }

    #[test]
    fn extra_collection_yields_teardown() {
        let desired = PairingDesired::default();
        let actual = PairingActual {
            collections: s(&["c1"]),
            ..Default::default()
        };
        assert_eq!(
            compute_pairing_diff(&desired, &actual),
            vec![DiffOp::TeardownCollection("c1".into())]
        );
    }

    #[test]
    fn same_state_yields_no_ops() {
        let desired = PairingDesired {
            collections: s(&["c1"]),
            replicator_addresses: s(&["/ip4/1/p2p/p"]),
            ..Default::default()
        };
        let actual = PairingActual {
            collections: s(&["c1"]),
            replicator_addresses: s(&["/ip4/1/p2p/p"]),
            ..Default::default()
        };
        assert!(compute_pairing_diff(&desired, &actual).is_empty());
    }

    #[test]
    fn collections_diff_emits_before_replicators_diff() {
        let desired = PairingDesired {
            collections: s(&["c1"]),
            replicator_addresses: s(&["/ip4/1/p2p/p"]),
            ..Default::default()
        };
        let actual = PairingActual::default();
        let ops = compute_pairing_diff(&desired, &actual);
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], DiffOp::InstallCollection(_)));
        assert!(matches!(ops[1], DiffOp::InstallReplicator(_)));
    }

    #[test]
    fn owned_diff_does_not_teardown_unmanaged_actual() {
        let desired = PairingDesired::default();
        let actual = PairingActual {
            collections: s(&["manual"]),
            replicator_addresses: s(&["/ip4/manual/p2p/p"]),
            ..Default::default()
        };
        let applied = PairingApplied::default();
        assert!(compute_owned_pairing_diff(&desired, &actual, &applied).is_empty());
    }

    fn filter(field: &str, value: &str) -> PairingFilters {
        let mut f = PairingFilters::new();
        f.insert(
            "AgentRequest".to_string(),
            super::super::templates::FilterPredicate {
                field: field.to_string(),
                value: value.to_string(),
            },
        );
        f
    }

    /// Mirrors Lean `filter_change_forces_reinstall`: a replicator on the same
    /// address but a different filter is a distinct identity, so the diff tears
    /// down the old filtered replicator and installs the new one.
    #[test]
    fn changing_scoped_did_reinstalls_replicator() {
        let desired = PairingDesired {
            collections: BTreeSet::new(),
            replicator_addresses: s(&["addr1"]),
            replicator_filter: filter("agent_did", "did:key:bob"),
            ..Default::default()
        };
        let actual = PairingActual {
            collections: BTreeSet::new(),
            replicator_addresses: s(&["addr1"]),
            ..Default::default()
        };
        let applied = PairingApplied {
            collections: BTreeSet::new(),
            replicator_addresses: s(&["addr1"]),
            replicator_filter: filter("agent_did", "did:key:alice"),
            ..Default::default()
        };
        assert_eq!(
            compute_owned_pairing_diff(&desired, &actual, &applied),
            vec![
                DiffOp::TeardownReplicator("addr1".into()),
                DiffOp::InstallReplicator("addr1".into()),
            ]
        );
    }

    /// An unchanged filter on a converged pairing yields no replicator churn.
    #[test]
    fn unchanged_filter_does_not_reinstall_replicator() {
        let f = filter("agent_did", "did:key:bob");
        let desired = PairingDesired {
            collections: BTreeSet::new(),
            replicator_addresses: s(&["addr1"]),
            replicator_filter: f.clone(),
            ..Default::default()
        };
        let actual = PairingActual {
            collections: BTreeSet::new(),
            replicator_addresses: s(&["addr1"]),
            ..Default::default()
        };
        let applied = PairingApplied {
            collections: BTreeSet::new(),
            replicator_addresses: s(&["addr1"]),
            replicator_filter: f,
            ..Default::default()
        };
        assert!(compute_owned_pairing_diff(&desired, &actual, &applied).is_empty());
    }

    /// Mirrors Lean `collections_change_forces_reinstall`: a replicator on
    /// the same address with the same filter whose remotely-observed carried
    /// collection set differs from the desired effective set is a distinct
    /// identity, so the diff tears it down and reinstalls it. This is the
    /// demo layer-order race: the replicator was installed from the
    /// data-plane layer alone and kept its narrow set after the
    /// control-plane layer merged in.
    #[test]
    fn changed_replicator_collections_reinstall_replicator() {
        let desired = PairingDesired {
            collections: s(&["AgentNetwork"]),
            replicator_addresses: s(&["addr1"]),
            replicator_collections: s(&["AgentNetwork", "AgentRequest"]),
            ..Default::default()
        };
        let actual = PairingActual {
            collections: s(&["AgentNetwork"]),
            replicator_addresses: s(&["addr1"]),
            replicator_collections: BTreeMap::from([("addr1".to_string(), s(&["AgentRequest"]))]),
        };
        let applied = PairingApplied {
            collections: s(&["AgentNetwork"]),
            replicator_addresses: s(&["addr1"]),
            ..Default::default()
        };
        assert_eq!(
            compute_owned_pairing_diff(&desired, &actual, &applied),
            vec![
                DiffOp::TeardownReplicator("addr1".into()),
                DiffOp::InstallReplicator("addr1".into()),
            ]
        );
    }

    /// A carried set that matches the desired effective set yields no churn.
    #[test]
    fn matching_replicator_collections_do_not_reinstall() {
        let desired = PairingDesired {
            collections: s(&["AgentNetwork"]),
            replicator_addresses: s(&["addr1"]),
            ..Default::default()
        };
        let actual = PairingActual {
            collections: s(&["AgentNetwork"]),
            replicator_addresses: s(&["addr1"]),
            replicator_collections: BTreeMap::from([("addr1".to_string(), s(&["AgentNetwork"]))]),
        };
        let applied = PairingApplied {
            collections: s(&["AgentNetwork"]),
            replicator_addresses: s(&["addr1"]),
            ..Default::default()
        };
        assert!(compute_owned_pairing_diff(&desired, &actual, &applied).is_empty());
    }

    /// An unobservable carried set (no map entry for the address) must not
    /// churn: absence means "could not be observed", not "empty".
    #[test]
    fn unobservable_replicator_collections_do_not_reinstall() {
        let desired = PairingDesired {
            collections: s(&["AgentNetwork"]),
            replicator_addresses: s(&["addr1"]),
            ..Default::default()
        };
        let actual = PairingActual {
            collections: s(&["AgentNetwork"]),
            replicator_addresses: s(&["addr1"]),
            ..Default::default()
        };
        let applied = PairingApplied {
            collections: s(&["AgentNetwork"]),
            replicator_addresses: s(&["addr1"]),
            ..Default::default()
        };
        assert!(compute_owned_pairing_diff(&desired, &actual, &applied).is_empty());
    }

    #[test]
    fn owned_diff_tears_down_only_applied_extras() {
        let desired = PairingDesired::default();
        let actual = PairingActual {
            collections: s(&["manual", "managed"]),
            replicator_addresses: s(&["/ip4/manual/p2p/p", "/ip4/managed/p2p/p"]),
            ..Default::default()
        };
        let applied = PairingApplied {
            collections: s(&["managed"]),
            replicator_addresses: s(&["/ip4/managed/p2p/p"]),
            ..Default::default()
        };
        assert_eq!(
            compute_owned_pairing_diff(&desired, &actual, &applied),
            vec![
                DiffOp::TeardownCollection("managed".into()),
                DiffOp::TeardownReplicator("/ip4/managed/p2p/p".into())
            ]
        );
    }
}
