use tauri::State;

use super::managed_server::co_hosted_runtime;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;
use crate::types::{JoinRequestDecideRequest, JoinRequestDecisionView, JoinRequestView};

/// Join requests are decided with the operator identity of the runtime this
/// desktop co-hosts; another node's requests are decided on that node.
const NOT_CO_HOSTED: &str =
    "Join requests can only be read and decided for the node this desktop runs.";
const NOT_PENDING: &str = "That request was already decided or has expired.";

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
        EnrollmentDecisionRejected, ManagedEnrollmentDecision, ManagedPendingEnrollment,
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
        assert!(serde_json::from_value::<JoinRequestDecision>(
            serde_json::json!({ "kind": "revoke" })
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
