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

async fn runtime_owned_witnesses(
    txn: &ConfigApplyTxn<'_>,
    collection: &str,
    rows: &[serde_json::Value],
    runtime: &identity::Did,
) -> Result<std::collections::BTreeSet<String>> {
    let ids = rows
        .iter()
        .filter_map(|row| row["_docID"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let owners = if ids.is_empty() {
        Vec::new()
    } else {
        txn.document_owners(collection, &ids).await?
    };
    anyhow::ensure!(
        owners.len() == ids.len(),
        "route witness ownership is incomplete"
    );
    Ok(owners
        .into_iter()
        .filter(|owner| owner.visible && owner.owner.as_ref() == Some(runtime))
        .map(|owner| owner.doc_id)
        .collect())
}

pub(super) async fn current_admission(
    txn: &ConfigApplyTxn<'_>,
    store: &GraphqlEnrollmentStore,
    scope: AuthorScope<'_>,
    collection: &'static str,
    now: DateTime<Utc>,
) -> Result<Result<AgentRequestAdmissionRecord, String>> {
    let local = scope.requester_peer_id.is_none();
    let mut expected_admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::client(scope.requester_did);
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
        let node = escape_graphql_string(scope.node_did);
        let response = txn.execute(&format!(r#"{{
            PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer}" }}, node_did: {{_eq: "{node}"}}, actor_did: {{_eq: "{node}"}}, source: {{ _eq: "enrollment" }} }}) {{ _docID node_did }}
            PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{peer}" }}, node_did: {{_eq: "{node}"}} }}) {{ _docID replicator_filter }}
        }}"#)).await?;
        let desired = response["data"]["PeerPairingDesired"]
            .as_array()
            .context("missing desired routes")?;
        let applied = response["data"]["PeerPairingApplied"]
            .as_array()
            .context("missing applied routes")?;
        let native_owner = identity::Did::new(scope.node_did)?;
        let desired_owned =
            runtime_owned_witnesses(txn, "PeerPairingDesired", desired, &native_owner).await?;
        let applied_owned =
            runtime_owned_witnesses(txn, "PeerPairingApplied", applied, &native_owner).await?;
        let route = desired.iter().any(|row| {
            row["node_did"].as_str() == Some(scope.node_did)
                && row["_docID"]
                    .as_str()
                    .is_some_and(|id| desired_owned.contains(id))
        }) && applied.iter().any(|row| {
            row["_docID"]
                .as_str()
                .is_some_and(|id| applied_owned.contains(id))
                && row["replicator_filter"]
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

    #[tokio::test]
    async fn runtime_route_witnesses_match_native_owners_and_visibility() -> anyhow::Result<()> {
        use crate::store::config_client::{ConfigAccess, GraphqlEndpoint, NodePermission};
        use crate::NodeIdentity;
        use std::sync::Arc;
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("runtime");
        let runtime = crate::KeyIdentity::load_or_create(&path, None)?;
        let key = crate::store::identity::load_file_identity(&path)?.private_key_bytes();
        let user = crate::KeyIdentity::load_or_create(directory.path().join("user"), None)?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(runtime.did())
                .with_signing_key(defra_node::SigningKey::Ed25519(key))
                .with_node_acp_enabled()
                .with_http(defra_node::HttpConfig::with_listener(listener)?)
                .build()
                .await?,
        );
        node.add_dac_policy_with_counter_as(
            &defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned())),
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
            0,
        )
        .await?;
        for schema in [
            gents_protocol::schemas::NODE,
            gents_protocol::schemas::PEER_PAIRING_DESIRED,
            gents_protocol::schemas::PEER_PAIRING_APPLIED,
        ] {
            node.add_collection(schema).await?;
        }
        let actor = identity::Did::new(runtime.did())?;
        let access = ConfigAccess::local_as(node.clone(), actor.clone());
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
        ] {
            access
                .grant_node_permission(&identity::Did::new(user.did())?, permission)
                .await?;
        }
        let user_access = ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
                .with_client_signing_capability(user.client_signing_capability()?)?,
        );
        let parent = user_access
            .write(
                "test.witness.node",
                &format!(
                    r#"mutation {{ create_Node(input: {{node_did:"{}"}}) {{_docID}} }}"#,
                    escape_graphql_string(user.did())
                ),
            )
            .await?;
        let parent = gents_protocol::graphql::extract_mutation_doc_id(&parent, "Node")?;
        user_access
            .add_actor_relationship("Node", &parent, "runtime_reader", &actor)
            .await?;
        let mut ids = std::collections::BTreeMap::new();
        for (collection, owner, peer, link) in [
            ("PeerPairingDesired", "runtime", "desired-own", false),
            ("PeerPairingApplied", "runtime", "applied-own", false),
            ("PeerPairingDesired", "other", "desired-foreign", true),
            ("PeerPairingApplied", "other", "applied-foreign", true),
            ("PeerPairingDesired", "other", "desired-hidden", false),
        ] {
            let actor_access = if owner == "runtime" {
                &access
            } else {
                &user_access
            };
            let mutation = format!(
                r#"mutation {{ create_{collection}(input: {{node_did:"{}", peer_id:"{}", parent_linked:false}}) {{_docID}} }}"#,
                escape_graphql_string(runtime.did()),
                escape_graphql_string(peer)
            );
            let response = actor_access.write("test.witness.create", &mutation).await?;
            let doc = gents_protocol::graphql::extract_mutation_doc_id(&response, collection)?;
            if link {
                user_access
                    .add_resource_relationship(collection, &doc, "parent", "nodes", &parent)
                    .await?;
            }
            ids.insert(peer, doc);
        }
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().identity_route_witness_cases;
        for (name, desired, applied) in [
            ("valid", "desired-own", "applied-own"),
            ("forged-desired", "desired-foreign", "applied-own"),
            ("forged-applied", "desired-own", "applied-foreign"),
            ("hidden", "desired-hidden", "applied-own"),
        ] {
            let desired_id = &ids[desired];
            let applied_id = &ids[applied];
            let result = access.transact_readonly("test.witness.check", |txn| {
                let actor = &actor;
                Box::pin(async move {
                    let query = format!(r#"{{ PeerPairingDesired(filter: {{_docID: {{_eq:"{}"}}}}) {{_docID}} PeerPairingApplied(filter: {{_docID: {{_eq:"{}"}}}}) {{_docID}} }}"#,
                        escape_graphql_string(desired_id), escape_graphql_string(applied_id));
                    let response = txn.execute(&query).await?;
                    let desired = gents_protocol::graphql::graphql_rows_from_response(&response, "PeerPairingDesired");
                    let applied = gents_protocol::graphql::graphql_rows_from_response(&response, "PeerPairingApplied");
                    let desired = runtime_owned_witnesses(txn, "PeerPairingDesired", &desired, actor).await?;
                    let applied = runtime_owned_witnesses(txn, "PeerPairingApplied", &applied, actor).await?;
                    Ok::<_, anyhow::Error>(desired.contains(desired_id) && applied.contains(applied_id))
                })
            }).await?;
            let expected = cases.iter().find(|case| case["name"] == name).unwrap()["allowed"]
                .as_bool()
                .unwrap();
            assert_eq!(result, expected, "{name}");
        }
        node.shutdown().await?;
        Ok(())
    }

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
}
