//! The per-request capture scope: how the loop's facts reach the transport.
//!
//! A rig completion client is built once per behavior and reused for every
//! request, so the capture identity (request, turn, attempt) cannot ride
//! constructor arguments; it lives in a task-local here, exactly like the
//! session id used for sticky-cache routing.
//!
//! `scope_request` must wrap both the completion stream's construction and its
//! drain loop: rig's SSE transports connect lazily on first poll, so the HTTP
//! send happens while polling. rig's SSE event source also reconnects by
//! re-posting the same request, so one armed attempt may produce several
//! byte-identical outbound bodies; only an exact body+destination match may
//! reuse the durable write.
//!
//! The contract is stated positively: inside a scope, a completion body with
//! nothing armed and nothing previously claimed is refused
//! ([`CaptureClaim::Unexplained`]), because "nothing armed" is not the same
//! statement as "capture succeeded" — the second was the shape under which the
//! pre-request compaction summarizer once reached the provider uncaptured.
//! Each completion loop in a request starts its counters at zero, so each loop
//! takes a distinct label (e.g. `compaction.2`) from this scope; the capture
//! sink is required to reject duplicate keys.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use super::{AdmissionJoin, AssemblyTrace, RenderedRequestCaptureSink, RenderedRequestContext};
use crate::loop_stream::RenderedRequestSink;
use crate::provider_audit::{
    AuxiliaryOutputEvent, AuxiliaryOutputObservation, AuxiliaryOutputSink,
    ProviderAuditObservation, ProviderAuditReceiver, ProviderAuditSender,
};

const AUDIT_CHANNEL_CAPACITY: usize = 32;

struct AttemptAuditChannel {
    capture_scope: String,
    turn_index: usize,
    attempt: u32,
    sender: mpsc::Sender<ProviderAuditObservation>,
    receiver: ProviderAuditReceiver,
}

// Re-exported here as well: arming call sites name the kind through this
// module.
pub use super::CaptureScopeKind;

/// One armed provider attempt, waiting for the transport to supply its body.
#[derive(Clone, Debug)]
pub struct PendingCapture {
    pub capture_scope: String,
    pub turn_index: usize,
    pub attempt: u32,
    pub assembly_trace: AssemblyTrace,
    pub activity: Arc<crate::provider_activity::ProviderActivity>,
}

#[derive(Default)]
struct ScopeState {
    allocated: BTreeMap<CaptureScopeKind, u64>,
    current: BTreeMap<CaptureScopeKind, String>,
    pending: Option<PendingCapture>,
    claimed: Option<PendingCapture>,
    /// Fingerprint of the exact transport body and destination whose durable
    /// write succeeded for `claimed`. A matching SSE reconnect may forward.
    durable_body_fingerprint: Option<[u8; 32]>,
    /// Every arm this scope has seen, in order. Not `#[cfg(test)]`: gents'
    /// own capture-seam tests read this through `armed_labels()`, and a
    /// cfg(test) item in this crate is invisible to a dependent crate's own
    /// test build.
    armed_labels: Vec<String>,
    audit: Option<AttemptAuditChannel>,
    active_auxiliary: Option<(gents_protocol::rendered_request::CaptureScope, usize, u32)>,
    last_auxiliary: Option<(gents_protocol::rendered_request::CaptureScope, usize, u32)>,
}

/// Looks up the admission-controller call this capture belongs to, if any.
/// The loop and tests default to `|_| None`.
pub type AdmissionJoinLookup = Arc<dyn Fn(&str) -> Option<AdmissionJoin> + Send + Sync>;

fn no_admission_join(_capture_scope: &str) -> Option<AdmissionJoin> {
    None
}

/// Everything the transport needs to turn a body into a durable row.
pub struct RequestCaptureScope {
    context: RenderedRequestContext,
    sink: RenderedRequestCaptureSink,
    state: Mutex<ScopeState>,
    admission_join_lookup: AdmissionJoinLookup,
    auxiliary_output_sink: Option<AuxiliaryOutputSink>,
    compaction_provider_family: Option<String>,
}

impl RequestCaptureScope {
    pub fn new(context: RenderedRequestContext, sink: RenderedRequestCaptureSink) -> Self {
        Self {
            context,
            sink,
            state: Mutex::new(ScopeState::default()),
            admission_join_lookup: Arc::new(no_admission_join),
            auxiliary_output_sink: None,
            compaction_provider_family: None,
        }
    }

    /// Install the native admission-join lookup when one exists.
    pub fn with_admission_join_lookup(mut self, lookup: AdmissionJoinLookup) -> Self {
        self.admission_join_lookup = lookup;
        self
    }

    pub fn set_auxiliary_output_sink(&mut self, sink: AuxiliaryOutputSink) {
        self.auxiliary_output_sink = Some(sink);
    }

    pub fn set_compaction_provider_family(&mut self, family: String) {
        self.compaction_provider_family = Some(family);
    }

    pub fn context(&self) -> &RenderedRequestContext {
        &self.context
    }

    pub fn sink(&self) -> &RenderedRequestCaptureSink {
        &self.sink
    }

    /// The label for a loop of `kind`; a `(0, 0)` arm starts a new loop.
    fn label_for(&self, kind: CaptureScopeKind, turn_index: usize, attempt: u32) -> String {
        let mut state = self.lock();
        let starts_a_loop = turn_index == 0 && attempt == 0;
        if !starts_a_loop {
            if let Some(existing) = state.current.get(&kind) {
                return existing.clone();
            }
        }
        let seq = state.allocated.entry(kind).or_insert(0);
        *seq += 1;
        let label = format!("{kind}.{seq}");
        state.current.insert(kind, label.clone());
        label
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ScopeState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Fingerprint the exact durable body for `claimed`; a stale completion
    /// must not poison a newer arm's reconnect cache.
    pub fn mark_claimed_durable(&self, pending: &PendingCapture, fingerprint: [u8; 32]) {
        let mut state = self.lock();
        let still_current = state.claimed.as_ref().is_some_and(|claimed| {
            claimed.capture_scope == pending.capture_scope
                && claimed.turn_index == pending.turn_index
                && claimed.attempt == pending.attempt
        });
        if still_current {
            state.durable_body_fingerprint = Some(fingerprint);
        }
    }
}

tokio::task_local! {
    static CAPTURE_SCOPE: Arc<RequestCaptureScope>;
}

/// Install `scope` for the duration of `future`.
///
/// Must wrap both the completion stream's construction and its drain loop:
/// rig's SSE transports connect lazily on first poll, so the HTTP send often
/// happens while the loop is polling.
pub async fn scope_request<T>(
    scope: Arc<RequestCaptureScope>,
    future: impl std::future::Future<Output = T>,
) -> T {
    CAPTURE_SCOPE.scope(scope, future).await
}

pub fn current_scope() -> Option<Arc<RequestCaptureScope>> {
    CAPTURE_SCOPE.try_with(Arc::clone).ok()
}

/// Arm this attempt's capture; idempotent per attempt.
pub fn arm(
    kind: CaptureScopeKind,
    turn_index: usize,
    attempt: u32,
    assembly_trace: AssemblyTrace,
) -> Option<String> {
    let scope = current_scope()?;
    let capture_scope = scope.label_for(kind, turn_index, attempt);
    let pending = PendingCapture {
        capture_scope: capture_scope.clone(),
        turn_index,
        attempt,
        assembly_trace,
        activity: Arc::default(),
    };
    let (audit_sender, audit_receiver) = mpsc::channel(AUDIT_CHANNEL_CAPACITY);
    let replaced = {
        let mut state = scope.lock();
        state.armed_labels.push(capture_scope.clone());
        state.claimed = None;
        state.durable_body_fingerprint = None;
        if !state.audit.as_ref().is_some_and(|audit| {
            audit.capture_scope == capture_scope
                && audit.turn_index == turn_index
                && audit.attempt == attempt
        }) {
            state.audit = Some(AttemptAuditChannel {
                capture_scope: capture_scope.clone(),
                turn_index,
                attempt,
                sender: audit_sender,
                receiver: Arc::new(Mutex::new(audit_receiver)),
            });
        }
        state.pending.replace(pending).is_some()
    };
    if replaced {
        tracing::debug!(
            capture_scope = %capture_scope,
            turn_index,
            attempt,
            "replaced an unconsumed rendered-request capture; the previous attempt never reached the transport"
        );
    }
    Some(capture_scope)
}

/// Capture the identity of the provider call whose body is being decoded.
pub fn current_audit_sender() -> Option<ProviderAuditSender> {
    let scope = current_scope()?;
    let state = scope.lock();
    let claimed = state.claimed.as_ref()?;
    let audit = state.audit.as_ref()?;
    if audit.capture_scope != claimed.capture_scope
        || audit.turn_index != claimed.turn_index
        || audit.attempt != claimed.attempt
    {
        return None;
    }
    let capture_scope = claimed.capture_scope.parse().ok()?;
    Some(ProviderAuditSender::new(
        capture_scope,
        claimed.turn_index,
        claimed.attempt,
        audit.sender.clone(),
    ))
}

/// The owned loop receives audit events for the exact attempt it armed.
pub fn take_audit_receiver(turn_index: usize, attempt: u32) -> Option<ProviderAuditReceiver> {
    let scope = current_scope()?;
    let state = scope.lock();
    state.audit.as_ref().and_then(|audit| {
        (audit.turn_index == turn_index && audit.attempt == attempt)
            .then(|| Arc::clone(&audit.receiver))
    })
}

/// Drain observations already parsed by the current attempt without polling
/// the provider stream or advancing tool dispatch.
pub async fn drain_ready_audit() -> Vec<ProviderAuditObservation> {
    let receiver = {
        let Some(scope) = current_scope() else {
            return Vec::new();
        };
        let state = scope.lock();
        state
            .audit
            .as_ref()
            .map(|audit| Arc::clone(&audit.receiver))
    };
    let Some(receiver) = receiver else {
        return Vec::new();
    };
    crate::provider_audit::drain_ready(&receiver)
}

pub fn auxiliary_output_sink() -> Option<AuxiliaryOutputSink> {
    current_scope()?.auxiliary_output_sink.clone()
}

pub async fn emit_auxiliary_output(observation: AuxiliaryOutputObservation) -> anyhow::Result<()> {
    let scope =
        current_scope().ok_or_else(|| anyhow::anyhow!("auxiliary output has no request scope"))?;
    let sink = scope
        .auxiliary_output_sink
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("auxiliary output has no canonical sink"))?
        .clone();
    let identity = (
        observation.capture_scope,
        observation.turn,
        observation.attempt,
    );
    {
        let state = scope.lock();
        match &observation.event {
            AuxiliaryOutputEvent::AttemptStarted => {
                anyhow::ensure!(
                    state.active_auxiliary.is_none(),
                    "auxiliary attempt already active"
                );
            }
            AuxiliaryOutputEvent::OutputObligationPending { .. }
            | AuxiliaryOutputEvent::FinalText(_)
                if state.active_auxiliary.is_none() =>
            {
                anyhow::ensure!(
                    state.last_auxiliary == Some(identity),
                    "auxiliary reminder changed source"
                );
            }
            _ => {
                anyhow::ensure!(
                    state.active_auxiliary == Some(identity),
                    "auxiliary output changed source"
                );
            }
        }
    }
    (sink.observe)(observation.clone()).await?;
    if matches!(observation.event, AuxiliaryOutputEvent::AttemptStarted) {
        let mut state = scope.lock();
        state.active_auxiliary = Some(identity);
        state.last_auxiliary = Some(identity);
    }
    if matches!(
        observation.event,
        AuxiliaryOutputEvent::TurnReady { .. }
            | AuxiliaryOutputEvent::Retract
            | AuxiliaryOutputEvent::AttemptFailed { .. }
            | AuxiliaryOutputEvent::ClosePartial
    ) {
        let mut state = scope.lock();
        if state.active_auxiliary == Some(identity) {
            state.active_auxiliary = None;
        }
    }
    Ok(())
}

/// Close the active auxiliary source after draining already parsed Claude
/// observations.
pub async fn flush_received_auxiliary_partial() -> anyhow::Result<bool> {
    let Some(scope) = current_scope() else {
        return Ok(false);
    };
    let (identity, receiver) = {
        let state = scope.lock();
        let Some(identity) = state.active_auxiliary else {
            return Ok(false);
        };
        let audit = state
            .audit
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("active auxiliary has no audit receiver"))?;
        anyhow::ensure!(
            audit.capture_scope == identity.0.to_string()
                && audit.turn_index == identity.1
                && audit.attempt == identity.2,
            "active auxiliary audit receiver changed source"
        );
        (identity, Arc::clone(&audit.receiver))
    };
    while let Some(audit) = crate::provider_audit::try_recv_one(&receiver) {
        anyhow::ensure!(
            (audit.capture_scope, audit.turn, audit.attempt) == identity,
            "queued auxiliary audit changed source"
        );
        emit_auxiliary_output(AuxiliaryOutputObservation {
            capture_scope: identity.0,
            turn: identity.1,
            attempt: identity.2,
            event: AuxiliaryOutputEvent::Audit(audit.event),
        })
        .await?;
    }
    emit_auxiliary_output(AuxiliaryOutputObservation {
        capture_scope: identity.0,
        turn: identity.1,
        attempt: identity.2,
        event: AuxiliaryOutputEvent::ClosePartial,
    })
    .await?;
    Ok(true)
}

/// What a completion body observed inside a capture scope is entitled to do.
#[derive(Clone, Debug)]
pub enum CaptureClaim {
    Armed(PendingCapture),
    /// No arm is pending but the scope already claimed one: an SSE reconnect
    /// resend. Exact bytes may reuse the durable write; changed bytes must be
    /// re-captured.
    Resend {
        pending: PendingCapture,
        durable_body_fingerprint: Option<[u8; 32]>,
    },
    /// A completion body with no arm and no prior claim; it must not be sent.
    Unexplained,
}

/// Claim the armed capture; called by the capturing transport on a completion
/// body. `None` means no scope is installed; `Unexplained` inside a scope is
/// a refusal.
pub fn claim_pending() -> Option<(Arc<RequestCaptureScope>, CaptureClaim)> {
    let scope = current_scope()?;
    let claim = {
        let mut state = scope.lock();
        match state.pending.take() {
            Some(pending) => {
                state.claimed = Some(pending.clone());
                CaptureClaim::Armed(pending)
            }
            None => match state.claimed.clone() {
                Some(claimed) => CaptureClaim::Resend {
                    pending: claimed,
                    durable_body_fingerprint: state.durable_body_fingerprint,
                },
                None => CaptureClaim::Unexplained,
            },
        }
    };
    Some((scope, claim))
}

/// Whether an armed capture is still waiting; `true` after a provider response
/// has arrived means the send bypassed the capturing transport.
pub fn pending_is_armed() -> bool {
    current_scope().is_some_and(|scope| scope.lock().pending.is_some())
}

pub(crate) fn armed_capture_scope(turn_index: usize, attempt: u32) -> Option<String> {
    let scope = current_scope()?;
    let state = scope.lock();
    state
        .pending
        .as_ref()
        .filter(|pending| pending.turn_index == turn_index && pending.attempt == attempt)
        .map(|pending| pending.capture_scope.clone())
        .or_else(|| {
            state
                .claimed
                .as_ref()
                .filter(|claimed| claimed.turn_index == turn_index && claimed.attempt == attempt)
                .map(|claimed| claimed.capture_scope.clone())
        })
}

/// Transport activity of the armed or in-flight attempt `(turn_index, attempt)`.
pub(crate) fn attempt_activity(
    turn_index: usize,
    attempt: u32,
) -> Option<Arc<crate::provider_activity::ProviderActivity>> {
    let scope = current_scope()?;
    let state = scope.lock();
    state
        .pending
        .iter()
        .chain(state.claimed.iter())
        .find(|capture| capture.turn_index == turn_index && capture.attempt == attempt)
        .map(|capture| Arc::clone(&capture.activity))
}

/// Transport activity of the most recently armed attempt in this task's scope.
pub fn current_attempt_activity() -> Option<Arc<crate::provider_activity::ProviderActivity>> {
    let scope = current_scope()?;
    let state = scope.lock();
    state
        .pending
        .as_ref()
        .or(state.claimed.as_ref())
        .map(|capture| Arc::clone(&capture.activity))
}

/// The `LoopConfig::on_rendered_request` callback every production completion
/// loop installs: arm the ambient scope, never write. Returning `Ok(())` with
/// no scope installed is not a licence to send — inside a scope, an unarmed
/// completion body is refused by the transport.
pub fn ambient_arming_sink(kind: CaptureScopeKind) -> RenderedRequestSink {
    Arc::new(move |turn_index, attempt, _request, assembly_trace| {
        let armed = arm(kind, turn_index, attempt, assembly_trace);
        Box::pin(async move {
            if let Some(capture_scope) = armed {
                tracing::trace!(
                    capture_scope = %capture_scope,
                    turn_index,
                    attempt,
                    "armed rendered-request capture"
                );
            }
            Ok::<(), anyhow::Error>(())
        })
    })
}

/// Build a scope from an optional factory; `None` when capture is not configured.
pub fn scope_from_factory(
    context: RenderedRequestContext,
    factory: Option<&super::RenderedRequestCaptureFactory>,
) -> Option<Arc<RequestCaptureScope>> {
    let factory = factory?;
    let sink = factory(context.clone());
    Some(Arc::new(RequestCaptureScope::new(context, sink)))
}

pub async fn scope_request_if_configured<T>(
    context: RenderedRequestContext,
    factory: Option<&super::RenderedRequestCaptureFactory>,
    future: impl std::future::Future<Output = T>,
) -> T {
    match scope_from_factory(context, factory) {
        Some(scope) => scope_request(scope, future).await,
        None => future.await,
    }
}

/// Stage of a capture attempt, kept separate from `anyhow` so the transport
/// can log the failing stage without inspecting error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureFailureStage {
    DecodeBody,
    BuildFact,
    Persist,
}

impl CaptureFailureStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DecodeBody => "decode_body",
            Self::BuildFact => "build_fact",
            Self::Persist => "persist",
        }
    }
}

/// Build and persist one capture from a body observed at the transport seam.
/// Fail-closed: the caller must not forward the body unless this returns `Ok`.
pub async fn capture_body(
    scope: &RequestCaptureScope,
    pending: PendingCapture,
    source: super::RenderedRequestSource,
    provider_endpoint: Option<String>,
    provider_route_path_sha256: Option<String>,
    body: &[u8],
) -> std::result::Result<(), (CaptureFailureStage, anyhow::Error)> {
    let request_json: serde_json::Value = serde_json::from_slice(body)
        .map_err(|error| (CaptureFailureStage::DecodeBody, anyhow::Error::from(error)))?;
    let components = super::RenderedRequestComponents::from_provider_body(request_json, source);
    let admission_join = (scope.admission_join_lookup)(&pending.capture_scope);
    let capture_kind = pending
        .capture_scope
        .parse::<gents_protocol::rendered_request::CaptureScope>()
        .map_err(|error| (CaptureFailureStage::BuildFact, anyhow::Error::from(error)))?
        .kind;
    let mut context = scope.context().clone();
    if matches!(
        capture_kind,
        CaptureScopeKind::Compaction | CaptureScopeKind::CompactionFallback
    ) {
        context.provider_family = scope.compaction_provider_family.clone();
    }
    let rendered = super::build_rendered_completion_request(
        &context,
        &pending.capture_scope,
        source,
        provider_endpoint,
        provider_route_path_sha256,
        pending.turn_index,
        pending.attempt,
        pending.assembly_trace,
        components,
        admission_join,
    )
    .map_err(|error| (CaptureFailureStage::BuildFact, error))?;

    let capture_key = rendered.capture_key.clone();
    let request_id = rendered.request_id.clone();
    (scope.sink())(rendered)
        .await
        .map_err(|error| (CaptureFailureStage::Persist, error))?;

    tracing::debug!(
        capture_key = %capture_key,
        request_id = %request_id,
        capture_scope = %pending.capture_scope,
        turn_index = pending.turn_index,
        attempt = pending.attempt,
        "captured rendered provider request"
    );
    Ok(())
}

/// The refusal for a completion body that arrived inside a capture scope with
/// nothing armed and nothing previously claimed.
pub fn unexplained_send_message(context: &RenderedRequestContext, path: &str) -> String {
    format!(
        "a completion request to {path} reached the provider transport for request {} \
         with no armed rendered-request capture; the completion loop that issued it does not \
         install `LoopConfig::on_rendered_request`, so the call cannot be made durable and was \
         not issued",
        context.request_id
    )
}

/// Typed error the transport turns into a refusal to send.
pub fn capture_failure_message(
    stage: CaptureFailureStage,
    capture_scope: &str,
    turn_index: usize,
    attempt: u32,
    error: &anyhow::Error,
) -> String {
    format!(
        "rendered-request capture failed at stage {} for scope {capture_scope} turn {turn_index} \
         attempt {attempt}; the provider call was not issued: {error:#}",
        stage.as_str()
    )
}

/// Escape hatch used only by tests that need a scope without a daemon.
/// Not `#[cfg(test)]`: gents' own capture-seam tests build real scopes with
/// this, and a cfg(test) item in this crate is invisible to a dependent
/// crate's own test build.
pub fn test_scope(
    context: RenderedRequestContext,
    sink: RenderedRequestCaptureSink,
) -> Arc<RequestCaptureScope> {
    Arc::new(RequestCaptureScope::new(context, sink))
}

/// Every scope label armed inside the current scope, in order. Not
/// `#[cfg(test)]`, for the same reason as `test_scope`.
pub fn armed_labels() -> Vec<String> {
    current_scope()
        .map(|scope| scope.lock().armed_labels.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_audit::{
        recv_one, try_recv_one, AuxiliaryOutputEvent, AuxiliaryOutputSink, ClaudeAuditEvent,
        ClaudeBlockKind,
    };
    use crate::rendered_request::{AssemblyBuildPath, AssemblyTrace};
    use futures::FutureExt;

    fn context() -> RenderedRequestContext {
        RenderedRequestContext {
            request_doc_id: "doc-1".to_string(),
            request_commit_cid: "bafy-request-commit".to_string(),
            request_id: "req-1".to_string(),
            node_did: "did:key:agent".to_string(),
            requester_did: String::new(),
            agent_id: "behavior".to_string(),
            session_id: "session".to_string(),
            model_name: "test-model".to_string(),
            provider_family: None,
        }
    }

    fn trace() -> AssemblyTrace {
        AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, Vec::new())
    }

    fn noop_sink() -> RenderedRequestCaptureSink {
        Arc::new(|_| Box::pin(async { Ok(()) }))
    }

    #[tokio::test]
    async fn pending_generator_receive_does_not_hold_the_cancellation_drain_lock() {
        let scope = test_scope(context(), noop_sink());
        scope_request(scope, async {
            arm(CaptureScopeKind::Inference, 0, 0, trace()).unwrap();
            claim_pending().expect("armed attempt");
            let sender = current_audit_sender().expect("claimed audit sender");
            let receiver = take_audit_receiver(0, 0).expect("audit receiver");
            let mut pending_receive = Box::pin(recv_one(&receiver));
            assert!(pending_receive.as_mut().now_or_never().is_none());

            let mut reservation = sender.reserve().await.expect("audit capacity");
            reservation
                .emit(ClaudeAuditEvent::BlockStart {
                    index: 0,
                    kind: ClaudeBlockKind::Thinking,
                })
                .unwrap();
            reservation
                .emit(ClaudeAuditEvent::Signature {
                    index: 0,
                    fragment: "received".into(),
                })
                .unwrap();
            drop(reservation);
            let first = try_recv_one(&receiver).expect("first audit observation");
            assert!(matches!(
                first.event,
                ClaudeAuditEvent::BlockStart { index: 0, .. }
            ));
            let observed =
                tokio::time::timeout(std::time::Duration::from_secs(2), drain_ready_audit())
                    .await
                    .expect("cancellation drain must not wait on a suspended receive");
            assert!(matches!(
                observed.as_slice(),
                [ProviderAuditObservation {
                    event: ClaudeAuditEvent::Signature { fragment, .. },
                    ..
                }] if fragment == "received"
            ));
            drop(pending_receive);
        })
        .await;
    }

    #[tokio::test]
    async fn cancellation_routes_queued_auxiliary_audit_to_its_exact_source() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let event_observed = Arc::clone(&observed);
        let mut scope = RequestCaptureScope::new(context(), noop_sink());
        scope.set_auxiliary_output_sink(AuxiliaryOutputSink {
            observe: Arc::new(move |event| {
                event_observed
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(event);
                Box::pin(async { Ok(()) })
            }),
            next_flush_deadline: Arc::new(|_, _, _| Box::pin(async { Ok(None) })),
            flush_pending: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        });
        scope_request(Arc::new(scope), async {
            let label = arm(CaptureScopeKind::Compaction, 0, 0, trace()).unwrap();
            claim_pending().expect("armed auxiliary attempt");
            let capture_scope = label.parse().expect("capture scope");
            emit_auxiliary_output(AuxiliaryOutputObservation {
                capture_scope,
                turn: 0,
                attempt: 0,
                event: AuxiliaryOutputEvent::AttemptStarted,
            })
            .await
            .unwrap();
            let sender = current_audit_sender().expect("claimed audit sender");
            let mut reservation = sender.reserve().await.expect("audit capacity");
            reservation
                .emit(ClaudeAuditEvent::BlockStart {
                    index: 0,
                    kind: ClaudeBlockKind::Thinking,
                })
                .unwrap();
            reservation
                .emit(ClaudeAuditEvent::Signature {
                    index: 0,
                    fragment: "partial-signature".into(),
                })
                .unwrap();
            drop(reservation);
            assert!(tokio::time::timeout(
                std::time::Duration::from_secs(2),
                flush_received_auxiliary_partial(),
            )
            .await
            .expect("bounded auxiliary cancellation drain")
            .unwrap());
            let events = observed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert_eq!(events.len(), 4);
            assert!(events.iter().all(|event| {
                (event.capture_scope, event.turn, event.attempt) == (capture_scope, 0, 0)
            }));
            assert!(matches!(
                events[0].event,
                AuxiliaryOutputEvent::AttemptStarted
            ));
            assert!(matches!(
                events[1].event,
                AuxiliaryOutputEvent::Audit(ClaudeAuditEvent::BlockStart { .. })
            ));
            assert!(matches!(
                events[2].event,
                AuxiliaryOutputEvent::Audit(ClaudeAuditEvent::Signature { .. })
            ));
            assert!(matches!(
                events[3].event,
                AuxiliaryOutputEvent::ClosePartial
            ));
            assert!(!flush_received_auxiliary_partial().await.unwrap());
        })
        .await;
    }

    #[tokio::test]
    async fn each_loop_of_a_kind_gets_its_own_label() {
        let scope = test_scope(context(), noop_sink());
        scope_request(scope, async {
            assert_eq!(
                arm(CaptureScopeKind::Compaction, 0, 0, trace()).unwrap(),
                "compaction.1"
            );
            let _ = claim_pending();
            assert_eq!(
                arm(CaptureScopeKind::Compaction, 0, 0, trace()).unwrap(),
                "compaction.2"
            );
            let _ = claim_pending();
            assert_eq!(
                arm(CaptureScopeKind::Compaction, 0, 1, trace()).unwrap(),
                "compaction.2"
            );
            let _ = claim_pending();
            assert_eq!(
                arm(CaptureScopeKind::Compaction, 1, 0, trace()).unwrap(),
                "compaction.2"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn kinds_allocate_independently() {
        let scope = test_scope(context(), noop_sink());
        scope_request(scope, async {
            assert_eq!(
                arm(CaptureScopeKind::Inference, 0, 0, trace()).unwrap(),
                "inference.1"
            );
            let _ = claim_pending();
            assert_eq!(
                arm(CaptureScopeKind::Title, 0, 0, trace()).unwrap(),
                "title.1"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn an_arm_is_claimed_exactly_once_and_then_reads_as_a_resend() {
        let scope = test_scope(context(), noop_sink());
        scope_request(scope, async {
            arm(CaptureScopeKind::Inference, 7, 2, trace()).expect("armed");
            assert!(pending_is_armed());

            let (_, claim) = claim_pending().expect("scope installed");
            let CaptureClaim::Armed(pending) = claim else {
                panic!("the first claim must consume the arm");
            };
            assert_eq!((pending.turn_index, pending.attempt), (7, 2));
            assert!(!pending_is_armed());

            let (_, claim) = claim_pending().expect("scope installed");
            let CaptureClaim::Resend {
                pending,
                durable_body_fingerprint,
            } = claim
            else {
                panic!("a send after the arm was claimed must read as a resend");
            };
            assert_eq!((pending.turn_index, pending.attempt), (7, 2));
            assert_eq!(durable_body_fingerprint, None);
            assert!(!pending_is_armed());

            arm(CaptureScopeKind::Inference, 8, 0, trace()).expect("armed");
            let (_, claim) = claim_pending().expect("scope installed");
            let CaptureClaim::Armed(pending) = claim else {
                panic!("the new attempt's arm must be claimed as an arm");
            };
            assert_eq!((pending.turn_index, pending.attempt), (8, 0));
        })
        .await;
    }

    #[tokio::test]
    async fn a_scope_that_never_armed_reports_an_unexplained_send() {
        let scope = test_scope(context(), noop_sink());
        scope_request(scope, async {
            let (_, claim) = claim_pending().expect("scope installed");
            assert!(
                matches!(claim, CaptureClaim::Unexplained),
                "a scope with no arm and no prior claim must be unexplained"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn arming_without_a_scope_is_a_noop() {
        assert!(arm(CaptureScopeKind::Inference, 0, 0, trace()).is_none());
        assert!(!pending_is_armed());
        assert!(claim_pending().is_none());
    }
}
