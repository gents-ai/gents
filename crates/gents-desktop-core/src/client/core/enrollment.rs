use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use defra_p2p_adapter::{
    P2PError, P2POperations as P2POps, P2pDocumentRequest, ReplicationFilter, TransportPeerId,
};
use futures::{stream, StreamExt};
use gents::agent::p2p_reconcile::enrollment::{
    AuthorizationRevision as PureRevision, AuthorizationRevisionKind as PureRevisionKind,
    DurableEnrollmentDocuments, EnrollmentDecision as PureDecision,
    EnrollmentDecisionKind as PureDecisionKind, EnrollmentOffer as PureOffer,
    EnrollmentRequest as PureRequest, EnrollmentRouteDirection as PureRouteDirection,
    EnrollmentRouteReceipt as PureRouteReceipt, NetworkAdminPin as PureAdminPin,
};
use gents::graphql::{ensure_no_errors, escape_graphql_string, rows};
use gents::AgentIdentity;
use gents_protocol::enrollment::{
    decode_offer, derive_enrollment_id, enrollment_schema_fingerprint, AuthorizationRevisionKind,
    AuthorizationRevisionRecord, EnrollmentDecisionKind, EnrollmentDecisionRecord,
    EnrollmentRequestRecord, EnrollmentRouteReceiptDirection, EnrollmentRouteReceiptRecord,
    ENROLLMENT_PROTOCOL_VERSION,
};
use gents_protocol::network_token::EndpointRecord;
use p2p::iroh::parse_public_peer_addr;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::time::timeout;
use uuid::Uuid;

use super::super::principal_identity::PrincipalIdentity;
use super::route_manager::ClientRouteManager;
use super::sync_state::ClientSyncStateOwner;
use super::{ClientCore, P2P_OPERATION_TIMEOUT};

pub(super) async fn current_local_endpoint(
    p2p: &Arc<dyn P2POps>,
    identity: &dyn AgentIdentity,
) -> Result<EndpointRecord> {
    let peer_id = timeout(P2P_OPERATION_TIMEOUT, p2p.local_peer_id())
        .await
        .context("timed out reading desktop P2P peer id")?
        .map_err(map_p2p_error)?;
    let address = timeout(P2P_OPERATION_TIMEOUT, p2p.shareable_address())
        .await
        .context("timed out reading desktop shareable P2P address")?
        .map_err(map_p2p_error)?
        .context("desktop P2P transport has no dialable shareable address")?;
    Ok(EndpointRecord {
        did: identity.did().to_string(),
        node_id: peer_id,
        address,
        updated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        sig: Vec::new(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentRequestResult {
    pub request_id: String,
    pub network_id: String,
    pub admin_did: String,
    pub server_peer: String,
    pub owner_agent: String,
    pub state: String,
}

#[derive(Deserialize)]
struct AdminPinRow {
    admin_did: String,
}

impl ClientCore {
    pub async fn request_status_enrollment(
        &self,
        offer_token: &str,
    ) -> Result<EnrollmentRequestResult> {
        let offer = decode_offer(offer_token).context("decoding server enrollment offer")?;
        anyhow::ensure!(
            offer.schema_fingerprint == enrollment_schema_fingerprint(),
            "server enrollment schema {} is incompatible with {}",
            offer.schema_fingerprint,
            enrollment_schema_fingerprint()
        );
        anyhow::ensure!(offer.profile == "client", "unsupported enrollment profile");
        validate_fresh_window(&offer.issued_at, &offer.expires_at)?;

        let (ticket_peer, _) = parse_public_peer_addr(&offer.server_ticket)
            .context("server enrollment offer contains an invalid Iroh ticket")?;
        anyhow::ensure!(
            ticket_peer.to_string() == offer.server_peer,
            "server enrollment ticket does not match its signed peer ID"
        );
        timeout(
            P2P_OPERATION_TIMEOUT,
            self.p2p.connect_peer(&offer.server_ticket),
        )
        .await
        .context("timed out connecting to enrollment server")?
        .map_err(map_p2p_error)
        .context("connecting to enrollment server")?;

        let transport_peer = TransportPeerId::new(offer.server_peer.clone())
            .map_err(map_p2p_error)
            .context("validating enrollment server peer ID")?;
        let resolved_server_did = timeout(
            P2P_OPERATION_TIMEOUT,
            self.p2p.resolve_peer_identity(&transport_peer),
        )
        .await
        .context("timed out authenticating enrollment server identity")?
        .map_err(map_p2p_error)?
        .context("enrollment server has no configured authenticated identity")?;
        validate_authenticated_server_did(&offer.admin_did, &resolved_server_did.to_string())?;
        anyhow::ensure!(
            self.principal
                .verify(&offer.admin_did, &offer.signing_payload(), &offer.admin_sig)
                .await?,
            "enrollment offer signature is invalid"
        );

        self.confirm_admin_pin(&offer.network_id, &offer.admin_did, &offer.offer_id)
            .await?;

        let candidate_peer = self.local_peer_id.clone();
        let candidate_ticket = timeout(P2P_OPERATION_TIMEOUT, self.p2p.shareable_address())
            .await
            .context("timed out reading local enrollment ticket")?
            .map_err(map_p2p_error)?
            .context("desktop client has no shareable P2P address")?;
        let (ticket_candidate_peer, _) = parse_public_peer_addr(&candidate_ticket)
            .context("desktop client produced an invalid shareable Iroh ticket")?;
        anyhow::ensure!(
            ticket_candidate_peer.to_string() == candidate_peer,
            "desktop shareable ticket does not match its local peer ID"
        );

        let (request, document_id) = match self
            .existing_request_for_offer(&offer, offer_token, &candidate_peer)
            .await?
        {
            Some(existing) => existing,
            None => {
                let issued_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
                let client_nonce = Uuid::new_v4().simple().to_string();
                let request_id = format!(
                    "enroll-{}",
                    derive_enrollment_id(
                        "gents-enrollment-request-id-v1",
                        &[
                            &offer.offer_id,
                            self.principal.did(),
                            &candidate_peer,
                            &client_nonce,
                        ],
                    )
                );
                let mut request = EnrollmentRequestRecord {
                    protocol_version: ENROLLMENT_PROTOCOL_VERSION,
                    request_id,
                    request_digest: String::new(),
                    offer_id: offer.offer_id.clone(),
                    offer_token: offer_token.to_string(),
                    challenge: offer.challenge.clone(),
                    network_id: offer.network_id.clone(),
                    admin_did: offer.admin_did.clone(),
                    server_peer: offer.server_peer.clone(),
                    candidate_did: self.principal.did().to_string(),
                    candidate_peer,
                    candidate_ticket,
                    owner_agent: offer.owner_agent.clone(),
                    profile: offer.profile.clone(),
                    client_nonce,
                    issued_at,
                    expires_at: offer.expires_at.clone(),
                    candidate_sig: Vec::new(),
                };
                request.request_digest = request.computed_digest();
                request.candidate_sig = self.principal.sign(&request.signing_payload())?;
                request
                    .validate_against_offer(&offer)
                    .context("validating authored enrollment request")?;
                let document_id = match self.write_enrollment_request(&request).await {
                    Ok(document_id) => document_id,
                    Err(write_error) => {
                        let recovered = self
                            .existing_request_for_offer(&offer, offer_token, &request.candidate_peer)
                            .await?
                            .filter(|(persisted, _)| persisted == &request)
                            .with_context(|| {
                                format!(
                                    "enrollment request commit was not observably recovered after: {write_error:#}"
                                )
                            })?;
                        recovered.1
                    }
                };
                (request, document_id)
            }
        };
        push_enrollment_request(&self.p2p, &offer, &request.request_id, &document_id).await?;

        Ok(EnrollmentRequestResult {
            request_id: request.request_id,
            network_id: request.network_id,
            admin_did: request.admin_did,
            server_peer: request.server_peer,
            owner_agent: request.owner_agent,
            state: "pending_approval".to_string(),
        })
    }

    async fn confirm_admin_pin(
        &self,
        network_id: &str,
        admin_did: &str,
        offer_id: &str,
    ) -> Result<()> {
        let network_id_escaped = escape_graphql_string(network_id);
        let query = format!(
            r#"{{ NetworkAdminPin(filter: {{ network_id: {{ _eq: "{network_id_escaped}" }} }}) {{ admin_did }} }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "loading local enrollment admin pin")?;
        let pins = rows::<AdminPinRow>(&response, "NetworkAdminPin")?;
        match pins.as_slice() {
            [pin] if pin.admin_did == admin_did => return Ok(()),
            [pin] => anyhow::bail!(
                "network {network_id} is pinned to admin {}; refusing conflicting admin {admin_did}",
                pin.admin_did
            ),
            [] => {}
            pins => anyhow::bail!(
                "network {network_id} has {} local admin pins; refusing enrollment",
                pins.len()
            ),
        }

        let pin_key = format!(
            "pin-{}",
            derive_enrollment_id("gents-network-admin-pin-v1", &[network_id])
        );
        let pin_key = escape_graphql_string(&pin_key);
        let admin_did_escaped = escape_graphql_string(admin_did);
        let offer_id_escaped = escape_graphql_string(offer_id);
        let confirmed_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let mutation = format!(
            r#"mutation {{
                create_NetworkAdminPin(input: {{
                    pin_key: "{pin_key}",
                    network_id: "{network_id_escaped}",
                    admin_did: "{admin_did_escaped}",
                    offer_id: "{offer_id_escaped}",
                    confirmed_at: "{confirmed_at}"
                }}) {{ _docID }}
            }}"#
        );
        let committed = gents::graphql::graphql_mutation_with_transaction_retry(
            self.node.as_ref(),
            &mutation,
            "create_network_admin_pin",
        )
        .await;
        match committed {
            Ok(_) => Ok(()),
            Err(commit_error) => {
                let response = self.node.execute(&query).await;
                ensure_no_errors(&response, "recovering local enrollment admin pin")?;
                let pins = rows::<AdminPinRow>(&response, "NetworkAdminPin")?;
                anyhow::ensure!(
                    matches!(pins.as_slice(), [pin] if pin.admin_did == admin_did),
                    "admin pin commit was not observably recovered after: {commit_error:#}"
                );
                Ok(())
            }
        }
    }

    async fn write_enrollment_request(&self, request: &EnrollmentRequestRecord) -> Result<String> {
        let input = enrollment_request_input(request);
        let mutation =
            format!("mutation {{ create_NetworkEnrollmentRequest(input: {input}) {{ _docID }} }}");
        let response = gents::graphql::graphql_mutation_with_transaction_retry(
            self.node.as_ref(),
            &mutation,
            "create_network_enrollment_request",
        )
        .await?;
        let response = serde_json::json!({ "data": response.data.unwrap_or_default() });
        gents_protocol::graphql::extract_mutation_doc_id(&response, "NetworkEnrollmentRequest")
            .context("enrollment request mutation returned no document ID")
    }

    async fn existing_request_for_offer(
        &self,
        offer: &gents_protocol::enrollment::EnrollmentOfferRecord,
        offer_token: &str,
        candidate_peer: &str,
    ) -> Result<Option<(EnrollmentRequestRecord, String)>> {
        let offer_id = escape_graphql_string(&offer.offer_id);
        let query = format!(
            r#"{{ NetworkEnrollmentRequest(filter: {{ offer_id: {{ _eq: "{offer_id}" }} }}) {{
                _docID protocol_version request_id request_digest offer_id offer_token challenge
                network_id admin_did server_peer candidate_did candidate_peer candidate_ticket
                owner_agent profile client_nonce issued_at expires_at candidate_sig
            }} }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "loading retryable enrollment request")?;
        let rows = rows::<EnrollmentRequestRow>(&response, "NetworkEnrollmentRequest")?;
        let Some(row) = select_retryable_local_request(
            &rows,
            self.principal.did(),
            candidate_peer,
            &offer.offer_id,
        )?
        else {
            return Ok(None);
        };
        let doc_id = row.doc_id.clone();
        let request = row.to_record()?;
        anyhow::ensure!(
            request.offer_token == offer_token,
            "persisted enrollment request embeds a different signed offer"
        );
        request.validate_against_offer(offer)?;
        anyhow::ensure!(
            self.principal
                .verify(
                    &request.candidate_did,
                    &request.signing_payload(),
                    &request.candidate_sig,
                )
                .await?,
            "persisted enrollment request signature is invalid"
        );
        Ok(Some((request, doc_id)))
    }
}

async fn push_enrollment_request(
    p2p: &Arc<dyn P2POps>,
    offer: &gents_protocol::enrollment::EnrollmentOfferRecord,
    request_id: &str,
    document_id: &str,
) -> Result<()> {
    const COLLECTION: &str = "NetworkEnrollmentRequest";

    // DefraDB's explicit document replay is intentionally guarded by a live
    // replicator. Before approval there is no authority-backed data route yet,
    // so install the smallest possible bootstrap route: one authenticated
    // server and one immutable enrollment request. It is removed before this
    // operation returns; the signed enrollment reconciler owns every durable
    // route after approval.
    let mut conditions = Map::new();
    conditions.insert("request_id".to_string(), json!({ "_eq": request_id }));
    let filters = BTreeMap::from([(
        COLLECTION.to_string(),
        ReplicationFilter::predicate(conditions),
    )]);
    let collections = vec![COLLECTION.to_string()];
    let install = timeout(
        P2P_OPERATION_TIMEOUT,
        p2p.add_replicator(
            collections.clone(),
            Some(&offer.server_ticket),
            filters,
            Vec::new(),
            Some(&offer.admin_did),
        ),
    )
    .await;
    let install_error = match install {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(
            map_p2p_error(error).context("installing authenticated enrollment bootstrap route"),
        ),
        Err(_) => Some(anyhow::anyhow!(
            "timed out installing authenticated enrollment bootstrap route"
        )),
    };

    let delivery_error = if install_error.is_none() {
        match timeout(
            P2P_OPERATION_TIMEOUT,
            p2p.push_documents_to_peer(
                &offer.server_peer,
                vec![P2pDocumentRequest {
                    collection: COLLECTION.to_string(),
                    doc_id: document_id.to_string(),
                }],
            ),
        )
        .await
        {
            Ok(Ok(())) => None,
            Ok(Err(error)) => {
                Some(map_p2p_error(error).context("pushing enrollment request to server"))
            }
            Err(_) => Some(anyhow::anyhow!(
                "timed out pushing enrollment request to server"
            )),
        }
    } else {
        None
    };

    let cleanup_error = match timeout(
        P2P_OPERATION_TIMEOUT,
        p2p.remove_replicator(collections, Some(&offer.server_ticket)),
    )
    .await
    {
        Ok(Ok(())) => None,
        Ok(Err(error)) => {
            Some(map_p2p_error(error).context("removing authenticated enrollment bootstrap route"))
        }
        Err(_) => Some(anyhow::anyhow!(
            "timed out removing authenticated enrollment bootstrap route"
        )),
    };

    match (install_error.or(delivery_error), cleanup_error) {
        (None, None) => Ok(()),
        (Some(operation), None) => Err(operation),
        (None, Some(cleanup)) => Err(cleanup),
        (Some(operation), Some(cleanup)) => Err(operation.context(format!(
            "enrollment bootstrap route cleanup also failed: {cleanup:#}"
        ))),
    }
}

fn select_retryable_local_request<'a>(
    rows: &'a [EnrollmentRequestRow],
    candidate_did: &str,
    candidate_peer: &str,
    offer_id: &str,
) -> Result<Option<&'a EnrollmentRequestRow>> {
    let candidates = rows
        .iter()
        .filter(|row| row.candidate_did == candidate_did && row.candidate_peer == candidate_peer)
        .collect::<Vec<_>>();
    let [row] = candidates.as_slice() else {
        anyhow::ensure!(
            candidates.is_empty(),
            "offer {offer_id} has multiple local enrollment requests"
        );
        return Ok(None);
    };
    Ok(Some(*row))
}

fn enrollment_request_input(request: &EnrollmentRequestRecord) -> String {
    let field = |value: &str| escape_graphql_string(value);
    let candidate_sig = bs58::encode(&request.candidate_sig).into_string();
    format!(
        r#"{{
            protocol_version: {},
            request_id: "{}",
            request_digest: "{}",
            offer_id: "{}",
            offer_token: "{}",
            challenge: "{}",
            network_id: "{}",
            admin_did: "{}",
            server_peer: "{}",
            candidate_did: "{}",
            candidate_peer: "{}",
            candidate_ticket: "{}",
            owner_agent: "{}",
            profile: "{}",
            client_nonce: "{}",
            issued_at: "{}",
            expires_at: "{}",
            candidate_sig: "{}"
        }}"#,
        request.protocol_version,
        field(&request.request_id),
        field(&request.request_digest),
        field(&request.offer_id),
        field(&request.offer_token),
        field(&request.challenge),
        field(&request.network_id),
        field(&request.admin_did),
        field(&request.server_peer),
        field(&request.candidate_did),
        field(&request.candidate_peer),
        field(&request.candidate_ticket),
        field(&request.owner_agent),
        field(&request.profile),
        field(&request.client_nonce),
        field(&request.issued_at),
        field(&request.expires_at),
        field(&candidate_sig),
    )
}

fn validate_fresh_window(issued_at: &str, expires_at: &str) -> Result<()> {
    let issued = DateTime::parse_from_rfc3339(issued_at).context("parsing offer issued_at")?;
    let expires = DateTime::parse_from_rfc3339(expires_at).context("parsing offer expires_at")?;
    let now = Utc::now();
    anyhow::ensure!(
        issued <= now + chrono::Duration::seconds(30),
        "enrollment offer is from the future"
    );
    anyhow::ensure!(expires > now, "enrollment offer has expired");
    anyhow::ensure!(
        issued <= expires,
        "enrollment offer expires before issuance"
    );
    anyhow::ensure!(
        expires - issued <= chrono::Duration::minutes(10),
        "enrollment offer validity window is too long"
    );
    Ok(())
}

fn map_p2p_error(error: P2PError) -> anyhow::Error {
    anyhow::anyhow!(error.to_string())
}

fn validate_authenticated_server_did(expected_admin_did: &str, resolved_did: &str) -> Result<()> {
    anyhow::ensure!(
        resolved_did == expected_admin_did,
        "authenticated server DID does not match the signed enrollment admin"
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApprovedStatusEnrollment {
    network_id: String,
    request_id: String,
    server_peer: String,
    server_ticket: String,
    admin_did: String,
    owner_agent: String,
    request_digest: String,
    authorization_sequence: u64,
    authorization_expires_at: String,
    decided_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EnrollmentAuthorizationGeneration {
    pub request_digest: String,
    pub sequence: u64,
    pub expires_at: String,
}

impl EnrollmentAuthorizationGeneration {
    fn matches_record_at(
        &self,
        record: &super::super::peer_directory::PeerRecord,
        now: DateTime<Utc>,
    ) -> bool {
        record.enrollment_request_digest.as_deref() == Some(self.request_digest.as_str())
            && record.enrollment_authorization_sequence == Some(self.sequence)
            && record.enrollment_authorization_expires_at.as_deref() == Some(&self.expires_at)
            && gents_protocol::enrollment::authorization_lease_is_fresh_at(&self.expires_at, now)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EnrollmentAuthorityOutcome {
    Current(ApprovedStatusEnrollment),
    Conflicted { reason: String },
}

fn prioritized_current_approvals(
    outcomes: &BTreeMap<String, EnrollmentAuthorityOutcome>,
    known_peers: &BTreeSet<String>,
) -> Vec<(String, ApprovedStatusEnrollment)> {
    let mut approvals = outcomes
        .iter()
        .filter_map(|(peer_id, outcome)| match outcome {
            EnrollmentAuthorityOutcome::Current(approval) => {
                Some((peer_id.clone(), approval.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    approvals.sort_by(|(left_peer, left), (right_peer, right)| {
        known_peers
            .contains(left_peer)
            .cmp(&known_peers.contains(right_peer))
            .then_with(|| right.decided_at.cmp(&left.decided_at))
            .then_with(|| left_peer.cmp(right_peer))
    });
    approvals
}

pub(super) fn enrollment_record_lacks_current_authority(
    record: &super::super::peer_directory::PeerRecord,
    approved_peers: &BTreeMap<String, EnrollmentAuthorizationGeneration>,
) -> bool {
    record.source.as_deref() == Some("enrollment")
        && !approved_peers
            .get(&record.peer_id)
            .is_some_and(|generation| generation.matches_record_at(record, Utc::now()))
}

pub(super) async fn reconcile_status_enrollment_approvals(
    node: &Arc<defra_node::EmbeddedNode>,
    p2p: &Arc<dyn defra_p2p_adapter::P2POperations>,
    principal: &Arc<PrincipalIdentity>,
    local_peer_id: &str,
    sync_state: &ClientSyncStateOwner,
    route_manager: &Arc<ClientRouteManager>,
) -> Result<BTreeMap<String, EnrollmentAuthorizationGeneration>> {
    let outcomes =
        load_status_enrollment_approvals(node.as_ref(), principal.as_ref(), local_peer_id).await?;
    let known_peers = sync_state
        .records()
        .into_iter()
        .map(|record| record.peer_id)
        .collect::<BTreeSet<_>>();
    let approvals = prioritized_current_approvals(&outcomes, &known_peers);
    let mut authentications = stream::iter(approvals)
        .map(|(peer_id, approval)| {
            let p2p = Arc::clone(p2p);
            async move {
                let result = authenticate_enrolled_server(&p2p, &approval).await;
                (peer_id, approval, result)
            }
        })
        .buffer_unordered(8);

    // Absence from the complete scoped observation means the prior grant is
    // no longer current (denied/revoked/superseded). Conflicted peers remain
    // present but closed so hostile state in peer A cannot erase peer B or
    // lose A's retry identity.
    for existing in sync_state.records().into_iter().filter(|record| {
        record.source.as_deref() == Some("enrollment") && !outcomes.contains_key(&record.peer_id)
    }) {
        let removal = route_manager
            .remove_peer(sync_state, &existing.peer_id)
            .await?;
        if let Some(error) = removal.cleanup_error {
            tracing::warn!(peer_id = %existing.peer_id, error = %error, "revoked enrollment route cleanup will retry");
        }
    }

    for (peer_id, outcome) in &outcomes {
        match outcome {
            EnrollmentAuthorityOutcome::Current(_) => {}
            EnrollmentAuthorityOutcome::Conflicted { reason } => {
                tracing::warn!(peer_id, reason, "enrollment peer authority is conflicted");
                demote_enrollment_peer(sync_state, peer_id).await;
            }
        }
    }

    let mut current_authority = BTreeMap::new();
    while let Some((peer_id, approval, authentication)) = authentications.next().await {
        if let Err(error) = authentication {
            tracing::warn!(peer_id, request_id = %approval.request_digest, error = %error, "enrollment peer is temporarily unavailable");
            demote_enrollment_peer(sync_state, &peer_id).await;
            continue;
        }
        let generation = EnrollmentAuthorizationGeneration {
            request_digest: approval.request_digest.clone(),
            sequence: approval.authorization_sequence,
            expires_at: approval.authorization_expires_at.clone(),
        };
        let current = sync_state
            .records()
            .into_iter()
            .find(|record| record.peer_id == approval.server_peer);
        let applied = async {
            let record = sync_state
                .upsert_enrollment_peer(
                    &approval.server_peer,
                    "Enrolled Agent",
                    &approval.server_ticket,
                    &approval.owner_agent,
                    &approval.network_id,
                    &approval.request_id,
                    &approval.request_digest,
                    &approval.admin_did,
                    approval.authorization_sequence,
                    &approval.authorization_expires_at,
                )
                .await?;
            if current.as_ref() != Some(&record) || !record.pairing_ready {
                route_manager.configure_enrollment_peer(&record).await?;
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        match applied {
            Ok(()) => {
                current_authority.insert(peer_id, generation);
            }
            Err(error) => {
                tracing::warn!(peer_id, error = %error, "enrollment route activation failed closed for one peer");
                demote_enrollment_peer(sync_state, &peer_id).await;
            }
        }
    }
    Ok(current_authority)
}

async fn authenticate_enrolled_server(
    p2p: &Arc<dyn defra_p2p_adapter::P2POperations>,
    approval: &ApprovedStatusEnrollment,
) -> Result<()> {
    timeout(P2P_OPERATION_TIMEOUT, async {
        p2p.connect_peer(&approval.server_ticket)
            .await
            .map_err(map_p2p_error)?;
        let peer = TransportPeerId::new(approval.server_peer.clone()).map_err(map_p2p_error)?;
        let resolved = p2p
            .resolve_peer_identity(&peer)
            .await
            .map_err(map_p2p_error)?
            .context("enrolled server has no authenticated transport identity")?;
        validate_authenticated_server_did(&approval.admin_did, &resolved.to_string())
    })
    .await
    .context("timed out re-authenticating enrolled server")?
}

async fn demote_enrollment_peer(sync_state: &ClientSyncStateOwner, peer_id: &str) {
    let Some(record) = sync_state
        .records()
        .into_iter()
        .find(|record| record.peer_id == peer_id && record.source.as_deref() == Some("enrollment"))
    else {
        return;
    };
    if let Err(error) = sync_state.set_pairing_ready(&record, false).await {
        tracing::warn!(peer_id, error = %error, "failed to persist scoped enrollment demotion");
    }
}

async fn load_status_enrollment_approvals(
    node: &defra_node::EmbeddedNode,
    principal: &PrincipalIdentity,
    local_peer_id: &str,
) -> Result<BTreeMap<String, EnrollmentAuthorityOutcome>> {
    let response = node.execute(STATUS_ENROLLMENT_QUERY).await;
    ensure_no_errors(&response, "load status enrollment approvals")?;
    let mut conflicts = BTreeMap::<String, Vec<String>>::new();
    let mut generational_conflicts = BTreeMap::<String, Vec<(Option<u64>, String)>>::new();
    let mut request_scopes = BTreeMap::<String, (String, String)>::new();
    let mut requests = Vec::new();
    let raw_requests = rows::<Value>(&response, "NetworkEnrollmentRequest")?;
    for raw in &raw_requests {
        let relevant = raw_string(&raw, "candidate_did") == Some(principal.did())
            || raw_string(&raw, "candidate_peer") == Some(local_peer_id);
        if !relevant {
            continue;
        }
        let (Some(request_id), Some(server_peer), Some(network_id)) = (
            raw_string(raw, "request_id"),
            raw_string(raw, "server_peer"),
            raw_string(raw, "network_id"),
        ) else {
            continue;
        };
        let scope = (server_peer.to_string(), network_id.to_string());
        if let Some(previous) = request_scopes.insert(request_id.to_string(), scope.clone()) {
            if previous != scope {
                add_scoped_conflict(
                    &mut conflicts,
                    &previous.0,
                    "request identity is bound to another enrolled server",
                );
                add_scoped_conflict(
                    &mut conflicts,
                    &scope.0,
                    "request identity is bound to another enrolled server",
                );
            }
        }
    }
    for raw in raw_requests {
        let relevant = raw_string(&raw, "candidate_did") == Some(principal.did())
            || raw_string(&raw, "candidate_peer") == Some(local_peer_id);
        if !relevant {
            continue;
        }
        let request_id = raw_string(&raw, "request_id").map(str::to_string);
        let Some(server_peer) = raw_string(&raw, "server_peer")
            .map(str::to_string)
            .or_else(|| {
                request_id
                    .as_ref()
                    .and_then(|request_id| request_scopes.get(request_id))
                    .map(|scope| scope.0.clone())
            })
        else {
            tracing::warn!("quarantining unattributable malformed local enrollment request");
            continue;
        };
        let Some(_network_id) = raw_string(&raw, "network_id") else {
            add_scoped_conflict(
                &mut conflicts,
                &server_peer,
                "local enrollment request has no network_id",
            );
            continue;
        };
        let Some(request_id) = request_id else {
            add_scoped_conflict(
                &mut conflicts,
                &server_peer,
                "local enrollment request has no request_id",
            );
            continue;
        };
        match serde_json::from_value::<EnrollmentRequestRow>(raw) {
            Ok(row) => requests.push(row),
            Err(error) => add_scoped_conflict(
                &mut conflicts,
                &server_peer,
                format!("malformed local enrollment request {request_id}: {error}"),
            ),
        }
    }

    let network_servers = request_scopes.values().fold(
        BTreeMap::<String, Vec<String>>::new(),
        |mut by_network, (server_peer, network_id)| {
            by_network
                .entry(network_id.clone())
                .or_default()
                .push(server_peer.clone());
            by_network
        },
    );
    let mut pins = BTreeMap::<String, Vec<String>>::new();
    for raw in rows::<Value>(&response, "NetworkAdminPin")? {
        let Some(network_id) = raw_string(&raw, "network_id").map(str::to_string) else {
            continue;
        };
        let Some(targets) = network_servers.get(&network_id) else {
            continue;
        };
        match serde_json::from_value::<EnrollmentPinRow>(raw) {
            Ok(row) => pins.entry(row.network_id).or_default().push(row.admin_did),
            Err(error) => {
                for server_peer in targets {
                    add_scoped_conflict(
                        &mut conflicts,
                        server_peer,
                        format!("malformed admin pin for network {network_id}: {error}"),
                    );
                }
            }
        }
    }

    let mut decisions = Vec::new();
    for raw in rows::<Value>(&response, "NetworkEnrollmentDecision")? {
        let targets = raw_authority_targets(
            &raw,
            &request_scopes,
            &network_servers,
            principal.did(),
            local_peer_id,
        );
        if targets.is_empty() {
            continue;
        }
        let sequence = raw
            .get("authorization_sequence")
            .and_then(Value::as_i64)
            .and_then(|value| u64::try_from(value).ok());
        let Some(request_id) = raw_string(&raw, "request_id").map(str::to_string) else {
            for server_peer in targets {
                add_generational_conflict(
                    &mut generational_conflicts,
                    &server_peer,
                    sequence,
                    "malformed enrollment decision has no request_id".to_string(),
                );
            }
            continue;
        };
        match serde_json::from_value::<EnrollmentDecisionRow>(raw) {
            Ok(row) => decisions.push(row),
            Err(error) => {
                for server_peer in targets {
                    add_generational_conflict(
                        &mut generational_conflicts,
                        &server_peer,
                        sequence,
                        format!("malformed decision for request {request_id}: {error}"),
                    );
                }
            }
        }
    }

    let mut revisions = Vec::new();
    for raw in rows::<Value>(&response, "NetworkAuthorizationRevision")? {
        let targets = raw_authority_targets(
            &raw,
            &request_scopes,
            &network_servers,
            principal.did(),
            local_peer_id,
        );
        if targets.is_empty() {
            continue;
        }
        let sequence = raw
            .get("sequence")
            .and_then(Value::as_i64)
            .and_then(|value| u64::try_from(value).ok());
        match serde_json::from_value::<EnrollmentRevisionRow>(raw) {
            Ok(row) if row.to_record().is_ok() => revisions.push(row),
            Ok(row) => {
                let error = row
                    .to_record()
                    .expect_err("invalid revision was preflighted");
                for server_peer in targets {
                    add_generational_conflict(
                        &mut generational_conflicts,
                        &server_peer,
                        sequence,
                        format!("malformed authorization revision: {error}"),
                    );
                }
            }
            Err(error) => {
                for server_peer in targets {
                    add_generational_conflict(
                        &mut generational_conflicts,
                        &server_peer,
                        sequence,
                        format!("malformed authorization revision: {error}"),
                    );
                }
            }
        }
    }

    let mut receipts = Vec::new();
    for raw in rows::<Value>(&response, "NetworkEnrollmentRouteReceipt")? {
        let targets = raw_authority_targets(
            &raw,
            &request_scopes,
            &network_servers,
            principal.did(),
            local_peer_id,
        );
        if targets.is_empty() {
            continue;
        }
        let sequence = raw
            .get("authorization_sequence")
            .and_then(Value::as_i64)
            .and_then(|value| u64::try_from(value).ok());
        match serde_json::from_value::<EnrollmentRouteReceiptRow>(raw) {
            Ok(row) if row.to_record().is_ok() => receipts.push(row),
            Ok(row) => {
                let error = row
                    .to_record()
                    .expect_err("invalid receipt was preflighted");
                for server_peer in targets {
                    add_generational_conflict(
                        &mut generational_conflicts,
                        &server_peer,
                        sequence,
                        format!("malformed enrollment route receipt: {error}"),
                    );
                }
            }
            Err(error) => {
                for server_peer in targets {
                    add_generational_conflict(
                        &mut generational_conflicts,
                        &server_peer,
                        sequence,
                        format!("malformed enrollment route receipt: {error}"),
                    );
                }
            }
        }
    }

    let mut approved = Vec::new();
    for request_row in &requests {
        let server_peer = request_row.server_peer.clone();
        match project_desktop_approval(
            request_row,
            &requests,
            &decisions,
            &revisions,
            &receipts,
            &pins,
            principal,
            local_peer_id,
        )
        .await
        {
            Ok(Some(approval)) => approved.push(approval),
            Ok(None) => {}
            Err(error) => add_scoped_conflict(
                &mut conflicts,
                &server_peer,
                format!("invalid enrollment authority: {error:#}"),
            ),
        }
    }
    apply_current_generational_conflicts(&approved, generational_conflicts, &mut conflicts);
    Ok(scoped_authority_outcomes(approved, conflicts))
}

fn add_generational_conflict(
    conflicts: &mut BTreeMap<String, Vec<(Option<u64>, String)>>,
    server_peer: &str,
    generation: Option<u64>,
    reason: String,
) {
    if !server_peer.is_empty() {
        conflicts
            .entry(server_peer.to_string())
            .or_default()
            .push((generation, reason));
    }
}

fn apply_current_generational_conflicts(
    approved: &[ApprovedStatusEnrollment],
    generational: BTreeMap<String, Vec<(Option<u64>, String)>>,
    conflicts: &mut BTreeMap<String, Vec<String>>,
) {
    let current = approved
        .iter()
        .map(|approval| {
            (
                approval.server_peer.as_str(),
                approval.authorization_sequence,
            )
        })
        .collect::<BTreeMap<_, _>>();
    for (server_peer, witnesses) in generational {
        for (generation, reason) in witnesses {
            let superseded = generation.is_some_and(|generation| {
                current
                    .get(server_peer.as_str())
                    .is_some_and(|current| *current > generation)
            });
            if !superseded {
                add_scoped_conflict(conflicts, &server_peer, reason);
            }
        }
    }
}

fn scoped_authority_outcomes(
    mut approved: Vec<ApprovedStatusEnrollment>,
    mut conflicts: BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, EnrollmentAuthorityOutcome> {
    approved.sort_by(|left, right| {
        (
            &left.server_peer,
            &left.request_digest,
            left.authorization_sequence,
        )
            .cmp(&(
                &right.server_peer,
                &right.request_digest,
                right.authorization_sequence,
            ))
    });
    for pair in approved.windows(2) {
        if pair[0].server_peer == pair[1].server_peer {
            add_scoped_conflict(
                &mut conflicts,
                &pair[0].server_peer,
                "one enrolled server peer has multiple current authorization generations",
            );
        }
    }
    let mut owner_peers = BTreeMap::<String, Vec<String>>::new();
    for approval in &approved {
        owner_peers
            .entry(approval.owner_agent.clone())
            .or_default()
            .push(approval.server_peer.clone());
    }
    for peers in owner_peers.values() {
        if peers
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            > 1
        {
            for peer in peers {
                add_scoped_conflict(
                    &mut conflicts,
                    peer,
                    "one enrolled owner agent has multiple current transport routes",
                );
            }
        }
    }
    let mut outcomes = BTreeMap::new();
    for approval in approved {
        outcomes
            .entry(approval.server_peer.clone())
            .or_insert(EnrollmentAuthorityOutcome::Current(approval));
    }
    for (peer_id, reasons) in conflicts {
        outcomes.insert(
            peer_id,
            EnrollmentAuthorityOutcome::Conflicted {
                reason: reasons.join("; "),
            },
        );
    }
    outcomes
}

fn raw_string<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field)?.as_str()
}

fn add_scoped_conflict(
    conflicts: &mut BTreeMap<String, Vec<String>>,
    server_peer: &str,
    reason: impl Into<String>,
) {
    if server_peer.is_empty() {
        return;
    }
    let reason = reason.into();
    let reasons = conflicts.entry(server_peer.to_string()).or_default();
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

fn raw_authority_targets(
    raw: &Value,
    request_scopes: &BTreeMap<String, (String, String)>,
    network_servers: &BTreeMap<String, Vec<String>>,
    principal_did: &str,
    local_peer_id: &str,
) -> Vec<String> {
    if let Some(request_id) = raw_string(raw, "request_id") {
        if let Some((server_peer, _)) = request_scopes.get(request_id) {
            return vec![server_peer.clone()];
        }
    }
    let local_member = raw_string(raw, "member_did") == Some(principal_did)
        || raw_string(raw, "member_peer") == Some(local_peer_id)
        || raw_string(raw, "candidate_did") == Some(principal_did)
        || raw_string(raw, "candidate_peer") == Some(local_peer_id);
    if !local_member {
        return Vec::new();
    }
    raw_string(raw, "network_id")
        .and_then(|network_id| network_servers.get(network_id))
        .cloned()
        .unwrap_or_default()
}

async fn project_desktop_approval(
    request_row: &EnrollmentRequestRow,
    request_rows: &[EnrollmentRequestRow],
    decision_rows: &[EnrollmentDecisionRow],
    revision_rows: &[EnrollmentRevisionRow],
    receipt_rows: &[EnrollmentRouteReceiptRow],
    pins: &BTreeMap<String, Vec<String>>,
    principal: &PrincipalIdentity,
    local_peer_id: &str,
) -> Result<Option<ApprovedStatusEnrollment>> {
    let request = request_row.to_record()?;
    anyhow::ensure!(
        request.candidate_did == principal.did() && request.candidate_peer == local_peer_id,
        "enrollment request is not owned by this desktop principal and transport"
    );
    let [admin_did] = pins
        .get(&request.network_id)
        .map(Vec::as_slice)
        .unwrap_or_default()
    else {
        anyhow::bail!("network has no unique durable admin pin");
    };
    anyhow::ensure!(
        admin_did == &request.admin_did,
        "request admin does not match the durable network pin"
    );

    let offer =
        decode_offer(&request.offer_token).context("decoding persisted enrollment offer")?;
    request
        .validate_against_offer(&offer)
        .context("validating persisted enrollment request against offer")?;
    anyhow::ensure!(
        offer.schema_fingerprint == enrollment_schema_fingerprint(),
        "persisted enrollment offer has an incompatible schema"
    );
    let (server_ticket_peer, _) = parse_public_peer_addr(&offer.server_ticket)
        .context("persisted enrollment offer contains an invalid server ticket")?;
    anyhow::ensure!(
        server_ticket_peer.to_string() == request.server_peer,
        "persisted server ticket is bound to another transport peer"
    );
    anyhow::ensure!(
        principal
            .verify(&offer.admin_did, &offer.signing_payload(), &offer.admin_sig)
            .await?,
        "persisted enrollment offer signature is invalid"
    );
    anyhow::ensure!(
        principal
            .verify(
                &request.candidate_did,
                &request.signing_payload(),
                &request.candidate_sig,
            )
            .await?,
        "persisted enrollment request signature is invalid"
    );

    let mut durable = DurableEnrollmentDocuments::default();
    let pure_offer = to_pure_offer(&offer, true);
    let pure_request = to_pure_request(&request, true, true);
    durable.offers.insert(pure_offer.clone());
    durable.admin_pins.insert(PureAdminPin {
        network_id: request.network_id.clone(),
        admin_did: admin_did.clone(),
    });
    for row in request_rows {
        let candidate = match row.to_record() {
            Ok(candidate) => candidate,
            Err(error) if row.server_peer == request.server_peer => return Err(error),
            Err(_) => continue,
        };
        let verified = principal
            .verify(
                &candidate.candidate_did,
                &candidate.signing_payload(),
                &candidate.candidate_sig,
            )
            .await;
        let verified = match verified {
            Ok(verified) => verified,
            Err(error) if candidate.server_peer == request.server_peer => return Err(error),
            Err(_) => continue,
        };
        if verified || candidate.server_peer == request.server_peer {
            durable
                .requests
                .insert(to_pure_request(&candidate, verified, true));
        }
    }

    let mut decisions = Vec::new();
    for row in decision_rows
        .iter()
        .filter(|row| row.request_id == request.request_id)
    {
        let decision = row.to_record()?;
        let verified = principal
            .verify(
                &decision.signer_did,
                &decision.signing_payload(),
                &decision.admin_sig,
            )
            .await?;
        durable
            .decisions
            .insert(to_pure_decision(&decision, verified));
        decisions.push((decision, verified));
    }

    for row in revision_rows.iter().filter(|row| {
        row.network_id == request.network_id
            && (row.member_did == principal.did() || row.member_peer == local_peer_id)
    }) {
        let revision = row.to_record()?;
        let verified = principal
            .verify(
                &revision.signer_did,
                &revision.signing_payload(),
                &revision.admin_sig,
            )
            .await
            .unwrap_or(false);
        durable
            .revisions
            .insert(to_pure_revision(&revision, verified));
    }

    let mut receipts = Vec::new();
    for row in receipt_rows.iter().filter(|row| {
        row.network_id == request.network_id
            && row.request_id == request.request_id
            && (row.member_did == principal.did() || row.member_peer == local_peer_id)
    }) {
        let receipt = row.to_record()?;
        let verified = principal
            .verify(
                &receipt.signer_did,
                &receipt.signing_payload(),
                &receipt.admin_sig,
            )
            .await
            .unwrap_or(false);
        durable
            .route_receipts
            .insert(to_pure_receipt(&receipt, verified));
        receipts.push((receipt, verified));
    }

    for (decision, decision_verified) in decisions {
        let pure_decision = to_pure_decision(&decision, decision_verified);
        if !durable.current_approval(&pure_offer, &pure_request, &pure_decision) {
            continue;
        }
        let has_current_receipt = receipts.iter().any(|(receipt, verified)| {
            durable.current_server_route_receipt(
                &pure_offer,
                &pure_request,
                &pure_decision,
                &to_pure_receipt(receipt, *verified),
            )
        });
        if has_current_receipt {
            return Ok(Some(ApprovedStatusEnrollment {
                network_id: request.network_id,
                request_id: request.request_id,
                server_peer: request.server_peer,
                server_ticket: offer.server_ticket,
                admin_did: request.admin_did,
                owner_agent: request.owner_agent,
                request_digest: request.request_digest,
                authorization_sequence: decision.authorization_sequence,
                authorization_expires_at: decision.authorization_expires_at,
                decided_at: decision.decided_at,
            }));
        }
    }
    Ok(None)
}

const STATUS_ENROLLMENT_QUERY: &str = r#"{
  NetworkAdminPin { network_id admin_did }
  NetworkEnrollmentRequest {
    _docID protocol_version request_id request_digest offer_id offer_token challenge network_id
    admin_did server_peer candidate_did candidate_peer candidate_ticket owner_agent profile
    client_nonce issued_at expires_at candidate_sig
  }
  NetworkEnrollmentDecision {
    protocol_version decision_id request_id request_digest network_id admin_did candidate_did
    candidate_peer owner_agent decision authorization_sequence authorization_expires_at
    decided_at signer_did admin_sig
  }
  NetworkAuthorizationRevision {
    protocol_version revision_id request_id request_digest network_id admin_did member_did
    member_peer owner_agent sequence authorization_expires_at kind issued_at signer_did admin_sig
  }
  NetworkEnrollmentRouteReceipt {
    protocol_version receipt_id request_id request_digest network_id admin_did member_did
    member_peer server_peer owner_agent authorization_sequence authorization_expires_at
    direction applied_at signer_did
    admin_sig
  }
}"#;

#[derive(Deserialize)]
struct EnrollmentPinRow {
    network_id: String,
    admin_did: String,
}

#[derive(Deserialize)]
struct EnrollmentRequestRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    protocol_version: i64,
    request_id: String,
    request_digest: String,
    offer_id: String,
    offer_token: String,
    challenge: String,
    network_id: String,
    admin_did: String,
    server_peer: String,
    candidate_did: String,
    candidate_peer: String,
    candidate_ticket: String,
    owner_agent: String,
    profile: String,
    client_nonce: String,
    issued_at: String,
    expires_at: String,
    candidate_sig: String,
}

impl EnrollmentRequestRow {
    fn to_record(&self) -> Result<EnrollmentRequestRecord> {
        Ok(EnrollmentRequestRecord {
            protocol_version: enrollment_version(self.protocol_version)?,
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            offer_id: self.offer_id.clone(),
            offer_token: self.offer_token.clone(),
            challenge: self.challenge.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            server_peer: self.server_peer.clone(),
            candidate_did: self.candidate_did.clone(),
            candidate_peer: self.candidate_peer.clone(),
            candidate_ticket: self.candidate_ticket.clone(),
            owner_agent: self.owner_agent.clone(),
            profile: self.profile.clone(),
            client_nonce: self.client_nonce.clone(),
            issued_at: self.issued_at.clone(),
            expires_at: self.expires_at.clone(),
            candidate_sig: enrollment_signature("request", &self.candidate_sig)?,
        })
    }
}

#[derive(Deserialize)]
struct EnrollmentDecisionRow {
    protocol_version: i64,
    decision_id: String,
    request_id: String,
    request_digest: String,
    network_id: String,
    admin_did: String,
    candidate_did: String,
    candidate_peer: String,
    owner_agent: String,
    decision: String,
    authorization_sequence: i64,
    authorization_expires_at: String,
    decided_at: String,
    signer_did: String,
    admin_sig: String,
}

impl EnrollmentDecisionRow {
    fn to_record(&self) -> Result<EnrollmentDecisionRecord> {
        Ok(EnrollmentDecisionRecord {
            protocol_version: enrollment_version(self.protocol_version)?,
            decision_id: self.decision_id.clone(),
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            candidate_did: self.candidate_did.clone(),
            candidate_peer: self.candidate_peer.clone(),
            owner_agent: self.owner_agent.clone(),
            decision: match self.decision.as_str() {
                "approved" => EnrollmentDecisionKind::Approved,
                "denied" => EnrollmentDecisionKind::Denied,
                other => anyhow::bail!("unknown enrollment decision {other:?}"),
            },
            authorization_sequence: u64::try_from(self.authorization_sequence)
                .context("negative enrollment decision sequence")?,
            authorization_expires_at: self.authorization_expires_at.clone(),
            decided_at: self.decided_at.clone(),
            signer_did: self.signer_did.clone(),
            admin_sig: enrollment_signature("decision", &self.admin_sig)?,
        })
    }
}

#[derive(Deserialize)]
struct EnrollmentRevisionRow {
    protocol_version: i64,
    revision_id: String,
    request_id: String,
    request_digest: String,
    network_id: String,
    admin_did: String,
    member_did: String,
    member_peer: String,
    owner_agent: String,
    sequence: i64,
    authorization_expires_at: String,
    kind: String,
    issued_at: String,
    signer_did: String,
    admin_sig: String,
}

#[derive(Deserialize)]
struct EnrollmentRouteReceiptRow {
    protocol_version: i64,
    receipt_id: String,
    request_id: String,
    request_digest: String,
    network_id: String,
    admin_did: String,
    member_did: String,
    member_peer: String,
    server_peer: String,
    owner_agent: String,
    authorization_sequence: i64,
    authorization_expires_at: String,
    direction: String,
    applied_at: String,
    signer_did: String,
    admin_sig: String,
}

impl EnrollmentRouteReceiptRow {
    fn to_record(&self) -> Result<EnrollmentRouteReceiptRecord> {
        Ok(EnrollmentRouteReceiptRecord {
            protocol_version: enrollment_version(self.protocol_version)?,
            receipt_id: self.receipt_id.clone(),
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            member_did: self.member_did.clone(),
            member_peer: self.member_peer.clone(),
            server_peer: self.server_peer.clone(),
            owner_agent: self.owner_agent.clone(),
            authorization_sequence: u64::try_from(self.authorization_sequence)
                .context("negative enrollment route receipt sequence")?,
            authorization_expires_at: self.authorization_expires_at.clone(),
            direction: match self.direction.as_str() {
                "client_to_server" => EnrollmentRouteReceiptDirection::ClientToServer,
                other => anyhow::bail!("unknown enrollment route receipt direction {other:?}"),
            },
            applied_at: self.applied_at.clone(),
            signer_did: self.signer_did.clone(),
            admin_sig: enrollment_signature("route receipt", &self.admin_sig)?,
        })
    }
}

impl EnrollmentRevisionRow {
    fn to_record(&self) -> Result<AuthorizationRevisionRecord> {
        Ok(AuthorizationRevisionRecord {
            protocol_version: enrollment_version(self.protocol_version)?,
            revision_id: self.revision_id.clone(),
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            network_id: self.network_id.clone(),
            admin_did: self.admin_did.clone(),
            member_did: self.member_did.clone(),
            member_peer: self.member_peer.clone(),
            owner_agent: self.owner_agent.clone(),
            sequence: u64::try_from(self.sequence)
                .context("negative enrollment revision sequence")?,
            authorization_expires_at: self.authorization_expires_at.clone(),
            kind: match self.kind.as_str() {
                "active" => AuthorizationRevisionKind::Active,
                "revoked" => AuthorizationRevisionKind::Revoked,
                other => anyhow::bail!("unknown enrollment revision kind {other:?}"),
            },
            issued_at: self.issued_at.clone(),
            signer_did: self.signer_did.clone(),
            admin_sig: enrollment_signature("revision", &self.admin_sig)?,
        })
    }
}

fn to_pure_offer(
    offer: &gents_protocol::enrollment::EnrollmentOfferRecord,
    verified: bool,
) -> PureOffer {
    PureOffer {
        offer_id: offer.offer_id.clone(),
        challenge: offer.challenge.clone(),
        network_id: offer.network_id.clone(),
        admin_did: offer.admin_did.clone(),
        server_peer: offer.server_peer.clone(),
        server_ticket_peer: offer.server_peer.clone(),
        resolved_server_did: verified
            .then(|| offer.admin_did.clone())
            .unwrap_or_default(),
        owner_agent: offer.owner_agent.clone(),
        profile: offer.profile.clone(),
        schema_compatible: offer.schema_fingerprint == enrollment_schema_fingerprint(),
        admin_signed: verified,
        fresh: verified,
    }
}

fn to_pure_request(request: &EnrollmentRequestRecord, verified: bool, fresh: bool) -> PureRequest {
    PureRequest {
        request_id: request.request_id.clone(),
        digest: request.request_digest.clone(),
        offer_id: request.offer_id.clone(),
        challenge: request.challenge.clone(),
        network_id: request.network_id.clone(),
        admin_did: request.admin_did.clone(),
        server_peer: request.server_peer.clone(),
        candidate_did: request.candidate_did.clone(),
        candidate_peer: request.candidate_peer.clone(),
        observed_candidate_peer: verified
            .then(|| request.candidate_peer.clone())
            .unwrap_or_default(),
        resolved_candidate_did: verified
            .then(|| request.candidate_did.clone())
            .unwrap_or_default(),
        candidate_ticket_peer: request.candidate_peer.clone(),
        owner_agent: request.owner_agent.clone(),
        profile: request.profile.clone(),
        client_nonce: request.client_nonce.clone(),
        issued_at: request.issued_at.clone(),
        expires_at: request.expires_at.clone(),
        candidate_signed: verified,
        fresh,
    }
}

fn to_pure_decision(decision: &EnrollmentDecisionRecord, verified: bool) -> PureDecision {
    PureDecision {
        request_id: decision.request_id.clone(),
        request_digest: decision.request_digest.clone(),
        network_id: decision.network_id.clone(),
        admin_did: decision.admin_did.clone(),
        candidate_did: decision.candidate_did.clone(),
        candidate_peer: decision.candidate_peer.clone(),
        owner_agent: decision.owner_agent.clone(),
        kind: match decision.decision {
            EnrollmentDecisionKind::Approved => PureDecisionKind::Approved,
            EnrollmentDecisionKind::Denied => PureDecisionKind::Denied,
        },
        authorization_sequence: decision.authorization_sequence as usize,
        authorization_expires_at: decision.authorization_expires_at.clone(),
        signer_did: decision.signer_did.clone(),
        admin_signed: verified,
        fresh: verified
            && DateTime::parse_from_rfc3339(&decision.authorization_expires_at)
                .map(|expires| Utc::now() < expires.with_timezone(&Utc))
                .unwrap_or(false),
    }
}

fn to_pure_revision(revision: &AuthorizationRevisionRecord, verified: bool) -> PureRevision {
    PureRevision {
        request_id: revision.request_id.clone(),
        request_digest: revision.request_digest.clone(),
        network_id: revision.network_id.clone(),
        admin_did: revision.admin_did.clone(),
        member_did: revision.member_did.clone(),
        member_peer: revision.member_peer.clone(),
        owner_agent: revision.owner_agent.clone(),
        sequence: revision.sequence as usize,
        authorization_expires_at: revision.authorization_expires_at.clone(),
        kind: match revision.kind {
            AuthorizationRevisionKind::Active => PureRevisionKind::Active,
            AuthorizationRevisionKind::Revoked => PureRevisionKind::Revoked,
        },
        signer_did: revision.signer_did.clone(),
        admin_signed: verified,
    }
}

fn to_pure_receipt(receipt: &EnrollmentRouteReceiptRecord, verified: bool) -> PureRouteReceipt {
    PureRouteReceipt {
        request_id: receipt.request_id.clone(),
        request_digest: receipt.request_digest.clone(),
        network_id: receipt.network_id.clone(),
        admin_did: receipt.admin_did.clone(),
        member_did: receipt.member_did.clone(),
        member_peer: receipt.member_peer.clone(),
        server_peer: receipt.server_peer.clone(),
        owner_agent: receipt.owner_agent.clone(),
        authorization_sequence: receipt.authorization_sequence as usize,
        authorization_expires_at: receipt.authorization_expires_at.clone(),
        direction: PureRouteDirection::ClientToServer,
        signer_did: receipt.signer_did.clone(),
        admin_signed: verified,
        applied: verified,
    }
}

fn enrollment_version(value: i64) -> Result<u8> {
    let value = u8::try_from(value).context("invalid enrollment protocol version")?;
    anyhow::ensure!(
        value == ENROLLMENT_PROTOCOL_VERSION,
        "unsupported enrollment protocol version {value}"
    );
    Ok(value)
}

fn enrollment_signature(kind: &str, value: &str) -> Result<Vec<u8>> {
    let signature = bs58::decode(value)
        .into_vec()
        .with_context(|| format!("decode enrollment {kind} signature"))?;
    anyhow::ensure!(signature.len() == 64, "invalid enrollment {kind} signature");
    Ok(signature)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_mutation_escapes_every_string_and_never_emits_an_empty_array() {
        let request = EnrollmentRequestRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            request_id: "req-\"unsafe".into(),
            request_digest: "digest".into(),
            offer_id: "offer".into(),
            offer_token: "token".into(),
            challenge: "challenge".into(),
            network_id: "network".into(),
            admin_did: "did:key:admin".into(),
            server_peer: "server".into(),
            candidate_did: "did:key:candidate".into(),
            candidate_peer: "candidate".into(),
            candidate_ticket: "ticket".into(),
            owner_agent: "did:key:agent".into(),
            profile: "client".into(),
            client_nonce: "nonce".into(),
            issued_at: "2026-08-29T00:00:00Z".into(),
            expires_at: "2026-08-29T00:05:00Z".into(),
            candidate_sig: vec![1, 2, 3],
        };
        let input = enrollment_request_input(&request);
        assert!(input.contains(r#"request_id: "req-\"unsafe""#));
        assert!(!input.contains("[]"));
    }

    #[test]
    fn offer_window_rejects_expiry_before_issuance() {
        let issued =
            (Utc::now() + chrono::Duration::seconds(20)).to_rfc3339_opts(SecondsFormat::Secs, true);
        let expires =
            (Utc::now() + chrono::Duration::seconds(10)).to_rfc3339_opts(SecondsFormat::Secs, true);
        assert!(validate_fresh_window(&issued, &expires).is_err());
    }

    #[test]
    fn fresh_transport_identity_must_match_the_signed_admin() {
        assert!(validate_authenticated_server_did("did:key:admin", "did:key:admin").is_ok());
        assert!(validate_authenticated_server_did("did:key:admin", "did:key:attacker").is_err());
    }

    fn approval(server_peer: &str, owner_agent: &str) -> ApprovedStatusEnrollment {
        ApprovedStatusEnrollment {
            network_id: "network".into(),
            request_id: format!("request-{server_peer}-{owner_agent}"),
            server_peer: server_peer.into(),
            server_ticket: "ticket".into(),
            admin_did: "did:key:admin".into(),
            owner_agent: owner_agent.into(),
            request_digest: format!("digest-{server_peer}-{owner_agent}"),
            authorization_sequence: 1,
            authorization_expires_at: "2026-09-29T00:00:00Z".into(),
            decided_at: "2026-08-29T00:00:00Z".into(),
        }
    }

    #[test]
    fn enrollment_authentication_prioritizes_newest_unknown_peer() {
        let mut old_unknown = approval("peer-old", "agent-old");
        old_unknown.decided_at = "2026-08-29T00:00:01Z".into();
        let mut active = approval("peer-active", "agent-active");
        active.decided_at = "2026-08-29T00:00:03Z".into();
        let mut known = approval("peer-known", "agent-known");
        known.decided_at = "2026-08-29T00:00:04Z".into();
        let outcomes = BTreeMap::from([
            (
                old_unknown.server_peer.clone(),
                EnrollmentAuthorityOutcome::Current(old_unknown),
            ),
            (
                active.server_peer.clone(),
                EnrollmentAuthorityOutcome::Current(active),
            ),
            (
                known.server_peer.clone(),
                EnrollmentAuthorityOutcome::Current(known),
            ),
        ]);

        let ordered =
            prioritized_current_approvals(&outcomes, &BTreeSet::from(["peer-known".to_string()]))
                .into_iter()
                .map(|(peer_id, _)| peer_id)
                .collect::<Vec<_>>();

        assert_eq!(ordered, vec!["peer-active", "peer-old", "peer-known"]);
    }

    #[test]
    fn current_authority_rejects_transport_and_owner_collisions() {
        let duplicate_peer = scoped_authority_outcomes(
            vec![approval("peer", "agent-a"), approval("peer", "agent-b")],
            BTreeMap::new(),
        );
        assert!(matches!(
            duplicate_peer.get("peer"),
            Some(EnrollmentAuthorityOutcome::Conflicted { .. })
        ));

        let duplicate_owner = scoped_authority_outcomes(
            vec![approval("peer-a", "agent"), approval("peer-b", "agent")],
            BTreeMap::new(),
        );
        assert!(duplicate_owner
            .values()
            .all(|outcome| matches!(outcome, EnrollmentAuthorityOutcome::Conflicted { .. })));

        let distinct = scoped_authority_outcomes(
            vec![approval("peer-a", "agent-a"), approval("peer-b", "agent-b")],
            BTreeMap::new(),
        );
        assert!(distinct
            .values()
            .all(|outcome| matches!(outcome, EnrollmentAuthorityOutcome::Current(_))));
    }

    #[test]
    fn hostile_rows_are_attributed_to_only_the_owned_server_scope() {
        let requests = BTreeMap::from([
            ("request-a".into(), ("peer-a".into(), "network-a".into())),
            ("request-b".into(), ("peer-b".into(), "network-b".into())),
        ]);
        let networks = BTreeMap::from([
            ("network-a".into(), vec!["peer-a".into()]),
            ("network-b".into(), vec!["peer-b".into()]),
        ]);
        let malformed_a = serde_json::json!({
            "request_id": "request-a",
            "network_id": "network-a",
            "member_did": "did:key:local"
        });
        assert_eq!(
            raw_authority_targets(
                &malformed_a,
                &requests,
                &networks,
                "did:key:local",
                "local-peer",
            ),
            ["peer-a"]
        );
        let malformed_decision_without_request_id = serde_json::json!({
            "network_id": "network-a",
            "candidate_did": "did:key:local",
            "candidate_peer": "local-peer",
            "authorization_sequence": 8,
        });
        assert_eq!(
            raw_authority_targets(
                &malformed_decision_without_request_id,
                &requests,
                &networks,
                "did:key:local",
                "local-peer",
            ),
            ["peer-a"]
        );
        let unrelated = serde_json::json!({
            "network_id": "network-z",
            "member_did": "did:key:other"
        });
        assert!(raw_authority_targets(
            &unrelated,
            &requests,
            &networks,
            "did:key:local",
            "local-peer",
        )
        .is_empty());

        let outcomes = scoped_authority_outcomes(
            vec![approval("peer-a", "agent-a"), approval("peer-b", "agent-b")],
            BTreeMap::from([("peer-a".into(), vec!["malformed relevant row".into()])]),
        );
        assert!(matches!(
            outcomes.get("peer-a"),
            Some(EnrollmentAuthorityOutcome::Conflicted { .. })
        ));
        assert!(matches!(
            outcomes.get("peer-b"),
            Some(EnrollmentAuthorityOutcome::Current(_))
        ));
    }

    #[test]
    fn malformed_authority_rows_fail_closed_before_projection() {
        let revision = EnrollmentRevisionRow {
            protocol_version: i64::from(ENROLLMENT_PROTOCOL_VERSION),
            revision_id: "revision".into(),
            request_id: "request".into(),
            request_digest: "digest".into(),
            network_id: "network".into(),
            admin_did: "did:key:admin".into(),
            member_did: "did:key:member".into(),
            member_peer: "member-peer".into(),
            owner_agent: "did:key:agent".into(),
            sequence: -1,
            authorization_expires_at: "2099-09-29T00:00:00Z".into(),
            kind: "active".into(),
            issued_at: "2026-08-29T00:00:00Z".into(),
            signer_did: "did:key:admin".into(),
            admin_sig: bs58::encode([0_u8; 64]).into_string(),
        };
        assert!(revision.to_record().is_err());

        let receipt = EnrollmentRouteReceiptRow {
            protocol_version: i64::from(ENROLLMENT_PROTOCOL_VERSION),
            receipt_id: "receipt".into(),
            request_id: "request".into(),
            request_digest: "digest".into(),
            network_id: "network".into(),
            admin_did: "did:key:admin".into(),
            member_did: "did:key:member".into(),
            member_peer: "member-peer".into(),
            server_peer: "server-peer".into(),
            owner_agent: "did:key:agent".into(),
            authorization_sequence: 1,
            authorization_expires_at: "2099-09-29T00:00:00Z".into(),
            direction: "server_to_client".into(),
            applied_at: "2026-08-29T00:00:00Z".into(),
            signer_did: "did:key:admin".into(),
            admin_sig: bs58::encode([0_u8; 64]).into_string(),
        };
        assert!(receipt.to_record().is_err());
    }

    #[test]
    fn malformed_historical_authority_recovers_only_after_a_higher_generation() {
        let mut conflicts = BTreeMap::new();
        apply_current_generational_conflicts(
            &[ApprovedStatusEnrollment {
                authorization_sequence: 8,
                ..approval("peer-a", "agent-a")
            }],
            BTreeMap::from([(
                "peer-a".into(),
                vec![(Some(7), "old malformed revision".into())],
            )]),
            &mut conflicts,
        );
        assert!(conflicts.is_empty());

        for hostile_generation in [None, Some(8), Some(9)] {
            let mut conflicts = BTreeMap::new();
            apply_current_generational_conflicts(
                &[ApprovedStatusEnrollment {
                    authorization_sequence: 8,
                    ..approval("peer-a", "agent-a")
                }],
                BTreeMap::from([(
                    "peer-a".into(),
                    vec![(hostile_generation, "current malformed revision".into())],
                )]),
                &mut conflicts,
            );
            assert!(conflicts.contains_key("peer-a"));
        }
    }

    #[tokio::test]
    async fn signed_current_receipt_opens_generation_and_revocation_closes_it() {
        use crate::client::core::route_manager::{
            combined_route_readiness, enrollment_remote_route_state, RouteReconcileState,
        };
        use crate::client::paths::DesktopPaths;
        use gents_protocol::enrollment::{
            derive_decision_id, derive_enrollment_id, derive_revision_id, derive_route_receipt_id,
            encode_offer, EnrollmentOfferRecord,
        };

        let temp = tempfile::tempdir().unwrap();
        let admin =
            PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("admin")))
                .await
                .unwrap();
        let candidate = PrincipalIdentity::load_or_create(&DesktopPaths::from_root(
            temp.path().join("candidate"),
        ))
        .await
        .unwrap();
        let server_ticket =
            "127.0.0.1:56000/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
        let server_peer = parse_public_peer_addr(server_ticket).unwrap().0.to_string();
        let issued = Utc::now();
        let issued_at = issued.to_rfc3339_opts(SecondsFormat::Secs, true);
        let request_expires_at =
            (issued + chrono::Duration::minutes(5)).to_rfc3339_opts(SecondsFormat::Secs, true);
        let authorization_expires_at =
            (issued + chrono::Duration::days(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut offer = EnrollmentOfferRecord {
            version: ENROLLMENT_PROTOCOL_VERSION,
            offer_id: "offer-1".into(),
            challenge: "challenge-1".into(),
            network_id: "network-1".into(),
            admin_did: admin.did().into(),
            server_peer: server_peer.clone(),
            server_ticket: server_ticket.into(),
            owner_agent: "did:key:owner".into(),
            profile: "client".into(),
            schema_fingerprint: enrollment_schema_fingerprint(),
            issued_at: issued_at.clone(),
            expires_at: request_expires_at.clone(),
            admin_sig: Vec::new(),
        };
        offer.admin_sig = admin.sign(&offer.signing_payload()).unwrap();
        let offer_token = encode_offer(&offer).unwrap();

        let client_nonce = "nonce-1";
        let request_id = format!(
            "enroll-{}",
            derive_enrollment_id(
                "gents-enrollment-request-id-v1",
                &[
                    &offer.offer_id,
                    candidate.did(),
                    "client-peer",
                    client_nonce,
                ],
            )
        );
        let mut request = EnrollmentRequestRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            request_id,
            request_digest: String::new(),
            offer_id: offer.offer_id.clone(),
            offer_token: offer_token.clone(),
            challenge: offer.challenge.clone(),
            network_id: offer.network_id.clone(),
            admin_did: offer.admin_did.clone(),
            server_peer: server_peer.clone(),
            candidate_did: candidate.did().into(),
            candidate_peer: "client-peer".into(),
            candidate_ticket: "client-ticket".into(),
            owner_agent: offer.owner_agent.clone(),
            profile: offer.profile.clone(),
            client_nonce: client_nonce.into(),
            issued_at: issued_at.clone(),
            expires_at: request_expires_at,
            candidate_sig: Vec::new(),
        };
        request.request_digest = request.computed_digest();
        request.candidate_sig = candidate.sign(&request.signing_payload()).unwrap();

        let mut decision = EnrollmentDecisionRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            decision_id: derive_decision_id(&request.request_id, &request.request_digest),
            request_id: request.request_id.clone(),
            request_digest: request.request_digest.clone(),
            network_id: request.network_id.clone(),
            admin_did: request.admin_did.clone(),
            candidate_did: request.candidate_did.clone(),
            candidate_peer: request.candidate_peer.clone(),
            owner_agent: request.owner_agent.clone(),
            decision: EnrollmentDecisionKind::Approved,
            authorization_sequence: 1,
            authorization_expires_at: authorization_expires_at.clone(),
            decided_at: issued_at.clone(),
            signer_did: admin.did().into(),
            admin_sig: Vec::new(),
        };
        decision.admin_sig = admin.sign(&decision.signing_payload()).unwrap();
        let mut revision = AuthorizationRevisionRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            revision_id: derive_revision_id(
                &request.network_id,
                &request.candidate_did,
                1,
                &AuthorizationRevisionKind::Active,
                &request.request_digest,
            ),
            request_id: request.request_id.clone(),
            request_digest: request.request_digest.clone(),
            network_id: request.network_id.clone(),
            admin_did: request.admin_did.clone(),
            member_did: request.candidate_did.clone(),
            member_peer: request.candidate_peer.clone(),
            owner_agent: request.owner_agent.clone(),
            sequence: 1,
            authorization_expires_at: authorization_expires_at.clone(),
            kind: AuthorizationRevisionKind::Active,
            issued_at: issued_at.clone(),
            signer_did: admin.did().into(),
            admin_sig: Vec::new(),
        };
        revision.admin_sig = admin.sign(&revision.signing_payload()).unwrap();
        let direction = EnrollmentRouteReceiptDirection::ClientToServer;
        let mut receipt = EnrollmentRouteReceiptRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            receipt_id: derive_route_receipt_id(
                &request.request_id,
                &request.request_digest,
                1,
                &direction,
            ),
            request_id: request.request_id.clone(),
            request_digest: request.request_digest.clone(),
            network_id: request.network_id.clone(),
            admin_did: request.admin_did.clone(),
            member_did: request.candidate_did.clone(),
            member_peer: request.candidate_peer.clone(),
            server_peer: request.server_peer.clone(),
            owner_agent: request.owner_agent.clone(),
            authorization_sequence: 1,
            authorization_expires_at: authorization_expires_at.clone(),
            direction,
            applied_at: issued_at.clone(),
            signer_did: admin.did().into(),
            admin_sig: Vec::new(),
        };
        receipt.admin_sig = admin.sign(&receipt.signing_payload()).unwrap();

        let request_row = EnrollmentRequestRow {
            doc_id: "request-doc".into(),
            protocol_version: i64::from(ENROLLMENT_PROTOCOL_VERSION),
            request_id: request.request_id.clone(),
            request_digest: request.request_digest.clone(),
            offer_id: request.offer_id.clone(),
            offer_token,
            challenge: request.challenge.clone(),
            network_id: request.network_id.clone(),
            admin_did: request.admin_did.clone(),
            server_peer: request.server_peer.clone(),
            candidate_did: request.candidate_did.clone(),
            candidate_peer: request.candidate_peer.clone(),
            candidate_ticket: request.candidate_ticket.clone(),
            owner_agent: request.owner_agent.clone(),
            profile: request.profile.clone(),
            client_nonce: request.client_nonce.clone(),
            issued_at: request.issued_at.clone(),
            expires_at: request.expires_at.clone(),
            candidate_sig: bs58::encode(&request.candidate_sig).into_string(),
        };
        let decision_row = EnrollmentDecisionRow {
            protocol_version: i64::from(ENROLLMENT_PROTOCOL_VERSION),
            decision_id: decision.decision_id.clone(),
            request_id: decision.request_id.clone(),
            request_digest: decision.request_digest.clone(),
            network_id: decision.network_id.clone(),
            admin_did: decision.admin_did.clone(),
            candidate_did: decision.candidate_did.clone(),
            candidate_peer: decision.candidate_peer.clone(),
            owner_agent: decision.owner_agent.clone(),
            decision: "approved".into(),
            authorization_sequence: 1,
            authorization_expires_at: authorization_expires_at.clone(),
            decided_at: decision.decided_at.clone(),
            signer_did: decision.signer_did.clone(),
            admin_sig: bs58::encode(&decision.admin_sig).into_string(),
        };
        let revision_row = |record: &AuthorizationRevisionRecord| EnrollmentRevisionRow {
            protocol_version: i64::from(ENROLLMENT_PROTOCOL_VERSION),
            revision_id: record.revision_id.clone(),
            request_id: record.request_id.clone(),
            request_digest: record.request_digest.clone(),
            network_id: record.network_id.clone(),
            admin_did: record.admin_did.clone(),
            member_did: record.member_did.clone(),
            member_peer: record.member_peer.clone(),
            owner_agent: record.owner_agent.clone(),
            sequence: record.sequence as i64,
            authorization_expires_at: record.authorization_expires_at.clone(),
            kind: record.kind.as_str().into(),
            issued_at: record.issued_at.clone(),
            signer_did: record.signer_did.clone(),
            admin_sig: bs58::encode(&record.admin_sig).into_string(),
        };
        let receipt_row = EnrollmentRouteReceiptRow {
            protocol_version: i64::from(ENROLLMENT_PROTOCOL_VERSION),
            receipt_id: receipt.receipt_id.clone(),
            request_id: receipt.request_id.clone(),
            request_digest: receipt.request_digest.clone(),
            network_id: receipt.network_id.clone(),
            admin_did: receipt.admin_did.clone(),
            member_did: receipt.member_did.clone(),
            member_peer: receipt.member_peer.clone(),
            server_peer: receipt.server_peer.clone(),
            owner_agent: receipt.owner_agent.clone(),
            authorization_sequence: 1,
            authorization_expires_at: authorization_expires_at.clone(),
            direction: "client_to_server".into(),
            applied_at: receipt.applied_at.clone(),
            signer_did: receipt.signer_did.clone(),
            admin_sig: bs58::encode(&receipt.admin_sig).into_string(),
        };
        let pins = BTreeMap::from([("network-1".into(), vec![admin.did().into()])]);

        assert!(project_desktop_approval(
            &request_row,
            std::slice::from_ref(&request_row),
            std::slice::from_ref(&decision_row),
            std::slice::from_ref(&revision_row(&revision)),
            &[],
            &pins,
            &candidate,
            "client-peer",
        )
        .await
        .unwrap()
        .is_none());
        let approved = project_desktop_approval(
            &request_row,
            std::slice::from_ref(&request_row),
            std::slice::from_ref(&decision_row),
            std::slice::from_ref(&revision_row(&revision)),
            std::slice::from_ref(&receipt_row),
            &pins,
            &candidate,
            "client-peer",
        )
        .await
        .unwrap()
        .expect("signed current receipt opens exact generation");
        assert_eq!(approved.request_digest, request.request_digest);
        assert_eq!(approved.authorization_sequence, 1);
        assert_eq!(approved.authorization_expires_at, authorization_expires_at);

        let (_directory, sync_state) = ClientSyncStateOwner::for_test(Vec::new(), Vec::new()).await;
        let configured = sync_state
            .upsert_enrollment_peer(
                &approved.server_peer,
                "Enrolled Agent",
                &approved.server_ticket,
                &approved.owner_agent,
                &approved.network_id,
                &approved.request_id,
                &approved.request_digest,
                &approved.admin_did,
                approved.authorization_sequence,
                &approved.authorization_expires_at,
            )
            .await
            .unwrap();
        assert!(!configured.is_chat_ready_at(Utc::now()));
        let ready = combined_route_readiness(
            RouteReconcileState::Ready,
            enrollment_remote_route_state(true),
        )
        .expect("exact local applied evidence plus current receipt is decisive");
        let ready_record = sync_state
            .set_pairing_ready(&configured, ready)
            .await
            .unwrap()
            .expect("approved generation remains configured");
        assert!(
            ready_record.is_chat_ready_at(Utc::now()),
            "send gate opens only after both legs"
        );
        let stable = sync_state
            .upsert_enrollment_peer(
                &approved.server_peer,
                "Enrolled Agent",
                &approved.server_ticket,
                &approved.owner_agent,
                &approved.network_id,
                &approved.request_id,
                &approved.request_digest,
                &approved.admin_did,
                approved.authorization_sequence,
                &approved.authorization_expires_at,
            )
            .await
            .unwrap();
        assert!(
            stable.pairing_ready,
            "exact idempotent generation stays ready"
        );

        let mut revoked = revision;
        revoked.sequence = 2;
        revoked.kind = AuthorizationRevisionKind::Revoked;
        revoked.revision_id = derive_revision_id(
            &request.network_id,
            &request.candidate_did,
            2,
            &revoked.kind,
            &request.request_digest,
        );
        revoked.admin_sig = admin.sign(&revoked.signing_payload()).unwrap();
        assert!(project_desktop_approval(
            &request_row,
            std::slice::from_ref(&request_row),
            std::slice::from_ref(&decision_row),
            &[revision_row(&revoked)],
            std::slice::from_ref(&receipt_row),
            &pins,
            &candidate,
            "client-peer",
        )
        .await
        .unwrap()
        .is_none());
        demote_enrollment_peer(&sync_state, &approved.server_peer).await;
        let revoked_record = sync_state.records().into_iter().next().unwrap();
        assert!(
            !revoked_record.is_chat_ready_at(Utc::now()),
            "revocation closes the send gate"
        );
        let replacement = sync_state
            .upsert_enrollment_peer(
                &approved.server_peer,
                "Enrolled Agent",
                &approved.server_ticket,
                &approved.owner_agent,
                &approved.network_id,
                "replacement-request-id",
                "replacement-request-digest",
                &approved.admin_did,
                3,
                "2099-10-29T00:00:00Z",
            )
            .await
            .unwrap();
        assert!(
            !replacement.is_chat_ready_at(Utc::now()),
            "a new authorization generation must prove both route legs again"
        );
    }

    fn request_row(doc_id: &str) -> EnrollmentRequestRow {
        EnrollmentRequestRow {
            doc_id: doc_id.into(),
            protocol_version: i64::from(ENROLLMENT_PROTOCOL_VERSION),
            request_id: "request".into(),
            request_digest: "digest".into(),
            offer_id: "offer".into(),
            offer_token: "token".into(),
            challenge: "challenge".into(),
            network_id: "network".into(),
            admin_did: "did:key:admin".into(),
            server_peer: "server-peer".into(),
            candidate_did: "did:key:client".into(),
            candidate_peer: "client-peer".into(),
            candidate_ticket: "ticket".into(),
            owner_agent: "did:key:agent".into(),
            profile: "client".into(),
            client_nonce: "nonce".into(),
            issued_at: "2026-08-29T00:00:00Z".into(),
            expires_at: "2026-08-29T00:05:00Z".into(),
            candidate_sig: bs58::encode([0_u8; 64]).into_string(),
        }
    }

    #[test]
    fn retry_reuses_one_exact_persisted_request_and_rejects_duplicates() {
        let persisted = request_row("doc-exact");
        let selected = select_retryable_local_request(
            std::slice::from_ref(&persisted),
            "did:key:client",
            "client-peer",
            "offer",
        )
        .expect("one persisted request")
        .expect("selected request");
        assert_eq!(selected.doc_id, "doc-exact");
        assert_eq!(selected.request_id, "request");

        let duplicate = request_row("doc-conflict");
        assert!(select_retryable_local_request(
            &[persisted, duplicate],
            "did:key:client",
            "client-peer",
            "offer",
        )
        .is_err());
    }

    #[test]
    fn revocation_removes_enrollment_from_the_current_authority_set() {
        let mut enrollment =
            crate::client::peer_directory::PeerRecord::new("Enrollment", "endpoint", "did");
        enrollment.source = Some("enrollment".into());
        enrollment.enrollment_request_digest = Some("digest".into());
        enrollment.enrollment_authorization_sequence = Some(1);
        enrollment.enrollment_authorization_expires_at = Some("2099-09-29T00:00:00Z".into());
        let authority = BTreeMap::from([(
            enrollment.peer_id.clone(),
            EnrollmentAuthorizationGeneration {
                request_digest: "digest".into(),
                sequence: 1,
                expires_at: "2099-09-29T00:00:00Z".into(),
            },
        )]);
        assert!(!enrollment_record_lacks_current_authority(
            &enrollment,
            &authority
        ));
        assert!(enrollment_record_lacks_current_authority(
            &enrollment,
            &BTreeMap::new()
        ));
    }
}
