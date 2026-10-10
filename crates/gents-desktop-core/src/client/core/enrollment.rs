use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use defra_node::QueryResponse;
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
use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry, rows};
use gents::NodeIdentity;
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
use super::sync_state::{ClientSyncStateOwner, RuntimeSchemaObservation};
use super::{ClientCore, P2P_OPERATION_TIMEOUT};
use crate::client::peer_directory::RemovalCause;

pub(super) async fn current_local_endpoint(
    p2p: &Arc<dyn P2POps>,
    identity: &dyn NodeIdentity,
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
    pub owner_node: String,
    pub state: String,
    pub expires_at: String,
}

#[derive(Deserialize)]
struct AdminPinRow {
    admin_did: String,
}

impl ClientCore {
    /// Start observing runtime `runtime_did` through its configured route at
    /// `endpoint`; call before fetching that endpoint's `/status`. `None` when
    /// no current route of that runtime uses `endpoint`.
    pub fn begin_runtime_schema_observation(
        &self,
        runtime_did: &str,
        endpoint: &str,
    ) -> Option<RuntimeSchemaObservation> {
        self.sync_state
            .begin_runtime_schema_observation(runtime_did, Some(endpoint))
    }

    /// Compare the fetched `status` with this node's replicated collection
    /// versions; a mismatch on a configured peer stays visible in sync health.
    pub async fn finish_runtime_schema_observation(
        &self,
        observation: &RuntimeSchemaObservation,
        status: &Value,
    ) -> Result<()> {
        self.sync_state
            .finish_runtime_schema_observation(&self.node, observation, status)
            .await
            .context("runtime cannot sync with this app")
    }

    /// Author an enrollment request from a runtime's `/status` payload.
    pub async fn request_status_enrollment(
        &self,
        status: &Value,
    ) -> Result<EnrollmentRequestResult> {
        self.request_status_enrollment_with_label(status, None)
            .await
    }

    pub async fn request_status_enrollment_with_label(
        &self,
        status: &Value,
        advertised_label: Option<&str>,
    ) -> Result<EnrollmentRequestResult> {
        let offer_token = status
            .pointer("/enrollment/token")
            .and_then(Value::as_str)
            .filter(|token| !token.trim().is_empty())
            .context("server does not advertise authenticated status enrollment")?;
        let offer = decode_offer(offer_token).context("decoding server enrollment offer")?;
        self.sync_state
            .compare_runtime_schema(&self.node, &offer.owner_agent, status)
            .await
            .context("refusing to enroll with an incompatible runtime")?;
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
            self.node_identity
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
                            self.node_identity.did(),
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
                    candidate_did: self.node_identity.did().to_string(),
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
                request.candidate_sig = self.node_identity.sign(&request.signing_payload())?;
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
        if let Some(label) = advertised_label {
            self.sync_state
                .remember_enrollment_label(&offer.server_peer, label);
        }

        Ok(EnrollmentRequestResult {
            request_id: request.request_id,
            network_id: request.network_id,
            admin_did: request.admin_did,
            server_peer: request.server_peer,
            owner_node: request.owner_agent,
            state: "pending_approval".to_string(),
            expires_at: request.expires_at,
        })
    }

    pub async fn active_status_enrollment_requests(&self) -> Result<Vec<EnrollmentRequestResult>> {
        let response = graphql_with_transaction_retry(
            &self.node,
            STATUS_ENROLLMENT_QUERY,
            "load desktop enrollment requests",
        )
        .await?;
        let pins = rows::<EnrollmentPinRow>(&response, "NetworkAdminPin")?
            .into_iter()
            .fold(BTreeMap::<String, Vec<String>>::new(), |mut pins, row| {
                pins.entry(row.network_id).or_default().push(row.admin_did);
                pins
            });
        let decisions = rows::<EnrollmentDecisionRow>(&response, "NetworkEnrollmentDecision")?;
        let revisions = listing_revision_rows(&response)?;
        let request_rows = rows::<EnrollmentRequestRow>(&response, "NetworkEnrollmentRequest")?;
        let retired = self.sync_state.retired_enrollment_digests().await;
        let assembly_requests = listing_assembly_requests(&request_rows, &self.node_identity).await;
        let mut active = Vec::new();

        for row in &request_rows {
            if row.candidate_did != self.node_identity.did()
                || row.candidate_peer != self.local_peer_id
            {
                continue;
            }
            if retired.contains(&row.request_digest) {
                continue;
            }
            let request = row.to_record()?;
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
            let offer = decode_offer(&request.offer_token)
                .context("decoding persisted enrollment offer")?;
            request.validate_against_offer(&offer)?;
            anyhow::ensure!(
                offer.schema_fingerprint == enrollment_schema_fingerprint(),
                "persisted enrollment offer has an incompatible schema"
            );
            anyhow::ensure!(
                self.node_identity
                    .verify(&offer.admin_did, &offer.signing_payload(), &offer.admin_sig)
                    .await?,
                "persisted enrollment offer signature is invalid"
            );
            anyhow::ensure!(
                self.node_identity
                    .verify(
                        &request.candidate_did,
                        &request.signing_payload(),
                        &request.candidate_sig,
                    )
                    .await?,
                "persisted enrollment request signature is invalid"
            );
            let expires_at = DateTime::parse_from_rfc3339(&request.expires_at)
                .context("parsing enrollment request expiry")?
                .with_timezone(&Utc);
            if expires_at <= Utc::now() {
                continue;
            }

            let projection = assemble_durable_enrollment_documents(
                &offer,
                &request,
                admin_did,
                &assembly_requests,
                &decisions,
                &revisions,
                &self.node_identity,
                &self.local_peer_id,
            )
            .await?;
            anyhow::ensure!(
                projection.decisions.len() <= 1,
                "enrollment request has multiple authenticated decisions"
            );
            for (decision, _) in &projection.decisions {
                decision.validate_against_request(&request)?;
            }

            let state = match projection.decisions.as_slice() {
                [] => "pending_approval",
                [(decision, _)] if decision.decision == EnrollmentDecisionKind::Denied => continue,
                [(decision, verified)] => {
                    let pure_decision = to_pure_decision(decision, *verified);
                    if !projection.documents.current_approval(
                        &projection.offer,
                        &projection.request,
                        &pure_decision,
                    ) {
                        // The approval is no longer current (revoked or
                        // lease-expired), exactly as the reconciler reads it.
                        continue;
                    }
                    "approved"
                }
                _ => unreachable!("multiple authenticated decisions fail closed above"),
            };
            if self.sync_state.records().iter().any(|peer| {
                peer.enrollment_request_id.as_deref() == Some(&request.request_id)
                    && peer.is_chat_ready_at(Utc::now())
            }) {
                continue;
            }
            active.push(EnrollmentRequestResult {
                request_id: request.request_id,
                network_id: request.network_id,
                admin_did: request.admin_did,
                server_peer: request.server_peer,
                owner_node: request.owner_agent,
                state: state.to_string(),
                expires_at: request.expires_at,
            });
        }
        active.sort_by(|left, right| left.request_id.cmp(&right.request_id));
        Ok(active)
    }

    /// Pushes one of this desktop's persisted requests to its server again. A
    /// request is written locally before it is pushed, so one whose push
    /// failed stays pending without the server ever seeing it.
    pub async fn resend_status_enrollment(&self, request_id: &str) -> Result<()> {
        let escaped = escape_graphql_string(request_id);
        let query = format!(
            r#"{{ NetworkEnrollmentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}) {{
                _docID protocol_version request_id request_digest offer_id offer_token challenge
                network_id admin_did server_peer candidate_did candidate_peer candidate_ticket
                owner_agent profile client_nonce issued_at expires_at candidate_sig
            }} }}"#
        );
        let response = graphql_with_transaction_retry(
            &self.node,
            &query,
            "loading enrollment request to resend",
        )
        .await?;
        let rows = rows::<EnrollmentRequestRow>(&response, "NetworkEnrollmentRequest")?;
        let row = select_retryable_local_request(
            &rows,
            self.node_identity.did(),
            &self.local_peer_id,
            request_id,
        )?
        .with_context(|| format!("no local enrollment request {request_id}"))?;
        let offer_token = row.to_record()?.offer_token;
        let offer = decode_offer(&offer_token).context("decoding persisted enrollment offer")?;
        let (request, document_id) = self
            .existing_request_for_offer(&offer, &offer_token, &self.local_peer_id)
            .await?
            .with_context(|| format!("no local enrollment request {request_id}"))?;
        anyhow::ensure!(
            request.request_id == request_id,
            "enrollment request {request_id} is not the request persisted for its offer"
        );
        push_enrollment_request(&self.p2p, &offer, &request.request_id, &document_id).await
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
        let response = graphql_with_transaction_retry(
            &self.node,
            &query,
            "loading local enrollment admin pin",
        )
        .await?;
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
        let committed = gents::config_client::ConfigAccess::write_local(
            self.node.as_ref(),
            "desktop.enrollment.create_admin_pin",
            &mutation,
        )
        .await;
        match committed {
            Ok(_) => Ok(()),
            Err(commit_error) => {
                let response = graphql_with_transaction_retry(
                    &self.node,
                    &query,
                    "recovering local enrollment admin pin",
                )
                .await?;
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
        let response = gents::config_client::ConfigAccess::write_local(
            self.node.as_ref(),
            "desktop.enrollment.create_request",
            &mutation,
        )
        .await?;
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
        let response = graphql_with_transaction_retry(
            &self.node,
            &query,
            "loading retryable enrollment request",
        )
        .await?;
        let rows = rows::<EnrollmentRequestRow>(&response, "NetworkEnrollmentRequest")?;
        let Some(row) = select_retryable_local_request(
            &rows,
            self.node_identity.did(),
            candidate_peer,
            &offer.offer_id,
        )?
        else {
            return Ok(None);
        };
        // A locally retired request stays durable, so a second request under
        // this offer would share its challenge and neither could be approved.
        anyhow::ensure!(
            !self
                .sync_state
                .retired_enrollment_digests()
                .await
                .contains(&row.request_digest),
            "enrollment request {} for offer {} was removed on this desktop; fetch a fresh \
             offer from the server's /status to enroll again",
            row.request_id,
            offer.offer_id
        );
        let doc_id = row.doc_id.clone();
        let request = row.to_record()?;
        anyhow::ensure!(
            request.offer_token == offer_token,
            "persisted enrollment request embeds a different signed offer"
        );
        request.validate_against_offer(offer)?;
        anyhow::ensure!(
            self.node_identity
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
    owner_node: String,
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

/// The durable projection of one local enrollment request. A current
/// decision and revision keep the authorization generation current whether
/// or not a current route receipt opens the transport route.
enum DesktopApprovalProjection {
    Routed(ApprovedStatusEnrollment),
    CurrentWithoutRoute,
    Absent,
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
    let (outcomes, authorized_server_peers) =
        load_status_enrollment_approvals(node.as_ref(), principal.as_ref(), local_peer_id).await?;
    // The prune keys on authority presence, not on the receipt-gated
    // outcomes: a missing route receipt must not end a retirement whose
    // decision and revision still read current, or the peer it removed
    // could be reinstalled once the receipt observation returns. Conflicted
    // scopes keep their retirements too.
    let mut authority_server_peers = authorized_server_peers;
    authority_server_peers.extend(outcomes.keys().cloned());
    if let Err(error) = sync_state
        .prune_retired_enrollments(&authority_server_peers)
        .await
    {
        tracing::warn!(error = %error, "failed to prune retired enrollment generations");
    }
    let known_records = sync_state
        .records()
        .into_iter()
        .map(|record| (record.peer_id.clone(), record))
        .collect::<BTreeMap<_, _>>();
    let known_peers = known_records.keys().cloned().collect();
    let retired = sync_state.retired_enrollment_digests().await;
    let mut approvals = prioritized_current_approvals(&outcomes, &known_peers);
    // A locally removed generation stays removed: the durable documents still
    // read as current for the whole lease, so without this filter every tick
    // would reinstall and dial the server the user removed.
    approvals.retain(|(_, approval)| !retired.contains(&approval.request_digest));
    let mut authentications = stream::iter(approvals)
        .map(|(peer_id, approval)| {
            let p2p = Arc::clone(p2p);
            let known = known_records.get(&peer_id).cloned();
            async move {
                let result = authenticate_enrolled_server(&p2p, &approval, known.as_ref()).await;
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
            .remove_peer(sync_state, &existing.peer_id, RemovalCause::Reconciliation)
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
        let address = match authentication {
            Ok(address) => address,
            Err(error) => {
                tracing::warn!(peer_id, request_id = %approval.request_id, error = %error, "enrollment peer is temporarily unavailable");
                demote_enrollment_peer(sync_state, &peer_id).await;
                continue;
            }
        };
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
            let label = sync_state
                .advertised_enrollment_label(&approval.server_peer)
                .unwrap_or_else(|| "Enrolled Agent".to_string());
            let record = sync_state
                .upsert_enrollment_peer(
                    &approval.server_peer,
                    &label,
                    &address,
                    &approval.owner_node,
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
    known: Option<&super::super::peer_directory::PeerRecord>,
) -> Result<String> {
    let started = std::time::Instant::now();
    let mut stage = "discovering enrolled server address";
    let result = timeout(P2P_OPERATION_TIMEOUT, async {
        let address = enrolled_server_address(approval, known).await?;
        let peer = TransportPeerId::new(approval.server_peer.clone()).map_err(map_p2p_error)?;
        stage = "checking enrolled server connection";
        if !super::bootstrap::is_connected_peer(p2p, peer.as_str()).await? {
            stage = "connecting to enrolled server";
            p2p.connect_peer(&address).await.map_err(map_p2p_error)?;
        }
        stage = "resolving enrolled server identity";
        let resolved = p2p
            .resolve_peer_identity(&peer)
            .await
            .map_err(map_p2p_error)?
            .context("enrolled server has no authenticated transport identity")?;
        validate_authenticated_server_did(&approval.admin_did, &resolved.to_string())?;
        Ok(address)
    })
    .await;
    result.with_context(|| {
        format!(
            "timed out re-authenticating enrolled server while {stage} after {} ms",
            started.elapsed().as_millis()
        )
    })?
}

/// A co-hosted runtime can change its socket address while retaining its
/// approved transport identity. Discovery supplies location only: the ticket
/// must name the approved peer, and authentication still checks the signed admin.
async fn enrolled_server_address(
    approval: &ApprovedStatusEnrollment,
    known: Option<&super::super::peer_directory::PeerRecord>,
) -> Result<String> {
    let Some(known) = known.filter(|record| record.is_enrollment() && record.is_managed_runtime())
    else {
        return Ok(approval.server_ticket.clone());
    };
    let home = known
        .local_node_home
        .as_deref()
        .context("managed enrollment has no local home")?;
    let live = crate::local_runtime::discover_standard_runtime(std::path::Path::new(home)).await?;
    let (ticket_peer, _) = parse_public_peer_addr(&live.p2p_listen_address)
        .map_err(|error| anyhow::anyhow!("managed runtime has an invalid P2P address: {error}"))?;
    anyhow::ensure!(
        live.node_did == approval.owner_node
            && live.p2p_peer_id == approval.server_peer
            && ticket_peer.to_string() == approval.server_peer,
        "discovered managed runtime identity does not match approved enrollment"
    );
    Ok(live.p2p_listen_address)
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

/// Loads the scoped enrollment authority. The outcomes are receipt-gated
/// (an approval only counts once its route receipt is current); the
/// returned server peers are authority-gated instead, so a retirement can
/// be held against the generation it named even while the receipt
/// observation is empty.
async fn load_status_enrollment_approvals(
    node: &defra_node::EmbeddedNode,
    principal: &PrincipalIdentity,
    local_peer_id: &str,
) -> Result<(
    BTreeMap<String, EnrollmentAuthorityOutcome>,
    BTreeSet<String>,
)> {
    let response = graphql_with_transaction_retry(
        node,
        STATUS_ENROLLMENT_QUERY,
        "load status enrollment approvals",
    )
    .await?;
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
    let mut authorized_server_peers = BTreeSet::new();
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
            Ok(DesktopApprovalProjection::Routed(approval)) => {
                authorized_server_peers.insert(server_peer);
                approved.push(approval);
            }
            Ok(DesktopApprovalProjection::CurrentWithoutRoute) => {
                authorized_server_peers.insert(server_peer);
            }
            Ok(DesktopApprovalProjection::Absent) => {}
            Err(error) => add_scoped_conflict(
                &mut conflicts,
                &server_peer,
                format!("invalid enrollment authority: {error:#}"),
            ),
        }
    }
    apply_current_generational_conflicts(&approved, generational_conflicts, &mut conflicts);
    Ok((
        scoped_authority_outcomes(approved, conflicts),
        authorized_server_peers,
    ))
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
            .entry(approval.owner_node.clone())
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
                    "one enrolled owner node has multiple current transport routes",
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
) -> Result<DesktopApprovalProjection> {
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

    let mut projection = assemble_durable_enrollment_documents(
        &offer,
        &request,
        admin_did,
        request_rows,
        decision_rows,
        revision_rows,
        principal,
        local_peer_id,
    )
    .await?;

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
        projection
            .documents
            .route_receipts
            .insert(to_pure_receipt(&receipt, verified));
        receipts.push((receipt, verified));
    }

    let mut current_without_route = false;
    for (decision, decision_verified) in projection.decisions {
        let pure_decision = to_pure_decision(&decision, decision_verified);
        if !projection.documents.current_approval(
            &projection.offer,
            &projection.request,
            &pure_decision,
        ) {
            continue;
        }
        let has_current_receipt = receipts.iter().any(|(receipt, verified)| {
            projection.documents.current_server_route_receipt(
                &projection.offer,
                &projection.request,
                &pure_decision,
                &to_pure_receipt(receipt, *verified),
            )
        });
        if has_current_receipt {
            return Ok(DesktopApprovalProjection::Routed(
                ApprovedStatusEnrollment {
                    network_id: request.network_id,
                    request_id: request.request_id,
                    server_peer: request.server_peer,
                    server_ticket: offer.server_ticket,
                    admin_did: request.admin_did,
                    owner_node: request.owner_agent,
                    request_digest: request.request_digest,
                    authorization_sequence: decision.authorization_sequence,
                    authorization_expires_at: decision.authorization_expires_at,
                    decided_at: decision.decided_at,
                },
            ));
        }
        current_without_route = true;
    }
    Ok(if current_without_route {
        DesktopApprovalProjection::CurrentWithoutRoute
    } else {
        DesktopApprovalProjection::Absent
    })
}

/// The durable observation one local enrollment request is projected against.
/// The reconciler's authority and the active-request listing must derive
/// approval from this same assembly; deriving it independently lets the two
/// disagree about the same signed documents.
struct DurableEnrollmentProjection {
    documents: DurableEnrollmentDocuments,
    offer: PureOffer,
    request: PureRequest,
    decisions: Vec<(EnrollmentDecisionRecord, bool)>,
}

/// The reconciler preflights malformed authority rows into scoped conflicts
/// before projecting them; the listing has no conflict channel, so the same
/// rows are skipped per row and the well-formed remainder still lists.
fn listing_revision_rows(response: &QueryResponse) -> Result<Vec<EnrollmentRevisionRow>> {
    Ok(rows::<Value>(response, "NetworkAuthorizationRevision")?
        .into_iter()
        .filter_map(|raw| {
            let row = serde_json::from_value::<EnrollmentRevisionRow>(raw).ok()?;
            row.to_record().ok()?;
            Some(row)
        })
        .collect())
}

/// A foreign candidate request that fails to decode or verify must not fail
/// the whole listing; the assembly propagates same-server rows so the
/// reconciler can turn them into per-scope conflicts.
async fn listing_assembly_requests(
    request_rows: &[EnrollmentRequestRow],
    principal: &PrincipalIdentity,
) -> Vec<EnrollmentRequestRow> {
    let mut contained = Vec::new();
    for row in request_rows {
        let Ok(candidate) = row.to_record() else {
            continue;
        };
        if principal
            .verify(
                &candidate.candidate_did,
                &candidate.signing_payload(),
                &candidate.candidate_sig,
            )
            .await
            .is_err()
        {
            continue;
        }
        contained.push(row.clone());
    }
    contained
}

async fn assemble_durable_enrollment_documents(
    offer: &gents_protocol::enrollment::EnrollmentOfferRecord,
    request: &EnrollmentRequestRecord,
    admin_did: &str,
    request_rows: &[EnrollmentRequestRow],
    decision_rows: &[EnrollmentDecisionRow],
    revision_rows: &[EnrollmentRevisionRow],
    principal: &PrincipalIdentity,
    local_peer_id: &str,
) -> Result<DurableEnrollmentProjection> {
    let pure_offer = to_pure_offer(offer, true);
    let pure_request = to_pure_request(request, true, true);
    let mut documents = DurableEnrollmentDocuments::default();
    documents.offers.insert(pure_offer.clone());
    documents.admin_pins.insert(PureAdminPin {
        network_id: request.network_id.clone(),
        admin_did: admin_did.to_string(),
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
            documents
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
        documents
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
        documents
            .revisions
            .insert(to_pure_revision(&revision, verified));
    }

    Ok(DurableEnrollmentProjection {
        documents,
        offer: pure_offer,
        request: pure_request,
        decisions,
    })
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

#[derive(Clone, Deserialize)]
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
        owner_node: offer.owner_agent.clone(),
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
        owner_node: request.owner_agent.clone(),
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
        owner_node: decision.owner_agent.clone(),
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
        owner_node: revision.owner_agent.clone(),
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
        owner_node: receipt.owner_agent.clone(),
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
            owner_node: owner_agent.into(),
            request_digest: format!("digest-{server_peer}-{owner_agent}"),
            authorization_sequence: 1,
            authorization_expires_at: "2026-09-29T00:00:00Z".into(),
            decided_at: "2026-08-29T00:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn managed_enrollment_discovers_restarted_endpoint_without_changing_authority() {
        use crate::client::PeerDirectory;
        use wiremock::{matchers::path, Mock, MockServer, ResponseTemplate};

        let peer = "6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
        let other = "352aec0771cb90685b41d6f7fd7d89b8586d38a3ed7fe1d08f98f5592b453365";
        let owner = "did:key:managed-owner";
        for (live_owner, live_peer, address_peer, accepted) in [
            (owner, peer, peer, true),
            ("did:key:another-owner", peer, peer, false),
            (owner, other, other, false),
            (owner, peer, other, false),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("runtime");
            std::fs::create_dir(&home).unwrap();
            let server = MockServer::start().await;
            let graphql = format!("{}/api/v0/graphql", server.uri());
            let new_address = format!("127.0.0.1:56001/p2p/{address_peer}");
            for (route, body) in [
                (
                    "/status",
                    json!({"node_did": live_owner, "lifecycle": "ready"}),
                ),
                (
                    "/api/v0/p2p/shareable-address",
                    json!({"address": new_address}),
                ),
                ("/api/v0/node/identity", json!({"peer_id": live_peer})),
            ] {
                Mock::given(path(route))
                    .respond_with(ResponseTemplate::new(200).set_body_json(body))
                    .mount(&server)
                    .await;
            }
            std::fs::write(
                home.join("init.json"),
                json!({
                    "node_name": "managed", "node_did": live_owner,
                })
                .to_string(),
            )
            .unwrap();
            std::fs::write(
                home.join(gents::home::RUNTIME_STATE_FILE_NAME),
                json!({
                    "node_name": "managed", "node_did": live_owner,
                    "graphql": graphql, "p2p_transport": "iroh", "p2p_peer_id": live_peer,
                })
                .to_string(),
            )
            .unwrap();

            let mut approval = approval(peer, owner);
            approval.server_ticket = format!("127.0.0.1:56000/p2p/{peer}");
            let unchanged = approval.clone();
            let mut directory = PeerDirectory::open_writer(temp.path().join("peers.json"))
                .await
                .unwrap();
            directory
                .upsert_local_standard_peer(
                    "Managed",
                    &approval.server_ticket,
                    owner,
                    &graphql,
                    home.to_str().unwrap(),
                )
                .await
                .unwrap();
            let known = directory
                .upsert_enrollment_peer(
                    peer,
                    "Managed",
                    &approval.server_ticket,
                    owner,
                    &approval.network_id,
                    &approval.request_id,
                    &approval.request_digest,
                    &approval.admin_did,
                    approval.authorization_sequence,
                    &approval.authorization_expires_at,
                )
                .await
                .unwrap();
            let resolved = enrolled_server_address(&approval, Some(&known)).await;
            if accepted {
                let address = resolved.unwrap();
                assert_eq!(address, new_address);
                let updated = directory
                    .upsert_enrollment_peer(
                        peer,
                        "Managed",
                        &address,
                        owner,
                        &approval.network_id,
                        &approval.request_id,
                        &approval.request_digest,
                        &approval.admin_did,
                        approval.authorization_sequence,
                        &approval.authorization_expires_at,
                    )
                    .await
                    .unwrap();
                assert_eq!(updated.addr, new_address);
                assert_eq!(
                    updated.enrollment_request_digest,
                    known.enrollment_request_digest
                );
                assert_eq!(
                    updated.enrollment_authorization_sequence,
                    known.enrollment_authorization_sequence
                );
                assert_eq!(updated.enrollment_admin_did, known.enrollment_admin_did);
                assert!(!updated.pairing_ready);
            } else {
                assert!(resolved.is_err());
                assert_eq!(directory.records()[0], known);
            }
            assert_eq!(approval, unchanged);
            let mut remote = known.clone();
            remote.local_node_home = None;
            assert_eq!(
                enrolled_server_address(&approval, Some(&remote))
                    .await
                    .unwrap(),
                approval.server_ticket
            );
            assert_eq!(
                enrolled_server_address(&approval, None).await.unwrap(),
                approval.server_ticket
            );
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

        assert!(matches!(
            project_desktop_approval(
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
            .unwrap(),
            DesktopApprovalProjection::CurrentWithoutRoute,
        ));
        let approved = match project_desktop_approval(
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
        {
            DesktopApprovalProjection::Routed(approved) => approved,
            _ => panic!("signed current receipt opens exact generation"),
        };
        assert_eq!(approved.request_digest, request.request_digest);
        assert_eq!(approved.authorization_sequence, 1);
        assert_eq!(approved.authorization_expires_at, authorization_expires_at);

        let (_directory, sync_state) = ClientSyncStateOwner::for_test(Vec::new(), Vec::new()).await;
        let configured = sync_state
            .upsert_enrollment_peer(
                &approved.server_peer,
                "Enrolled Agent",
                &approved.server_ticket,
                &approved.owner_node,
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
                &approved.owner_node,
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
        assert!(matches!(
            project_desktop_approval(
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
            .unwrap(),
            DesktopApprovalProjection::Absent,
        ));
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
                &approved.owner_node,
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
    use defra_p2p_adapter::{
        ExplicitReplayCapabilityInput, P2PResult, P2pDocumentInfo, ReplicationFilters,
        ReplicatorInfo,
    };
    struct EnrollmentTransport {
        peer: String,
        resolved: Option<identity::Did>,
        connected: std::sync::atomic::AtomicBool,
        dials: std::sync::atomic::AtomicUsize,
        observations: std::sync::atomic::AtomicUsize,
    }
    #[async_trait::async_trait]
    impl P2POps for EnrollmentTransport {
        async fn local_peer_id(&self) -> P2PResult<String> {
            unimplemented!()
        }
        async fn listen_addresses(&self) -> P2PResult<Vec<String>> {
            unimplemented!()
        }
        async fn connected_peers(&self) -> P2PResult<Vec<String>> {
            Ok(
                if self.connected.load(std::sync::atomic::Ordering::SeqCst) {
                    vec![self.peer.clone()]
                } else {
                    vec![]
                },
            )
        }
        async fn connect_peer(&self, _addr: &str) -> P2PResult<()> {
            self.dials.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.connected
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        async fn disconnect_peer(&self, _addr: &str) -> P2PResult<()> {
            self.connected
                .store(false, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        async fn get_replicators(&self) -> P2PResult<Vec<ReplicatorInfo>> {
            unimplemented!()
        }
        async fn add_replicator(
            &self,
            _collections: Vec<String>,
            _addr: Option<&str>,
            _filters: ReplicationFilters,
            _explicit_replay_capabilities: Vec<ExplicitReplayCapabilityInput>,
            _expected_authorizer_did: Option<&str>,
        ) -> P2PResult<()> {
            unimplemented!()
        }
        async fn remove_replicator(
            &self,
            _collections: Vec<String>,
            _addr: Option<&str>,
        ) -> P2PResult<()> {
            Ok(())
        }
        async fn get_collections(&self) -> P2PResult<Vec<String>> {
            unimplemented!()
        }
        async fn add_collections(&self, _collections: Vec<String>) -> P2PResult<()> {
            unimplemented!()
        }
        async fn remove_collections(&self, _collections: Vec<String>) -> P2PResult<()> {
            unimplemented!()
        }
        async fn get_documents(&self) -> P2PResult<Vec<P2pDocumentInfo>> {
            unimplemented!()
        }
        async fn add_documents(&self, _docs: Vec<P2pDocumentRequest>) -> P2PResult<()> {
            unimplemented!()
        }
        async fn remove_documents(&self, _docs: Vec<P2pDocumentRequest>) -> P2PResult<()> {
            unimplemented!()
        }
        async fn sync_documents(
            &self,
            _collection_name: &str,
            _doc_ids: Vec<String>,
            _timeout: Option<std::time::Duration>,
        ) -> P2PResult<()> {
            unimplemented!()
        }
        async fn sync_branchable_collection(&self, _collection_id: &str) -> P2PResult<()> {
            unimplemented!()
        }
        async fn sync_collection_versions(&self, _version_ids: Vec<String>) -> P2PResult<()> {
            unimplemented!()
        }
        async fn resolve_peer_identity(
            &self,
            _: &TransportPeerId,
        ) -> P2PResult<Option<identity::Did>> {
            self.observations
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match &self.resolved {
                Some(did) => Ok(Some(did.clone())),
                None => Err(P2PError::unsupported("identity challenge rejected")),
            }
        }
    }
    #[tokio::test]
    async fn repeated_enrollment_observations_reuse_connection_but_authenticate_fail_closed() {
        let transport = Arc::new(EnrollmentTransport {
            peer: "approved-peer".into(),
            resolved: None,
            connected: false.into(),
            dials: 0.into(),
            observations: 0.into(),
        });
        let p2p: Arc<dyn P2POps> = transport.clone();
        let approval = ApprovedStatusEnrollment {
            network_id: String::new(),
            request_id: String::new(),
            server_peer: transport.peer.clone(),
            server_ticket: "ticket".into(),
            admin_did: "did:key:approved".into(),
            owner_node: String::new(),
            request_digest: String::new(),
            authorization_sequence: 1,
            authorization_expires_at: String::new(),
            decided_at: String::new(),
        };
        for _ in 0..8 {
            assert!(authenticate_enrolled_server(&p2p, &approval, None)
                .await
                .is_err());
        }
        use std::sync::atomic::Ordering::SeqCst;
        assert_eq!(transport.dials.load(SeqCst), 1);
        assert_eq!(transport.observations.load(SeqCst), 8);
        transport.connected.store(false, SeqCst);
        assert!(authenticate_enrolled_server(&p2p, &approval, None)
            .await
            .is_err());
        assert_eq!(transport.dials.load(SeqCst), 2);
        assert_eq!(transport.observations.load(SeqCst), 9);
    }

    /// Signed authority documents for one approved generation, plus the
    /// admin-signed revocation that supersedes it.
    struct SignedEnrollmentAuthority {
        offer: gents_protocol::enrollment::EnrollmentOfferRecord,
        offer_token: String,
        request: EnrollmentRequestRecord,
        decision: EnrollmentDecisionRecord,
        revision: AuthorizationRevisionRecord,
        revoked: AuthorizationRevisionRecord,
        receipt: EnrollmentRouteReceiptRecord,
    }

    fn signed_enrollment_authority(
        admin: &PrincipalIdentity,
        candidate: &PrincipalIdentity,
        candidate_peer: &str,
    ) -> SignedEnrollmentAuthority {
        use gents_protocol::enrollment::{
            derive_decision_id, derive_revision_id, derive_route_receipt_id, encode_offer,
            EnrollmentOfferRecord,
        };

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
                    candidate_peer,
                    client_nonce
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
            server_peer: offer.server_peer.clone(),
            candidate_did: candidate.did().into(),
            candidate_peer: candidate_peer.into(),
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

        let signed_revision =
            |sequence: u64, kind: AuthorizationRevisionKind| -> AuthorizationRevisionRecord {
                let mut revision = AuthorizationRevisionRecord {
                    protocol_version: ENROLLMENT_PROTOCOL_VERSION,
                    revision_id: derive_revision_id(
                        &request.network_id,
                        &request.candidate_did,
                        sequence,
                        &kind,
                        &request.request_digest,
                    ),
                    request_id: request.request_id.clone(),
                    request_digest: request.request_digest.clone(),
                    network_id: request.network_id.clone(),
                    admin_did: request.admin_did.clone(),
                    member_did: request.candidate_did.clone(),
                    member_peer: request.candidate_peer.clone(),
                    owner_agent: request.owner_agent.clone(),
                    sequence,
                    authorization_expires_at: authorization_expires_at.clone(),
                    kind,
                    issued_at: issued_at.clone(),
                    signer_did: admin.did().into(),
                    admin_sig: Vec::new(),
                };
                revision.admin_sig = admin.sign(&revision.signing_payload()).unwrap();
                revision
            };
        let revision = signed_revision(1, AuthorizationRevisionKind::Active);
        let revoked = signed_revision(2, AuthorizationRevisionKind::Revoked);

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

        SignedEnrollmentAuthority {
            offer,
            offer_token,
            request,
            decision,
            revision,
            revoked,
            receipt,
        }
    }

    async fn commit_document(core: &ClientCore, stage: &'static str, mutation: &str) {
        gents::config_client::ConfigAccess::write_local(core.node(), stage, mutation)
            .await
            .expect("commit enrollment authority document");
    }

    async fn commit_admin_pin(core: &ClientCore, authority: &SignedEnrollmentAuthority) {
        let pin_key = escape_graphql_string(&format!(
            "pin-{}",
            derive_enrollment_id(
                "gents-network-admin-pin-v1",
                &[&authority.request.network_id],
            )
        ));
        let mutation = format!(
            r#"mutation {{ create_NetworkAdminPin(input: {{
                pin_key: "{pin_key}",
                network_id: "{}",
                admin_did: "{}",
                offer_id: "{}",
                confirmed_at: "{}"
            }}) {{ _docID }} }}"#,
            escape_graphql_string(&authority.request.network_id),
            escape_graphql_string(&authority.request.admin_did),
            escape_graphql_string(&authority.request.offer_id),
            escape_graphql_string(&authority.request.issued_at),
        );
        commit_document(core, "desktop.enrollment.test.admin_pin", &mutation).await;
    }

    async fn commit_enrollment_request(core: &ClientCore, request: &EnrollmentRequestRecord) {
        let input = enrollment_request_input(request);
        let mutation =
            format!("mutation {{ create_NetworkEnrollmentRequest(input: {input}) {{ _docID }} }}");
        commit_document(core, "desktop.enrollment.test.request", &mutation).await;
    }

    async fn commit_decision(core: &ClientCore, authority: &SignedEnrollmentAuthority) {
        let decision = &authority.decision;
        let mutation = format!(
            r#"mutation {{ create_NetworkEnrollmentDecision(input: {{
                protocol_version: {}, decision_id: "{}", request_id: "{}", request_digest: "{}",
                network_id: "{}", admin_did: "{}", candidate_did: "{}", candidate_peer: "{}",
                owner_agent: "{}", decision: "{}", authorization_sequence: {},
                authorization_expires_at: "{}", decided_at: "{}", signer_did: "{}", admin_sig: "{}"
            }}) {{ _docID }} }}"#,
            decision.protocol_version,
            escape_graphql_string(&decision.decision_id),
            escape_graphql_string(&decision.request_id),
            escape_graphql_string(&decision.request_digest),
            escape_graphql_string(&decision.network_id),
            escape_graphql_string(&decision.admin_did),
            escape_graphql_string(&decision.candidate_did),
            escape_graphql_string(&decision.candidate_peer),
            escape_graphql_string(&decision.owner_agent),
            decision.decision.as_str(),
            decision.authorization_sequence,
            escape_graphql_string(&decision.authorization_expires_at),
            escape_graphql_string(&decision.decided_at),
            escape_graphql_string(&decision.signer_did),
            bs58::encode(&decision.admin_sig).into_string(),
        );
        commit_document(core, "desktop.enrollment.test.decision", &mutation).await;
    }

    async fn commit_revision(core: &ClientCore, revision: &AuthorizationRevisionRecord) {
        commit_revision_with_admin_sig(
            core,
            revision,
            &bs58::encode(&revision.admin_sig).into_string(),
        )
        .await;
    }

    async fn commit_revision_with_admin_sig(
        core: &ClientCore,
        revision: &AuthorizationRevisionRecord,
        admin_sig: &str,
    ) {
        let mutation = format!(
            r#"mutation {{ create_NetworkAuthorizationRevision(input: {{
                protocol_version: {}, revision_id: "{}", request_id: "{}", request_digest: "{}",
                network_id: "{}", admin_did: "{}", member_did: "{}", member_peer: "{}",
                owner_agent: "{}", sequence: {}, authorization_expires_at: "{}", kind: "{}",
                issued_at: "{}", signer_did: "{}", admin_sig: "{}"
            }}) {{ _docID }} }}"#,
            revision.protocol_version,
            escape_graphql_string(&revision.revision_id),
            escape_graphql_string(&revision.request_id),
            escape_graphql_string(&revision.request_digest),
            escape_graphql_string(&revision.network_id),
            escape_graphql_string(&revision.admin_did),
            escape_graphql_string(&revision.member_did),
            escape_graphql_string(&revision.member_peer),
            escape_graphql_string(&revision.owner_agent),
            revision.sequence,
            escape_graphql_string(&revision.authorization_expires_at),
            revision.kind.as_str(),
            escape_graphql_string(&revision.issued_at),
            escape_graphql_string(&revision.signer_did),
            escape_graphql_string(admin_sig),
        );
        commit_document(core, "desktop.enrollment.test.revision", &mutation).await;
    }

    async fn commit_route_receipt(core: &ClientCore, authority: &SignedEnrollmentAuthority) {
        let receipt = &authority.receipt;
        let mutation = format!(
            r#"mutation {{ create_NetworkEnrollmentRouteReceipt(input: {{
                protocol_version: {}, receipt_id: "{}", request_id: "{}", request_digest: "{}",
                network_id: "{}", admin_did: "{}", member_did: "{}", member_peer: "{}",
                server_peer: "{}", owner_agent: "{}", authorization_sequence: {},
                authorization_expires_at: "{}", direction: "{}", applied_at: "{}",
                signer_did: "{}", admin_sig: "{}"
            }}) {{ _docID }} }}"#,
            receipt.protocol_version,
            escape_graphql_string(&receipt.receipt_id),
            escape_graphql_string(&receipt.request_id),
            escape_graphql_string(&receipt.request_digest),
            escape_graphql_string(&receipt.network_id),
            escape_graphql_string(&receipt.admin_did),
            escape_graphql_string(&receipt.member_did),
            escape_graphql_string(&receipt.member_peer),
            escape_graphql_string(&receipt.server_peer),
            escape_graphql_string(&receipt.owner_agent),
            receipt.authorization_sequence,
            escape_graphql_string(&receipt.authorization_expires_at),
            receipt.direction.as_str(),
            escape_graphql_string(&receipt.applied_at),
            escape_graphql_string(&receipt.signer_did),
            bs58::encode(&receipt.admin_sig).into_string(),
        );
        commit_document(core, "desktop.enrollment.test.route_receipt", &mutation).await;
    }

    /// A core whose own P2P supervisor is stopped, so the test's reconciler
    /// is the only one acting on the peer directory.
    async fn start_unsupervised_core(root: std::path::PathBuf) -> ClientCore {
        let core = ClientCore::start_with_paths_and_options(
            crate::client::paths::DesktopPaths::from_root(root),
            super::super::ClientCoreOptions::local_only(),
        )
        .await
        .unwrap();
        if let Some(supervisor) = core.p2p_supervisor.lock().await.take() {
            supervisor.abort();
            let _ = supervisor.await;
        }
        core
    }

    async fn locally_retire(
        core: &ClientCore,
        authority: &SignedEnrollmentAuthority,
    ) -> crate::client::peer_directory::PeerRecord {
        core.sync_state
            .upsert_enrollment_peer(
                &authority.request.server_peer,
                "Enrolled server",
                &authority.offer.server_ticket,
                &authority.request.owner_agent,
                &authority.request.network_id,
                &authority.request.request_id,
                &authority.request.request_digest,
                &authority.request.admin_did,
                authority.decision.authorization_sequence,
                &authority.decision.authorization_expires_at,
            )
            .await
            .unwrap();
        let installed = core.peer_records().await;
        let [record] = installed.as_slice() else {
            panic!("one enrollment record is configured, got {installed:?}");
        };
        core.sync_state
            .queue_removal(record, RemovalCause::Operator)
            .await
            .unwrap()
            .expect("removal is queued");
        record.clone()
    }

    #[tokio::test]
    async fn locally_removed_enrollment_stays_absent_across_reconcile() {
        use crate::client::paths::DesktopPaths;

        let temp = tempfile::tempdir().unwrap();
        let core = start_unsupervised_core(temp.path().join("desktop")).await;
        let admin =
            PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("admin")))
                .await
                .unwrap();
        let authority =
            signed_enrollment_authority(&admin, core.node_identity(), core.local_peer_id());
        commit_admin_pin(&core, &authority).await;
        commit_enrollment_request(&core, &authority.request).await;
        commit_decision(&core, &authority).await;
        commit_revision(&core, &authority.revision).await;
        commit_route_receipt(&core, &authority).await;

        let transport = Arc::new(EnrollmentTransport {
            peer: authority.offer.server_ticket.clone(),
            resolved: Some(identity::Did::new(admin.did().to_string()).unwrap()),
            connected: false.into(),
            dials: 0.into(),
            observations: 0.into(),
        });
        let p2p: Arc<dyn P2POps> = transport.clone();
        let principal = Arc::new(core.node_identity().clone());
        let route_manager = Arc::new(ClientRouteManager::new(
            core.node_arc(),
            Arc::clone(&p2p),
            Arc::clone(&principal),
        ));
        let reconcile = || async {
            reconcile_status_enrollment_approvals(
                &core.node_arc(),
                &p2p,
                &principal,
                core.local_peer_id(),
                &core.sync_state,
                &route_manager,
            )
            .await
            .unwrap()
        };

        let authority_map = reconcile().await;
        assert!(
            authority_map.contains_key(&authority.request.server_peer),
            "the durable authority installs the enrolled peer: {authority_map:?}"
        );
        let installed = core.peer_records().await;
        assert_eq!(installed.len(), 1);
        assert_eq!(
            installed[0].enrollment_request_digest.as_deref(),
            Some(authority.request.request_digest.as_str())
        );

        core.sync_state
            .queue_removal(&installed[0], RemovalCause::Operator)
            .await
            .unwrap()
            .expect("removal is queued");

        reconcile().await;
        reconcile().await;
        assert!(
            core.peer_records().await.is_empty(),
            "a locally removed enrollment must stay absent while its lease still reads as current"
        );
        use std::sync::atomic::Ordering::SeqCst;
        assert_eq!(
            transport.dials.load(SeqCst),
            1,
            "a removed enrollment is not re-dialled by later reconciles"
        );
        assert_eq!(
            transport.observations.load(SeqCst),
            1,
            "a removed enrollment is filtered before the authentication stream"
        );
        assert!(
            core.active_status_enrollment_requests()
                .await
                .unwrap()
                .is_empty(),
            "a locally removed request is no longer listed"
        );
        assert_eq!(
            core.sync_state.retired_enrollment_digests().await,
            BTreeSet::from([authority.request.request_digest.clone()]),
            "the retirement lasts exactly as long as the durable authorization"
        );

        commit_revision(&core, &authority.revoked).await;
        reconcile().await;
        assert!(
            core.sync_state
                .retired_enrollment_digests()
                .await
                .is_empty(),
            "revocation removes the outcome and prunes the retirement with it"
        );
        assert!(core.peer_records().await.is_empty());

        core.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn revoked_enrollment_ends_the_active_request_listing() {
        use crate::client::paths::DesktopPaths;

        let temp = tempfile::tempdir().unwrap();
        let core = start_unsupervised_core(temp.path().to_path_buf()).await;
        let admin =
            PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("admin")))
                .await
                .unwrap();
        let authority =
            signed_enrollment_authority(&admin, core.node_identity(), core.local_peer_id());
        commit_admin_pin(&core, &authority).await;
        commit_enrollment_request(&core, &authority.request).await;
        commit_decision(&core, &authority).await;
        commit_revision(&core, &authority.revision).await;

        assert!(
            core.peer_records()
                .await
                .iter()
                .all(|record| !record.is_chat_ready_at(Utc::now())),
            "no configured peer is chat-ready before the route is finished"
        );
        let listed = core.active_status_enrollment_requests().await.unwrap();
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].state, "approved");
        assert_eq!(listed[0].request_id, authority.request.request_id);

        commit_revision(&core, &authority.revoked).await;
        let listed = core.active_status_enrollment_requests().await.unwrap();
        assert!(
            listed.is_empty(),
            "a revoked authorization ends the request before its own expiry: {listed:?}"
        );

        core.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_locally_retired_request_refuses_its_offer_instead_of_sharing_its_challenge() {
        use crate::client::paths::DesktopPaths;

        let temp = tempfile::tempdir().unwrap();
        let core = start_unsupervised_core(temp.path().to_path_buf()).await;
        let admin =
            PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("admin")))
                .await
                .unwrap();
        let authority =
            signed_enrollment_authority(&admin, core.node_identity(), core.local_peer_id());
        commit_enrollment_request(&core, &authority.request).await;
        locally_retire(&core, &authority).await;

        let offer = decode_offer(&authority.offer_token).unwrap();
        let error = core
            .existing_request_for_offer(&offer, &authority.offer_token, core.local_peer_id())
            .await
            .expect_err("a retired request must not be reused or replaced under its offer");
        assert!(
            error.to_string().contains("fetch a fresh offer"),
            "{error:#}"
        );

        core.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn malformed_authority_rows_are_skipped_by_the_active_listing() {
        use crate::client::paths::DesktopPaths;

        let temp = tempfile::tempdir().unwrap();
        let core = start_unsupervised_core(temp.path().to_path_buf()).await;
        let admin =
            PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("admin")))
                .await
                .unwrap();
        let authority =
            signed_enrollment_authority(&admin, core.node_identity(), core.local_peer_id());
        commit_admin_pin(&core, &authority).await;
        commit_enrollment_request(&core, &authority.request).await;
        commit_decision(&core, &authority).await;
        commit_revision(&core, &authority.revision).await;

        let mut malformed_revision = authority.revision.clone();
        malformed_revision.revision_id = format!("{}-malformed", malformed_revision.revision_id);
        commit_revision_with_admin_sig(&core, &malformed_revision, "not-a-base58-signature").await;
        let mut foreign = authority.request.clone();
        foreign.request_id = format!("{}-foreign", foreign.request_id);
        foreign.request_digest = format!("{}-foreign", foreign.request_digest);
        foreign.challenge = format!("{}-foreign", foreign.challenge);
        foreign.candidate_did = admin.did().to_string();
        foreign.candidate_peer = "foreign-peer".into();
        foreign.candidate_sig = vec![1, 2, 3];
        commit_enrollment_request(&core, &foreign).await;

        let listed = core.active_status_enrollment_requests().await.unwrap();
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].state, "approved");
        assert_eq!(listed[0].request_id, authority.request.request_id);

        core.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_saved_peer_removed_for_an_absent_receipt_reinstalls_when_it_returns() {
        use crate::client::paths::DesktopPaths;

        let temp = tempfile::tempdir().unwrap();
        let core = start_unsupervised_core(temp.path().to_path_buf()).await;
        let admin =
            PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("admin")))
                .await
                .unwrap();
        let authority =
            signed_enrollment_authority(&admin, core.node_identity(), core.local_peer_id());
        commit_admin_pin(&core, &authority).await;
        commit_enrollment_request(&core, &authority.request).await;
        commit_decision(&core, &authority).await;
        commit_revision(&core, &authority.revision).await;
        core.sync_state
            .upsert_enrollment_peer(
                &authority.request.server_peer,
                "Enrolled server",
                &authority.offer.server_ticket,
                &authority.request.owner_agent,
                &authority.request.network_id,
                &authority.request.request_id,
                &authority.request.request_digest,
                &authority.request.admin_did,
                authority.decision.authorization_sequence,
                &authority.decision.authorization_expires_at,
            )
            .await
            .unwrap();

        let transport = Arc::new(EnrollmentTransport {
            peer: authority.offer.server_ticket.clone(),
            resolved: Some(identity::Did::new(admin.did().to_string()).unwrap()),
            connected: false.into(),
            dials: 0.into(),
            observations: 0.into(),
        });
        let p2p: Arc<dyn P2POps> = transport.clone();
        let principal = Arc::new(core.node_identity().clone());
        let route_manager = Arc::new(ClientRouteManager::new(
            core.node_arc(),
            Arc::clone(&p2p),
            Arc::clone(&principal),
        ));
        let reconcile = || async {
            reconcile_status_enrollment_approvals(
                &core.node_arc(),
                &p2p,
                &principal,
                core.local_peer_id(),
                &core.sync_state,
                &route_manager,
            )
            .await
            .unwrap()
        };

        reconcile().await;
        assert!(
            core.peer_records().await.is_empty(),
            "reconciliation tears down a saved peer whose route receipt is unobserved"
        );
        assert!(
            core.sync_state
                .retired_enrollment_digests()
                .await
                .is_empty(),
            "an automatic teardown must not retire a generation the operator kept"
        );

        commit_route_receipt(&core, &authority).await;
        let authority_map = reconcile().await;
        assert!(
            authority_map.contains_key(&authority.request.server_peer),
            "the restored receipt reinstalls the enrolled server: {authority_map:?}"
        );
        let installed = core.peer_records().await;
        assert_eq!(
            installed
                .iter()
                .map(|record| record.enrollment_request_digest.as_deref())
                .collect::<Vec<_>>(),
            vec![Some(authority.request.request_digest.as_str())]
        );

        core.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_retirement_survives_an_absent_route_receipt_while_authority_is_current() {
        use crate::client::paths::DesktopPaths;
        use std::sync::atomic::Ordering::SeqCst;

        let temp = tempfile::tempdir().unwrap();
        let core = start_unsupervised_core(temp.path().to_path_buf()).await;
        let admin =
            PrincipalIdentity::load_or_create(&DesktopPaths::from_root(temp.path().join("admin")))
                .await
                .unwrap();
        let authority =
            signed_enrollment_authority(&admin, core.node_identity(), core.local_peer_id());
        commit_admin_pin(&core, &authority).await;
        commit_enrollment_request(&core, &authority.request).await;
        commit_decision(&core, &authority).await;
        commit_revision(&core, &authority.revision).await;
        locally_retire(&core, &authority).await;

        let transport = Arc::new(EnrollmentTransport {
            peer: authority.offer.server_ticket.clone(),
            resolved: Some(identity::Did::new(admin.did().to_string()).unwrap()),
            connected: false.into(),
            dials: 0.into(),
            observations: 0.into(),
        });
        let p2p: Arc<dyn P2POps> = transport.clone();
        let principal = Arc::new(core.node_identity().clone());
        let route_manager = Arc::new(ClientRouteManager::new(
            core.node_arc(),
            Arc::clone(&p2p),
            Arc::clone(&principal),
        ));
        let reconcile = || async {
            reconcile_status_enrollment_approvals(
                &core.node_arc(),
                &p2p,
                &principal,
                core.local_peer_id(),
                &core.sync_state,
                &route_manager,
            )
            .await
            .unwrap()
        };

        reconcile().await;
        assert_eq!(
            core.sync_state.retired_enrollment_digests().await,
            BTreeSet::from([authority.request.request_digest.clone()]),
            "an absent route receipt must not prune a retirement whose authority is current"
        );
        assert!(core.peer_records().await.is_empty());

        commit_route_receipt(&core, &authority).await;
        reconcile().await;
        assert_eq!(
            core.sync_state.retired_enrollment_digests().await,
            BTreeSet::from([authority.request.request_digest.clone()]),
            "the retirement outlives the authorization generation it names"
        );
        assert!(
            core.peer_records().await.is_empty(),
            "a receipt arriving after removal must not reinstall the enrolled server"
        );
        assert_eq!(
            transport.dials.load(SeqCst),
            0,
            "a removed enrollment is not re-dialled once its receipt returns"
        );
        assert_eq!(transport.observations.load(SeqCst), 0);

        core.shutdown().await.unwrap();
    }
}
