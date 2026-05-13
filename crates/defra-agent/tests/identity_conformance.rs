use std::collections::HashSet;

#[path = "../src/lean_vocab_test.rs"]
mod lean_vocab_test;

use lean_vocab_test::{
    lean_identity_structural_cases, LeanIdentityBehavior, LeanIdentityDeployment,
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
