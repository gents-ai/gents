use std::collections::BTreeSet;
use std::path::PathBuf;

use gents::agent::p2p_reconcile::{
    compute_owned_pairing_diff, equality_filter, merge_layered_desired, DiffOp, FilterPredicate,
    PairingActual, PairingApplied, PairingDesired, MAX_CONCURRENT_PEER_PREPARATIONS,
};

use crate::lean_vocab_test::{
    lean_pairing_reconcile_cases, lean_pairing_reconcile_shutdown_boundary_cases,
    lean_pairing_reconcile_sweep_retry_boundary_cases,
    lean_pairing_reconcile_sweep_scheduling_cases, LeanPairingReconcileSnapshot,
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

impl LeanPairingReconcileSnapshot {
    fn desired(&self) -> PairingDesired {
        PairingDesired {
            collections: self.desired_collections.iter().cloned().collect(),
            ..Default::default()
        }
    }

    fn actual(&self) -> PairingActual {
        PairingActual {
            collections: self.actual_collections.iter().cloned().collect(),
            ..Default::default()
        }
    }

    /// Resource ops the production projector would run from this state. The
    /// Fixtures export install-only resource observations; applied ownership
    /// affects teardown and is outside this projection.
    fn owned_ops(&self) -> Vec<DiffOp> {
        compute_owned_pairing_diff(&self.desired(), &self.actual(), &PairingApplied::default())
    }
}

#[test]
fn generated_pairing_reconcile_cases_drive_production_projector() {
    let cases = lean_pairing_reconcile_cases();
    assert!(
        !cases.is_empty(),
        "Lean must emit pairing reconcile samples"
    );

    let mut names = BTreeSet::new();
    for case in cases {
        assert!(
            names.insert(case.name.as_str()),
            "duplicate generated pairing case {:?}",
            case.name
        );
        assert!(
            case.before.desired_collections.len() >= 2,
            "{}: samples must be multi-resource, got {:?}",
            case.name,
            case.before.desired_collections
        );
        assert_eq!(
            case.before.desired_collections, case.after.desired_collections,
            "{}: sampled actions never rewrite desired state",
            case.name
        );
        // These samples validate the real resource projector. The connected
        // field is an observed transport input, not evidence that this test dialed.
        for state in [&case.before, &case.after] {
            if state.connected {
                assert_eq!(
                    state.owned_ops().is_empty(),
                    state.converged,
                    "{}: resource completion",
                    case.name
                );
            }
        }

        let before_ops = case.before.owned_ops();
        let after_ops = case.after.owned_ops();
        match case.action.as_str() {
            // Dialing only moves transport readiness: the projector's pending
            // resource work is identical on both sides of the transition.
            "dial" | "dialFailed" => {
                assert!(
                    !case.before.connected,
                    "{}: dial premises require a disconnected transport",
                    case.name
                );
                assert_eq!(
                    case.before.actual_collections, case.after.actual_collections,
                    "{}: dial must not change observed resources",
                    case.name
                );
                assert_eq!(
                    before_ops, after_ops,
                    "{}: dial must not change pending owned ops",
                    case.name
                );
                if case.action == "dial" {
                    assert!(
                        case.after.connected,
                        "{}: dial must establish transport readiness",
                        case.name
                    );
                } else {
                    assert!(
                        !case.after.connected,
                        "{}: dialFailed must leave the transport disconnected",
                        case.name
                    );
                }
            }
            "reconcileInstall" => {
                let installed: Vec<String> = case
                    .after
                    .actual_collections
                    .iter()
                    .filter(|collection| !case.before.actual_collections.contains(*collection))
                    .cloned()
                    .collect();
                assert_eq!(
                    installed.len(),
                    1,
                    "{}: one install must add exactly one collection, got {installed:?}",
                    case.name
                );
                let (first, rest) = before_ops.split_first().unwrap_or_else(|| {
                    panic!(
                        "{}: the projector must still have pending work before an install, got {before_ops:?}",
                        case.name
                    )
                });
                assert_eq!(
                    first,
                    &DiffOp::InstallCollection(installed[0].clone()),
                    "{}: the projector's first op must be the modeled install; ops {before_ops:?}",
                    case.name
                );
                assert_eq!(
                    rest, after_ops,
                    "{}: the remaining projector ops must survive the install; \
                     before {before_ops:?} after {after_ops:?}",
                    case.name
                );
            }
            other => panic!("unmapped Lean pairing reconcile action {other}"),
        }
    }

    // Readiness is not convergence: the samples must include a connected state
    // with resources still missing, and convergence only on the state where
    // every desired resource is installed.
    assert!(
        cases
            .iter()
            .any(|case| case.after.connected && !case.after.converged),
        "samples must include a connected-but-unconverged state (readiness != convergence)"
    );
    assert!(
        cases
            .iter()
            .any(|case| case.after.converged && case.after.owned_ops().is_empty()),
        "samples must include a state converged on every desired resource"
    );
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
