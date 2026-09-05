use std::collections::BTreeSet;
use std::path::PathBuf;

use gents::agent::p2p_reconcile::{
    compute_owned_pairing_diff, equality_filter, merge_layered_desired, single_string_eq, DiffOp,
    FilterPredicate, PairingActual, PairingApplied, PairingDesired, PairingFilters,
    MAX_CONCURRENT_PEER_PREPARATIONS,
};

use crate::lean_vocab_test::{
    lean_pairing_reconcile_shutdown_boundary_cases,
    lean_pairing_reconcile_sweep_retry_boundary_cases,
    lean_pairing_reconcile_sweep_scheduling_cases,
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
    filters.insert(collection.to_string(), equality_filter(field, value));
    filters
}

fn merge_desired(
    base: Option<PairingDesired>,
    data_plane: Option<PairingDesired>,
) -> Option<PairingDesired> {
    merge_layered_desired("did:key:local", "did:key:peer", base, data_plane)
}

#[tokio::test]
async fn pairing_reconcile_scenarios_satisfy_safety_and_liveness() {
    for fixture in [
        "install_teardown_happy_path.json",
        "replicator_install_teardown.json",
        "read_failure_noop.json",
        "unmanaged_survival.json",
        "delete_after_restart.json",
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
    assert!(
        !matches!(ops.last(), Some(DiffOp::TeardownReplicator(_))),
        "reconverged pairing must not keep tearing down, got {ops:?}"
    );
}

#[test]
fn operator_delete_owns_endpoint_despite_observed_configuration_drift() {
    let address = "/ip4/127.0.0.1/tcp/4103/p2p/peer-b";
    let desired = PairingDesired::default();
    let actual = PairingActual {
        replicator_addresses: set(&[address]),
        replicator_collections: [(address.to_string(), set(&["UnexpectedDriftedCollection"]))]
            .into_iter()
            .collect(),
        ..Default::default()
    };
    let applied = PairingApplied {
        replicator_addresses: set(&[address]),
        ..Default::default()
    };

    assert_eq!(
        compute_owned_pairing_diff(&desired, &actual, &applied),
        vec![DiffOp::TeardownReplicator(address.to_string())],
        "mutable live configuration must not hide an owned endpoint from delete"
    );
}

#[test]
fn layered_desired_merge_keeps_data_plane_replicator_only() {
    let bootstrap_address = "/ip4/127.0.0.1/tcp/4103/p2p/peer-a";
    let signed_address = "/ip4/127.0.0.1/tcp/5103/p2p/peer-a";
    let control = PairingDesired {
        collections: set(&["ControlA", "ControlB", "ControlC"]),
        replicator_addresses: set(&[bootstrap_address]),
        replicator_collections: set(&["ControlA", "ControlB", "ControlC"]),
        replicator_filter: PairingFilters::new(),
        template_ids: BTreeSet::new(),
    };
    let data_plane = PairingDesired {
        collections: set(&["AgentRequest", "AgentResponse"]),
        replicator_addresses: set(&[signed_address]),
        replicator_collections: set(&["AgentRequest", "AgentResponse"]),
        replicator_filter: one_filter("AgentRequest", "requester_did", "did:key:a")
            .into_iter()
            .chain(one_filter("AgentResponse", "requester_did", "did:key:a"))
            .collect(),
        template_ids: BTreeSet::new(),
    };

    let merged = merge_desired(Some(control), Some(data_plane)).expect("merged desired state");

    assert_eq!(
        merged.collections,
        set(&["ControlA", "ControlB", "ControlC"]),
        "data-plane collections must not become unfiltered subscriptions"
    );
    assert_eq!(
        merged.replicator_collections,
        set(&[
            "ControlA",
            "ControlB",
            "ControlC",
            "AgentRequest",
            "AgentResponse",
        ])
    );
    assert_eq!(merged.replicator_addresses, set(&[signed_address]));
    assert_eq!(
        merged
            .replicator_filter
            .get("AgentRequest")
            .and_then(single_string_eq),
        Some(("requester_did", "did:key:a"))
    );
    assert_eq!(
        merged
            .replicator_filter
            .get("AgentResponse")
            .and_then(single_string_eq),
        Some(("requester_did", "did:key:a"))
    );
    assert!(
        !merged.replicator_filter.contains_key("ControlA"),
        "explicit control collections stay unfiltered inside the mixed replicator"
    );
}

#[test]
fn layered_desired_merge_prefers_signed_data_plane_filter() {
    let base_filter = equality_filter("requester_did", "did:key:phone");
    let data_filter = FilterPredicate::predicate(
        serde_json::json!({ "lifecycle_state": { "_in": ["pending", "processing"] } })
            .as_object()
            .expect("object")
            .clone(),
    );
    let layer = |filter| PairingDesired {
        collections: BTreeSet::new(),
        replicator_addresses: set(&["addr"]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: [("AgentRequest".to_string(), filter)].into_iter().collect(),
        template_ids: BTreeSet::new(),
    };

    let merged = merge_desired(
        Some(layer(base_filter.clone())),
        Some(layer(data_filter.clone())),
    )
    .expect("merged desired state");

    assert_eq!(
        merged.replicator_filter.get("AgentRequest"),
        Some(&data_filter)
    );
}

#[test]
fn self_pairing_base_is_not_materialized() {
    assert!(merge_layered_desired(
        "did:key:self",
        "did:key:self",
        Some(PairingDesired::default()),
        None
    )
    .is_none());
}

#[test]
fn layered_desired_merge_absent_data_plane_preserves_control_only() {
    let control = PairingDesired {
        collections: set(&["ControlA", "ControlB"]),
        replicator_addresses: set(&["/ip4/127.0.0.1/tcp/4103/p2p/peer-a"]),
        replicator_collections: set(&["ControlA", "ControlB"]),
        replicator_filter: PairingFilters::new(),
        template_ids: BTreeSet::new(),
    };

    let merged = merge_desired(Some(control.clone()), None).expect("control desired state");

    assert_eq!(merged, control);
}

#[test]
fn merge_preserves_app_collections_subscription_only() {
    let app_layer = PairingDesired {
        collections: set(&["ChangeProposed"]),
        replicator_addresses: set(&["addr-b"]),
        replicator_collections: set(&["ChangeProposed"]),
        replicator_filter: Default::default(),
        template_ids: set(&["app-collections"]),
    };
    let merged = merge_desired(None, Some(app_layer)).expect("merged");
    assert!(
        merged.collections.contains("ChangeProposed"),
        "app-collections subscription must survive the merge: {merged:?}"
    );

    let nc_layer = PairingDesired {
        collections: set(&["AgentRequest"]),
        replicator_addresses: set(&["addr-b"]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: Default::default(),
        template_ids: set(&["explicit-control"]),
    };
    let merged_nc = merge_desired(None, Some(nc_layer)).expect("merged nc");
    assert!(
        merged_nc.collections.is_empty(),
        "non-app-collections data-plane subscription must be cleared: {merged_nc:?}"
    );
}

#[test]
fn app_collections_coexists_with_control_pairing() {
    let base = PairingDesired {
        collections: set(&["ControlA", "ControlB"]),
        replicator_addresses: set(&["addr-b"]),
        replicator_collections: set(&["ControlA", "ControlB"]),
        replicator_filter: Default::default(),
        template_ids: set(&["explicit-control"]),
    };
    let app_layer = PairingDesired {
        collections: set(&["ChangeProposed"]),
        replicator_addresses: set(&["addr-b"]),
        replicator_collections: set(&["ChangeProposed"]),
        replicator_filter: Default::default(),
        template_ids: set(&["app-collections"]),
    };
    let merged = merge_desired(Some(base), Some(app_layer)).expect("merged");
    assert!(merged.collections.contains("ControlA"));
    assert!(merged.collections.contains("ControlB"));
    assert!(merged.collections.contains("ChangeProposed"));
    assert!(merged.replicator_collections.contains("ControlA"));
    assert!(merged.replicator_collections.contains("ChangeProposed"));
    assert!(
        merged.replicator_filter.is_empty(),
        "both layers unscoped => no filter"
    );
    assert!(merged.template_ids.contains("explicit-control"));
    assert!(merged.template_ids.contains("app-collections"));
}

pub(super) fn pairing_reconcile_shutdown_boundary_preempts_in_flight_sweep() {
    let cases = lean_pairing_reconcile_shutdown_boundary_cases();
    assert_eq!(cases.len(), 1);
    let case = &cases[0];
    assert_eq!(
        case.name,
        "shutdown_preempts_in_flight_pairing_reconcile_sweep"
    );
    assert_eq!(case.supervisor, "pairingReconciler");
    assert_eq!(case.work_class, "p2pReconcileSweep");
    assert_eq!(case.boundary, "pairingReconcileSupervisorBoundary");
    assert_eq!(case.per_admin_call_timeout_ms, 10_000);
    assert!(case.cancellation_observed_inside_sweep);
    assert!(case.current_admin_future_dropped);
    assert!(case.remaining_peers_skipped);
    assert!(case.shutdown_join_bounded);
}

pub(super) fn pairing_reconcile_top_level_sweep_failure_is_nonterminal_and_retried() {
    let cases = lean_pairing_reconcile_sweep_retry_boundary_cases();
    assert_eq!(cases.len(), 1);
    let case = &cases[0];
    assert_eq!(
        case.name,
        "initial_top_level_sweep_failure_retries_without_terminating_reconciler"
    );
    assert_eq!(case.supervisor, "pairingReconciler");
    assert_eq!(case.work_class, "p2pReconcileSweep");
    assert_eq!(case.boundary, "pairingReconcileSupervisorBoundary");
    assert_eq!(case.failure_scope, "topLevelSweepEnumeration");
    assert!(!case.failure_terminal);
    assert_eq!(case.retry_trigger, "immediateFirstIntervalTick");
    assert!(case.cancellation_prioritized);
    assert!(case.convergence_retried);
}

pub(super) fn pairing_reconcile_sweep_does_not_head_of_line_block_ready_peer() {
    let cases = lean_pairing_reconcile_sweep_scheduling_cases();
    assert_eq!(cases.len(), 1);
    let case = &cases[0];
    assert_eq!(case.name, "stale_peer_dial_does_not_block_ready_peer");
    assert_eq!(case.supervisor, "pairingReconciler");
    assert_eq!(case.work_class, "p2pReconcileSweep");
    assert_eq!(case.boundary, "pairingReconcilePeerPreparationBoundary");
    assert_eq!(
        case.max_concurrent_peer_preparations,
        MAX_CONCURRENT_PEER_PREPARATIONS
    );
    assert!(case.peer_preparation_bounded);
    assert!(case.topology_mutation_serialized);
    assert!(!case.stale_peer_blocks_ready_peer);
    assert!(case.every_peer_result_accounted);
}
