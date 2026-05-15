use std::collections::HashSet;

#[path = "../src/lean_vocab_test.rs"]
mod lean_vocab_test;

use lean_vocab_test::{
    lean_identity_contracts, lean_identity_permission_cases, lean_identity_structural_cases,
    LeanIdentityBehavior, LeanIdentityContract, LeanIdentityDeployment, LeanIdentityPermissionCase,
    LeanIdentityStructuralCase,
};

/// Rust mirror of `Identity.World.WellFormed` from
/// `Proofs/Identity/State.lean`. Returns true iff:
///   - principal DIDs are unique
///   - behavior ids are unique
///   - deployment ids are unique
///   - every behavior.principal references an existing principal
///   - every deployment.principal references an existing principal
fn rust_well_formed(case: &LeanIdentityStructuralCase) -> bool {
    let principal_dids: HashSet<&str> = case.principals.iter().map(|p| p.did.as_str()).collect();
    if principal_dids.len() != case.principals.len() {
        return false;
    }

    let behavior_ids: HashSet<&str> = case.behaviors.iter().map(|b| b.id.as_str()).collect();
    if behavior_ids.len() != case.behaviors.len() {
        return false;
    }

    let deployment_ids: HashSet<&str> = case.deployments.iter().map(|d| d.id.as_str()).collect();
    if deployment_ids.len() != case.deployments.len() {
        return false;
    }

    if case
        .behaviors
        .iter()
        .any(|b: &LeanIdentityBehavior| !principal_dids.contains(b.principal.as_str()))
    {
        return false;
    }

    if case
        .deployments
        .iter()
        .any(|d: &LeanIdentityDeployment| !principal_dids.contains(d.principal.as_str()))
    {
        return false;
    }

    true
}

fn behavior_for_id<'a>(
    case: &'a LeanIdentityPermissionCase,
    behavior_id: &str,
) -> &'a LeanIdentityBehavior {
    case.behaviors
        .iter()
        .find(|behavior| behavior.id == behavior_id)
        .unwrap_or_else(|| {
            panic!(
                "permission case {:?} references missing behavior {:?}",
                case.name, behavior_id
            )
        })
}

fn deployment_for_id<'a>(
    case: &'a LeanIdentityPermissionCase,
    deployment_id: &str,
) -> &'a LeanIdentityDeployment {
    case.deployments
        .iter()
        .find(|deployment| deployment.id == deployment_id)
        .unwrap_or_else(|| {
            panic!(
                "permission case {:?} references missing deployment {:?}",
                case.name, deployment_id
            )
        })
}

fn rust_canonical_permission_decision(case: &LeanIdentityPermissionCase, principal: &str) -> bool {
    case.grants
        .iter()
        .any(|grant| grant.principal == principal && grant.permission == case.permission)
}

fn rust_hostability_decision(
    case: &LeanIdentityPermissionCase,
    deployment: &LeanIdentityDeployment,
    behavior: &LeanIdentityBehavior,
) -> bool {
    let principal_dids: HashSet<&str> = case.principals.iter().map(|p| p.did.as_str()).collect();
    principal_dids.contains(deployment.principal.as_str())
        && principal_dids.contains(behavior.principal.as_str())
        && deployment.principal == behavior.principal
}

#[test]
fn identity_structural_cases_match_lean_verdicts() {
    let cases = lean_identity_structural_cases();
    assert!(
        !cases.is_empty(),
        "Lean must emit at least one identity structural case"
    );

    for case in cases {
        let rust_verdict = rust_well_formed(case);
        assert_eq!(
            rust_verdict, case.well_formed,
            "case {:?}: Rust WellFormed = {}, Lean WellFormed = {}",
            case.name, rust_verdict, case.well_formed
        );
    }
}

#[test]
fn identity_structural_cases_cover_named_scenarios() {
    let cases = lean_identity_structural_cases();
    let names: HashSet<&str> = cases.iter().map(|c| c.name.as_str()).collect();

    for expected in [
        "amy_general_and_amy_code_share_principal",
        "amy_rumination_separate_principal",
        "dangling_behavior_fk_violates",
        "duplicate_behavior_id_violates",
        "deployment_fk_violates",
        "two_deployments_different_principals_ok",
    ] {
        assert!(
            names.contains(expected),
            "missing expected identity conformance case: {expected}"
        );
    }
}

#[test]
fn identity_permission_cases_pin_runtime_permission_contract_shape() {
    let cases = lean_identity_permission_cases();
    assert_eq!(
        cases.len(),
        4,
        "Lean should emit the four executable identity permission rows that unblock #193"
    );

    let names: HashSet<&str> = cases.iter().map(|case| case.name.as_str()).collect();
    for expected in [
        "same_principal_row_owner_grant_allows_shared_behaviors",
        "separate_principal_without_grant_blocks_peer",
        "separate_principal_with_grant_allows_peer",
        "behavior_id_lookup_selects_declared_principal",
    ] {
        assert!(
            names.contains(expected),
            "missing expected identity permission case: {expected}"
        );
    }

    for case in cases {
        let principal_dids: HashSet<&str> =
            case.principals.iter().map(|p| p.did.as_str()).collect();
        assert!(
            principal_dids.contains(case.row_owner.as_str()),
            "case {:?}: row_owner must be a declared principal",
            case.name
        );
        assert!(
            case.permission.contains(case.row_owner.as_str()),
            "case {:?}: permission {:?} must identify the owned row principal {:?}",
            case.name,
            case.permission,
            case.row_owner
        );

        for grant in &case.grants {
            assert!(
                principal_dids.contains(grant.principal.as_str()),
                "case {:?}: grant principal {:?} must be declared",
                case.name,
                grant.principal
            );
        }

        let actor = behavior_for_id(case, &case.actor_behavior);
        let peer = behavior_for_id(case, &case.peer_behavior);
        assert_eq!(
            actor.principal, case.expected_actor_principal,
            "case {:?}: actor behavior-id lookup drifted",
            case.name
        );
        assert_eq!(
            peer.principal, case.expected_peer_principal,
            "case {:?}: peer behavior-id lookup drifted",
            case.name
        );

        let actor_allowed = rust_canonical_permission_decision(case, &actor.principal);
        let peer_allowed = rust_canonical_permission_decision(case, &peer.principal);
        assert_eq!(
            actor_allowed, case.expected_actor_allowed,
            "case {:?}: actor permission decision drifted",
            case.name
        );
        assert_eq!(
            peer_allowed, case.expected_peer_allowed,
            "case {:?}: peer permission decision drifted",
            case.name
        );
        assert_eq!(
            actor.principal == peer.principal,
            case.same_principal,
            "case {:?}: same-principal witness drifted",
            case.name
        );
        assert_eq!(
            actor_allowed == peer_allowed,
            case.expected_decisions_equal,
            "case {:?}: expected decision equality drifted",
            case.name
        );

        let host = deployment_for_id(case, &case.host_deployment);
        assert_eq!(
            rust_hostability_decision(case, host, actor),
            case.expected_actor_hostable,
            "case {:?}: actor hostability decision drifted",
            case.name
        );
        assert_eq!(
            rust_hostability_decision(case, host, peer),
            case.expected_peer_hostable,
            "case {:?}: peer hostability decision drifted",
            case.name
        );
    }
}

#[test]
fn identity_respects_principal_contract_is_declared() {
    let contracts = lean_identity_contracts();
    let target = contracts
        .iter()
        .find(|c: &&LeanIdentityContract| c.name == "identity.respects_principal_boundary")
        .expect(
            "Lean must emit the identity.respects_principal_boundary contract \
             — this is the spec the future runtime permission engine (#193) lands against",
        );

    // The contract is declared today and not yet enforced by a runtime
    // permission decision module. The finite rows consumed by
    // `identity_permission_cases_pin_runtime_permission_contract_shape`
    // are the #193 handoff: replace that Rust mirror with the runtime
    // decide function, then flip `enforced` to `true`.
    assert!(
        !target.enforced,
        "identity.respects_principal_boundary is marked enforced=true in Lean, \
         but the Rust runtime permission decision module is not yet wired up. \
         Either revert the Lean flag or extend this test to drive the runtime."
    );
    assert_eq!(
        target.tracked_by, "#193",
        "tracked_by must point at the runtime-refactor tracker so the deferred \
         enforcement has a discoverable owner"
    );
    assert!(
        target.statement.contains("agent_did"),
        "contract statement must mention agent_did so a reader unfamiliar with the \
         Lean model can grasp the boundary; statement was: {}",
        target.statement
    );
}
