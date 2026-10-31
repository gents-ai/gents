use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use defra_p2p_adapter::{P2PError, P2POperations as P2POps};
use gents::runtime::agent::p2p_reconcile::enrollment::{
    active_request_visible, pending_request_satisfied, EnrollmentRequestAuthority,
};
use gents::runtime::agent::p2p_reconcile::enrollment_candidate::{
    confirm_network_admin_pin, local_enrollment_ticket, push_enrollment_request,
    sign_enrollment_request, verify_enrollment_offer, write_enrollment_request,
};
use gents::store::graphql::{escape_graphql_string, graphql_with_transaction_retry, rows};
use gents::NodeIdentity;
use gents_protocol::enrollment::{
    decode_offer, enrollment_schema_fingerprint, EnrollmentDecisionKind, EnrollmentRequestRecord,
};
use gents_protocol::network_token::EndpointRecord;
use serde_json::Value;
use tokio::time::timeout;

use super::sync_state::RuntimeSchemaObservation;
use super::{ClientCore, P2P_OPERATION_TIMEOUT};
use projection::{
    assemble_durable_enrollment_documents, listing_assembly_requests, listing_revision_rows,
};
use reconcile::EnrollmentAuthorizationGeneration;
use rows::{
    to_pure_decision, EnrollmentDecisionRow, EnrollmentPinRow, EnrollmentRequestRow,
    STATUS_ENROLLMENT_QUERY,
};

mod authority;
mod projection;
pub(super) mod reconcile;
mod rows;
#[cfg(test)]
mod tests;

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
    pub activation_error: Option<String>,
}

impl ClientCore {
    pub fn begin_runtime_schema_observation(
        &self,
        runtime_did: &str,
        endpoint: &str,
    ) -> Option<RuntimeSchemaObservation> {
        self.sync_state
            .begin_runtime_schema_observation(runtime_did, Some(endpoint))
    }

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

    pub async fn request_status_enrollment(
        &self,
        status: &Value,
    ) -> Result<EnrollmentRequestResult> {
        self.request_status_enrollment_with_label(status, None)
            .await
    }

    /// Route removal can hide a peer before its transport teardown completes.
    /// A fresh challenge must await those effects so they cannot disconnect its
    /// newly authenticated connection while the server verifies the candidate.
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
        let _lifecycle = self.route_manager.lock().await;
        verify_enrollment_offer(&self.p2p, &self.node_identity, &offer).await?;
        confirm_network_admin_pin(&self.node, &offer).await?;

        let candidate_peer = self.local_peer_id.clone();
        let candidate_ticket = local_enrollment_ticket(&self.p2p, &candidate_peer).await?;

        let (request, document_id) = match self
            .existing_request_for_offer(&offer, offer_token, &candidate_peer)
            .await?
        {
            Some(existing) => existing,
            None => {
                let request = sign_enrollment_request(
                    &self.node_identity,
                    &offer,
                    offer_token,
                    candidate_peer,
                    candidate_ticket,
                )
                .await?;
                let document_id = match write_enrollment_request(&self.node, &request).await {
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
        if let Some(endpoint) = status.get("desktop_graphql").and_then(Value::as_str) {
            self.sync_state
                .remember_enrollment_endpoint(
                    &request.server_peer,
                    &request.owner_agent,
                    &request.request_digest,
                    &request.expires_at,
                    endpoint,
                )
                .await?;
        }
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
            activation_error: None,
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
        let sync = self.sync_state.snapshot();
        let peers = &sync.directory;
        let now = Utc::now();
        let mut satisfied_authorities = Vec::new();
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
            let projection = match assemble_durable_enrollment_documents(
                &offer,
                &request,
                admin_did,
                &assembly_requests,
                &decisions,
                &revisions,
                &self.node_identity,
                &self.local_peer_id,
            )
            .await
            {
                Ok(projection) => projection,
                Err(_) if expires_at <= now => continue,
                Err(error) => return Err(error),
            };
            let current_approval = projection.decisions.iter().any(|(decision, verified)| {
                projection.documents.current_approval(
                    &projection.offer,
                    &projection.request,
                    &to_pure_decision(decision, *verified),
                )
            });
            if expires_at <= now && !current_approval {
                continue;
            }
            anyhow::ensure!(
                projection.decisions.len() <= 1,
                "enrollment request has multiple authenticated decisions"
            );
            for (decision, _) in &projection.decisions {
                decision.validate_against_request(&request)?;
            }

            let authority = EnrollmentRequestAuthority::from(&projection.request);
            let has_decision = !projection.decisions.is_empty();
            let mut route_applied = false;
            let mut activation_error = None;
            let state = match projection.decisions.as_slice() {
                [] => "pending_approval",
                [(decision, _)] if decision.decision == EnrollmentDecisionKind::Denied => continue,
                [(decision, _)] => {
                    if !current_approval {
                        continue;
                    }
                    let matching_peer = peers.iter().find(|peer| {
                        peer.is_enrollment()
                            && peer.peer_id == request.server_peer
                            && peer.node_did == request.owner_agent
                            && peer.pairing_network_id.as_deref() == Some(&request.network_id)
                            && peer.pairing_template.as_deref() == Some(&request.profile)
                            && peer.enrollment_request_id.as_deref() == Some(&request.request_id)
                            && peer.enrollment_request_digest.as_deref()
                                == Some(&request.request_digest)
                            && peer.enrollment_admin_did.as_deref() == Some(&request.admin_did)
                            && peer.enrollment_authorization_sequence
                                == Some(decision.authorization_sequence)
                            && peer.enrollment_authorization_expires_at.as_deref()
                                == Some(&decision.authorization_expires_at)
                    });
                    route_applied = matching_peer.is_some_and(|peer| peer.is_chat_ready_at(now));
                    let route_failure = matching_peer.is_some_and(|peer| {
                        sync.peers
                            .iter()
                            .find(|status| {
                                status.peer_id == peer.peer_id && status.addr == peer.addr
                            })
                            .is_some_and(|status| {
                                status.routes.iter().any(|route| route.last_error.is_some())
                            })
                    });
                    let generation = EnrollmentAuthorizationGeneration {
                        request_digest: request.request_digest.clone(),
                        sequence: decision.authorization_sequence,
                        expires_at: decision.authorization_expires_at.clone(),
                    };
                    activation_error = self
                        .sync_state
                        .project_enrollment_activation(
                            &request.request_id,
                            &generation,
                            current_approval,
                            route_failure,
                        )
                        .map(str::to_owned);
                    if route_applied {
                        satisfied_authorities.push(authority.clone());
                    }
                    "approved"
                }
                _ => unreachable!("multiple authenticated decisions fail closed above"),
            };
            if !active_request_visible(
                expires_at > now,
                has_decision,
                current_approval,
                route_applied,
            ) {
                continue;
            }
            active.push((
                authority,
                has_decision,
                EnrollmentRequestResult {
                    request_id: request.request_id,
                    network_id: request.network_id,
                    admin_did: request.admin_did,
                    server_peer: request.server_peer,
                    owner_node: request.owner_agent,
                    state: state.to_string(),
                    expires_at: request.expires_at,
                    activation_error,
                },
            ));
        }
        let mut active: Vec<_> = active
            .into_iter()
            .filter(|(requested, has_decision, _)| {
                !satisfied_authorities.iter().any(|current| {
                    pending_request_satisfied(requested, current, *has_decision, true, true)
                })
            })
            .map(|(_, _, request)| request)
            .collect();
        active.sort_by(|left, right| left.request_id.cmp(&right.request_id));
        Ok(active)
    }

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
        // A locally retired request stays durable; a second request under this
        // offer would share its challenge and neither could be approved.
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

fn map_p2p_error(error: P2PError) -> anyhow::Error {
    anyhow::anyhow!(error.to_string())
}
