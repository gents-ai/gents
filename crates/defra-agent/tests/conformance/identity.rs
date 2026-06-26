use std::collections::HashSet;
use std::sync::Arc;

use crate::lean_vocab_test::{
    lean_identity_contracts, lean_identity_permission_cases, lean_identity_structural_cases,
    LeanIdentityBehavior, LeanIdentityContract, LeanIdentityDeployment, LeanIdentityPermissionCase,
    LeanIdentityStructuralCase,
};
use acp::{
    AcpStore, DocumentACP, DocumentPermission, Identity, LocalDocumentACP, MemoryAcpStore,
    RelationTuple, READER_RELATION,
};

use defra_agent::{AgentBehavior, AgentIdentity, AgentPrincipal};
use identity::Did;

#[path = "../support/identity_stubs.rs"]
mod identity_stubs;
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
        skills: Vec::new(),
        behavior_id,
        principal,
        backend_id: None,
        backend_provider_kind: defra_agent::BackendProviderKind::default(),
        openai_wire_api: defra_agent::OpenAiWireApi::ChatCompletions,
        backend_endpoint: String::new(),
        backend_api_key: None,
        backend_api_key_env_var: None,
        model_name: defra_agent::DEFAULT_MODEL_NAME.to_string(),
        context_window: defra_agent::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: defra_agent::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: defra_agent::DEFAULT_MAX_TURNS,
        system_prompt: String::new(),
        request_context_template: None,
        tools: defra_agent::BehaviorToolConfig::default(),
        compaction_threshold: defra_agent::DEFAULT_COMPACTION_THRESHOLD,
        compaction_strategy: defra_agent::CompactionStrategy::StripThenSummarize,
        stream_batch_ms: defra_agent::DEFAULT_STREAM_BATCH_MS,
        stream_liveness_timeout: std::time::Duration::from_secs(
            defra_agent::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
        ),
        deadline_duration: std::time::Duration::from_secs(
            defra_agent::DEFAULT_DEADLINE_DURATION_SECS,
        ),
        sampling: defra_agent::SamplingConfig::default(),
    }
}

const IDENTITY_PERMISSION_POLICY_ID: &str = "identity-permission-cases";
const IDENTITY_PERMISSION_RESOURCE_NAME: &str = "row";

async fn build_local_acp_from_lean_case(
    case: &LeanIdentityPermissionCase,
) -> anyhow::Result<LocalDocumentACP> {
    assert!(
        case.permission.ends_with(".read"),
        "case {:?}: only .read permission fixtures are supported by this ACP witness, got {:?}",
        case.name,
        case.permission
    );
    assert!(
        case.permission
            .starts_with(format!("row:{}:", case.row_owner).as_str()),
        "case {:?}: permission {:?} must be scoped to row owner {:?}",
        case.name,
        case.permission,
        case.row_owner
    );

    let store = Arc::new(MemoryAcpStore::new());
    let acp = LocalDocumentACP::new(store.clone());
    let row_owner = did_from_lean_case(&case.row_owner, case, "row_owner");

    acp.register_doc_object(
        &row_owner,
        IDENTITY_PERMISSION_POLICY_ID,
        IDENTITY_PERMISSION_RESOURCE_NAME,
        &case.row_owner,
    )
    .await?;

    let namespaced_resource =
        format!("{IDENTITY_PERMISSION_POLICY_ID}:{IDENTITY_PERMISSION_RESOURCE_NAME}");
    for grant in &case.grants {
        assert_eq!(
            grant.permission, case.permission,
            "case {:?}: grant {:?} targets a different permission than the row under test",
            case.name, grant
        );
        let principal = did_from_lean_case(&grant.principal, case, "grant.principal");
        let tuple = RelationTuple::try_new(
            principal,
            READER_RELATION,
            namespaced_resource.as_str(),
            case.row_owner.as_str(),
        )?;
        store.put_tuple(&tuple).await?;
    }

    Ok(acp)
}

fn acp_actor_for(behavior: &AgentBehavior) -> Identity {
    Identity::Authenticated(
        Did::new(behavior.principal.agent_did.as_str()).unwrap_or_else(|error| {
            panic!(
                "behavior {:?}: principal DID {:?} is not a valid Defra identity DID: {error}",
                behavior.behavior_id, behavior.principal.agent_did
            )
        }),
    )
}

fn did_from_lean_case(value: &str, case: &LeanIdentityPermissionCase, field: &str) -> Did {
    Did::new(value).unwrap_or_else(|error| {
        panic!(
            "case {:?}: {field} {:?} is not a valid Defra identity DID: {error}",
            case.name, value
        )
    })
}

fn host_deployment_for_case<'a>(
    case: &'a LeanIdentityPermissionCase,
) -> &'a LeanIdentityDeployment {
    case.deployments
        .iter()
        .find(|deployment| deployment.id == case.host_deployment)
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: host_deployment {:?} not declared",
                case.name, case.host_deployment
            )
        })
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

#[tokio::test]
async fn identity_permission_cases_pin_runtime_permission_contract_shape() -> anyhow::Result<()> {
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
        // Fixture integrity: guard against a malformed Lean export. These
        // assertions verify the IdentityPermissionCase row is internally
        // consistent before the runtime-witness assertions run.
        let principal_dids: std::collections::HashSet<&str> =
            case.principals.iter().map(|p| p.did.as_str()).collect();
        assert!(
            principal_dids.contains(case.row_owner.as_str()),
            "case {:?}: row_owner {:?} must be a declared principal",
            case.name,
            case.row_owner
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

        let acp = build_local_acp_from_lean_case(case).await?;
        let actor_allowed = acp
            .check_doc_access(
                &acp_actor_for(actor),
                DocumentPermission::Read,
                IDENTITY_PERMISSION_POLICY_ID,
                IDENTITY_PERMISSION_RESOURCE_NAME,
                &case.row_owner,
            )
            .await?;
        let peer_allowed = acp
            .check_doc_access(
                &acp_actor_for(peer),
                DocumentPermission::Read,
                IDENTITY_PERMISSION_POLICY_ID,
                IDENTITY_PERMISSION_RESOURCE_NAME,
                &case.row_owner,
            )
            .await?;

        assert_eq!(
            actor_allowed, case.expected_actor_allowed,
            "case {:?}: ACP actor permission decision drifted from Lean witness",
            case.name
        );
        assert_eq!(
            peer_allowed, case.expected_peer_allowed,
            "case {:?}: ACP peer permission decision drifted from Lean witness",
            case.name
        );

        let host_deployment = host_deployment_for_case(case);
        assert_eq!(
            host_deployment.principal == actor.principal.agent_did,
            case.expected_actor_hostable,
            "case {:?}: actor hostability equality drifted from Lean witness",
            case.name
        );
        assert_eq!(
            host_deployment.principal == peer.principal.agent_did,
            case.expected_peer_hostable,
            "case {:?}: peer hostability equality drifted from Lean witness",
            case.name
        );
    }

    Ok(())
}

#[test]
fn identity_respects_principal_contract_enforced_by_runtime_routing() {
    let contracts = lean_identity_contracts();
    let target = contracts
        .iter()
        .find(|c: &&LeanIdentityContract| c.name == "identity.respects_principal_boundary")
        .expect(
            "Lean must emit the identity.respects_principal_boundary contract \
             — this is the runtime routing witness for #193",
        );

    // After #193 lands, the contract is enforced by the runtime: the
    // AgentBehavior::principal back-reference makes behavior -> principal
    // -> Identity::Authenticated(did) routing single-valued by
    // construction. DefraDB ACP, being DID-keyed, returns identical
    // results for behaviors sharing a principal.
    assert!(
        target.enforced,
        "identity.respects_principal_boundary must be enforced=true \
         now that AgentBehavior holds Arc<AgentPrincipal> as a back-ref \
         and the loader threads a single principal Arc through every \
         behavior in the snapshot"
    );
    assert_eq!(
        target.tracked_by, "#193",
        "tracked_by must continue to point at the runtime-refactor tracker"
    );
    assert!(
        target.statement.contains("agent_did"),
        "contract statement must name agent_did so a reader unfamiliar \
         with the Lean model can grasp the boundary; statement was: {}",
        target.statement
    );
    assert!(
        target.statement.contains("routing")
            || target.statement.contains("resolution")
            || target.statement.contains("Identity::Authenticated"),
        "contract statement must name the routing-witness interpretation: \
         the runtime resolves behavior -> agent_did and supplies that DID \
         as the ACP actor; statement was: {}",
        target.statement
    );

    // Exercise the runtime routing witness over every Lean row.
    for case in lean_identity_permission_cases() {
        let runtime_behaviors = build_runtime_behaviors_from_lean_case(case);
        let by_id: std::collections::HashMap<&str, &AgentBehavior> = runtime_behaviors
            .iter()
            .map(|b| (b.behavior_id.as_str(), b.as_ref()))
            .collect();

        let actor = by_id[case.actor_behavior.as_str()];
        let peer = by_id[case.peer_behavior.as_str()];

        // The structural claim: behaviors with the same Lean principal
        // resolve to the same agent_did at the runtime layer.
        assert_eq!(
            actor.principal.agent_did, case.expected_actor_principal,
            "case {:?}: actor.principal.agent_did mismatch",
            case.name
        );
        assert_eq!(
            peer.principal.agent_did, case.expected_peer_principal,
            "case {:?}: peer.principal.agent_did mismatch",
            case.name
        );
        assert_eq!(
            actor.principal.agent_did == peer.principal.agent_did,
            case.same_principal,
            "case {:?}: routing-witness same_principal mismatch",
            case.name
        );
    }
}
