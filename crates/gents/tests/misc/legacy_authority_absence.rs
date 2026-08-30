//! Clean-break fence: authenticated enrollment is the only remote route and
//! persona authority. These names may appear here only as deletion targets.

use std::path::PathBuf;

const REMOVED_COLLECTIONS: &[&str] = &[
    "NetworkMembership",
    "NetworkJoinRequest",
    "PairingBearerClaim",
    "BearerPairingReady",
    "ConsumedInviteNonce",
    "ReciprocalConversationIntent",
];

#[test]
fn removed_authority_collections_are_absent_from_schema_and_replication_inventories() {
    for removed in REMOVED_COLLECTIONS {
        assert!(
            !gents_protocol::schemas::ALL_COLLECTION_NAMES.contains(removed),
            "removed authority {removed} re-entered the protocol catalog"
        );
        assert!(
            !gents_migration::CLIENT_AUTHORED_COLLECTIONS.contains(removed),
            "removed authority {removed} re-entered the client replication inventory"
        );
    }

    let schema_root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../gents-schemas/schemas/agent");
    for file in [
        "network_membership.graphql",
        "network_join_request.graphql",
        "pairing_bearer_claim.graphql",
        "bearer_pairing_ready.graphql",
        "consumed_invite_nonce.graphql",
        "reciprocal_conversation_intent.graphql",
    ] {
        assert!(
            !schema_root.join(file).exists(),
            "removed schema {file} returned"
        );
    }
}

#[test]
fn hydration_repair_and_cli_sources_cannot_reference_removed_authorities() {
    let production = [
        include_str!("../../src/agent/p2p_reconcile/engine.rs"),
        include_str!("../../src/agent/p2p_reconcile/enrollment_reconcile.rs"),
        include_str!("../../src/agent/p2p_reconcile/session_hydration_reconcile.rs"),
        include_str!("../../../gents-cli/src/commands/p2p/mod.rs"),
        include_str!("../../../gents-cli/src/commands/p2p/network.rs"),
        include_str!("../../../gents-cli/src/commands/p2p/pairings.rs"),
        include_str!("../../../gents-desktop-core/src/client/core/route_manager.rs"),
        include_str!("../../../gents-desktop-core/src/client/core/supervisor.rs"),
    ]
    .join("\n");
    for removed in REMOVED_COLLECTIONS {
        assert!(
            !production.contains(removed),
            "removed authority {removed} returned to hydration, repair, or CLI production code"
        );
    }
}

#[test]
fn desktop_has_no_source_less_or_server_status_compatibility_authority() {
    let peer_directory = include_str!("../../../gents-desktop-core/src/client/peer_directory.rs");
    let bootstrap = include_str!("../../../gents-desktop-core/src/client/core/bootstrap.rs");
    let supervisor = include_str!("../../../gents-desktop-core/src/client/core/supervisor.rs");
    let route_manager =
        include_str!("../../../gents-desktop-core/src/client/core/route_manager.rs");

    for forbidden in [
        "server-status",
        "upsert_saved_peer_with_graphql",
        "upsert_saved_peer(",
    ] {
        assert!(
            !peer_directory.contains(forbidden),
            "desktop compatibility authority {forbidden:?} returned"
        );
    }
    assert!(!bootstrap.contains("record.source.is_none()"));
    assert!(!supervisor.contains("saved_record.source.is_none()"));
    assert!(!route_manager.contains("cleanup_legacy"));
    assert!(!route_manager.contains("delete_legacy_state"));
}

#[test]
fn operator_nonce_is_local_only_and_never_broad_synced() {
    const NAME: &str = "EnrollmentOperatorNonce";
    assert!(gents_protocol::schemas::ALL_COLLECTION_NAMES.contains(&NAME));
    assert!(gents_protocol::schemas::LOCAL_ONLY_COLLECTION_NAMES.contains(&NAME));
    assert!(!gents_protocol::schemas::BRANCHABLE_COLLECTION_NAMES.contains(&NAME));
    assert!(!gents_migration::CLIENT_AUTHORED_COLLECTIONS.contains(&NAME));
}

#[test]
fn production_pending_request_writers_use_the_signed_canonical_builder() {
    let writers = [
        include_str!("../../src/lifecycle/materialize.rs"),
        include_str!("../../src/lifecycle/queue/mutation.rs"),
        include_str!("../../src/lifecycle/queue/goal_continuation.rs"),
        include_str!("../../src/tool_call_lifecycle/subagent_request.rs"),
        include_str!("../../../gents-cli/src/request_helpers.rs"),
        include_str!("../../../gents-cli/src/commands/config/task_run.rs"),
        include_str!("../../../gents-desktop-core/src/client/mutations/chat/request.rs"),
    ];
    for source in writers {
        assert!(
            !source.contains("create_AgentRequest(input:"),
            "a production Pending writer bypassed AgentRequestCreate"
        );
        assert!(
            source.contains("AgentRequestCreate") || source.contains("write_pending_agent_request"),
            "a production Pending writer lost its canonical signed authoring seam"
        );
    }

    // Background redrive composes a transaction with another mutation, so it
    // embeds the canonical input fields rather than the standalone mutation.
    let background = include_str!("../../src/lifecycle/background_wake_recovery.rs");
    assert!(background.contains("create.graphql_input_fields()"));
    assert_eq!(background.matches("create_AgentRequest(input:").count(), 1);

    let materialize = include_str!("../../src/lifecycle/materialize.rs");
    let oneshot = include_str!("../../src/oneshot.rs");
    let signed_direct_materializer = materialize
        .split("pub async fn materialize_claimed_with_execution_binding")
        .nth(1)
        .and_then(|tail| tail.split("pub fn request(&self)").next())
        .expect("signed direct materializer source boundary");
    assert!(!signed_direct_materializer.contains("add_AgentRequest(input:"));
    assert!(!signed_direct_materializer.contains("status: \"processing\""));
    assert!(signed_direct_materializer.contains("Arc<dyn crate::identity::AgentIdentity>"));
    assert!(signed_direct_materializer.contains("AgentRequestCreate::base"));
    assert!(signed_direct_materializer.contains("verify_fresh_local_self_request"));
    assert!(signed_direct_materializer.contains("claim_with_identity"));
    assert!(!oneshot.contains("add_AgentRequest(input:"));
    assert!(oneshot.contains("behavior.principal_identity().clone()"));
}

#[test]
fn clean_break_has_no_legacy_pairing_or_remote_snapshot_authority() {
    let policy = include_str!("../../src/agent/p2p_reconcile/policy.rs");
    let engine = include_str!("../../src/agent/p2p_reconcile/engine.rs");
    let topology = include_str!("../../src/agent/p2p_reconcile/engine/remote_topology.rs");
    let observer = include_str!("../../../gents-desktop-core/src/client/observe.rs");
    let merges = include_str!("../../../gents-desktop-core/src/client/store/merges.rs");
    let writes = include_str!("../../../gents-desktop-core/src/client/core/writes.rs");

    assert!(!engine.contains("unwrap_or(super::policy::PairingDirection::ClientToRuntime)"));
    assert!(!topology.contains("teardown_unowned_replicators_at_endpoint"));
    assert!(!observer.contains("selected_is_legacy_remote"));
    assert!(!merges.contains("replace_remote_agent_scope"));
    assert!(!writes.contains("machines.next()?.clone()"));
    assert!(policy.contains("client pairing id has no recognized direction suffix"));
}

#[test]
fn request_and_persona_admission_have_no_logical_only_recovery_or_terminal_mutation() {
    let subagent_source = include_str!("../../src/trigger_engine/subagent_source.rs");
    let request_admission = include_str!("../../src/request_admission.rs");
    let persona = include_str!("../../src/agent/p2p_reconcile/persona_requests.rs");

    assert!(!subagent_source.contains("resolve_request_doc_id("));
    assert!(!subagent_source.contains("recovered legacy logical-only request binding"));
    assert!(!request_admission.contains("parent_authorizes_subagent_target"));
    assert!(request_admission.contains("load_exact_parent_request"));
    assert!(request_admission.contains("request_doc_id.as_deref() == Some(parent_doc_id)"));
    assert!(!persona.contains("filter: {{ request_key:"));
    assert!(persona.contains("filter: {{ _docID:"));
}

#[test]
fn runtime_internal_request_branches_cannot_collapse_back_to_ambiguous_parent_authority() {
    let protocol = include_str!("../../../gents-protocol/src/request_admission.rs");
    let authoring = include_str!("../../src/tool_call_lifecycle/subagent_request.rs");
    let verifier = include_str!("../../src/request_admission.rs");
    let request_schema = include_str!("../../../gents-schemas/schemas/agent/agent_request.graphql");
    let tool_schema = include_str!("../../../gents-schemas/schemas/agent/agent_tool_call.graphql");

    assert!(!protocol.contains("pub fn runtime_internal("));
    assert!(protocol.contains("pub fn runtime_local_child("));
    assert!(protocol.contains("pub fn runtime_cross_deployment_child("));
    assert!(!authoring.contains("require_parent_agent_match"));
    assert!(authoring.contains("SubagentAdmissionSource::LocalChild"));
    assert!(authoring.contains("SubagentAdmissionSource::CrossDeploymentChild"));

    let cross_verifier = verifier
        .split("async fn verify_cross_deployment_child_source")
        .nth(1)
        .and_then(|tail| {
            tail.split("async fn verify_target_cross_deployment_policy")
                .next()
        })
        .expect("cross-deployment verifier boundary");
    assert!(!cross_verifier.contains("load_exact_parent_request"));
    assert!(cross_verifier.contains("fresh_member_authorized_for_agent"));
    assert!(request_schema.contains("runtime_source_kind: String @index @immutable"));
    assert!(request_schema.contains("runtime_bridge_author_did: String @index @immutable"));
    assert!(tool_schema.contains("request_id: String @index @immutable"));
    assert!(tool_schema.contains("tool_call_id: String @index @immutable"));
    assert!(tool_schema.contains("child_request_id: String @index @immutable"));
}
