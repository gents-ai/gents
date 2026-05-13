use std::collections::HashSet;

#[path = "../src/lean_vocab_test.rs"]
mod lean_vocab_test;

use lean_vocab_test::{
    lean_identity_contracts, lean_identity_structural_cases, LeanIdentityBehavior,
    LeanIdentityContract, LeanIdentityDeployment, LeanIdentityStructuralCase,
};

/// Rust mirror of `Identity.World.WellFormed` from
/// `Proofs/Identity/State.lean`. Returns true iff:
///   - principal DIDs are unique
///   - behavior ids are unique
///   - deployment ids are unique
///   - every behavior.principal references an existing principal
///   - every deployment.principal references an existing principal
fn rust_well_formed(case: &LeanIdentityStructuralCase) -> bool {
    let principal_dids: HashSet<&str> =
        case.principals.iter().map(|p| p.did.as_str()).collect();
    if principal_dids.len() != case.principals.len() {
        return false;
    }

    let behavior_ids: HashSet<&str> =
        case.behaviors.iter().map(|b| b.id.as_str()).collect();
    if behavior_ids.len() != case.behaviors.len() {
        return false;
    }

    let deployment_ids: HashSet<&str> =
        case.deployments.iter().map(|d| d.id.as_str()).collect();
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
    // permission decision module. When that module lands (#193), flip
    // `enforced` to `true` in Proofs/Identity/Conformance.lean AND
    // replace the assertion below with a property-based test driving
    // the runtime decide function on the structural cases.
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
