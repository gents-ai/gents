use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::request_admission::AgentRequestAdmissionRecord;

use super::enrollment_store::GraphqlEnrollmentStore;
use super::templates::{conjunctive_string_eq, decode_pairing_filters};
use crate::store::config_client::ConfigApplyTxn;
use crate::store::graphql::escape_graphql_string;

pub(super) struct AuthorScope<'a> {
    pub requester_did: &'a str,
    pub requester_peer_id: Option<&'a str>,
    pub node_did: &'a str,
}

/// A remote command is authorized only while the exact signed enrollment
/// generation it names is still inside its authorization lease.
///
/// The projection checks decision freshness at its load instant. The exact
/// fence carries the revision lease; do not infer its freshness or canonical
/// spelling from the decision. Recheck that exact revision lease at the
/// command observation time. Invalid, expired, or non-canonical expiries fail
/// closed.
pub(super) fn enrollment_authorization_is_current(expires_at: &str, now: DateTime<Utc>) -> bool {
    gents_protocol::enrollment::authorization_lease_is_fresh_at(expires_at, now)
}

/// The owner and lease checks a remote command's exact fence must pass before
/// it can become an expected admission. Kept pure so the fail-closed ordering
/// is testable without a live enrollment projection.
pub(super) fn remote_fence_rejection(
    fence: &super::enrollment_reconcile::EnrollmentAuthorizationFence,
    node_did: &str,
    now: DateTime<Utc>,
) -> Option<&'static str> {
    if fence.owner_node != node_did {
        return Some("enrollment targets another node");
    }
    if !enrollment_authorization_is_current(&fence.authorization_expires_at, now) {
        return Some("requester enrollment authorization has expired");
    }
    None
}

/// A command authored by the node itself, with no peer route, is admitted as
/// local self authority and never consults an enrollment fence.
pub(super) fn is_local_scope(
    requester_did: &str,
    requester_peer_id: Option<&str>,
    node_did: &str,
) -> bool {
    requester_peer_id.is_none() && requester_did == node_did
}

pub(super) async fn current_admission(
    txn: &ConfigApplyTxn<'_>,
    store: &GraphqlEnrollmentStore,
    scope: AuthorScope<'_>,
    collection: &'static str,
    now: DateTime<Utc>,
) -> Result<Result<AgentRequestAdmissionRecord, String>> {
    let local = is_local_scope(scope.requester_did, scope.requester_peer_id, scope.node_did);
    let mut expected_admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
            scope.requester_did,
        );
    if !local {
        let Some(peer) = scope.requester_peer_id else {
            return Ok(Err("remote command lacks requester peer identity".into()));
        };
        let projection = store.load_projection_in_txn(txn).await?;
        let Some(fence) = super::enrollment_reconcile::exact_authorization_fence(
            &projection,
            scope.requester_did,
            peer,
        )?
        else {
            return Ok(Err("requester enrollment is not current".into()));
        };
        if let Some(reason) = remote_fence_rejection(&fence, scope.node_did, now) {
            return Ok(Err(reason.into()));
        }
        expected_admission =
            gents_protocol::request_admission::AgentRequestAdmissionRecord::enrollment(
                scope.requester_did,
                fence.request_id,
                fence.request_digest,
                fence.admin_did,
                fence.authorization_sequence,
                fence.authorization_expires_at,
            );
        for collection in ["PeerPairingDesired", "PeerPairingApplied"] {
            crate::store::graphql::fence_arrivals(txn, collection).await?;
        }
        let peer = escape_graphql_string(peer);
        let response = txn.execute(&format!(r#"{{
            PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer}" }}, source: {{ _eq: "enrollment" }} }}) {{ node_did }}
            PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{peer}" }} }}) {{ replicator_filter }}
        }}"#)).await?;
        let desired = response["data"]["PeerPairingDesired"]
            .as_array()
            .context("missing desired routes")?;
        let applied = response["data"]["PeerPairingApplied"]
            .as_array()
            .context("missing applied routes")?;
        let route = desired
            .iter()
            .any(|row| row["node_did"].as_str() == Some(scope.node_did))
            && applied.iter().any(|row| {
                row["replicator_filter"]
                    .as_str()
                    .and_then(|raw| decode_pairing_filters(raw).ok())
                    .and_then(|filters| filters.get(collection).cloned())
                    .is_some_and(|filter| {
                        conjunctive_string_eq(&filter, "requester_did") == Some(scope.requester_did)
                            && conjunctive_string_eq(&filter, "node_did") == Some(scope.node_did)
                    })
            });
        if !route {
            return Ok(Err("requester command route is not applied".into()));
        }
    }
    Ok(Ok(expected_admission))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::agent::p2p_reconcile::enrollment_reconcile::EnrollmentAuthorizationFence;

    const NODE: &str = "did:key:z6Mknode";
    const MEMBER: &str = "did:key:z6Mkmember";

    fn fence(owner_node: &str, expires_at: &str) -> EnrollmentAuthorizationFence {
        EnrollmentAuthorizationFence {
            network_id: "network-1".into(),
            request_id: "enrollment-request-1".into(),
            admin_did: "did:key:z6Mkadmin".into(),
            member_did: MEMBER.into(),
            member_peer: "member-peer".into(),
            member_ticket: "member-ticket".into(),
            owner_node: owner_node.into(),
            profile: "client".into(),
            agent_ids: None,
            request_digest: "enrollment-digest-1".into(),
            authorization_sequence: 1,
            authorization_expires_at: expires_at.into(),
        }
    }

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn remote_fence_accepts_a_future_lease() {
        let current = fence(NODE, "2030-01-01T00:05:00Z");
        assert_eq!(
            remote_fence_rejection(&current, NODE, at("2030-01-01T00:04:59Z")),
            None
        );
    }

    #[test]
    fn remote_fence_rejects_the_exact_expiry_boundary() {
        let current = fence(NODE, "2030-01-01T00:05:00Z");
        assert_eq!(
            remote_fence_rejection(&current, NODE, at("2030-01-01T00:05:00Z")),
            Some("requester enrollment authorization has expired")
        );
    }

    #[test]
    fn remote_fence_rejects_a_past_lease() {
        let current = fence(NODE, "2030-01-01T00:05:00Z");
        assert_eq!(
            remote_fence_rejection(&current, NODE, at("2030-01-01T00:05:01Z")),
            Some("requester enrollment authorization has expired")
        );
    }

    #[test]
    fn remote_fence_fails_closed_on_a_malformed_lease() {
        for malformed in [
            "",
            "not-a-timestamp",
            "2030-01-01T00:05:00+00:00",
            "2030-01-01T00:05:00.500Z",
            "2030-01-01 00:05:00Z",
        ] {
            let current = fence(NODE, malformed);
            assert_eq!(
                remote_fence_rejection(&current, NODE, at("2030-01-01T00:04:59Z")),
                Some("requester enrollment authorization has expired"),
                "malformed lease {malformed:?} must fail closed"
            );
        }
    }

    #[test]
    fn remote_fence_still_rejects_another_owner_before_the_lease() {
        let elsewhere = fence("did:key:z6Mkother", "2030-01-01T00:05:00Z");
        assert_eq!(
            remote_fence_rejection(&elsewhere, NODE, at("2030-01-01T00:04:59Z")),
            Some("enrollment targets another node")
        );
        let expired_elsewhere = fence("did:key:z6Mkother", "2030-01-01T00:00:00Z");
        assert_eq!(
            remote_fence_rejection(&expired_elsewhere, NODE, at("2030-01-01T00:04:59Z")),
            Some("enrollment targets another node")
        );
    }

    #[test]
    fn local_self_scope_is_unchanged() {
        assert!(is_local_scope(NODE, None, NODE));
        assert!(!is_local_scope(MEMBER, None, NODE));
        assert!(!is_local_scope(NODE, Some("member-peer"), NODE));
        assert!(!is_local_scope(MEMBER, Some("member-peer"), NODE));
    }
}
