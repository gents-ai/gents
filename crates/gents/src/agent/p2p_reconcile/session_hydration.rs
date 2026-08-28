//! Pure admission and selection core for `SessionHydrationRequest` (#1142).
//!
//! The background sweep and P2P delivery adapter are intentionally separate:
//! this module decides whether a request is authorized and returns the exact
//! tenant/session-scoped document set. The delivery adapter in the reconciler
//! sends that set through DefraDB's bounded, peer-targeted doc-pusher.

use std::collections::BTreeSet;

pub const HYDRATION_COLLECTIONS: &[&str] = &[
    "AgentRequest",
    "AgentResponse",
    "AgentMessage",
    "AgentToolCall",
    "AgentToolResult",
    "CompactionEntry",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydrationRequest {
    pub request_key: String,
    pub peer_id: String,
    pub requester_did: String,
    pub agent_did: String,
    pub session_id: String,
}

impl HydrationRequest {
    /// Decode the schema's `{peer_id}:{session_id}` key and reject a key whose
    /// session suffix does not match the immutable `session_id` column.
    pub fn from_row(
        request_key: String,
        requester_did: String,
        agent_did: String,
        session_id: String,
    ) -> Result<Self, &'static str> {
        let Some((peer_id, key_session_id)) = request_key.split_once(':') else {
            return Err("request_key must be {peer_id}:{session_id}");
        };
        if peer_id.is_empty() || key_session_id != session_id {
            return Err("request_key does not match peer/session columns");
        }
        let peer_id = peer_id.to_string();
        Ok(Self {
            request_key,
            peer_id,
            requester_did,
            agent_did,
            session_id,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionOwner {
    pub session_id: String,
    pub requester_did: String,
    pub agent_did: String,
}

/// A locally desired client route whose exact requester/agent filter is applied.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AppliedPairingRoute {
    pub peer_id: String,
    pub requester_did: String,
    pub agent_did: String,
}

/// An active membership whose network root and admin signature were verified.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VerifiedActiveMembership {
    pub network_id: String,
    pub member_did: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct HydrationDocument {
    pub collection: String,
    pub doc_id: String,
    pub requester_did: String,
    pub agent_did: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HydrationCatalog {
    pub applied_pairing_routes: BTreeSet<AppliedPairingRoute>,
    pub selected_network_id: String,
    pub verified_active_memberships: BTreeSet<VerifiedActiveMembership>,
    pub sessions: BTreeSet<SessionOwner>,
    pub documents: BTreeSet<HydrationDocument>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HydrationVerdict {
    Admit(BTreeSet<HydrationDocument>),
    Reject(&'static str),
}

pub fn decide_hydration(
    request: &HydrationRequest,
    catalog: &HydrationCatalog,
) -> HydrationVerdict {
    let pairing = AppliedPairingRoute {
        peer_id: request.peer_id.clone(),
        requester_did: request.requester_did.clone(),
        agent_did: request.agent_did.clone(),
    };
    if !catalog.applied_pairing_routes.contains(&pairing) {
        return HydrationVerdict::Reject("peer pairing does not match requester and agent");
    }
    let membership = VerifiedActiveMembership {
        network_id: catalog.selected_network_id.clone(),
        member_did: request.requester_did.clone(),
    };
    if !catalog.verified_active_memberships.contains(&membership) {
        return HydrationVerdict::Reject(
            "requester membership is not verified active in the selected network",
        );
    }
    let owner = SessionOwner {
        session_id: request.session_id.clone(),
        requester_did: request.requester_did.clone(),
        agent_did: request.agent_did.clone(),
    };
    if !catalog.sessions.contains(&owner) {
        return HydrationVerdict::Reject("session ownership does not match request");
    }

    HydrationVerdict::Admit(
        catalog
            .documents
            .iter()
            .filter(|doc| {
                HYDRATION_COLLECTIONS.contains(&doc.collection.as_str())
                    && doc.requester_did == request.requester_did
                    && doc.agent_did == request.agent_did
                    && doc.session_id == request.session_id
            })
            .cloned()
            .collect(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientHydrationPhase {
    Idle,
    Requested,
    Serving,
    Complete,
    Failed,
}

impl ClientHydrationPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Requested => "requested",
            Self::Serving => "serving",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "requested" => Self::Requested,
            "serving" => Self::Serving,
            "complete" => Self::Complete,
            "failed" => Self::Failed,
            _ => Self::Idle,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientHydrationProgress {
    pub session_id: String,
    pub agent_did: String,
    pub phase: ClientHydrationPhase,
    pub merged_count: usize,
    pub served_count: Option<usize>,
}

impl Default for ClientHydrationProgress {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            agent_did: String::new(),
            phase: ClientHydrationPhase::Idle,
            merged_count: 0,
            served_count: None,
        }
    }
}

/// Durable request state for one exact `(session_id, agent_did)` target.
///
/// This is deliberately a query result rather than retained client state. A
/// session snapshot derives progress from its own request row and locally
/// merged documents, so observing one target cannot overwrite another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientHydrationRequestState {
    Missing,
    Pending,
    Served(usize),
    Rejected(Option<usize>),
}

/// Begin a new receiver attempt, clearing any terminal state and denominator
/// retained by the previous request for this target.
pub fn begin_hydration_request(session_id: &str, agent_did: &str) -> ClientHydrationProgress {
    ClientHydrationProgress {
        session_id: session_id.to_string(),
        agent_did: agent_did.to_string(),
        phase: ClientHydrationPhase::Requested,
        merged_count: 0,
        served_count: None,
    }
}

/// Retry admission is target-specific and terminal-state-specific.
pub fn can_retry_hydration(
    prev: &ClientHydrationProgress,
    session_id: &str,
    agent_did: &str,
) -> bool {
    prev.session_id == session_id
        && prev.agent_did == agent_did
        && prev.phase == ClientHydrationPhase::Failed
}

fn merge_served(prev: Option<usize>, next: Option<usize>) -> Option<usize> {
    next.or(prev)
}

fn can_complete(merged_count: usize, served_count: Option<usize>) -> bool {
    served_count.is_some_and(|served| merged_count >= served)
}

/// Receiver-side progress. Completes only when unique locally merged
/// documents cover the server's `served_doc_count`. Sender status alone
/// never completes a request.
pub fn observe_hydration_progress(
    prev: &ClientHydrationProgress,
    session_id: &str,
    agent_did: &str,
    merged_count: usize,
    served_count: Option<usize>,
    failed: bool,
) -> ClientHydrationProgress {
    let base = if prev.session_id == session_id && prev.agent_did == agent_did {
        prev.clone()
    } else {
        ClientHydrationProgress {
            session_id: session_id.to_string(),
            agent_did: agent_did.to_string(),
            ..ClientHydrationProgress::default()
        }
    };
    let merged = base.merged_count.max(merged_count);
    let served = merge_served(base.served_count, served_count);
    if failed || base.phase == ClientHydrationPhase::Failed {
        return ClientHydrationProgress {
            session_id: session_id.to_string(),
            agent_did: agent_did.to_string(),
            phase: ClientHydrationPhase::Failed,
            merged_count: merged,
            served_count: served,
        };
    }
    if can_complete(merged, served) {
        return ClientHydrationProgress {
            session_id: session_id.to_string(),
            agent_did: agent_did.to_string(),
            phase: ClientHydrationPhase::Complete,
            merged_count: merged,
            served_count: served,
        };
    }
    if served.is_some()
        || base.phase == ClientHydrationPhase::Serving
        || (base.phase == ClientHydrationPhase::Requested && merged > 0)
    {
        return ClientHydrationProgress {
            session_id: session_id.to_string(),
            agent_did: agent_did.to_string(),
            phase: ClientHydrationPhase::Serving,
            merged_count: merged,
            served_count: served,
        };
    }
    ClientHydrationProgress {
        session_id: session_id.to_string(),
        agent_did: agent_did.to_string(),
        phase: if base.phase == ClientHydrationPhase::Requested {
            ClientHydrationPhase::Requested
        } else {
            ClientHydrationPhase::Idle
        },
        merged_count: merged,
        served_count: served,
    }
}

/// Project receiver progress from durable state for one exact target.
///
/// A pending row is the durable evidence that an attempt was started. A
/// rejected row is terminal until the explicit retry command rewrites it to
/// pending. No process-local progress survives or crosses target queries.
pub fn project_durable_hydration_progress(
    session_id: &str,
    agent_did: &str,
    merged_count: usize,
    request: ClientHydrationRequestState,
) -> ClientHydrationProgress {
    let (base, served_count, failed) = match request {
        ClientHydrationRequestState::Missing => (
            ClientHydrationProgress {
                session_id: session_id.to_string(),
                agent_did: agent_did.to_string(),
                ..ClientHydrationProgress::default()
            },
            None,
            false,
        ),
        ClientHydrationRequestState::Pending => {
            (begin_hydration_request(session_id, agent_did), None, false)
        }
        ClientHydrationRequestState::Served(count) => (
            begin_hydration_request(session_id, agent_did),
            Some(count),
            false,
        ),
        ClientHydrationRequestState::Rejected(count) => {
            (begin_hydration_request(session_id, agent_did), count, true)
        }
    };
    observe_hydration_progress(
        &base,
        session_id,
        agent_did,
        merged_count,
        served_count,
        failed,
    )
}
