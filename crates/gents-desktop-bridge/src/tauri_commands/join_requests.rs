use tauri::State;

use super::managed_server::co_hosted_runtime;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;
use crate::types::{
    JoinHistoryView, JoinInvitationView, JoinMemberView, JoinRequestDecideRequest,
    JoinRequestDecisionView, JoinRequestView,
};

/// Join requests are decided with the operator identity of the runtime this
/// desktop co-hosts; another node's requests are decided on that node.
const NOT_CO_HOSTED: &str =
    "Join requests can only be read and decided for the node this desktop runs.";
const NOT_PENDING: &str = "That request was already decided or has expired.";
/// The prefix the joining desktop strips before decoding an invitation.
const INVITATION_SCHEME: &str = "gents://invite/";

/// A fresh invitation to ask to join the node this desktop co-hosts. The
/// runtime mints and signs a new single-use offer on every `/status` read; its
/// validity window is the runtime's.
#[tauri::command]
pub async fn desktop_join_invitation_create(
    node_did: String,
    state: State<'_, DesktopAppState>,
) -> Result<JoinInvitationView, BridgeError> {
    create_join_invitation(&state, &node_did).await
}

#[tauri::command]
pub async fn desktop_join_requests_list(
    node_did: String,
    state: State<'_, DesktopAppState>,
) -> Result<Vec<JoinRequestView>, BridgeError> {
    list_join_requests(&state, &node_did).await
}

#[tauri::command]
pub async fn desktop_join_request_decide(
    request: JoinRequestDecideRequest,
    state: State<'_, DesktopAppState>,
) -> Result<JoinRequestDecisionView, BridgeError> {
    decide_join_request(&state, request).await
}

#[tauri::command]
pub async fn desktop_join_members_list(
    node_did: String,
    state: State<'_, DesktopAppState>,
) -> Result<Vec<JoinMemberView>, BridgeError> {
    list_join_members(&state, &node_did).await
}

#[tauri::command]
pub async fn desktop_join_history_read(
    node_did: String,
    state: State<'_, DesktopAppState>,
) -> Result<JoinHistoryView, BridgeError> {
    read_join_history(&state, &node_did).await
}

async fn list_join_requests(
    state: &DesktopAppState,
    node_did: &str,
) -> Result<Vec<JoinRequestView>, BridgeError> {
    let Some((home, target)) = co_hosted_runtime(state, node_did).await? else {
        return Err(not_co_hosted());
    };
    let pending = gents_server::server_host::pending_managed_enrollments(&home, &target.graphql)
        .await
        .map_err(|error| BridgeError::untyped(format!("{error:#}")))?;
    Ok(pending.into_iter().map(JoinRequestView::from).collect())
}

async fn list_join_members(
    state: &DesktopAppState,
    node_did: &str,
) -> Result<Vec<JoinMemberView>, BridgeError> {
    let Some((home, target)) = co_hosted_runtime(state, node_did).await? else {
        return Err(not_co_hosted());
    };
    let members = gents_server::server_host::managed_enrollment_members(&home, &target.graphql)
        .await
        .map_err(query_error)?;
    Ok(members.into_iter().map(JoinMemberView::from).collect())
}

async fn read_join_history(
    state: &DesktopAppState,
    node_did: &str,
) -> Result<JoinHistoryView, BridgeError> {
    let Some((home, target)) = co_hosted_runtime(state, node_did).await? else {
        return Err(not_co_hosted());
    };
    gents_server::server_host::managed_enrollment_history(&home, &target.graphql)
        .await
        .map(JoinHistoryView::from)
        .map_err(query_error)
}

async fn decide_join_request(
    state: &DesktopAppState,
    request: JoinRequestDecideRequest,
) -> Result<JoinRequestDecisionView, BridgeError> {
    let Some((home, target)) = co_hosted_runtime(state, &request.node_did).await? else {
        return Err(not_co_hosted());
    };
    let decided = match request.decision.into() {
        gents_server::server_host::ManagedEnrollmentDecision::Approve { lease_seconds } => {
            gents_server::server_host::admit_managed_enrollment(
                &home,
                &target.graphql,
                &request.request_id,
                lease_seconds,
            )
            .await
        }
        decision => {
            gents_server::server_host::decide_managed_enrollment(
                &home,
                &target.graphql,
                &request.request_id,
                decision,
            )
            .await
        }
    };
    decided
        .map(JoinRequestDecisionView::from)
        .map_err(decision_error)
}

async fn create_join_invitation(
    state: &DesktopAppState,
    node_did: &str,
) -> Result<JoinInvitationView, BridgeError> {
    let Some((_, target)) = co_hosted_runtime(state, node_did).await? else {
        return Err(not_co_hosted());
    };
    let status = gents_desktop_core::local_runtime::fetch_public_runtime_status(&target.graphql)
        .await
        .map_err(BridgeError::classify_transport_error)?;
    invitation_from_status(&status, &target.node_did)
}

/// The offer in a `/status` payload as an invitation link. The joining side
/// verifies the offer's signature; this only refuses another node's status so
/// a misrouted read is never handed out as this node's invitation.
fn invitation_from_status(
    status: &serde_json::Value,
    node_did: &str,
) -> Result<JoinInvitationView, BridgeError> {
    if status.get("node_did").and_then(serde_json::Value::as_str) != Some(node_did) {
        return Err(BridgeError::untyped(
            "The node answering here isn't the one this desktop runs.",
        ));
    }
    let enrollment = status.get("enrollment");
    let token = enrollment
        .and_then(|enrollment| enrollment.get("token"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty());
    let Some(token) = token else {
        return Err(
            match enrollment
                .and_then(|enrollment| enrollment.get("reason"))
                .and_then(serde_json::Value::as_str)
            {
                Some("runtime_not_ready") => BridgeError::new(
                    BridgeErrorCode::RuntimeStillBooting,
                    "The node is still starting; try again in a moment.",
                ),
                Some(reason) => BridgeError::untyped(format!(
                    "The node couldn't make an invitation ({reason})."
                )),
                None => BridgeError::new(
                    BridgeErrorCode::Unsupported,
                    "This node's runtime doesn't make invitations.",
                ),
            },
        );
    };
    let offer = gents_protocol::enrollment::decode_offer(token)
        .map_err(|error| BridgeError::untyped(format!("{error:#}")))?;
    Ok(JoinInvitationView {
        link: format!("{INVITATION_SCHEME}{token}"),
        expires_at: offer.expires_at,
    })
}

fn not_co_hosted() -> BridgeError {
    BridgeError::new(BridgeErrorCode::Unsupported, NOT_CO_HOSTED)
}

fn query_error(error: anyhow::Error) -> BridgeError {
    BridgeError::untyped(format!("{error:#}"))
}

fn decision_error(error: anyhow::Error) -> BridgeError {
    if crate::local_pairing::enrollment_request_is_not_visible_yet(&error) {
        BridgeError::new(BridgeErrorCode::NotFound, NOT_PENDING)
    } else {
        BridgeError::untyped(format!("{error:#}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::JoinRequestDecision;
    use gents_protocol::enrollment::{
        EnrollmentDecisionRefusalCode, MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
    };
    use gents_server::server_host::{
        EnrollmentDecisionRejected, ManagedEnrollmentDecision, ManagedEnrollmentHistory,
        ManagedEnrollmentHistoryEntry, ManagedEnrollmentHistoryState, ManagedEnrollmentMember,
        ManagedPendingEnrollment,
    };

    fn remote_only_state() -> (tempfile::TempDir, DesktopAppState) {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = crate::config::BridgeConfig {
            home: crate::config::HomePolicy::FixedRoot(temp.path().join("desktop")),
            bootstrap: crate::config::BootstrapPolicy::PairedRemoteOnly,
            store_key_custody: gents::store::local::store_key::StoreKeyCustodyChoice::File,
            ..Default::default()
        };
        let policy = crate::state::resolve_policy(&config, None).expect("policy");
        (temp, DesktopAppState::new(policy))
    }

    #[tokio::test]
    async fn a_node_this_desktop_does_not_run_is_unavailable() {
        let (_temp, state) = remote_only_state();
        let listed = list_join_requests(&state, "did:key:z6MkElsewhere")
            .await
            .expect_err("no co-hosted runtime");
        assert_eq!(listed.code, BridgeErrorCode::Unsupported);
        assert_eq!(listed.message, NOT_CO_HOSTED);
        let members = list_join_members(&state, "did:key:z6MkElsewhere")
            .await
            .expect_err("no co-hosted runtime");
        assert_eq!(members.code, BridgeErrorCode::Unsupported);
        let history = read_join_history(&state, "did:key:z6MkElsewhere")
            .await
            .expect_err("no co-hosted runtime");
        assert_eq!(history.code, BridgeErrorCode::Unsupported);
        let decided = decide_join_request(
            &state,
            JoinRequestDecideRequest {
                node_did: "did:key:z6MkElsewhere".into(),
                request_id: "request".into(),
                decision: JoinRequestDecision::Deny,
            },
        )
        .await
        .expect_err("no co-hosted runtime");
        assert_eq!(decided.code, BridgeErrorCode::Unsupported);
        let invitation = create_join_invitation(&state, "did:key:z6MkElsewhere")
            .await
            .expect_err("no co-hosted runtime");
        assert_eq!(invitation.code, BridgeErrorCode::Unsupported);
    }

    fn offer_token(expires_at: &str) -> String {
        gents_protocol::enrollment::encode_offer(
            &gents_protocol::enrollment::EnrollmentOfferRecord {
                version: gents_protocol::enrollment::ENROLLMENT_PROTOCOL_VERSION,
                offer_id: "offer".into(),
                challenge: "challenge".into(),
                network_id: "network".into(),
                admin_did: "did:key:z6MkNode".into(),
                server_peer: "peer".into(),
                server_ticket: "ticket".into(),
                owner_node_did: "did:key:z6MkNode".into(),
                profile: "client".into(),
                schema_fingerprint: "schema".into(),
                live_schema_digest: format!("sha256:{}", "a".repeat(64)),
                issued_at: "2030-01-01T00:00:00Z".into(),
                expires_at: expires_at.into(),
                admin_sig: vec![1; 64],
            },
        )
        .expect("token")
    }

    #[test]
    fn a_status_offer_becomes_an_invitation_link_with_its_expiry() {
        let token = offer_token("2030-01-01T00:05:00Z");
        let status = serde_json::json!({
            "node_did": "did:key:z6MkNode",
            "enrollment": { "state": "available", "token": token, "offer": {} },
        });
        assert_eq!(
            invitation_from_status(&status, "did:key:z6MkNode").expect("invitation"),
            JoinInvitationView {
                link: format!("gents://invite/{token}"),
                expires_at: "2030-01-01T00:05:00Z".into(),
            }
        );
    }

    #[test]
    fn no_invitation_from_another_node_or_without_an_offer() {
        let token = offer_token("2030-01-01T00:05:00Z");
        let elsewhere = serde_json::json!({
            "node_did": "did:key:z6MkElsewhere",
            "enrollment": { "state": "available", "token": token },
        });
        invitation_from_status(&elsewhere, "did:key:z6MkNode").expect_err("another node");
        let starting = serde_json::json!({
            "node_did": "did:key:z6MkNode",
            "enrollment": { "state": "unavailable", "reason": "runtime_not_ready" },
        });
        assert_eq!(
            invitation_from_status(&starting, "did:key:z6MkNode")
                .expect_err("not ready")
                .code,
            BridgeErrorCode::RuntimeStillBooting
        );
        let older = serde_json::json!({ "node_did": "did:key:z6MkNode" });
        assert_eq!(
            invitation_from_status(&older, "did:key:z6MkNode")
                .expect_err("no offer")
                .code,
            BridgeErrorCode::Unsupported
        );
    }

    #[test]
    fn decisions_translate_to_the_operator_api() {
        let approve: JoinRequestDecideRequest = serde_json::from_value(serde_json::json!({
            "nodeDid": "did:key:z6MkNode",
            "requestId": "request",
            "decision": {
                "kind": "approve",
                "leaseSeconds": MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
            },
        }))
        .expect("approve request");
        assert_eq!(
            ManagedEnrollmentDecision::from(approve.decision),
            ManagedEnrollmentDecision::Approve {
                lease_seconds: MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS
            }
        );
        let deny: JoinRequestDecision =
            serde_json::from_value(serde_json::json!({ "kind": "deny" })).expect("deny");
        assert_eq!(
            ManagedEnrollmentDecision::from(deny),
            ManagedEnrollmentDecision::Deny
        );
        let revoke: JoinRequestDecision =
            serde_json::from_value(serde_json::json!({ "kind": "revoke" })).expect("revoke");
        assert_eq!(
            ManagedEnrollmentDecision::from(revoke),
            ManagedEnrollmentDecision::Revoke
        );
        assert!(serde_json::from_value::<JoinRequestDecision>(
            serde_json::json!({ "kind": "renew" })
        )
        .is_err());
    }

    #[test]
    fn pending_requests_keep_their_fields_on_the_wire() {
        let view = JoinRequestView::from(ManagedPendingEnrollment {
            request_id: "request".into(),
            network_id: "network".into(),
            candidate_did: "did:key:z6MkCandidate".into(),
            candidate_peer: "peer".into(),
            owner_node_did: "did:key:z6MkOwner".into(),
            expires_at: "2026-11-05T00:00:00Z".into(),
            profile: Some("client".into()),
            issued_at: None,
        });
        assert_eq!(
            serde_json::to_value(view).expect("serialize"),
            serde_json::json!({
                "requestId": "request",
                "networkId": "network",
                "candidateDid": "did:key:z6MkCandidate",
                "candidatePeer": "peer",
                "ownerNodeDid": "did:key:z6MkOwner",
                "expiresAt": "2026-11-05T00:00:00Z",
                "profile": "client",
                "issuedAt": null,
            })
        );
    }

    #[test]
    fn members_keep_their_fields_on_the_wire() {
        let view = JoinMemberView::from(ManagedEnrollmentMember {
            request_id: "request".into(),
            network_id: "network".into(),
            candidate_did: "did:key:z6MkCandidate".into(),
            candidate_peer: "peer".into(),
            profile: "client".into(),
            authorization_expires_at: "2026-12-05T00:00:00Z".into(),
        });
        assert_eq!(
            serde_json::to_value(view).expect("serialize"),
            serde_json::json!({
                "requestId": "request",
                "networkId": "network",
                "candidateDid": "did:key:z6MkCandidate",
                "candidatePeer": "peer",
                "profile": "client",
                "authorizationExpiresAt": "2026-12-05T00:00:00Z",
            })
        );
    }

    #[test]
    fn history_keeps_its_fields_and_limits_on_the_wire() {
        let view = JoinHistoryView::from(ManagedEnrollmentHistory {
            history: vec![ManagedEnrollmentHistoryEntry {
                request_id: "request".into(),
                candidate_did: "did:key:z6MkCandidate".into(),
                candidate_peer: "peer".into(),
                profile: "client".into(),
                state: ManagedEnrollmentHistoryState::Revoked,
                decided_at: "2026-11-01T00:00:00Z".into(),
                decided_by: "did:key:z6MkOperator".into(),
                revoked_at: Some("2026-11-05T00:00:00Z".into()),
                revoked_by: Some("did:key:z6MkOperator".into()),
                expired_at: None,
            }],
            observed_count: 73,
            limit: 50,
            omits_superseded: true,
            conflicts_omitted: false,
        });
        assert_eq!(
            serde_json::to_value(view).expect("serialize"),
            serde_json::json!({
                "history": [{
                    "requestId": "request",
                    "candidateDid": "did:key:z6MkCandidate",
                    "candidatePeer": "peer",
                    "profile": "client",
                    "state": "revoked",
                    "decidedAt": "2026-11-01T00:00:00Z",
                    "decidedBy": "did:key:z6MkOperator",
                    "revokedAt": "2026-11-05T00:00:00Z",
                    "revokedBy": "did:key:z6MkOperator",
                    "expiredAt": null,
                }],
                "observedCount": 73,
                "limit": 50,
                "omitsSuperseded": true,
                "conflictsOmitted": false,
            })
        );
    }

    #[test]
    fn a_lapsed_approval_reads_as_ended_history() {
        let entry: ManagedEnrollmentHistoryEntry = serde_json::from_value(serde_json::json!({
            "request_id": "request", "candidate_did": "did:key:z6MkCandidate",
            "candidate_peer": "peer", "profile": "client", "state": "expired",
            "decided_at": "2026-11-01T00:00:00Z", "decided_by": "did:key:z6MkOperator",
            "revoked_at": null, "revoked_by": null, "expired_at": "2026-11-02T00:00:00Z",
        }))
        .expect("expired entry");
        let view = crate::types::JoinHistoryEntryView::from(entry);
        assert_eq!(view.state, crate::types::JoinHistoryState::Expired);
        assert_eq!(view.expired_at.as_deref(), Some("2026-11-02T00:00:00Z"));
        let older: ManagedEnrollmentHistoryEntry = serde_json::from_value(serde_json::json!({
            "request_id": "request", "candidate_did": "did:key:z6MkCandidate",
            "candidate_peer": "peer", "profile": "client", "state": "approved",
            "decided_at": "2026-11-01T00:00:00Z", "decided_by": "did:key:z6MkOperator",
            "revoked_at": null, "revoked_by": null,
        }))
        .expect("entry from a runtime before expiry was reported");
        assert_eq!(older.expired_at, None);
    }

    #[test]
    fn a_request_no_longer_pending_reads_as_not_found() {
        let refused = |code| {
            anyhow::Error::from(EnrollmentDecisionRejected {
                status: reqwest::StatusCode::CONFLICT,
                message: "request is not pending".into(),
                code,
            })
        };
        let gone = decision_error(refused(Some(EnrollmentDecisionRefusalCode::NotPending)));
        assert_eq!(gone.code, BridgeErrorCode::NotFound);
        assert_eq!(gone.message, NOT_PENDING);
        let conflicted = decision_error(refused(Some(EnrollmentDecisionRefusalCode::Conflicted)));
        assert_eq!(conflicted.code, BridgeErrorCode::Unknown);
        assert!(conflicted.message.contains("request is not pending"));
    }
}
