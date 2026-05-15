use std::collections::HashSet;
use std::sync::Arc;

#[path = "../src/lean_vocab_test.rs"]
mod lean_vocab_test;

use lean_vocab_test::{
    lean_identity_contracts, lean_identity_permission_cases, lean_identity_structural_cases,
    LeanIdentityBehavior, LeanIdentityContract, LeanIdentityDeployment, LeanIdentityPermissionCase,
    LeanIdentityStructuralCase,
};

use defra_agent::{AgentBehavior, AgentIdentity, AgentPrincipal};

#[path = "support/identity_stubs.rs"]
mod identity_stubs;
#[allow(unused_imports)]
use identity_stubs::StubAgentIdentity;

/// Build `Arc<AgentPrincipal>` instances (one per Lean principal row)
/// and `Arc<AgentBehavior>` instances with the matching principal
/// back-ref. The Lean rows may include multiple principals (e.g., the
/// `separate_principal_*` cases), so this helper legitimately produces
/// a multi-principal world. The single-principal-per-snapshot
/// invariant from the spec applies to the production loader (fenced
/// by the proptest in task 12), not to these test fixtures.
fn build_runtime_behaviors_from_lean_case(
    case: &LeanIdentityPermissionCase,
) -> Vec<Arc<AgentBehavior>> {
    use std::collections::HashMap;
    let principals: HashMap<String, Arc<AgentPrincipal>> = case
        .principals
        .iter()
        .map(|p| {
            let identity: Arc<dyn AgentIdentity> = StubAgentIdentity::arc(p.did.clone());
            let arc = Arc::new(AgentPrincipal {
                agent_did: p.did.clone(),
                identity,
                default_behavior_id: String::new(),
                display_name: None,
                enabled: p.enabled,
            });
            (p.did.clone(), arc)
        })
        .collect();
    case.behaviors
        .iter()
        .map(|b| {
            let principal = principals
                .get(b.principal.as_str())
                .unwrap_or_else(|| {
                    panic!(
                        "lean case {:?}: behavior {:?} references unknown principal {:?}",
                        case.name, b.id, b.principal
                    )
                })
                .clone();
            Arc::new(build_agent_behavior_for_routing_test(
                b.id.clone(),
                principal,
            ))
        })
        .collect()
}

/// Construct an `AgentBehavior` populated with default routing-test
/// values; only behavior_id and principal are load-bearing for the
/// routing tests.
fn build_agent_behavior_for_routing_test(
    behavior_id: String,
    principal: Arc<AgentPrincipal>,
) -> AgentBehavior {
    AgentBehavior {
        behavior_id,
        principal,
        backend_id: None,
        backend_provider_kind: defra_agent::BackendProviderKind::default(),
        backend_endpoint: String::new(),
        backend_api_key: None,
        backend_api_key_env_var: None,
        model_name: defra_agent::DEFAULT_MODEL_NAME.to_string(),
        context_window: defra_agent::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: defra_agent::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: defra_agent::DEFAULT_MAX_TURNS,
        system_prompt: String::new(),
        tools: defra_agent::BehaviorToolConfig::default(),
        compaction_threshold: defra_agent::DEFAULT_COMPACTION_THRESHOLD,
        compaction_strategy: defra_agent::CompactionStrategy::StripThenSummarize,
        stream_batch_ms: defra_agent::DEFAULT_STREAM_BATCH_MS,
        deadline_duration: std::time::Duration::from_secs(
            defra_agent::DEFAULT_DEADLINE_DURATION_SECS,
        ),
        sampling: defra_agent::SamplingConfig::default(),
    }
}

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
    deployment: &LeanIdentityDeployment,
    behavior: &LeanIdentityBehavior,
) -> bool {
    deployment.principal == behavior.principal
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
        // Drive runtime types from the Lean fixture. The assertions go
        // through AgentBehavior::principal.agent_did rather than the
        // local Rust mirror that this test used to maintain.
        let runtime_behaviors = build_runtime_behaviors_from_lean_case(case);
        let by_id: std::collections::HashMap<&str, &AgentBehavior> = runtime_behaviors
            .iter()
            .map(|b| (b.behavior_id.as_str(), b.as_ref()))
            .collect();

        let actor = by_id.get(case.actor_behavior.as_str()).unwrap_or_else(|| {
            panic!(
                "case {:?}: actor_behavior {:?} not constructed",
                case.name, case.actor_behavior
            )
        });
        let peer = by_id.get(case.peer_behavior.as_str()).unwrap_or_else(|| {
            panic!(
                "case {:?}: peer_behavior {:?} not constructed",
                case.name, case.peer_behavior
            )
        });

        assert_eq!(
            actor.principal.agent_did, case.expected_actor_principal,
            "case {:?}: actor behavior-id lookup drifted at runtime layer",
            case.name
        );
        assert_eq!(
            peer.principal.agent_did, case.expected_peer_principal,
            "case {:?}: peer behavior-id lookup drifted at runtime layer",
            case.name
        );
        assert_eq!(
            actor.principal.agent_did == peer.principal.agent_did,
            case.same_principal,
            "case {:?}: same-principal witness drifted at runtime layer",
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
