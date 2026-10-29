use gents_protocol::message::{Message, Reasoning, ToolCall};
use gents_protocol::rendered_request::CaptureScope;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub type ProviderAuditReceiver = Arc<Mutex<mpsc::Receiver<ProviderAuditObservation>>>;
/// A Claude SSE dispatch reserves these slots before producing its audit events;
/// excess events fail the stream instead of silently dropping audit evidence.
pub const MAX_AUDIT_EVENTS_PER_SSE: usize = 3;

pub fn drain_ready(receiver: &ProviderAuditReceiver) -> Vec<ProviderAuditObservation> {
    let mut receiver = receiver
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut observations = Vec::new();
    while let Ok(observation) = receiver.try_recv() {
        observations.push(observation);
    }
    observations
}

pub fn try_recv_one(receiver: &ProviderAuditReceiver) -> Option<ProviderAuditObservation> {
    receiver
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .try_recv()
        .ok()
}

pub async fn recv_one(receiver: &ProviderAuditReceiver) -> Option<ProviderAuditObservation> {
    futures::future::poll_fn(|cx| {
        receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .poll_recv(cx)
    })
    .await
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaudeAuditEvent {
    BlockStart { index: u32, kind: ClaudeBlockKind },
    ThinkingText { index: u32, fragment: String },
    Signature { index: u32, fragment: String },
    RedactedData { index: u32, data: String },
    BlockStop { index: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaudeBlockKind {
    Text,
    ToolUse { id: String, name: String },
    Thinking,
    RedactedThinking,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderAuditObservation {
    pub capture_scope: CaptureScope,
    pub turn: usize,
    pub attempt: u32,
    pub event: ClaudeAuditEvent,
}

#[derive(Clone, Debug)]
pub struct AuxiliaryOutputObservation {
    pub capture_scope: CaptureScope,
    pub turn: usize,
    pub attempt: u32,
    pub event: AuxiliaryOutputEvent,
}

#[derive(Clone, Debug)]
pub enum AuxiliaryOutputEvent {
    AttemptStarted,
    Audit(ClaudeAuditEvent),
    TextDelta(String),
    ReasoningDelta {
        id: Option<String>,
        fragment: String,
    },
    Reasoning(Reasoning),
    ToolCall(ToolCall),
    FinalText(String),
    TurnReady {
        message: Message,
    },
    Retract,
    AttemptFailed {
        will_retry: bool,
    },
    OutputObligationPending {
        reminder: Message,
    },
    ClosePartial,
}

#[derive(Debug, thiserror::Error)]
pub enum AuxiliaryOutputError {
    #[error("auxiliary output has no request scope")]
    NoRequestScope,
    #[error("auxiliary output has no canonical sink")]
    NoCanonicalSink,
    #[error("auxiliary attempt already active")]
    AttemptAlreadyActive,
    #[error("auxiliary reminder changed source")]
    ReminderChangedSource,
    #[error("auxiliary output changed source")]
    OutputChangedSource,
    #[error("active auxiliary has no audit receiver")]
    NoAuditReceiver,
    #[error("active auxiliary audit receiver changed source")]
    AuditReceiverChangedSource,
    #[error("queued auxiliary audit changed source")]
    QueuedAuditChangedSource,
    #[error("active auxiliary output could not be closed")]
    NotClosed,
    #[error("auxiliary output sink failed")]
    Sink {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

pub type AuxiliaryObserve = Arc<
    dyn Fn(
            AuxiliaryOutputObservation,
        ) -> Pin<Box<dyn Future<Output = Result<(), AuxiliaryOutputError>> + Send>>
        + Send
        + Sync,
>;

pub type AuxiliaryNextFlushDeadline = Arc<
    dyn Fn(
            CaptureScope,
            usize,
            u32,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Option<tokio::time::Instant>, AuxiliaryOutputError>>
                    + Send,
            >,
        > + Send
        + Sync,
>;

pub type AuxiliaryFlushPending = Arc<
    dyn Fn(
            CaptureScope,
            usize,
            u32,
        ) -> Pin<Box<dyn Future<Output = Result<(), AuxiliaryOutputError>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct AuxiliaryOutputSink {
    pub observe: AuxiliaryObserve,
    pub next_flush_deadline: AuxiliaryNextFlushDeadline,
    pub flush_pending: AuxiliaryFlushPending,
}

#[derive(Clone)]
pub struct ProviderAuditSender {
    capture_scope: CaptureScope,
    turn: usize,
    attempt: u32,
    sender: mpsc::Sender<ProviderAuditObservation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditCapacityExceeded;

impl std::fmt::Display for AuditCapacityExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("provider audit channel closed or out of capacity")
    }
}

impl std::error::Error for AuditCapacityExceeded {}

pub struct ProviderAuditReservation<'a> {
    capture_scope: CaptureScope,
    turn: usize,
    attempt: u32,
    permits: mpsc::PermitIterator<'a, ProviderAuditObservation>,
}

impl ProviderAuditReservation<'_> {
    pub fn emit(&mut self, event: ClaudeAuditEvent) -> Result<(), AuditCapacityExceeded> {
        let permit = self.permits.next().ok_or(AuditCapacityExceeded)?;
        permit.send(ProviderAuditObservation {
            capture_scope: self.capture_scope,
            turn: self.turn,
            attempt: self.attempt,
            event,
        });
        Ok(())
    }
}

impl ProviderAuditSender {
    pub(crate) fn new(
        capture_scope: CaptureScope,
        turn: usize,
        attempt: u32,
        sender: mpsc::Sender<ProviderAuditObservation>,
    ) -> Self {
        Self {
            capture_scope,
            turn,
            attempt,
            sender,
        }
    }

    pub async fn reserve(&self) -> Result<ProviderAuditReservation<'_>, AuditCapacityExceeded> {
        let permits = self
            .sender
            .reserve_many(MAX_AUDIT_EVENTS_PER_SSE)
            .await
            .map_err(|_| AuditCapacityExceeded)?;
        Ok(ProviderAuditReservation {
            capture_scope: self.capture_scope,
            turn: self.turn,
            attempt: self.attempt,
            permits,
        })
    }
}
