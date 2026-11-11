use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use defra_node::Node as EmbeddedNode;
use gents_protocol::enrollment::{
    derive_enrollment_id, EnrollmentOperatorDecisionCommand, EnrollmentOperatorQuery,
    EnrollmentOperatorQueryCommand,
};
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;

use super::EnrollmentProjection;

const OPERATOR_NONCE_RETENTION_SECONDS: i64 = 5 * 60;

/// Atomically consume one signed operator nonce before any protected read or
/// decision; the durable unique key survives restart and the stored digest
/// never retains the bearer value itself.
pub async fn consume_operator_nonce(
    node: &Arc<EmbeddedNode>,
    admin_did: &str,
    nonce: &str,
    now: chrono::DateTime<Utc>,
) -> Result<()> {
    let runtime = identity::Did::new(
        node.node_identity_did()
            .context("enrollment nonce requires the configured runtime identity")?,
    )?;
    let access = gents_store::config_client::ConfigAccess::local_as(node.clone(), runtime.clone());
    let nonce_key = derive_enrollment_id("gents-enrollment-operator-nonce-v1", &[admin_did, nonce]);
    let now = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let expires_at = (chrono::DateTime::parse_from_rfc3339(&now)?
        + Duration::seconds(OPERATOR_NONCE_RETENTION_SECONDS))
    .to_rfc3339_opts(SecondsFormat::Secs, true);
    let nonce_key_escaped = escape_graphql_string(&nonce_key);
    let admin_did = escape_graphql_string(admin_did);
    let now_escaped = escape_graphql_string(&now);
    let expires_at = escape_graphql_string(&expires_at);
    let mutation = format!(
        r#"mutation {{
            delete_EnrollmentOperatorNonce(filter: {{ expires_at: {{ _lte: "{now_escaped}" }} }}) {{ _docID }}
            create_EnrollmentOperatorNonce(input: {{
                nonce_key: "{nonce_key_escaped}", admin_did: "{admin_did}",
                expires_at: "{expires_at}", consumed_at: "{now_escaped}", parent_linked: false
            }}) {{ _docID }}
        }}"#
    );
    match access
        .transact_write("cli.enrollment.consume_operator_nonce", |txn| {
            let mutation = &mutation;
            let runtime = &runtime;
            Box::pin(async move {
                let response = txn.execute(mutation).await?;
                let doc = gents_protocol::graphql::extract_mutation_doc_id(
                    &response,
                    "EnrollmentOperatorNonce",
                )?;
                gents_store::config_client::maybe_link_deferred_operation_in_txn(
                    txn,
                    "EnrollmentOperatorNonce",
                    &doc,
                    runtime,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
    {
        Ok(_) => Ok(()),
        Err(write_error) => {
            let rendered = {
                use gents_protocol::document_query::{
                    fields::enrollment_operator_nonce as fields, Filter, Query, Selection,
                };
                Query::select(
                    Selection::new()
                        .field(fields::NONCE_KEY)
                        .field(fields::EXPIRES_AT),
                )
                .filter(Filter::eq(fields::NONCE_KEY, &nonce_key))
                .render()
            };
            let consumed = access.read(&rendered).await.is_ok_and(|response| {
                gents_protocol::graphql::graphql_rows_from_response(
                    &response,
                    "EnrollmentOperatorNonce",
                )
                .iter()
                .any(|row| row["nonce_key"].as_str() == Some(nonce_key.as_str()))
            });
            if consumed {
                anyhow::bail!("enrollment operator nonce was already consumed");
            }
            Err(write_error).context("persisting enrollment operator nonce replay fence")
        }
    }
}

pub async fn authenticate_operator_command(
    identity: &dyn NodeIdentity,
    command: &EnrollmentOperatorDecisionCommand,
    now: chrono::DateTime<Utc>,
) -> Result<()> {
    command.validate_at(now)?;
    anyhow::ensure!(
        command.admin_did == identity.did(),
        "operator command signer does not own this runtime"
    );
    anyhow::ensure!(
        identity
            .verify(
                &command.admin_did,
                &command.signing_payload(),
                &command.admin_sig,
            )
            .await?,
        "operator command signature is invalid"
    );
    Ok(())
}

pub async fn authenticate_operator_query(
    identity: &dyn NodeIdentity,
    command: &EnrollmentOperatorQueryCommand,
    now: chrono::DateTime<Utc>,
) -> Result<()> {
    command.validate_at(now)?;
    anyhow::ensure!(
        command.admin_did == identity.did(),
        "operator query signer does not own this runtime"
    );
    anyhow::ensure!(
        identity
            .verify(
                &command.admin_did,
                &command.signing_payload(),
                &command.admin_sig,
            )
            .await?,
        "operator query signature is invalid"
    );
    Ok(())
}

/// The operator Pending, Members and History read model over one enrollment
/// projection; the caller authenticates the query and consumes its nonce first.
pub fn operator_query_view(
    projection: EnrollmentProjection,
    query: &EnrollmentOperatorQuery,
    now: DateTime<Utc>,
) -> Result<serde_json::Value> {
    anyhow::ensure!(
        projection.conflict.is_none(),
        "enrollment root authority is conflicted"
    );
    match query {
        EnrollmentOperatorQuery::Pending => Ok(serde_json::json!({
            "pending": projection.pending.into_iter().map(|pending| serde_json::json!({
                "request_id": pending.request.request_id,
                "network_id": pending.request.network_id,
                "candidate_did": pending.request.candidate_did,
                "candidate_peer": pending.request.candidate_peer,
                "candidate_name": pending.request.candidate_name,
                "owner_node_did": pending.request.owner_node_did,
                "signed_request": pending.request,
                "profile": pending.request.profile,
                "issued_at": pending.request.issued_at,
                "expires_at": pending.request.expires_at,
            })).collect::<Vec<_>>()
        })),
        EnrollmentOperatorQuery::Members => Ok(serde_json::json!({
            "members": projection.active.into_iter()
                .filter(|entry| gents_protocol::enrollment::authorization_lease_is_fresh_at(&entry.revision.authorization_expires_at, now))
                .map(|entry| serde_json::json!({
                    "request_id": entry.request.request_id,
                    "network_id": entry.request.network_id,
                    "candidate_did": entry.request.candidate_did,
                    "candidate_peer": entry.request.candidate_peer,
                    "profile": entry.request.profile,
                    "authorization_expires_at": entry.revision.authorization_expires_at,
                })).collect::<Vec<_>>()
        })),
        EnrollmentOperatorQuery::History => {
            let conflicts_omitted = !projection.request_conflicts.is_empty()
                || !projection.scoped_conflicts.is_empty();
            let mut history = Vec::new();
            for entry in projection.active {
                /* a lapsed lease is no longer a member (the Members
                query's freshness rule): its approval has ended */
                let lapsed = !gents_protocol::enrollment::authorization_lease_is_fresh_at(
                    &entry.revision.authorization_expires_at,
                    now,
                );
                history.push(serde_json::json!({
                    "request_id": entry.request.request_id, "candidate_did": entry.request.candidate_did,
                    "candidate_peer": entry.request.candidate_peer, "profile": entry.request.profile,
                    "state": if lapsed { "expired" } else { "approved" }, "decided_at": entry.decision.decided_at,
                    "decided_by": entry.decision.signer_did, "revoked_at": null, "revoked_by": null,
                    "expired_at": lapsed.then_some(&entry.revision.authorization_expires_at),
                }));
            }
            for entry in projection.denied {
                history.push(serde_json::json!({
                    "request_id": entry.request.request_id, "candidate_did": entry.request.candidate_did,
                    "candidate_peer": entry.request.candidate_peer, "profile": entry.request.profile,
                    "state": "denied", "decided_at": entry.decision.decided_at,
                    "decided_by": entry.decision.signer_did, "revoked_at": null, "revoked_by": null,
                }));
            }
            for entry in projection.revoked {
                history.push(serde_json::json!({
                    "request_id": entry.request.request_id, "candidate_did": entry.request.candidate_did,
                    "candidate_peer": entry.request.candidate_peer, "profile": entry.request.profile,
                    "state": "revoked", "decided_at": entry.decision.decided_at,
                    "decided_by": entry.decision.signer_did, "revoked_at": entry.revision.issued_at,
                    "revoked_by": entry.revision.signer_did,
                }));
            }
            history.sort_by(|a, b| {
                let latest = |value: &serde_json::Value| {
                    value["revoked_at"]
                        .as_str()
                        .or_else(|| value["expired_at"].as_str())
                        .or_else(|| value["decided_at"].as_str())
                        .unwrap_or("")
                        .to_owned()
                };
                latest(b).cmp(&latest(a))
            });
            let observed_count = history.len();
            history.truncate(50);
            Ok(
                serde_json::json!({ "history": history, "observed_count": observed_count, "limit": 50, "omits_superseded": true, "conflicts_omitted": conflicts_omitted }),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use defra_node::StorageBackend;
    use gents_protocol::enrollment::ENROLLMENT_PROTOCOL_VERSION;
    use gents_protocol::enrollment::{
        EnrollmentOperatorAction, EnrollmentOperatorQuery,
        DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
    };

    #[tokio::test]
    async fn operator_command_requires_the_live_admin_identity_and_exact_signature() {
        let temp = tempfile::tempdir().unwrap();
        let identity =
            gents_store::identity::KeyIdentity::load_or_create(temp.path().join("admin.key"), None)
                .unwrap();
        let now = Utc::now();
        let mut command = EnrollmentOperatorDecisionCommand {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            request_id: "request-1".into(),
            action: EnrollmentOperatorAction::Approve,
            lease_seconds: DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
            admin_did: identity.did().to_string(),
            issued_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
            nonce: "nonce-1".into(),
            admin_sig: Vec::new(),
        };
        command.admin_sig = identity.sign(&command.signing_payload()).await.unwrap();
        authenticate_operator_command(&identity, &command, now)
            .await
            .unwrap();

        let mut changed = command.clone();
        changed.action = EnrollmentOperatorAction::Deny;
        changed.lease_seconds = 0;
        assert!(authenticate_operator_command(&identity, &changed, now)
            .await
            .is_err());
        let other =
            gents_store::identity::KeyIdentity::load_or_create(temp.path().join("other.key"), None)
                .unwrap();
        assert!(authenticate_operator_command(&other, &command, now)
            .await
            .is_err());

        let mut query = EnrollmentOperatorQueryCommand {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            query: EnrollmentOperatorQuery::Pending,
            admin_did: identity.did().to_string(),
            issued_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
            nonce: "nonce-2".into(),
            admin_sig: Vec::new(),
        };
        query.admin_sig = identity.sign(&query.signing_payload()).await.unwrap();
        authenticate_operator_query(&identity, &query, now)
            .await
            .unwrap();
        assert!(authenticate_operator_query(&other, &query, now)
            .await
            .is_err());
        for kind in [
            EnrollmentOperatorQuery::Members,
            EnrollmentOperatorQuery::History,
        ] {
            let mut changed = query.clone();
            changed.query = kind;
            assert!(authenticate_operator_query(&identity, &changed, now)
                .await
                .is_err());
            changed.admin_sig = identity.sign(&changed.signing_payload()).await.unwrap();
            authenticate_operator_query(&identity, &changed, now)
                .await
                .unwrap();
        }
    }

    async fn nonce_test_node(path: &std::path::Path) -> Arc<EmbeddedNode> {
        let identity = gents_store::identity::KeyIdentity::load_or_create(
            path.with_extension("node.key"),
            None,
        )
        .expect("nonce test identity");
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .data_path(path)
                .with_storage_backend(StorageBackend::Regolith)
                .build()
                .await
                .expect("nonce test node"),
        );
        gents_store::schema::migration::ensure_all_runtime_migrations(node.clone())
            .await
            .expect("nonce schema");
        node
    }

    #[tokio::test]
    async fn operator_nonce_is_atomic_across_concurrency_and_node_restart() {
        let temp = tempfile::tempdir().unwrap();
        let node_path = temp.path().join("node");
        let node = nonce_test_node(&node_path).await;
        let now = "2026-08-30T12:00:00Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap();

        let (first, second) = tokio::join!(
            consume_operator_nonce(&node, "did:key:admin", "nonce-concurrent", now),
            consume_operator_nonce(&node, "did:key:admin", "nonce-concurrent", now),
        );
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        let duplicate = first.err().or_else(|| second.err()).unwrap();
        assert!(duplicate.to_string().contains("already consumed"));

        node.shutdown().await.expect("node shutdown");
        drop(node);
        let restarted = nonce_test_node(&node_path).await;
        let replay = consume_operator_nonce(&restarted, "did:key:admin", "nonce-concurrent", now)
            .await
            .unwrap_err();
        assert!(replay.to_string().contains("already consumed"));
    }

    #[tokio::test]
    async fn operator_nonce_retention_deletes_expired_rows_before_consuming_new_nonce() {
        let temp = tempfile::tempdir().unwrap();
        let node = nonce_test_node(&temp.path().join("node")).await;
        let first = "2026-08-30T12:00:00Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap();
        consume_operator_nonce(&node, "did:key:admin", "nonce-old", first)
            .await
            .unwrap();
        consume_operator_nonce(
            &node,
            "did:key:admin",
            "nonce-new",
            first + Duration::seconds(OPERATOR_NONCE_RETENTION_SECONDS + 1),
        )
        .await
        .unwrap();

        let actor = identity::Did::new(node.node_identity_did().unwrap()).unwrap();
        let response = gents_store::config_client::ConfigAccess::local_as(node.clone(), actor)
            .execute("{ EnrollmentOperatorNonce { nonce_key } }")
            .await
            .unwrap();
        assert_eq!(
            gents_protocol::graphql::graphql_rows_from_response(
                &response,
                "EnrollmentOperatorNonce"
            )
            .len(),
            1
        );
    }
}
