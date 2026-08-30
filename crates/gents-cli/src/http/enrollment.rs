use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{Duration, SecondsFormat, Utc};
use defra_p2p_adapter::P2POperations;
use gents::defra_node::EmbeddedNode;
use gents::graphql::{ensure_no_errors, escape_graphql_string, rows};
use gents::AgentIdentity;
use gents_protocol::enrollment::{
    derive_enrollment_id, encode_offer, enrollment_schema_fingerprint, EnrollmentOfferRecord,
    EnrollmentOperatorAction, EnrollmentOperatorDecisionCommand, EnrollmentOperatorQuery,
    EnrollmentOperatorQueryCommand, ENROLLMENT_PROTOCOL_VERSION,
};
use gents_protocol::network_token::{derive_network_id, NetworkRecord};
use p2p::iroh::parse_public_peer_addr;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::RwLock;
use uuid::Uuid;

pub(crate) type EnrollmentOfferIssuerHandle = Arc<RwLock<Option<EnrollmentOfferIssuer>>>;
pub(crate) type EnrollmentDecisionServiceHandle = Arc<RwLock<Option<EnrollmentDecisionService>>>;

#[derive(Clone)]
pub(crate) struct EnrollmentDecisionService {
    identity: Arc<dyn AgentIdentity>,
    node: Arc<EmbeddedNode>,
    store: gents::agent::p2p_reconcile::GraphqlEnrollmentStore,
}

impl EnrollmentDecisionService {
    pub(crate) fn new(identity: Arc<dyn AgentIdentity>, node: Arc<EmbeddedNode>) -> Self {
        Self {
            store: gents::agent::p2p_reconcile::GraphqlEnrollmentStore::new(
                node.clone(),
                identity.clone(),
            ),
            identity,
            node,
        }
    }

    pub(crate) async fn decide(
        &self,
        command: &EnrollmentOperatorDecisionCommand,
    ) -> Result<gents::agent::p2p_reconcile::EnrollmentDecisionOutcome> {
        let now = Utc::now();
        authenticate_operator_command(self.identity.as_ref(), command, now).await?;
        consume_operator_nonce(self.node.as_ref(), &command.admin_did, &command.nonce, now).await?;
        match command.action {
            EnrollmentOperatorAction::Approve => {
                self.store
                    .decide_request_with_lease(
                        command.request_id.trim(),
                        gents_protocol::enrollment::EnrollmentDecisionKind::Approved,
                        std::time::Duration::from_secs(command.lease_seconds),
                    )
                    .await
            }
            EnrollmentOperatorAction::Deny => {
                self.store
                    .decide_request(
                        command.request_id.trim(),
                        gents_protocol::enrollment::EnrollmentDecisionKind::Denied,
                    )
                    .await
            }
            EnrollmentOperatorAction::Revoke => {
                self.store.revoke_request(command.request_id.trim()).await
            }
        }
    }

    pub(crate) async fn pending(
        &self,
        command: &EnrollmentOperatorQueryCommand,
    ) -> Result<Vec<serde_json::Value>> {
        let now = Utc::now();
        authenticate_operator_query(self.identity.as_ref(), command, now).await?;
        consume_operator_nonce(self.node.as_ref(), &command.admin_did, &command.nonce, now).await?;
        anyhow::ensure!(
            command.query == EnrollmentOperatorQuery::Pending,
            "unsupported enrollment operator query"
        );
        let projection = self.store.load_projection(Utc::now()).await?;
        anyhow::ensure!(
            projection.conflict.is_none(),
            "enrollment root authority is conflicted"
        );
        Ok(projection
            .pending
            .into_iter()
            .map(|pending| {
                serde_json::json!({
                    "request_id": pending.request.request_id,
                    "network_id": pending.request.network_id,
                    "candidate_did": pending.request.candidate_did,
                    "candidate_peer": pending.request.candidate_peer,
                    "owner_agent": pending.request.owner_agent,
                    "expires_at": pending.request.expires_at,
                })
            })
            .collect())
    }
}

const OPERATOR_NONCE_RETENTION_SECONDS: i64 = 5 * 60;

/// Atomically consume one signed operator nonce before any protected read or
/// decision. The durable unique key survives process restart; storing only a
/// domain-separated digest avoids retaining the bearer value itself.
async fn consume_operator_nonce(
    node: &EmbeddedNode,
    admin_did: &str,
    nonce: &str,
    now: chrono::DateTime<Utc>,
) -> Result<()> {
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
                expires_at: "{expires_at}", consumed_at: "{now_escaped}"
            }}) {{ _docID }}
        }}"#
    );
    match gents::graphql::graphql_mutation_with_transaction_retry(
        node,
        &mutation,
        "consume enrollment operator nonce",
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(write_error) => {
            let query = format!(
                r#"{{ EnrollmentOperatorNonce(filter: {{ nonce_key: {{ _eq: "{nonce_key_escaped}" }} }}) {{ nonce_key expires_at }} }}"#
            );
            let response = node.execute(&query).await;
            if ensure_no_errors(&response, "check consumed enrollment operator nonce").is_ok()
                && rows::<ConsumedNonceRow>(&response, "EnrollmentOperatorNonce")
                    .is_ok_and(|rows| rows.iter().any(|row| row.nonce_key == nonce_key))
            {
                anyhow::bail!("enrollment operator nonce was already consumed");
            }
            Err(write_error).context("persisting enrollment operator nonce replay fence")
        }
    }
}

#[derive(Deserialize)]
struct ConsumedNonceRow {
    nonce_key: String,
}

async fn authenticate_operator_command(
    identity: &dyn AgentIdentity,
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

async fn authenticate_operator_query(
    identity: &dyn AgentIdentity,
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

#[derive(Clone)]
pub(crate) struct EnrollmentOfferIssuer {
    identity: Arc<dyn AgentIdentity>,
    p2p: Arc<dyn P2POperations>,
    network_id: String,
    owner_agent: String,
    profile: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct EnrollmentOfferStatus {
    pub(crate) token: String,
    pub(crate) offer: EnrollmentOfferRecord,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum EnrollmentStatus {
    Available {
        token: String,
        offer: EnrollmentOfferRecord,
    },
    Unavailable {
        reason: &'static str,
    },
}

impl EnrollmentOfferIssuer {
    pub(crate) fn new(
        identity: Arc<dyn AgentIdentity>,
        p2p: Arc<dyn P2POperations>,
        network_id: String,
        owner_agent: String,
        profile: String,
    ) -> Self {
        Self {
            identity,
            p2p,
            network_id,
            owner_agent,
            profile,
        }
    }

    pub(crate) async fn mint(&self) -> Result<EnrollmentOfferStatus> {
        let server_peer = self
            .p2p
            .local_peer_id()
            .await
            .map_err(anyhow::Error::msg)
            .context("reading live server peer ID for enrollment offer")?;
        let server_ticket = self
            .p2p
            .shareable_address()
            .await
            .map_err(anyhow::Error::msg)
            .context("reading live server ticket for enrollment offer")?
            .context("server has no shareable P2P address for enrollment")?;
        let (ticket_peer, _) = parse_public_peer_addr(&server_ticket)
            .context("server produced an invalid shareable P2P ticket")?;
        anyhow::ensure!(
            ticket_peer.to_string() == server_peer,
            "server shareable ticket does not match its live peer ID"
        );
        let issued = Utc::now();
        let issued_at = issued.to_rfc3339_opts(SecondsFormat::Secs, true);
        let expires_at = (issued + Duration::minutes(5)).to_rfc3339_opts(SecondsFormat::Secs, true);
        let challenge = Uuid::new_v4().simple().to_string();
        let offer_id = format!(
            "offer-{}",
            derive_enrollment_id(
                "gents-enrollment-offer-v1",
                &[
                    &self.network_id,
                    self.identity.did(),
                    &server_peer,
                    &challenge,
                    &issued_at,
                ],
            )
        );
        let mut offer = EnrollmentOfferRecord {
            version: ENROLLMENT_PROTOCOL_VERSION,
            offer_id,
            challenge,
            network_id: self.network_id.clone(),
            admin_did: self.identity.did().to_string(),
            server_peer,
            server_ticket,
            owner_agent: self.owner_agent.clone(),
            profile: self.profile.clone(),
            schema_fingerprint: enrollment_schema_fingerprint(),
            issued_at,
            expires_at,
            admin_sig: Vec::new(),
        };
        offer.admin_sig = self
            .identity
            .sign(&offer.signing_payload())
            .await
            .context("signing authenticated enrollment offer")?;
        let token = encode_offer(&offer)?;
        Ok(EnrollmentOfferStatus { token, offer })
    }
}

pub(crate) fn empty_issuer_handle() -> EnrollmentOfferIssuerHandle {
    Arc::new(RwLock::new(None))
}

pub(crate) fn empty_decision_service_handle() -> EnrollmentDecisionServiceHandle {
    Arc::new(RwLock::new(None))
}

#[derive(Deserialize)]
struct AgentNetworkRow {
    network_id: String,
    admin_did: String,
    display_name: String,
    default_template: String,
    created_at: String,
    admin_sig: String,
}

pub(crate) async fn ensure_enrollment_network(
    node: &EmbeddedNode,
    identity: &dyn AgentIdentity,
    display_name: &str,
) -> Result<NetworkRecord> {
    let response = node
        .execute(
            "{ AgentNetwork { network_id admin_did display_name default_template created_at admin_sig } }",
        )
        .await;
    ensure_no_errors(&response, "loading enrollment AgentNetwork")?;
    let existing = rows::<AgentNetworkRow>(&response, "AgentNetwork")?;
    match existing.as_slice() {
        [row] => {
            let record = NetworkRecord {
                network_id: row.network_id.clone(),
                admin_did: row.admin_did.clone(),
                display_name: row.display_name.clone(),
                default_template: row.default_template.clone(),
                created_at: row.created_at.clone(),
                sig: bs58::decode(&row.admin_sig)
                    .into_vec()
                    .context("decoding enrollment AgentNetwork signature")?,
            };
            anyhow::ensure!(
                record.admin_did == identity.did(),
                "existing AgentNetwork admin {} does not match local identity {}",
                record.admin_did,
                identity.did()
            );
            anyhow::ensure!(
                identity
                    .verify(&record.admin_did, &record.signing_payload(), &record.sig)
                    .await?,
                "existing AgentNetwork signature is invalid"
            );
            Ok(record)
        }
        [] => {
            let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
            let mut record = NetworkRecord {
                network_id: derive_network_id(identity.did(), "default"),
                admin_did: identity.did().to_string(),
                display_name: display_name.to_string(),
                default_template: "conversation".to_string(),
                created_at: now,
                sig: Vec::new(),
            };
            record.sig = identity
                .sign(&record.signing_payload())
                .await
                .context("signing enrollment AgentNetwork")?;
            let network_id = escape_graphql_string(&record.network_id);
            let admin_did = escape_graphql_string(&record.admin_did);
            let display_name = escape_graphql_string(&record.display_name);
            let default_template = escape_graphql_string(&record.default_template);
            let created_at = escape_graphql_string(&record.created_at);
            let admin_sig = escape_graphql_string(&bs58::encode(&record.sig).into_string());
            let mutation = format!(
                r#"mutation {{
                    create_AgentNetwork(input: {{
                        network_id: "{network_id}",
                        admin_did: "{admin_did}",
                        display_name: "{display_name}",
                        default_template: "{default_template}",
                        created_at: "{created_at}",
                        admin_sig: "{admin_sig}"
                    }}) {{ _docID }}
                }}"#
            );
            gents::graphql::graphql_mutation_with_transaction_retry(
                node,
                &mutation,
                "create_enrollment_agent_network",
            )
            .await?;
            Ok(record)
        }
        rows => anyhow::bail!("expected one enrollment AgentNetwork, found {}", rows.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::defra_node::{EmbeddedNode, StorageBackend};
    use gents_protocol::enrollment::{
        EnrollmentOperatorAction, EnrollmentOperatorQuery,
        DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
    };

    #[tokio::test]
    async fn operator_command_requires_the_live_admin_identity_and_exact_signature() {
        let temp = tempfile::tempdir().unwrap();
        let identity =
            gents::KeyIdentity::load_or_create(&temp.path().join("admin.key"), None).unwrap();
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
            gents::KeyIdentity::load_or_create(&temp.path().join("other.key"), None).unwrap();
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
    }

    async fn nonce_test_node(path: &std::path::Path) -> Arc<EmbeddedNode> {
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(path)
                .with_storage_backend(StorageBackend::Lark)
                .build()
                .await
                .expect("nonce test node"),
        );
        gents::migration::ensure_all_runtime_migrations(node.clone())
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
            consume_operator_nonce(node.as_ref(), "did:key:admin", "nonce-concurrent", now),
            consume_operator_nonce(node.as_ref(), "did:key:admin", "nonce-concurrent", now),
        );
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        let duplicate = first.err().or_else(|| second.err()).unwrap();
        assert!(duplicate.to_string().contains("already consumed"));

        node.shutdown().await;
        drop(node);
        let restarted = nonce_test_node(&node_path).await;
        let replay =
            consume_operator_nonce(restarted.as_ref(), "did:key:admin", "nonce-concurrent", now)
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
        consume_operator_nonce(node.as_ref(), "did:key:admin", "nonce-old", first)
            .await
            .unwrap();
        consume_operator_nonce(
            node.as_ref(),
            "did:key:admin",
            "nonce-new",
            first + Duration::seconds(OPERATOR_NONCE_RETENTION_SECONDS + 1),
        )
        .await
        .unwrap();

        let response = node
            .execute("{ EnrollmentOperatorNonce { nonce_key } }")
            .await;
        ensure_no_errors(&response, "load retained enrollment operator nonces").unwrap();
        assert_eq!(
            rows::<ConsumedNonceRow>(&response, "EnrollmentOperatorNonce")
                .unwrap()
                .len(),
            1
        );
    }
}
