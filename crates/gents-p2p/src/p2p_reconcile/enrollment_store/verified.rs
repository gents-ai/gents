use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::enrollment::{
    decode_offer, enrollment_schema_fingerprint, AuthorizationRevisionKind as WireRevisionKind,
    AuthorizationRevisionRecord, EnrollmentDecisionKind as WireDecisionKind,
    EnrollmentDecisionRecord, EnrollmentRequestRecord, EnrollmentRouteReceiptRecord,
};
use gents_protocol::network_token::NetworkRecord;
use p2p::iroh::parse_public_peer_addr;
use serde_json::Value;

use super::super::enrollment::{
    AuthorizationRevision, AuthorizationRevisionKind, EnrollmentDecision, EnrollmentDecisionKind,
    EnrollmentOffer, EnrollmentRequest, EnrollmentRouteDirection, EnrollmentRouteReceipt,
};
use super::graphql::RequestRow;
use super::GraphqlEnrollmentStore;

impl GraphqlEnrollmentStore {
    pub(super) async fn verify_request(
        &self,
        row: RequestRow,
        network: &NetworkRecord,
    ) -> Result<VerifiedRequest> {
        let record = row.to_record()?;
        let offer = decode_offer(&record.offer_token)?;
        anyhow::ensure!(
            offer.schema_fingerprint == enrollment_schema_fingerprint(),
            "enrollment offer schema fingerprint mismatch"
        );
        anyhow::ensure!(
            offer.network_id == network.network_id,
            "offer network mismatch"
        );
        anyhow::ensure!(offer.admin_did == network.admin_did, "offer admin mismatch");
        record.validate_against_offer(&offer)?;
        let (server_ticket_peer, _) = parse_public_peer_addr(&offer.server_ticket)
            .context("parsing enrollment server ticket")?;
        let (candidate_ticket_peer, _) = parse_public_peer_addr(&record.candidate_ticket)
            .context("parsing enrollment candidate ticket")?;
        anyhow::ensure!(
            server_ticket_peer.to_string() == offer.server_peer,
            "offer server ticket peer mismatch"
        );
        anyhow::ensure!(
            candidate_ticket_peer.to_string() == record.candidate_peer,
            "request candidate ticket peer mismatch"
        );
        let offer_signed = self
            .identity
            .verify(&offer.admin_did, &offer.signing_payload(), &offer.admin_sig)
            .await?;
        let candidate_signed = self
            .identity
            .verify(
                &record.candidate_did,
                &record.signing_payload(),
                &record.candidate_sig,
            )
            .await?;
        Ok(VerifiedRequest {
            doc_id: row.doc_id,
            record,
            offer: EnrollmentOffer {
                offer_id: offer.offer_id,
                challenge: offer.challenge,
                network_id: offer.network_id,
                admin_did: offer.admin_did.clone(),
                server_peer: offer.server_peer.clone(),
                server_ticket_peer: server_ticket_peer.to_string(),
                resolved_server_did: offer.admin_did,
                owner_node: offer.owner_node_did,
                profile: offer.profile,
                schema_compatible: offer.schema_fingerprint == enrollment_schema_fingerprint(),
                admin_signed: offer_signed,
                fresh: true,
            },
            candidate_signed,
        })
    }
}

fn required_raw_text<'a>(raw: &'a Value, field: &str, row_kind: &str) -> Result<&'a str> {
    raw.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("unattributable {row_kind} has no {field}"))
}

pub(super) fn raw_text<'a>(raw: &'a Value, field: &str) -> Option<&'a str> {
    raw.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CandidateRequestAttribution {
    Exact {
        request_id: String,
        scope: (String, String),
    },
    RequestOnly(String),
    ScopeOnly((String, String)),
    /// Addressed to another network, such as this node's own request to another server.
    Foreign,
    Unattributable,
}

pub(super) fn attribute_candidate_request(
    raw: &Value,
    local_network: &str,
) -> CandidateRequestAttribution {
    let network_id = raw_text(raw, "network_id");
    if network_id.is_some_and(|network_id| network_id != local_network) {
        return CandidateRequestAttribution::Foreign;
    }
    let network_id = network_id.unwrap_or(local_network);
    let request_id = raw_text(raw, "request_id").map(str::to_string);
    let member_did = raw_text(raw, "candidate_did").map(str::to_string);
    match (request_id, member_did) {
        (Some(request_id), Some(member_did)) => CandidateRequestAttribution::Exact {
            request_id,
            scope: (network_id.to_string(), member_did),
        },
        (Some(request_id), None) => CandidateRequestAttribution::RequestOnly(request_id),
        (None, Some(member_did)) => {
            CandidateRequestAttribution::ScopeOnly((network_id.to_string(), member_did))
        }
        (None, None) => CandidateRequestAttribution::Unattributable,
    }
}

pub(super) fn raw_i64(raw: &Value, field: &str) -> Option<i64> {
    raw.get(field)?.as_i64()
}

pub(super) fn scope_for_authority_row(
    raw: &Value,
    request_id: &str,
    request_scopes: &BTreeMap<String, (String, String)>,
    local_network_id: &str,
    member_field: &str,
) -> Result<Option<(String, String)>> {
    if let Some(scope) = request_scopes.get(request_id) {
        return Ok(Some(scope.clone()));
    }
    let network_id = raw_text(raw, "network_id");
    if network_id.is_some_and(|network_id| network_id != local_network_id) {
        return Ok(None);
    }
    let member_did = required_raw_text(raw, member_field, "enrollment authority row")?;
    Ok(Some((
        network_id.unwrap_or(local_network_id).to_string(),
        member_did.to_string(),
    )))
}

pub(super) fn add_scope_conflict(
    conflicts: &mut BTreeMap<(String, String), Vec<String>>,
    scope: (String, String),
    reason: impl Into<String>,
) {
    add_conflict(conflicts, scope, reason);
}

pub(super) fn add_request_conflict(
    conflicts: &mut BTreeMap<String, Vec<String>>,
    request_id: &str,
    reason: impl Into<String>,
) {
    add_conflict(conflicts, request_id.to_string(), reason);
}

fn add_conflict<K>(conflicts: &mut BTreeMap<K, Vec<String>>, key: K, reason: impl Into<String>)
where
    K: Ord,
{
    let reason = reason.into();
    let reasons = conflicts.entry(key).or_default();
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

pub(super) fn finish_conflicts<K>(conflicts: BTreeMap<K, Vec<String>>) -> BTreeMap<K, String>
where
    K: Ord,
{
    conflicts
        .into_iter()
        .map(|(key, reasons)| (key, reasons.join("; ")))
        .collect()
}

pub(super) fn invalid_revision_is_current(valid_max: Option<u64>, invalid_max: u64) -> bool {
    valid_max.unwrap_or_default() <= invalid_max
}

pub(super) fn observe_next_authorization_sequence(
    next_by_scope: &mut BTreeMap<(String, String), u64>,
    conflicts: &mut BTreeMap<(String, String), Vec<String>>,
    scope: (String, String),
    observed: u64,
) {
    let Some(next) = observed.checked_add(1) else {
        add_scope_conflict(
            conflicts,
            scope,
            "authorization sequence exhausted the DefraDB Int range",
        );
        return;
    };
    if next > i64::MAX as u64 {
        add_scope_conflict(
            conflicts,
            scope.clone(),
            "authorization sequence exhausted the DefraDB Int range",
        );
    }
    next_by_scope
        .entry(scope)
        .and_modify(|current| *current = (*current).max(next))
        .or_insert(next);
}

#[derive(Debug)]
pub(super) struct VerifiedRequest {
    pub(super) doc_id: String,
    pub(super) record: EnrollmentRequestRecord,
    pub(super) offer: EnrollmentOffer,
    pub(super) candidate_signed: bool,
}

impl VerifiedRequest {
    pub(super) fn is_fresh(&self, now: DateTime<Utc>) -> Result<bool> {
        let issued_at = DateTime::parse_from_rfc3339(&self.record.issued_at)?.with_timezone(&Utc);
        let expires_at = DateTime::parse_from_rfc3339(&self.record.expires_at)?.with_timezone(&Utc);
        Ok(issued_at <= now && now <= expires_at)
    }

    pub(super) fn pure_request(
        &self,
        now: DateTime<Utc>,
        terminal_witness: bool,
    ) -> EnrollmentRequest {
        EnrollmentRequest {
            request_id: self.record.request_id.clone(),
            digest: self.record.request_digest.clone(),
            offer_id: self.record.offer_id.clone(),
            challenge: self.record.challenge.clone(),
            network_id: self.record.network_id.clone(),
            admin_did: self.record.admin_did.clone(),
            server_peer: self.record.server_peer.clone(),
            candidate_did: self.record.candidate_did.clone(),
            candidate_peer: self.record.candidate_peer.clone(),
            candidate_name: self.record.candidate_name.clone(),
            observed_candidate_peer: if terminal_witness {
                self.record.candidate_peer.clone()
            } else {
                Default::default()
            },
            resolved_candidate_did: if terminal_witness {
                self.record.candidate_did.clone()
            } else {
                Default::default()
            },
            candidate_ticket_peer: self.record.candidate_peer.clone(),
            owner_node: self.record.owner_node_did.clone(),
            profile: self.record.profile.clone(),
            client_nonce: self.record.client_nonce.clone(),
            issued_at: self.record.issued_at.clone(),
            expires_at: self.record.expires_at.clone(),
            candidate_signed: self.candidate_signed,
            fresh: terminal_witness || self.is_fresh(now).unwrap_or(false),
        }
    }
}

#[derive(Debug)]
pub(super) struct VerifiedDecision {
    pub(super) doc_id: String,
    pub(super) record: EnrollmentDecisionRecord,
    pub(super) signed: bool,
}

impl VerifiedDecision {
    pub(super) fn pure(&self, now: DateTime<Utc>) -> EnrollmentDecision {
        let fresh = self.record.decision == WireDecisionKind::Denied
            || DateTime::parse_from_rfc3339(&self.record.authorization_expires_at)
                .map(|expires| now < expires.with_timezone(&Utc))
                .unwrap_or(false);
        EnrollmentDecision {
            request_id: self.record.request_id.clone(),
            request_digest: self.record.request_digest.clone(),
            network_id: self.record.network_id.clone(),
            admin_did: self.record.admin_did.clone(),
            candidate_did: self.record.candidate_did.clone(),
            candidate_peer: self.record.candidate_peer.clone(),
            owner_node: self.record.owner_node_did.clone(),
            kind: match self.record.decision {
                WireDecisionKind::Approved => EnrollmentDecisionKind::Approved,
                WireDecisionKind::Denied => EnrollmentDecisionKind::Denied,
            },
            authorization_sequence: self.record.authorization_sequence as usize,
            authorization_expires_at: self.record.authorization_expires_at.clone(),
            signer_did: self.record.signer_did.clone(),
            admin_signed: self.signed,
            fresh,
        }
    }
}

#[derive(Debug)]
pub(super) struct VerifiedRevision {
    pub(super) doc_id: String,
    pub(super) record: AuthorizationRevisionRecord,
    pub(super) signed: bool,
}

#[derive(Debug)]
pub(super) struct VerifiedRouteReceipt {
    pub(super) doc_id: String,
    pub(super) record: EnrollmentRouteReceiptRecord,
}

impl VerifiedRouteReceipt {
    pub(super) fn pure(&self) -> EnrollmentRouteReceipt {
        EnrollmentRouteReceipt {
            request_id: self.record.request_id.clone(),
            request_digest: self.record.request_digest.clone(),
            network_id: self.record.network_id.clone(),
            admin_did: self.record.admin_did.clone(),
            member_did: self.record.member_did.clone(),
            member_peer: self.record.member_peer.clone(),
            server_peer: self.record.server_peer.clone(),
            owner_node: self.record.owner_node_did.clone(),
            authorization_sequence: self.record.authorization_sequence as usize,
            authorization_expires_at: self.record.authorization_expires_at.clone(),
            direction: EnrollmentRouteDirection::ClientToServer,
            signer_did: self.record.signer_did.clone(),
            admin_signed: true,
            applied: true,
        }
    }
}

impl VerifiedRevision {
    pub(super) fn pure(&self) -> AuthorizationRevision {
        AuthorizationRevision {
            request_id: self.record.request_id.clone(),
            request_digest: self.record.request_digest.clone(),
            network_id: self.record.network_id.clone(),
            admin_did: self.record.admin_did.clone(),
            member_did: self.record.member_did.clone(),
            member_peer: self.record.member_peer.clone(),
            owner_node: self.record.owner_node_did.clone(),
            sequence: self.record.sequence as usize,
            authorization_expires_at: self.record.authorization_expires_at.clone(),
            kind: match self.record.kind {
                WireRevisionKind::Active => AuthorizationRevisionKind::Active,
                WireRevisionKind::Revoked => AuthorizationRevisionKind::Revoked,
            },
            signer_did: self.record.signer_did.clone(),
            admin_signed: self.signed,
        }
    }
}

pub(super) fn unique_request<'a>(
    rows: &'a BTreeMap<String, Vec<&VerifiedRequest>>,
    request_id: &str,
) -> Result<&'a VerifiedRequest> {
    match rows.get(request_id).map(Vec::as_slice) {
        Some([request]) => Ok(*request),
        Some(rows) => anyhow::bail!("request {request_id} has {} conflicting rows", rows.len()),
        None => anyhow::bail!("request {request_id} is missing"),
    }
}

pub(super) fn unique_approved_decision<'a>(
    rows: &'a BTreeMap<String, Vec<&VerifiedDecision>>,
    request_id: &str,
) -> Result<&'a VerifiedDecision> {
    match rows.get(request_id).map(Vec::as_slice) {
        Some([decision]) if decision.record.decision == WireDecisionKind::Approved => Ok(*decision),
        Some(rows) => anyhow::bail!(
            "request {request_id} has no unique approval ({} terminal rows)",
            rows.len()
        ),
        None => anyhow::bail!("request {request_id} has no approval"),
    }
}

pub(super) fn revisions_for<'a>(
    rows: &'a [VerifiedRevision],
    request_id: &str,
) -> Option<Vec<&'a VerifiedRevision>> {
    let selected = rows
        .iter()
        .filter(|row| row.record.request_id == request_id)
        .collect::<Vec<_>>();
    (!selected.is_empty()).then_some(selected)
}
