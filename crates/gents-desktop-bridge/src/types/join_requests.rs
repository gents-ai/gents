use gents_server::server_host::{
    ManagedEnrollmentDecision, ManagedEnrollmentDecisionOutcome, ManagedPendingEnrollment,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A request to join the node this desktop co-hosts, as its operator API
/// reports it; nothing here is enrollment authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct JoinRequestView {
    pub request_id: String,
    pub network_id: String,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub owner_agent: String,
    pub expires_at: String,
    /// Absent from runtimes that predate request profiles.
    pub profile: Option<String>,
    /// Absent from runtimes that predate request issue times.
    pub issued_at: Option<String>,
}

impl From<ManagedPendingEnrollment> for JoinRequestView {
    fn from(pending: ManagedPendingEnrollment) -> Self {
        Self {
            request_id: pending.request_id,
            network_id: pending.network_id,
            candidate_did: pending.candidate_did,
            candidate_peer: pending.candidate_peer,
            owner_agent: pending.owner_agent,
            expires_at: pending.expires_at,
            profile: pending.profile,
            issued_at: pending.issued_at,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum JoinRequestDecision {
    /// The runtime validates the lease against the protocol's bounds.
    Approve {
        lease_seconds: u64,
    },
    Deny,
}

impl From<JoinRequestDecision> for ManagedEnrollmentDecision {
    fn from(decision: JoinRequestDecision) -> Self {
        match decision {
            JoinRequestDecision::Approve { lease_seconds } => Self::Approve { lease_seconds },
            JoinRequestDecision::Deny => Self::Deny,
        }
    }
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct JoinRequestDecideRequest {
    pub node_did: String,
    pub request_id: String,
    pub decision: JoinRequestDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct JoinRequestDecisionView {
    pub request_id: String,
    pub state: String,
    /// The decision is recorded; the candidate has not received it yet.
    pub delivery_pending: bool,
}

impl From<ManagedEnrollmentDecisionOutcome> for JoinRequestDecisionView {
    fn from(outcome: ManagedEnrollmentDecisionOutcome) -> Self {
        Self {
            request_id: outcome.request_id,
            state: outcome.state,
            delivery_pending: outcome.delivery_pending,
        }
    }
}
