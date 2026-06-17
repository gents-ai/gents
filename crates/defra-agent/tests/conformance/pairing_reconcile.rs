//! Entry test for the pairing reconcile conformance harness.

use std::collections::BTreeSet;
use std::path::PathBuf;

use defra_agent::agent::p2p_reconcile::{
    merge_layered_desired, DiffOp, FilterPredicate, PairingDesired, PairingFilters,
};

use crate::support::pairing_conformance::invariants::{
    check_liveness, check_safety, ObservedSnapshot,
};
use crate::support::pairing_conformance::runner::Harness;
use crate::support::pairing_conformance::scenario::Scenario;

fn fixture_path(fixture: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pairing_scenarios")
        .join(fixture)
}

fn set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn one_filter(collection: &str, field: &str, value: &str) -> PairingFilters {
    let mut filters = PairingFilters::new();
    filters.insert(
        collection.to_string(),
        FilterPredicate {
            field: field.to_string(),
            value: value.to_string(),
        },
    );
    filters
}

#[tokio::test]
async fn pairing_reconcile_scenarios_satisfy_safety_and_liveness() {
    for fixture in [
        "install_teardown_happy_path.json",
        "replicator_install_teardown.json",
        "read_failure_noop.json",
        "unmanaged_survival.json",
        "delete_after_restart.json",
        "filter_change_reinstall.json",
    ] {
        let scenario_path = fixture_path(fixture);
        let scenario = Scenario::from_json_file(&scenario_path).expect("scenario parses");

        let mut harness = Harness::start_two_nodes().await.expect("harness starts");
        harness
            .run(&scenario)
            .await
            .unwrap_or_else(|error| panic!("{fixture} runs to convergence: {error:?}"));

        let history = harness.observation_history();
        check_safety(&history).unwrap_or_else(|error| panic!("{fixture} safety holds: {error:?}"));
        let final_snapshot: &ObservedSnapshot = history.last().expect("non-empty history");
        assert!(
            check_liveness(final_snapshot),
            "{fixture} convergence reached"
        );
    }
}

/// Filter-change-reinstall scenario coverage (Finding #12): a converged pairing
/// whose scope filter changes must tear down the old filtered replicator and
/// install the new one on the same address — mirroring Lean
/// `PairingReconcile.filter_change_forces_reinstall`. This asserts the *op
/// shape*, so it would fail if the reconciler mutated the replicator in place.
#[tokio::test]
async fn filter_change_reinstalls_replicator() {
    const ADDR: &str = "/ip4/127.0.0.1/tcp/4103/p2p/peer-b";

    let scenario = Scenario::from_json_file(&fixture_path("filter_change_reinstall.json"))
        .expect("scenario parses");

    let mut harness = Harness::start_two_nodes().await.expect("harness starts");
    harness
        .run(&scenario)
        .await
        .expect("filter_change_reinstall runs to convergence");

    let history = harness.observation_history();
    check_safety(&history).expect("filter_change_reinstall safety holds");
    assert!(
        check_liveness(history.last().expect("non-empty history")),
        "filter_change_reinstall convergence reached"
    );

    let ops = harness.emitted_ops();
    // Initial install, then a teardown+install pair for the filter change.
    let teardown = ops
        .iter()
        .position(|op| matches!(op, DiffOp::TeardownReplicator(a) if a == ADDR))
        .expect("filter change must tear down the old filtered replicator");
    assert!(
        matches!(&ops[teardown + 1], DiffOp::InstallReplicator(a) if a == ADDR),
        "teardown must be immediately followed by reinstall on the same address, got {:?}",
        &ops[teardown..]
    );
    let installs = ops
        .iter()
        .filter(|op| matches!(op, DiffOp::InstallReplicator(a) if a == ADDR))
        .count();
    assert_eq!(
        installs, 2,
        "expected exactly two installs (initial + reinstall), got ops {ops:?}"
    );
    // Final reconcile is a no-op: no churn after reconvergence.
    assert!(
        !matches!(ops.last(), Some(DiffOp::TeardownReplicator(_))),
        "reconverged pairing must not keep tearing down, got {ops:?}"
    );
}

/// Conformance fence for cut 5's Layer-1/Layer-2 merge against the
/// PairingReconcile + ScopeTemplates Lean surfaces:
///
/// - unfiltered network-control collections remain subscriptions;
/// - filtered conversation collections extend only the replicator collection
///   set, never the subscription set;
/// - the resulting replicator identity carries a per-collection filter map.
#[test]
fn layered_desired_merge_keeps_data_plane_replicator_only() {
    let address = "/ip4/127.0.0.1/tcp/4103/p2p/peer-a";
    let control = PairingDesired {
        collections: set(&["AgentNetwork", "NetworkMembership", "PeerEndpoint"]),
        replicator_addresses: set(&[address]),
        replicator_collections: set(&["AgentNetwork", "NetworkMembership", "PeerEndpoint"]),
        replicator_filter: PairingFilters::new(),
    };
    let data_plane = PairingDesired {
        collections: set(&["AgentRequest", "AgentResponse"]),
        replicator_addresses: set(&[address]),
        replicator_collections: set(&["AgentRequest", "AgentResponse"]),
        replicator_filter: one_filter("AgentRequest", "agent_did", "did:key:a")
            .into_iter()
            .chain(one_filter("AgentResponse", "agent_did", "did:key:a"))
            .collect(),
    };

    let merged =
        merge_layered_desired(Some(control), Some(data_plane)).expect("merged desired state");

    assert_eq!(
        merged.collections,
        set(&["AgentNetwork", "NetworkMembership", "PeerEndpoint"]),
        "data-plane collections must not become unfiltered subscriptions"
    );
    assert_eq!(
        merged.replicator_collections,
        set(&[
            "AgentNetwork",
            "NetworkMembership",
            "PeerEndpoint",
            "AgentRequest",
            "AgentResponse",
        ])
    );
    assert_eq!(merged.replicator_addresses, set(&[address]));
    assert_eq!(
        merged
            .replicator_filter
            .get("AgentRequest")
            .map(|filter| (filter.field.as_str(), filter.value.as_str())),
        Some(("agent_did", "did:key:a"))
    );
    assert_eq!(
        merged
            .replicator_filter
            .get("AgentResponse")
            .map(|filter| (filter.field.as_str(), filter.value.as_str())),
        Some(("agent_did", "did:key:a"))
    );
    assert!(
        !merged.replicator_filter.contains_key("AgentNetwork"),
        "network-control collections stay unfiltered inside the mixed replicator"
    );
}

/// A denied materializability gate is represented at the merge boundary by
/// absence of Layer-2 desired state. This keeps the control-plane mesh intact
/// while withholding the data-plane push collections.
#[test]
fn layered_desired_merge_absent_data_plane_preserves_control_only() {
    let control = PairingDesired {
        collections: set(&["AgentNetwork", "NetworkMembership"]),
        replicator_addresses: set(&["/ip4/127.0.0.1/tcp/4103/p2p/peer-a"]),
        replicator_collections: set(&["AgentNetwork", "NetworkMembership"]),
        replicator_filter: PairingFilters::new(),
    };

    let merged = merge_layered_desired(Some(control.clone()), None).expect("control desired state");

    assert_eq!(merged, control);
}
