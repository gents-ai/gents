//! Enrollment fixtures shared by this owner's tests and by the root crate's
//! claim-boundary tests, which cannot reach the private authority loop.

use std::collections::VecDeque;
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use gents_protocol::enrollment::{
    AuthorizationRevisionKind, AuthorizationRevisionRecord, EnrollmentDecisionKind,
    EnrollmentDecisionRecord, EnrollmentRequestRecord, ENROLLMENT_PROTOCOL_VERSION,
};

use super::{handle_authority_command, EnrollmentAuthorityOwner, EnrollmentProjectionReader};
use crate::p2p_reconcile::enrollment_store::{
    ActiveEnrollment, EnrollmentProjection, RevokedEnrollment,
};

pub enum ScriptedProjection {
    Projection(Box<EnrollmentProjection>),
    Error(&'static str),
}

pub struct ScriptedProjectionReader {
    pub steps: Mutex<VecDeque<ScriptedProjection>>,
}

#[async_trait]
impl EnrollmentProjectionReader for ScriptedProjectionReader {
    async fn load_authority_projection(&self) -> Result<EnrollmentProjection> {
        match self
            .steps
            .lock()
            .expect("scripted projection lock")
            .pop_front()
            .expect("scripted projection step")
        {
            ScriptedProjection::Projection(projection) => Ok(*projection),
            ScriptedProjection::Error(message) => anyhow::bail!(message),
        }
    }
}

pub fn active_enrollment(sequence: u64, expires_at: &str) -> ActiveEnrollment {
    let request_id = format!("enrollment-request-{sequence}");
    let request_digest = format!("enrollment-digest-{sequence}");
    let request = EnrollmentRequestRecord {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        request_id: request_id.clone(),
        request_digest: request_digest.clone(),
        offer_id: format!("offer-{sequence}"),
        offer_token: format!("token-{sequence}"),
        challenge: format!("challenge-{sequence}"),
        network_id: "network-1".into(),
        admin_did: "did:key:admin".into(),
        server_peer: "server-peer".into(),
        candidate_did: "did:key:member-placeholder".into(),
        candidate_peer: "member-peer".into(),
        candidate_name: None,
        candidate_ticket: "member-ticket".into(),
        owner_node_did: "did:key:owner-placeholder".into(),
        profile: "client".into(),
        client_nonce: format!("nonce-{sequence}"),
        issued_at: "2030-01-01T00:00:00Z".into(),
        expires_at: "2031-01-01T00:00:00Z".into(),
        candidate_sig: vec![1; 64],
    };
    let decision = EnrollmentDecisionRecord {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        decision_id: format!("decision-{sequence}"),
        request_id: request_id.clone(),
        request_digest: request_digest.clone(),
        network_id: request.network_id.clone(),
        admin_did: request.admin_did.clone(),
        candidate_did: request.candidate_did.clone(),
        candidate_peer: request.candidate_peer.clone(),
        owner_node_did: request.owner_node_did.clone(),
        decision: EnrollmentDecisionKind::Approved,
        authorization_sequence: sequence,
        authorization_expires_at: expires_at.into(),
        decided_at: "2030-01-01T00:00:01Z".into(),
        signer_did: request.admin_did.clone(),
        admin_sig: vec![2; 64],
    };
    let revision = AuthorizationRevisionRecord {
        protocol_version: ENROLLMENT_PROTOCOL_VERSION,
        revision_id: format!("revision-{sequence}"),
        request_id,
        request_digest,
        network_id: request.network_id.clone(),
        admin_did: request.admin_did.clone(),
        member_did: request.candidate_did.clone(),
        member_peer: request.candidate_peer.clone(),
        owner_node_did: request.owner_node_did.clone(),
        sequence,
        authorization_expires_at: expires_at.into(),
        kind: AuthorizationRevisionKind::Active,
        issued_at: "2030-01-01T00:00:01Z".into(),
        signer_did: request.admin_did.clone(),
        admin_sig: vec![3; 64],
    };
    ActiveEnrollment {
        request_doc_id: format!("request-doc-{sequence}"),
        decision_doc_id: format!("decision-doc-{sequence}"),
        revision_doc_id: format!("revision-doc-{sequence}"),
        route_receipt_doc_id: None,
        request,
        decision,
        revision,
        route_receipt: None,
    }
}

pub fn projection_with_active(active: ActiveEnrollment) -> EnrollmentProjection {
    EnrollmentProjection {
        network_id: Some(active.request.network_id.clone()),
        active: vec![active],
        ..EnrollmentProjection::default()
    }
}

pub fn projection_with_revoked(active: &ActiveEnrollment) -> EnrollmentProjection {
    let mut revision = active.revision.clone();
    revision.kind = AuthorizationRevisionKind::Revoked;
    EnrollmentProjection {
        network_id: Some(active.request.network_id.clone()),
        revoked: vec![RevokedEnrollment {
            request_doc_id: active.request_doc_id.clone(),
            decision_doc_id: active.decision_doc_id.clone(),
            revision_doc_id: format!("revoked-{}", active.revision.sequence + 1),
            request: active.request.clone(),
            decision: active.decision.clone(),
            revision,
        }],
        ..EnrollmentProjection::default()
    }
}

/// Serves one authority command per scripted step, then ends.
pub fn spawn_scripted_authority(
    mut owner: EnrollmentAuthorityOwner,
    steps: Vec<ScriptedProjection>,
) -> tokio::task::JoinHandle<()> {
    let count = steps.len();
    let script = ScriptedProjectionReader {
        steps: Mutex::new(VecDeque::from(steps)),
    };
    tokio::spawn(async move {
        for _ in 0..count {
            let command = owner.commands.recv().await.expect("authority command");
            handle_authority_command(&script, &owner.sender, command).await;
        }
    })
}
