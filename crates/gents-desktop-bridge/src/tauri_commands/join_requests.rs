use tauri::State;

use super::managed_server::co_hosted_runtime;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;
use crate::types::{
    JoinHistoryView, JoinMemberView, JoinRequestDecideRequest, JoinRequestDecisionView,
    JoinRequestView,
};

/// Join requests are decided with the operator identity of the runtime this
/// desktop co-hosts; another node's requests are decided on that node.
const NOT_CO_HOSTED: &str =
    "Join requests can only be read and decided for the node this desktop runs.";
const NOT_PENDING: &str = "That request was already decided or has expired.";
const RUNTIME_TOO_OLD: &str = "This node's runtime is too old to say who has access.";

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
    gents_server::server_host::decide_managed_enrollment(
        &home,
        &target.graphql,
        &request.request_id,
        request.decision.into(),
    )
    .await
    .map(JoinRequestDecisionView::from)
    .map_err(decision_error)
}

fn not_co_hosted() -> BridgeError {
    BridgeError::new(BridgeErrorCode::Unsupported, NOT_CO_HOSTED)
}

fn query_error(error: anyhow::Error) -> BridgeError {
    let too_old = gents_protocol::error_chain::find_source::<
        gents_server::server_host::EnrollmentQueryRejected,
    >(error.as_ref())
    .is_some_and(|rejected| rejected.query_is_unknown_to_runtime());
    if too_old {
        BridgeError::new(BridgeErrorCode::Unsupported, RUNTIME_TOO_OLD)
    } else {
        BridgeError::untyped(format!("{error:#}"))
    }
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
        EnrollmentDecisionRejected, EnrollmentQueryRejected, ManagedEnrollmentDecision,
        ManagedEnrollmentHistory, ManagedEnrollmentHistoryEntry, ManagedEnrollmentHistoryState,
        ManagedEnrollmentMember, ManagedPendingEnrollment,
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
            owner_agent: "did:key:z6MkOwner".into(),
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
                "ownerAgent": "did:key:z6MkOwner",
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
                }],
                "observedCount": 73,
                "limit": 50,
                "omitsSuperseded": true,
                "conflictsOmitted": false,
            })
        );
    }

    #[test]
    fn only_a_query_the_runtime_lacks_reads_as_unsupported() {
        let refused = |status, message: &str| {
            anyhow::Error::from(EnrollmentQueryRejected {
                status,
                message: message.into(),
            })
            .context("loading")
        };
        let older = query_error(refused(
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            "query: unknown variant `members`, expected `pending`",
        ));
        assert_eq!(older.code, BridgeErrorCode::Unsupported);
        assert_eq!(older.message, RUNTIME_TOO_OLD);
        for failed in [
            refused(reqwest::StatusCode::BAD_REQUEST, "nonce was used"),
            refused(
                reqwest::StatusCode::SERVICE_UNAVAILABLE,
                "enrollment authority is not ready",
            ),
            anyhow::anyhow!("error sending request: connection refused"),
        ] {
            let error = query_error(failed);
            assert_eq!(error.code, BridgeErrorCode::Unknown);
        }
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
