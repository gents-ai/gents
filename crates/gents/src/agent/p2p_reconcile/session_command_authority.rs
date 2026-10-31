use anyhow::{Context, Result};
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

pub(super) async fn current_admission(
    txn: &ConfigApplyTxn<'_>,
    store: &GraphqlEnrollmentStore,
    scope: AuthorScope<'_>,
    collection: &'static str,
) -> Result<Result<AgentRequestAdmissionRecord, String>> {
    let local = scope.requester_peer_id.is_none() && scope.requester_did == scope.node_did;
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
        if fence.owner_node != scope.node_did {
            return Ok(Err("enrollment targets another node".into()));
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
