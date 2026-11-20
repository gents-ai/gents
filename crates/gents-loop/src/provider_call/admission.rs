use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use gents_protocol::request_admission::RequestPurpose;
use gents_protocol::row::AgentRequest;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallKind {
    Inference,
    Compaction,
    Scheduled,
    OneOff,
}

impl CallKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inference => "inference",
            Self::Compaction => "compaction",
            Self::Scheduled => "scheduled",
            Self::OneOff => "oneoff",
        }
    }
}

/// The identity of the admitted call in flight: exactly what
/// [`AdmissionCallContext::mint`] minted for it.
#[derive(Clone, Debug)]
pub struct CurrentCallJoin {
    pub call_id: String,
    pub call_seq: i64,
    pub call_kind: CallKind,
}

pub type TerminalFailureReasonObserver = Arc<Mutex<Option<String>>>;

pub fn terminal_failure_reason_observer() -> TerminalFailureReasonObserver {
    Arc::new(Mutex::new(None))
}

pub fn set_terminal_failure_reason(
    observer: &TerminalFailureReasonObserver,
    reason: impl Into<String>,
) {
    let reason = reason.into();
    match observer.lock() {
        Ok(mut slot) => *slot = Some(reason),
        Err(poisoned) => *poisoned.into_inner() = Some(reason),
    }
}

/// What a provider call is admitted under: the request it serves, its kind
/// and attempt, and the request's call sequence. Admission mints each call's
/// identity from it.
#[derive(Clone)]
pub struct AdmissionCallContext {
    request_id: String,
    request_doc_id: String,
    request_purpose: RequestPurpose,
    backend_id: String,
    agent_did: String,
    node_did: String,
    session_id: String,
    requester_did: Option<String>,
    call_kind: CallKind,
    attempt: i64,
    call_seq: Arc<AtomicU64>,
    /// Written by [`Self::mint`]; every context [`Self::for_call`] derives
    /// has its own slot, so concurrent calls cannot replace each other's joins.
    current_call: Arc<Mutex<Option<CurrentCallJoin>>>,
    inference_token: Option<CancellationToken>,
    terminal_failure_reason: Option<TerminalFailureReasonObserver>,
}

impl AdmissionCallContext {
    pub fn for_request(
        request: &AgentRequest,
        agent_did: impl Into<String>,
        backend_id: impl Into<String>,
    ) -> Self {
        Self {
            request_id: request.request_id.clone(),
            request_doc_id: request.doc_id.clone(),
            request_purpose: request.purpose,
            backend_id: backend_id.into(),
            agent_did: agent_did.into(),
            node_did: request.node_did.clone(),
            session_id: request.session_id.clone(),
            requester_did: request.requester_did.clone(),
            call_kind: CallKind::Inference,
            attempt: 1,
            call_seq: Arc::new(AtomicU64::new(0)),
            current_call: Arc::new(Mutex::new(None)),
            inference_token: None,
            terminal_failure_reason: None,
        }
    }

    /// Continue the request's call sequence after `last_call_seq`, the
    /// largest sequence its earlier executions recorded.
    pub fn resumed_after(mut self, last_call_seq: u64) -> Self {
        self.call_seq = Arc::new(AtomicU64::new(last_call_seq));
        self
    }

    /// The calls of one kind and attempt under this request: the request's
    /// identity, cancellation and call sequence, with a join slot of their own.
    pub fn for_call(&self, call_kind: CallKind, attempt: i64) -> Self {
        Self {
            call_kind,
            attempt,
            current_call: Arc::new(Mutex::new(None)),
            ..self.clone()
        }
    }

    /// These calls raced against `token`, reporting why the request failed
    /// through `terminal_failure_reason`.
    pub fn with_cancellation(
        mut self,
        token: CancellationToken,
        terminal_failure_reason: TerminalFailureReasonObserver,
    ) -> Self {
        self.inference_token = Some(token);
        self.terminal_failure_reason = Some(terminal_failure_reason);
        self
    }

    /// These calls on another backend, keeping the physical request identity,
    /// cancellation, call sequence and join slot.
    pub fn with_backend(&self, backend_id: &str) -> Self {
        Self {
            backend_id: backend_id.to_owned(),
            ..self.clone()
        }
    }

    /// Mint the next call: its sequence number in the request, and its join,
    /// which becomes [`Self::current_join`].
    pub fn mint(&self, call_id: String) -> (u64, CurrentCallJoin) {
        let call_seq = self.call_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let join = CurrentCallJoin {
            call_id,
            call_seq: i64::try_from(call_seq).unwrap_or(i64::MAX),
            call_kind: self.call_kind,
        };
        match self.current_call.lock() {
            Ok(mut slot) => *slot = Some(join.clone()),
            Err(poisoned) => *poisoned.into_inner() = Some(join.clone()),
        }
        (call_seq, join)
    }

    /// The call these calls last minted, if any.
    pub fn current_join(&self) -> Option<CurrentCallJoin> {
        match self.current_call.lock() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    pub fn request_doc_id(&self) -> &str {
        &self.request_doc_id
    }
    pub fn request_purpose(&self) -> RequestPurpose {
        self.request_purpose
    }
    pub fn backend_id(&self) -> &str {
        &self.backend_id
    }
    pub fn agent_did(&self) -> &str {
        &self.agent_did
    }
    pub fn node_did(&self) -> &str {
        &self.node_did
    }
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
    pub fn requester_did(&self) -> Option<&str> {
        self.requester_did.as_deref()
    }
    pub fn call_kind(&self) -> CallKind {
        self.call_kind
    }
    pub fn attempt(&self) -> i64 {
        self.attempt
    }
    pub fn inference_token(&self) -> Option<&CancellationToken> {
        self.inference_token.as_ref()
    }
    pub fn terminal_failure_reason(&self) -> Option<&TerminalFailureReasonObserver> {
        self.terminal_failure_reason.as_ref()
    }
}
