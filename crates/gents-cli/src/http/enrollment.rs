use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{Duration, SecondsFormat, Utc};
use defra_p2p_adapter::P2POperations;
use gents::defra_node::EmbeddedNode;
use gents::graphql::{ensure_no_errors, escape_graphql_string, rows};
use gents::AgentIdentity;
use gents_protocol::enrollment::{
    derive_enrollment_id, encode_offer, enrollment_schema_fingerprint, EnrollmentOfferRecord,
    ENROLLMENT_PROTOCOL_VERSION,
};
use gents_protocol::network_token::{derive_network_id, NetworkRecord};
use p2p::iroh::parse_public_peer_addr;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::RwLock;
use uuid::Uuid;

pub(crate) type EnrollmentOfferIssuerHandle = Arc<RwLock<Option<EnrollmentOfferIssuer>>>;

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
