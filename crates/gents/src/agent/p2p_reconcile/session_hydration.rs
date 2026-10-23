use std::collections::BTreeSet;

pub use gents_protocol::session_hydration::{
    HydrationRequest, SessionHydrationCollection, SessionHydrationDocumentKey,
};

pub const HYDRATION_COLLECTIONS: &[SessionHydrationCollection] = &[
    SessionHydrationCollection::AgentRequest,
    SessionHydrationCollection::AgentMessage,
    SessionHydrationCollection::AgentToolCall,
    SessionHydrationCollection::AgentOutputSegment,
    SessionHydrationCollection::CompactionEntry,
];

pub const SESSION_OWNERSHIP_MISMATCH: &str = "session ownership does not match request";

pub fn hydration_collection_name(collection: SessionHydrationCollection) -> &'static str {
    match collection {
        SessionHydrationCollection::AgentRequest => "AgentRequest",
        SessionHydrationCollection::AgentMessage => "AgentMessage",
        SessionHydrationCollection::AgentToolCall => "AgentToolCall",
        SessionHydrationCollection::AgentOutputSegment => "AgentOutputSegment",
        SessionHydrationCollection::CompactionEntry => "CompactionEntry",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionOwner {
    pub session_id: String,
    pub requester_did: String,
    pub node_did: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AppliedPairingRoute {
    pub peer_id: String,
    pub requester_did: String,
    pub node_did: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VerifiedActiveMembership {
    pub network_id: String,
    pub member_did: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct HydrationDocument {
    pub collection: SessionHydrationCollection,
    pub doc_id: String,
    pub requester_did: String,
    pub node_did: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HydrationCatalog {
    pub applied_pairing_routes: BTreeSet<AppliedPairingRoute>,
    pub selected_network_id: String,
    pub verified_active_memberships: BTreeSet<VerifiedActiveMembership>,
    pub sessions: BTreeSet<SessionOwner>,
    pub documents: BTreeSet<HydrationDocument>,
    pub authorized_reference_closure: BTreeSet<SessionHydrationDocumentKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HydrationVerdict {
    Admit(BTreeSet<HydrationDocument>),
    Reject(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HydrationDeliveryResult {
    Confirmed,
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HydrationTerminalWriteResult {
    Committed,
    Failed,
    NotAttempted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HydrationApplyOutcome {
    Served(BTreeSet<HydrationDocument>),
    Rejected {
        detail: &'static str,
        attempted_documents: BTreeSet<HydrationDocument>,
    },
    PendingAfterTerminalWriteFailure {
        attempted_documents: BTreeSet<HydrationDocument>,
        confirmed_documents: BTreeSet<HydrationDocument>,
    },
    PendingAfterIndeterminateDelivery {
        attempted_documents: BTreeSet<HydrationDocument>,
    },
}

pub fn apply_hydration_delivery(
    verdict: HydrationVerdict,
    delivery: HydrationDeliveryResult,
    terminal_write: HydrationTerminalWriteResult,
) -> HydrationApplyOutcome {
    match (verdict, delivery, terminal_write) {
        (HydrationVerdict::Admit(documents), HydrationDeliveryResult::Indeterminate, _) => {
            HydrationApplyOutcome::PendingAfterIndeterminateDelivery {
                attempted_documents: documents,
            }
        }
        (
            HydrationVerdict::Admit(documents),
            HydrationDeliveryResult::Confirmed,
            HydrationTerminalWriteResult::Committed,
        ) => HydrationApplyOutcome::Served(documents),
        (HydrationVerdict::Reject(detail), _, HydrationTerminalWriteResult::Committed) => {
            HydrationApplyOutcome::Rejected {
                detail,
                attempted_documents: BTreeSet::new(),
            }
        }
        (
            HydrationVerdict::Admit(documents),
            HydrationDeliveryResult::Confirmed,
            HydrationTerminalWriteResult::Failed | HydrationTerminalWriteResult::NotAttempted,
        ) => HydrationApplyOutcome::PendingAfterTerminalWriteFailure {
            attempted_documents: documents.clone(),
            confirmed_documents: documents,
        },
        (
            HydrationVerdict::Reject(_),
            _,
            HydrationTerminalWriteResult::Failed | HydrationTerminalWriteResult::NotAttempted,
        ) => HydrationApplyOutcome::PendingAfterTerminalWriteFailure {
            attempted_documents: BTreeSet::new(),
            confirmed_documents: BTreeSet::new(),
        },
    }
}

pub fn decide_hydration(
    request: &HydrationRequest,
    catalog: &HydrationCatalog,
) -> HydrationVerdict {
    let pairing = AppliedPairingRoute {
        peer_id: request.peer_id.clone(),
        requester_did: request.requester_did.clone(),
        node_did: request.node_did.clone(),
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
        node_did: request.node_did.clone(),
    };
    if !catalog.sessions.contains(&owner) {
        return HydrationVerdict::Reject(SESSION_OWNERSHIP_MISMATCH);
    }

    HydrationVerdict::Admit(
        catalog
            .documents
            .iter()
            .filter(|doc| {
                HYDRATION_COLLECTIONS.contains(&doc.collection)
                    && doc.requester_did == request.requester_did
                    && doc.node_did == request.node_did
                    && (doc.session_id == request.session_id
                        || catalog.authorized_reference_closure.contains(
                            &SessionHydrationDocumentKey {
                                collection: doc.collection,
                                doc_id: doc.doc_id.clone(),
                            },
                        ))
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
    pub node_did: String,
    pub phase: ClientHydrationPhase,
    pub merged_count: usize,
    pub covered_count: usize,
    pub served_count: Option<usize>,
    pub merged_documents: BTreeSet<SessionHydrationDocumentKey>,
    pub served_documents: Option<BTreeSet<SessionHydrationDocumentKey>>,
}

impl Default for ClientHydrationProgress {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            node_did: String::new(),
            phase: ClientHydrationPhase::Idle,
            merged_count: 0,
            covered_count: 0,
            served_count: None,
            merged_documents: BTreeSet::new(),
            served_documents: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientHydrationRequestState {
    Missing,
    Pending,
    Served(BTreeSet<SessionHydrationDocumentKey>),
    Rejected(Option<BTreeSet<SessionHydrationDocumentKey>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HydrationAdmissionAction {
    PassiveStart,
    ExplicitRetry,
}

#[expect(
    clippy::too_many_arguments,
    reason = "public conformance-modeled predicate called from gents-desktop-core and conformance tests"
)]
pub fn can_admit_hydration_request(
    action: HydrationAdmissionAction,
    request: &ClientHydrationRequestState,
    prev: &ClientHydrationProgress,
    session_id: &str,
    node_did: &str,
    foreign_header: bool,
    owned_session: bool,
    has_documents: bool,
    nonterminal_request: bool,
) -> bool {
    match action {
        HydrationAdmissionAction::PassiveStart => {
            matches!(request, ClientHydrationRequestState::Missing)
                && prev.session_id == session_id
                && prev.node_did == node_did
                && prev.phase == ClientHydrationPhase::Idle
                && can_start_hydration(
                    foreign_header,
                    owned_session,
                    has_documents,
                    nonterminal_request,
                )
        }
        HydrationAdmissionAction::ExplicitRetry => {
            matches!(request, ClientHydrationRequestState::Rejected(_))
                && !foreign_header
                && can_retry_hydration(prev, session_id, node_did)
        }
    }
}

pub fn begin_hydration_request(session_id: &str, node_did: &str) -> ClientHydrationProgress {
    ClientHydrationProgress {
        session_id: session_id.to_string(),
        node_did: node_did.to_string(),
        phase: ClientHydrationPhase::Requested,
        merged_count: 0,
        covered_count: 0,
        served_count: None,
        merged_documents: BTreeSet::new(),
        served_documents: None,
    }
}

pub fn can_start_hydration(
    foreign_header: bool,
    owned_session: bool,
    has_documents: bool,
    nonterminal_request: bool,
) -> bool {
    !foreign_header && (owned_session || (has_documents && !nonterminal_request))
}

pub fn can_retry_hydration(
    prev: &ClientHydrationProgress,
    session_id: &str,
    node_did: &str,
) -> bool {
    prev.session_id == session_id
        && prev.node_did == node_did
        && prev.phase == ClientHydrationPhase::Failed
}

fn merge_served(
    prev: Option<BTreeSet<SessionHydrationDocumentKey>>,
    next: Option<BTreeSet<SessionHydrationDocumentKey>>,
) -> Option<BTreeSet<SessionHydrationDocumentKey>> {
    next.or(prev)
}

fn can_complete(
    merged: &BTreeSet<SessionHydrationDocumentKey>,
    served: Option<&BTreeSet<SessionHydrationDocumentKey>>,
) -> bool {
    served.is_some_and(|served| served.is_subset(merged))
}

pub fn observe_hydration_progress(
    prev: &ClientHydrationProgress,
    session_id: &str,
    node_did: &str,
    merged_documents: BTreeSet<SessionHydrationDocumentKey>,
    served_documents: Option<BTreeSet<SessionHydrationDocumentKey>>,
    failed: bool,
) -> ClientHydrationProgress {
    let base = if prev.session_id == session_id && prev.node_did == node_did {
        prev.clone()
    } else {
        ClientHydrationProgress {
            session_id: session_id.to_string(),
            node_did: node_did.to_string(),
            ..ClientHydrationProgress::default()
        }
    };
    let merged = base
        .merged_documents
        .union(&merged_documents)
        .cloned()
        .collect::<BTreeSet<_>>();
    let served = merge_served(base.served_documents.clone(), served_documents);
    let merged_count = merged.len();
    let served_count = served.as_ref().map(BTreeSet::len);
    let covered_count = served
        .as_ref()
        .map_or(merged_count, |served| merged.intersection(served).count());
    if failed || base.phase == ClientHydrationPhase::Failed {
        return ClientHydrationProgress {
            session_id: session_id.to_string(),
            node_did: node_did.to_string(),
            phase: ClientHydrationPhase::Failed,
            merged_count,
            covered_count,
            served_count,
            merged_documents: merged,
            served_documents: served,
        };
    }
    if can_complete(&merged, served.as_ref()) {
        return ClientHydrationProgress {
            session_id: session_id.to_string(),
            node_did: node_did.to_string(),
            phase: ClientHydrationPhase::Complete,
            merged_count,
            covered_count,
            served_count,
            merged_documents: merged,
            served_documents: served,
        };
    }
    if served.is_some()
        || base.phase == ClientHydrationPhase::Serving
        || (base.phase == ClientHydrationPhase::Requested && !merged.is_empty())
    {
        return ClientHydrationProgress {
            session_id: session_id.to_string(),
            node_did: node_did.to_string(),
            phase: ClientHydrationPhase::Serving,
            merged_count,
            covered_count,
            served_count,
            merged_documents: merged,
            served_documents: served,
        };
    }
    ClientHydrationProgress {
        session_id: session_id.to_string(),
        node_did: node_did.to_string(),
        phase: if base.phase == ClientHydrationPhase::Requested {
            ClientHydrationPhase::Requested
        } else {
            ClientHydrationPhase::Idle
        },
        merged_count,
        covered_count,
        served_count,
        merged_documents: merged,
        served_documents: served,
    }
}

pub fn project_durable_hydration_progress(
    session_id: &str,
    node_did: &str,
    merged_documents: BTreeSet<SessionHydrationDocumentKey>,
    request: ClientHydrationRequestState,
) -> ClientHydrationProgress {
    let (base, served_documents, failed) = match request {
        ClientHydrationRequestState::Missing => (
            ClientHydrationProgress {
                session_id: session_id.to_string(),
                node_did: node_did.to_string(),
                ..ClientHydrationProgress::default()
            },
            None,
            false,
        ),
        ClientHydrationRequestState::Pending => {
            (begin_hydration_request(session_id, node_did), None, false)
        }
        ClientHydrationRequestState::Served(documents) => (
            begin_hydration_request(session_id, node_did),
            Some(documents),
            false,
        ),
        ClientHydrationRequestState::Rejected(documents) => (
            begin_hydration_request(session_id, node_did),
            documents,
            true,
        ),
    };
    observe_hydration_progress(
        &base,
        session_id,
        node_did,
        merged_documents,
        served_documents,
        failed,
    )
}
