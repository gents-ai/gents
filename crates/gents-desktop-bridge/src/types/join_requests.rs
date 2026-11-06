use gents_server::server_host::{
    ManagedEnrollmentDecision, ManagedEnrollmentDecisionOutcome, ManagedEnrollmentHistory,
    ManagedEnrollmentHistoryEntry, ManagedEnrollmentHistoryState, ManagedEnrollmentMember,
    ManagedPendingEnrollment,
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
    Revoke,
}

impl From<JoinRequestDecision> for ManagedEnrollmentDecision {
    fn from(decision: JoinRequestDecision) -> Self {
        match decision {
            JoinRequestDecision::Approve { lease_seconds } => Self::Approve { lease_seconds },
            JoinRequestDecision::Deny => Self::Deny,
            JoinRequestDecision::Revoke => Self::Revoke,
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

/// Someone with access to the node this desktop co-hosts, known by the
/// approved request that let them in. Expired leases aren't listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct JoinMemberView {
    pub request_id: String,
    pub network_id: String,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub profile: String,
    pub authorization_expires_at: String,
}

impl From<ManagedEnrollmentMember> for JoinMemberView {
    fn from(member: ManagedEnrollmentMember) -> Self {
        Self {
            request_id: member.request_id,
            network_id: member.network_id,
            candidate_did: member.candidate_did,
            candidate_peer: member.candidate_peer,
            profile: member.profile,
            authorization_expires_at: member.authorization_expires_at,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum JoinHistoryState {
    Approved,
    Denied,
    Revoked,
}

impl From<ManagedEnrollmentHistoryState> for JoinHistoryState {
    fn from(state: ManagedEnrollmentHistoryState) -> Self {
        match state {
            ManagedEnrollmentHistoryState::Approved => Self::Approved,
            ManagedEnrollmentHistoryState::Denied => Self::Denied,
            ManagedEnrollmentHistoryState::Revoked => Self::Revoked,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct JoinHistoryEntryView {
    pub request_id: String,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub profile: String,
    pub state: JoinHistoryState,
    pub decided_at: String,
    pub decided_by: String,
    pub revoked_at: Option<String>,
    pub revoked_by: Option<String>,
}

impl From<ManagedEnrollmentHistoryEntry> for JoinHistoryEntryView {
    fn from(entry: ManagedEnrollmentHistoryEntry) -> Self {
        Self {
            request_id: entry.request_id,
            candidate_did: entry.candidate_did,
            candidate_peer: entry.candidate_peer,
            profile: entry.profile,
            state: entry.state.into(),
            decided_at: entry.decided_at,
            decided_by: entry.decided_by,
            revoked_at: entry.revoked_at,
            revoked_by: entry.revoked_by,
        }
    }
}

/// The node's recent sharing decisions, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct JoinHistoryView {
    pub history: Vec<JoinHistoryEntryView>,
    /// How many records the runtime holds, before its limit.
    pub observed_count: usize,
    pub limit: usize,
    pub omits_superseded: bool,
    pub conflicts_omitted: bool,
}

impl From<ManagedEnrollmentHistory> for JoinHistoryView {
    fn from(history: ManagedEnrollmentHistory) -> Self {
        Self {
            history: history.history.into_iter().map(Into::into).collect(),
            observed_count: history.observed_count,
            limit: history.limit,
            omits_superseded: history.omits_superseded,
            conflicts_omitted: history.conflicts_omitted,
        }
    }
}
