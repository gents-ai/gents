use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_p2p_adapter::TransportPeerId;
use futures::{stream, StreamExt};
use gents::agent::p2p_reconcile::enrollment_candidate::validate_authenticated_server_did;
use p2p::iroh::parse_public_peer_addr;
use tokio::time::timeout;

use super::super::super::principal_identity::PrincipalIdentity;
use super::super::route_manager::ClientRouteManager;
use super::super::sync_state::ClientSyncStateOwner;
use super::super::{EnrollmentActivationError, EnrollmentActivationStage, P2P_OPERATION_TIMEOUT};
use super::authority::load_status_enrollment_approvals;
use super::map_p2p_error;
use crate::client::peer_directory::RemovalCause;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ApprovedStatusEnrollment {
    pub(super) network_id: String,
    pub(super) request_id: String,
    pub(super) server_peer: String,
    pub(super) server_ticket: String,
    pub(super) admin_did: String,
    pub(super) owner_node: String,
    pub(super) request_digest: String,
    pub(super) authorization_sequence: u64,
    pub(super) authorization_expires_at: String,
    pub(super) decided_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::client::core) struct EnrollmentAuthorizationGeneration {
    pub request_digest: String,
    pub sequence: u64,
    pub expires_at: String,
}

impl EnrollmentAuthorizationGeneration {
    fn matches_record_at(
        &self,
        record: &super::super::super::peer_directory::PeerRecord,
        now: DateTime<Utc>,
    ) -> bool {
        record.enrollment_request_digest.as_deref() == Some(self.request_digest.as_str())
            && record.enrollment_authorization_sequence == Some(self.sequence)
            && record.enrollment_authorization_expires_at.as_deref() == Some(&self.expires_at)
            && gents_protocol::enrollment::authorization_lease_is_fresh_at(&self.expires_at, now)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum EnrollmentAuthorityOutcome {
    Current(ApprovedStatusEnrollment),
    Conflicted { reason: String },
}

pub(super) enum DesktopApprovalProjection {
    Routed(Box<ApprovedStatusEnrollment>),
    CurrentWithoutRoute,
    Absent,
}

pub(super) fn prioritized_current_approvals(
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

pub(in crate::client::core) fn enrollment_record_lacks_current_authority(
    record: &super::super::super::peer_directory::PeerRecord,
    approved_peers: &BTreeMap<String, EnrollmentAuthorizationGeneration>,
) -> bool {
    record.source.as_deref() == Some("enrollment")
        && !approved_peers
            .get(&record.peer_id)
            .is_some_and(|generation| generation.matches_record_at(record, Utc::now()))
}

pub(in crate::client::core) async fn reconcile_status_enrollment_approvals(
    node: &Arc<defra_node::EmbeddedNode>,
    p2p: &Arc<dyn defra_p2p_adapter::P2POperations>,
    principal: &Arc<PrincipalIdentity>,
    local_peer_id: &str,
    sync_state: &ClientSyncStateOwner,
    route_manager: &Arc<ClientRouteManager>,
) -> Result<BTreeMap<String, EnrollmentAuthorizationGeneration>> {
    let (outcomes, authorized_server_peers) =
        load_status_enrollment_approvals(node.as_ref(), principal.as_ref(), local_peer_id).await?;
    // The prune keys on authority presence, not on the receipt-gated outcomes:
    // a missing route receipt must not end a retirement whose decision and
    // revision still read current, or the peer it removed could be reinstalled
    // once the receipt observation returns. Conflicted scopes keep their
    // retirements too.
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
    approvals.retain(|(_, approval)| !retired.contains(&approval.request_digest));
    let generations = approvals
        .iter()
        .map(|(_, approval)| {
            (
                approval.request_id.clone(),
                EnrollmentAuthorizationGeneration {
                    request_digest: approval.request_digest.clone(),
                    sequence: approval.authorization_sequence,
                    expires_at: approval.authorization_expires_at.clone(),
                },
            )
        })
        .collect();
    sync_state.retain_enrollment_activation_errors(&generations);
    let mut activation_errors = BTreeMap::new();
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
    // no longer current; conflicted peers remain present but closed so
    // hostile state in peer A cannot erase peer B or lose A's retry identity.
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
                activation_errors.insert(
                    approval.request_id.clone(),
                    enrollment_activation_error(
                        &approval,
                        EnrollmentActivationStage::Authentication,
                    ),
                );
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
                activation_errors.insert(
                    approval.request_id.clone(),
                    enrollment_activation_error(&approval, EnrollmentActivationStage::Route),
                );
                demote_enrollment_peer(sync_state, &peer_id).await;
            }
        }
    }
    sync_state.finish_enrollment_activation_errors(&generations, &activation_errors);
    Ok(current_authority)
}

fn enrollment_activation_error(
    approval: &ApprovedStatusEnrollment,
    stage: EnrollmentActivationStage,
) -> EnrollmentActivationError {
    EnrollmentActivationError {
        request_digest: approval.request_digest.clone(),
        authorization_sequence: approval.authorization_sequence,
        authorization_expires_at: approval.authorization_expires_at.clone(),
        stage,
    }
}

pub(super) async fn authenticate_enrolled_server(
    p2p: &Arc<dyn defra_p2p_adapter::P2POperations>,
    approval: &ApprovedStatusEnrollment,
    known: Option<&super::super::super::peer_directory::PeerRecord>,
) -> Result<String> {
    let started = std::time::Instant::now();
    let mut stage = "discovering enrolled server address";
    let result = timeout(P2P_OPERATION_TIMEOUT, async {
        let address = enrolled_server_address(approval, known).await?;
        let peer = TransportPeerId::new(approval.server_peer.clone()).map_err(map_p2p_error)?;
        stage = "checking enrolled server connection";
        if !super::super::bootstrap::is_connected_peer(p2p, peer.as_str()).await? {
            stage = "connecting to enrolled server";
            p2p.connect_peer(&address).await.map_err(map_p2p_error)?;
        }
        stage = "resolving enrolled server identity";
        let resolved = p2p
            .resolve_peer_identity(&peer)
            .await
            .map_err(map_p2p_error)?
            .context("enrolled server has no authenticated transport identity")?;
        validate_authenticated_server_did(&approval.admin_did, resolved.as_ref())?;
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

// Discovery supplies location only: the ticket must name the approved peer,
// and authentication still checks the signed admin.
pub(super) async fn enrolled_server_address(
    approval: &ApprovedStatusEnrollment,
    known: Option<&super::super::super::peer_directory::PeerRecord>,
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

pub(super) async fn demote_enrollment_peer(sync_state: &ClientSyncStateOwner, peer_id: &str) {
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
