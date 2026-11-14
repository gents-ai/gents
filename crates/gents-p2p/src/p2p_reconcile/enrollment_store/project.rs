use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use chrono::{DateTime, Utc};
use gents_protocol::network_token::NetworkRecord;
use serde_json::Value;

use super::super::enrollment::{DurableEnrollmentDocuments, NetworkAdminPin};
use super::{
    add_request_conflict, add_scope_conflict, attribute_candidate_request, finish_conflicts,
    invalid_revision_is_current, observe_next_authorization_sequence, raw_i64, raw_text,
    revisions_for, scope_for_authority_row, unique_approved_decision, unique_request,
    ActiveEnrollment, CandidateRequestAttribution, DecisionRow, DeniedEnrollment,
    EnrollmentProjection, GraphqlEnrollmentStore, NetworkRow, PendingEnrollment, RequestRow,
    RevisionRow, RevokedEnrollment, RouteReceiptRow, VerifiedDecision, VerifiedRequest,
    VerifiedRevision, VerifiedRouteReceipt, WireDecisionKind, WireRevisionKind,
};
use gents_store::graphql::rows;

type MemberScope = (String, String);

#[derive(Default)]
pub(super) struct ProjectionFindings {
    pub(super) scoped_conflicts: BTreeMap<MemberScope, Vec<String>>,
    pub(super) request_conflicts: BTreeMap<String, Vec<String>>,
    pub(super) invalid_authority_max: BTreeMap<MemberScope, u64>,
    pub(super) observed_revision_next: BTreeMap<MemberScope, u64>,
    pub(super) invalid_receipt_generations: BTreeSet<(MemberScope, u64)>,
}

pub(super) struct VerifiedRequests {
    pub(super) requests: Vec<VerifiedRequest>,
    pub(super) request_scopes: BTreeMap<String, MemberScope>,
    pub(super) findings: ProjectionFindings,
}

#[derive(Clone, Copy)]
pub(super) struct RequestIndex<'a> {
    pub(super) network: &'a NetworkRecord,
    pub(super) request_scopes: &'a BTreeMap<String, MemberScope>,
    pub(super) requests_by_id: &'a BTreeMap<String, Vec<&'a VerifiedRequest>>,
}

#[derive(Clone, Copy)]
pub(super) struct VerifiedDocuments<'a> {
    pub(super) requests: &'a [VerifiedRequest],
    pub(super) decisions: &'a [VerifiedDecision],
    pub(super) revisions: &'a [VerifiedRevision],
    pub(super) receipts: &'a [VerifiedRouteReceipt],
    pub(super) decisions_by_request: &'a BTreeMap<String, Vec<&'a VerifiedDecision>>,
}

impl GraphqlEnrollmentStore {
    pub(super) async fn verified_network(
        &self,
        network_rows: &[NetworkRow],
    ) -> Result<NetworkRecord> {
        let [network_row] = network_rows else {
            anyhow::bail!("expected exactly one Network, found {}", network_rows.len());
        };
        let network = network_row.to_record()?;
        anyhow::ensure!(
            network.admin_did == self.identity.did(),
            "Network admin does not match the local runtime identity"
        );
        anyhow::ensure!(
            self.identity
                .verify(&network.admin_did, &network.signing_payload(), &network.sig)
                .await?,
            "Network signature is invalid"
        );

        Ok(network)
    }

    pub(super) async fn verify_requests(
        &self,
        response: &query::QueryResponse,
        network: &NetworkRecord,
    ) -> Result<VerifiedRequests> {
        let mut scoped_conflicts = BTreeMap::<(String, String), Vec<String>>::new();
        let mut request_conflicts = BTreeMap::<String, Vec<String>>::new();
        let mut request_scopes = BTreeMap::<String, (String, String)>::new();
        let mut requests = Vec::new();
        for raw in rows::<Value>(response, "NetworkEnrollmentRequest")? {
            let (request_id, scope) = match attribute_candidate_request(&raw, &network.network_id) {
                CandidateRequestAttribution::Exact { request_id, scope } => (request_id, scope),
                CandidateRequestAttribution::RequestOnly(request_id) => {
                    add_request_conflict(
                        &mut request_conflicts,
                        &request_id,
                        "malformed enrollment request has no candidate_did",
                    );
                    continue;
                }
                CandidateRequestAttribution::ScopeOnly(scope) => {
                    add_scope_conflict(
                        &mut scoped_conflicts,
                        scope,
                        "malformed enrollment request has no request_id",
                    );
                    continue;
                }
                CandidateRequestAttribution::Foreign => continue,
                CandidateRequestAttribution::Unattributable => {
                    tracing::warn!(
                        "quarantining enrollment request without request or member identity"
                    );
                    continue;
                }
            };
            if let Some(existing) = request_scopes.insert(request_id.clone(), scope.clone()) {
                if existing != scope {
                    add_request_conflict(
                        &mut request_conflicts,
                        &request_id,
                        "request identity is bound to conflicting member scopes",
                    );
                    tracing::warn!(
                        ?existing,
                        ?scope,
                        request_id,
                        "immutable request identity is bound to conflicting member scopes"
                    );
                }
            }
            let row = match serde_json::from_value::<RequestRow>(raw) {
                Ok(row) => row,
                Err(error) => {
                    add_request_conflict(
                        &mut request_conflicts,
                        &request_id,
                        format!("malformed enrollment request: {error}"),
                    );
                    continue;
                }
            };
            match self.verify_request(row, network).await {
                Ok(request) => requests.push(request),
                Err(error) => add_request_conflict(
                    &mut request_conflicts,
                    &request_id,
                    format!("invalid enrollment request: {error:#}"),
                ),
            }
        }
        Ok(VerifiedRequests {
            requests,
            request_scopes,
            findings: ProjectionFindings {
                scoped_conflicts,
                request_conflicts,
                ..ProjectionFindings::default()
            },
        })
    }

    pub(super) async fn verify_decisions(
        &self,
        response: &query::QueryResponse,
        index: RequestIndex<'_>,
        findings: &mut ProjectionFindings,
    ) -> Result<Vec<VerifiedDecision>> {
        let RequestIndex {
            network,
            request_scopes,
            requests_by_id,
        } = index;
        let ProjectionFindings {
            scoped_conflicts,
            request_conflicts,
            invalid_authority_max,
            observed_revision_next,
            ..
        } = findings;
        let mut decisions = Vec::new();
        for raw in rows::<Value>(response, "NetworkEnrollmentDecision")? {
            let request_id = raw_text(&raw, "request_id").map(str::to_string);
            let request_key = request_id.as_deref().unwrap_or("");
            let Some(scope) = scope_for_authority_row(
                &raw,
                request_key,
                request_scopes,
                &network.network_id,
                "candidate_did",
            )?
            else {
                continue;
            };
            let observed_sequence =
                raw_i64(&raw, "authorization_sequence").and_then(|value| u64::try_from(value).ok());
            if let Some(sequence) = observed_sequence {
                observe_next_authorization_sequence(
                    observed_revision_next,
                    scoped_conflicts,
                    scope.clone(),
                    sequence,
                );
            }
            let Some(request_id) = request_id else {
                if let Some(sequence) = observed_sequence {
                    invalid_authority_max
                        .entry(scope)
                        .and_modify(|current| *current = (*current).max(sequence))
                        .or_insert(sequence);
                } else {
                    add_scope_conflict(
                        scoped_conflicts,
                        scope,
                        "malformed enrollment decision has no request_id or usable generation",
                    );
                }
                continue;
            };
            let row = match serde_json::from_value::<DecisionRow>(raw) {
                Ok(row) => row,
                Err(error) => {
                    if let Some(sequence) = observed_sequence {
                        invalid_authority_max
                            .entry(scope.clone())
                            .and_modify(|current| *current = (*current).max(sequence))
                            .or_insert(sequence);
                    }
                    add_request_conflict(
                        request_conflicts,
                        &request_id,
                        format!("malformed enrollment decision: {error}"),
                    );
                    continue;
                }
            };
            let verified = async {
                let record = row.to_record()?;
                let request = unique_request(requests_by_id, &record.request_id)?;
                record.validate_against_request(&request.record)?;
                let signed = self
                    .identity
                    .verify(
                        &record.signer_did,
                        &record.signing_payload(),
                        &record.admin_sig,
                    )
                    .await?;
                anyhow::ensure!(signed, "enrollment decision signature is invalid");
                Ok::<_, anyhow::Error>(VerifiedDecision {
                    doc_id: row.doc_id,
                    record,
                    signed,
                })
            }
            .await;
            match verified {
                Ok(decision) => decisions.push(decision),
                Err(error) => {
                    if let Some(sequence) = observed_sequence {
                        invalid_authority_max
                            .entry(scope.clone())
                            .and_modify(|current| *current = (*current).max(sequence))
                            .or_insert(sequence);
                    }
                    add_request_conflict(
                        request_conflicts,
                        &request_id,
                        format!("invalid enrollment decision: {error:#}"),
                    );
                    tracing::warn!(?scope, request_id, error = %error, "scoped invalid enrollment decision");
                }
            }
        }
        Ok(decisions)
    }

    pub(super) async fn verify_revisions(
        &self,
        response: &query::QueryResponse,
        index: RequestIndex<'_>,
        decisions_by_request: &BTreeMap<String, Vec<&VerifiedDecision>>,
        findings: &mut ProjectionFindings,
    ) -> Result<Vec<VerifiedRevision>> {
        let RequestIndex {
            network,
            request_scopes,
            requests_by_id,
        } = index;
        let ProjectionFindings {
            scoped_conflicts,
            invalid_authority_max,
            observed_revision_next,
            ..
        } = findings;
        let mut revisions = Vec::new();
        for raw in rows::<Value>(response, "NetworkAuthorizationRevision")? {
            let request_id = raw_text(&raw, "request_id").map(str::to_string);
            let request_key = request_id.as_deref().unwrap_or("");
            let Some(scope) = scope_for_authority_row(
                &raw,
                request_key,
                request_scopes,
                &network.network_id,
                "member_did",
            )?
            else {
                continue;
            };
            let observed_sequence =
                raw_i64(&raw, "sequence").and_then(|value| u64::try_from(value).ok());
            if let Some(sequence) = observed_sequence {
                observe_next_authorization_sequence(
                    observed_revision_next,
                    scoped_conflicts,
                    scope.clone(),
                    sequence,
                );
            }
            let Some(request_id) = request_id else {
                if let Some(sequence) = observed_sequence {
                    invalid_authority_max
                        .entry(scope)
                        .and_modify(|current| *current = (*current).max(sequence))
                        .or_insert(sequence);
                } else {
                    add_scope_conflict(
                        scoped_conflicts,
                        scope,
                        "malformed authorization revision has no request_id or usable sequence",
                    );
                }
                continue;
            };
            let row = match serde_json::from_value::<RevisionRow>(raw) {
                Ok(row) => row,
                Err(error) => {
                    if let Some(sequence) = observed_sequence {
                        invalid_authority_max
                            .entry(scope.clone())
                            .and_modify(|current| *current = (*current).max(sequence))
                            .or_insert(sequence);
                    } else {
                        add_scope_conflict(
                            scoped_conflicts,
                            scope,
                            format!(
                                "malformed authorization revision without usable sequence: {error}"
                            ),
                        );
                    }
                    continue;
                }
            };
            let verified = async {
                let record = row.to_record()?;
                let request = unique_request(requests_by_id, &record.request_id)?;
                let decision = unique_approved_decision(decisions_by_request, &record.request_id)?;
                record.validate_against_approval(&request.record, &decision.record)?;
                let signed = self
                    .identity
                    .verify(
                        &record.signer_did,
                        &record.signing_payload(),
                        &record.admin_sig,
                    )
                    .await?;
                anyhow::ensure!(signed, "authorization revision signature is invalid");
                Ok::<_, anyhow::Error>(VerifiedRevision {
                    doc_id: row.doc_id,
                    record,
                    signed,
                })
            }
            .await;
            match verified {
                Ok(revision) => revisions.push(revision),
                Err(error) => {
                    if let Some(sequence) = observed_sequence {
                        invalid_authority_max
                            .entry(scope.clone())
                            .and_modify(|current| *current = (*current).max(sequence))
                            .or_insert(sequence);
                    } else {
                        add_scope_conflict(
                            scoped_conflicts,
                            scope.clone(),
                            "invalid authorization revision without usable sequence",
                        );
                    }
                    tracing::warn!(?scope, request_id, error = %error, "scoped invalid authorization revision");
                }
            }
        }
        Ok(revisions)
    }

    pub(super) async fn verify_receipts(
        &self,
        response: &query::QueryResponse,
        index: RequestIndex<'_>,
        decisions_by_request: &BTreeMap<String, Vec<&VerifiedDecision>>,
        findings: &mut ProjectionFindings,
    ) -> Result<Vec<VerifiedRouteReceipt>> {
        let RequestIndex {
            network,
            request_scopes,
            requests_by_id,
        } = index;
        let ProjectionFindings {
            scoped_conflicts,
            invalid_receipt_generations,
            ..
        } = findings;
        let mut receipts = Vec::new();
        for raw in rows::<Value>(response, "NetworkEnrollmentRouteReceipt")? {
            let request_id = raw_text(&raw, "request_id").map(str::to_string);
            let request_key = request_id.as_deref().unwrap_or("");
            let Some(scope) = scope_for_authority_row(
                &raw,
                request_key,
                request_scopes,
                &network.network_id,
                "member_did",
            )?
            else {
                continue;
            };
            let sequence =
                raw_i64(&raw, "authorization_sequence").and_then(|value| u64::try_from(value).ok());
            let Some(request_id) = request_id else {
                if let Some(sequence) = sequence {
                    invalid_receipt_generations.insert((scope, sequence));
                } else {
                    add_scope_conflict(
                        scoped_conflicts,
                        scope,
                        "malformed route receipt has no request_id or usable generation",
                    );
                }
                continue;
            };
            let row = match serde_json::from_value::<RouteReceiptRow>(raw) {
                Ok(row) => row,
                Err(error) => {
                    if let Some(sequence) = sequence {
                        invalid_receipt_generations.insert((scope, sequence));
                    } else {
                        add_scope_conflict(
                            scoped_conflicts,
                            scope,
                            format!("malformed route receipt without usable generation: {error}"),
                        );
                    }
                    continue;
                }
            };
            let verified = async {
                let record = row.to_record()?;
                let request = unique_request(requests_by_id, &record.request_id)?;
                let decision = unique_approved_decision(decisions_by_request, &record.request_id)?;
                record.validate_against_approval(&request.record, &decision.record)?;
                let signed = self
                    .identity
                    .verify(
                        &record.signer_did,
                        &record.signing_payload(),
                        &record.admin_sig,
                    )
                    .await?;
                anyhow::ensure!(signed, "enrollment route receipt signature is invalid");
                Ok::<_, anyhow::Error>(VerifiedRouteReceipt {
                    doc_id: row.doc_id,
                    record,
                })
            }
            .await;
            match verified {
                Ok(receipt) => receipts.push(receipt),
                Err(error) => {
                    if let Some(sequence) = sequence {
                        invalid_receipt_generations.insert((scope.clone(), sequence));
                    } else {
                        add_scope_conflict(
                            scoped_conflicts,
                            scope.clone(),
                            "invalid route receipt without usable generation",
                        );
                    }
                    tracing::warn!(?scope, request_id, error = %error, "scoped invalid enrollment route receipt");
                }
            }
        }
        Ok(receipts)
    }
}

pub(super) fn flag_request_identity_conflicts(
    requests: &[VerifiedRequest],
    requests_by_id: &BTreeMap<String, Vec<&VerifiedRequest>>,
    request_conflicts: &mut BTreeMap<String, Vec<String>>,
) -> BTreeSet<String> {
    for (request_id, rows) in requests_by_id {
        if rows.len() != 1 {
            add_request_conflict(
                request_conflicts,
                request_id,
                format!("request identity has {} distinct rows", rows.len()),
            );
        }
    }
    let mut challenge_requests = BTreeMap::<String, BTreeSet<String>>::new();
    for request in requests {
        challenge_requests
            .entry(request.record.challenge.clone())
            .or_default()
            .insert(request.record.request_id.clone());
    }
    let mut reused_challenge_requests = BTreeSet::new();
    for request_ids in challenge_requests.values().filter(|ids| ids.len() > 1) {
        reused_challenge_requests.extend(request_ids.iter().cloned());
        for request_id in request_ids {
            add_request_conflict(
                request_conflicts,
                request_id,
                "enrollment challenge is reused by another request",
            );
        }
    }
    reused_challenge_requests
}

pub(super) fn flag_duplicate_decisions(
    decisions_by_request: &BTreeMap<String, Vec<&VerifiedDecision>>,
    request_conflicts: &mut BTreeMap<String, Vec<String>>,
) {
    for (request_id, rows) in decisions_by_request {
        if rows.len() != 1 {
            add_request_conflict(
                request_conflicts,
                request_id,
                format!("request has {} immutable terminal decisions", rows.len()),
            );
        }
    }
}

pub(super) fn flag_current_invalid_authority(
    revisions: &[VerifiedRevision],
    findings: &mut ProjectionFindings,
) -> BTreeMap<MemberScope, u64> {
    let ProjectionFindings {
        scoped_conflicts,
        invalid_authority_max,
        ..
    } = findings;
    let valid_revision_max = revisions
        .iter()
        .fold(BTreeMap::new(), |mut maxes, revision| {
            let scope = (
                revision.record.network_id.clone(),
                revision.record.member_did.clone(),
            );
            maxes
                .entry(scope)
                .and_modify(|current: &mut u64| *current = (*current).max(revision.record.sequence))
                .or_insert(revision.record.sequence);
            maxes
        });
    for (scope, invalid_max) in std::mem::take(invalid_authority_max) {
        if invalid_revision_is_current(valid_revision_max.get(&scope).copied(), invalid_max) {
            add_scope_conflict(
                scoped_conflicts,
                scope,
                format!("invalid authorization revision at observed sequence {invalid_max}"),
            );
        }
    }
    valid_revision_max
}

pub(super) fn durable_documents(
    network: &NetworkRecord,
    now: DateTime<Utc>,
    documents: VerifiedDocuments<'_>,
    valid_revision_max: &BTreeMap<MemberScope, u64>,
    findings: &mut ProjectionFindings,
) -> DurableEnrollmentDocuments {
    let VerifiedDocuments {
        requests,
        decisions,
        revisions,
        receipts,
        decisions_by_request,
    } = documents;
    let ProjectionFindings {
        scoped_conflicts,
        invalid_receipt_generations,
        ..
    } = findings;
    let mut durable = DurableEnrollmentDocuments {
        admin_pins: BTreeSet::from([NetworkAdminPin {
            network_id: network.network_id.clone(),
            admin_did: network.admin_did.clone(),
        }]),
        ..DurableEnrollmentDocuments::default()
    };
    for verified in requests {
        durable.offers.insert(verified.offer.clone());
        durable.requests.insert(
            verified.pure_request(
                now,
                decisions_by_request
                    .get(&verified.record.request_id)
                    .is_some_and(|rows| rows.iter().any(|decision| decision.signed)),
            ),
        );
    }
    for verified in decisions {
        durable.decisions.insert(verified.pure(now));
    }
    for verified in revisions {
        durable.revisions.insert(verified.pure());
    }
    for verified in receipts {
        durable.route_receipts.insert(verified.pure());
    }
    for receipt in receipts {
        if !durable.route_receipt_identity_unique(&receipt.pure()) {
            invalid_receipt_generations.insert((
                (
                    receipt.record.network_id.clone(),
                    receipt.record.member_did.clone(),
                ),
                receipt.record.authorization_sequence,
            ));
        }
    }
    for (scope, invalid_sequence) in &*invalid_receipt_generations {
        if valid_revision_max
            .get(scope)
            .is_some_and(|valid_sequence| valid_sequence == invalid_sequence)
        {
            add_scope_conflict(
                scoped_conflicts,
                scope.clone(),
                format!(
                    "invalid enrollment route receipt at authorization sequence {invalid_sequence}"
                ),
            );
        }
    }
    durable
}

pub(super) fn project_decided(
    now: DateTime<Utc>,
    documents: VerifiedDocuments<'_>,
    durable: &DurableEnrollmentDocuments,
    findings: &ProjectionFindings,
    projection: &mut EnrollmentProjection,
) -> Result<()> {
    let VerifiedDocuments {
        requests,
        revisions,
        receipts,
        decisions_by_request,
        ..
    } = documents;
    let ProjectionFindings {
        scoped_conflicts,
        request_conflicts,
        invalid_receipt_generations,
        ..
    } = findings;
    for verified in requests {
        if request_conflicts.contains_key(&verified.record.request_id) {
            continue;
        }
        let matching_decisions = decisions_by_request
            .get(&verified.record.request_id)
            .cloned()
            .unwrap_or_default();
        if matching_decisions.is_empty() {
            if verified.is_fresh(now)? {
                projection.pending.push(PendingEnrollment {
                    request_doc_id: verified.doc_id.clone(),
                    offer_token: verified.record.offer_token.clone(),
                    request: verified.record.clone(),
                });
            }
            continue;
        }
        for decision in matching_decisions {
            if decision.record.decision == WireDecisionKind::Denied && decision.signed {
                projection.denied.push(DeniedEnrollment {
                    request_doc_id: verified.doc_id.clone(),
                    decision_doc_id: decision.doc_id.clone(),
                    request: verified.record.clone(),
                    decision: decision.record.clone(),
                });
                continue;
            }
            let pure_request = verified.pure_request(now, decision.signed);
            let pure_decision = decision.pure(now);
            let Some(revision_rows) = revisions_for(revisions, &verified.record.request_id) else {
                continue;
            };
            for revision in revision_rows {
                let scope = (
                    verified.record.network_id.clone(),
                    verified.record.candidate_did.clone(),
                );
                if scoped_conflicts.contains_key(&scope) {
                    continue;
                }
                if durable.current_approval(&verified.offer, &pure_request, &pure_decision)
                    && revision.record.kind == WireRevisionKind::Active
                    && revision.record.sequence == decision.record.authorization_sequence
                {
                    let current_receipts = receipts
                        .iter()
                        .filter(|receipt| {
                            durable.current_server_route_receipt(
                                &verified.offer,
                                &pure_request,
                                &pure_decision,
                                &receipt.pure(),
                            )
                        })
                        .collect::<Vec<_>>();
                    let current_receipt = if invalid_receipt_generations
                        .contains(&(scope, decision.record.authorization_sequence))
                        || current_receipts.len() > 1
                    {
                        None
                    } else {
                        current_receipts.first().copied()
                    };
                    projection.active.push(ActiveEnrollment {
                        request_doc_id: verified.doc_id.clone(),
                        decision_doc_id: decision.doc_id.clone(),
                        revision_doc_id: revision.doc_id.clone(),
                        route_receipt_doc_id: current_receipt.map(|receipt| receipt.doc_id.clone()),
                        request: verified.record.clone(),
                        decision: decision.record.clone(),
                        revision: revision.record.clone(),
                        route_receipt: current_receipt.map(|receipt| receipt.record.clone()),
                    });
                }
            }
        }
    }
    Ok(())
}

pub(super) fn project_revoked(
    documents: VerifiedDocuments<'_>,
    findings: &ProjectionFindings,
    projection: &mut EnrollmentProjection,
) {
    let VerifiedDocuments {
        requests,
        revisions,
        decisions_by_request,
        ..
    } = documents;
    let ProjectionFindings {
        scoped_conflicts,
        request_conflicts,
        ..
    } = findings;
    for verified in requests {
        if request_conflicts.contains_key(&verified.record.request_id) {
            continue;
        }
        let scope = (
            verified.record.network_id.as_str(),
            verified.record.candidate_did.as_str(),
        );
        if scoped_conflicts.contains_key(&(scope.0.to_string(), scope.1.to_string())) {
            continue;
        }
        let scoped = revisions
            .iter()
            .filter(|revision| {
                revision.record.network_id == scope.0 && revision.record.member_did == scope.1
            })
            .collect::<Vec<_>>();
        let Some(max_sequence) = scoped.iter().map(|row| row.record.sequence).max() else {
            continue;
        };
        let maxima = scoped
            .into_iter()
            .filter(|row| row.record.sequence == max_sequence)
            .collect::<Vec<_>>();
        let [revision] = maxima.as_slice() else {
            continue;
        };
        if revision.record.kind != WireRevisionKind::Revoked
            || revision.record.request_id != verified.record.request_id
        {
            continue;
        }
        let Some([decision]) = decisions_by_request
            .get(&verified.record.request_id)
            .map(Vec::as_slice)
        else {
            continue;
        };
        if decision.record.decision != WireDecisionKind::Approved {
            continue;
        }
        projection.revoked.push(RevokedEnrollment {
            request_doc_id: verified.doc_id.clone(),
            decision_doc_id: decision.doc_id.clone(),
            revision_doc_id: revision.doc_id.clone(),
            request: verified.record.clone(),
            decision: decision.record.clone(),
            revision: revision.record.clone(),
        });
    }
}

pub(super) fn finish_projection(
    mut projection: EnrollmentProjection,
    findings: ProjectionFindings,
    reused_challenge_requests: BTreeSet<String>,
) -> EnrollmentProjection {
    let ProjectionFindings {
        mut scoped_conflicts,
        request_conflicts,
        ..
    } = findings;
    projection
        .pending
        .sort_by(|a, b| a.request.request_id.cmp(&b.request.request_id));
    projection
        .active
        .sort_by(|a, b| a.request.request_id.cmp(&b.request.request_id));
    let mut collision_scopes = BTreeSet::new();
    for (index, active) in projection.active.iter().enumerate() {
        for other in &projection.active[index + 1..] {
            if active.request.candidate_peer == other.request.candidate_peer
                && (active.request.candidate_did != other.request.candidate_did
                    || active.request.owner_node_did != other.request.owner_node_did)
            {
                collision_scopes.insert((
                    active.request.network_id.clone(),
                    active.request.candidate_did.clone(),
                ));
                collision_scopes.insert((
                    other.request.network_id.clone(),
                    other.request.candidate_did.clone(),
                ));
            }
        }
    }
    for scope in collision_scopes {
        add_scope_conflict(
            &mut scoped_conflicts,
            scope,
            "current enrollment routes collide on one transport peer",
        );
    }
    projection.active.retain(|active| {
        !scoped_conflicts.contains_key(&(
            active.request.network_id.clone(),
            active.request.candidate_did.clone(),
        ))
    });
    projection
        .denied
        .sort_by(|a, b| a.request.request_id.cmp(&b.request.request_id));
    projection
        .revoked
        .sort_by(|a, b| a.request.request_id.cmp(&b.request.request_id));
    projection.scoped_conflicts = finish_conflicts(scoped_conflicts);
    projection.request_conflicts = finish_conflicts(request_conflicts);
    projection.reused_challenge_requests = reused_challenge_requests;
    projection
}
