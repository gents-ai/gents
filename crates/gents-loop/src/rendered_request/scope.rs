use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use super::{
    AdmissionJoin, AssemblyTrace, RenderedRequestCaptureSink, RenderedRequestContext,
    RenderedRequestError,
};
use crate::loop_stream::RenderedRequestSink;
use crate::provider_audit::{
    AuxiliaryOutputError, AuxiliaryOutputEvent, AuxiliaryOutputObservation, AuxiliaryOutputSink,
    ProviderAuditObservation, ProviderAuditReceiver, ProviderAuditSender,
};

/// Bounds pending observations for one attempt; reserving slots applies
/// backpressure to the provider stream instead of dropping audit evidence.
const AUDIT_CHANNEL_CAPACITY: usize = 32;

struct AttemptAuditChannel {
    capture_scope: String,
    turn_index: usize,
    attempt: u32,
    sender: mpsc::Sender<ProviderAuditObservation>,
    receiver: ProviderAuditReceiver,
}

pub use super::CaptureScopeKind;

pub fn next_scope_sequence(persisted: impl IntoIterator<Item = u64>) -> Option<u64> {
    persisted.into_iter().max().unwrap_or(0).checked_add(1)
}

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
    durable_body_fingerprint: Option<[u8; 32]>,
    armed_labels: Vec<String>,
    audit: Option<AttemptAuditChannel>,
    active_auxiliary: Option<(gents_protocol::rendered_request::CaptureScope, usize, u32)>,
    last_auxiliary: Option<(gents_protocol::rendered_request::CaptureScope, usize, u32)>,
}

/// The join of the capture labelled by its first argument, from the admission
/// of the call that sends it.
pub type AdmissionJoinLookup = Arc<
    dyn Fn(
            &str,
            Option<&crate::provider_call::admission::AdmissionCallContext>,
        ) -> Option<AdmissionJoin>
        + Send
        + Sync,
>;

fn no_admission_join(
    _capture_scope: &str,
    _admission: Option<&crate::provider_call::admission::AdmissionCallContext>,
) -> Option<AdmissionJoin> {
    None
}

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

    /// Restore allocation counters before the resumed execution can arm calls.
    pub fn restore_allocations(
        &mut self,
        scopes: impl IntoIterator<Item = gents_protocol::rendered_request::CaptureScope>,
    ) -> Result<(), &'static str> {
        let mut grouped: BTreeMap<CaptureScopeKind, Vec<u64>> = BTreeMap::new();
        for scope in scopes {
            grouped.entry(scope.kind).or_default().push(scope.seq);
        }
        let mut allocated = BTreeMap::new();
        for (kind, sequences) in grouped {
            let next = next_scope_sequence(sequences).ok_or("capture scope sequence exhausted")?;
            allocated.insert(kind, next - 1);
        }
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.allocated.is_empty() || !state.armed_labels.is_empty() {
            return Err("capture scope is already active");
        }
        state.allocated = allocated;
        Ok(())
    }

    pub fn context(&self) -> &RenderedRequestContext {
        &self.context
    }

    pub fn sink(&self) -> &RenderedRequestCaptureSink {
        &self.sink
    }

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

pub fn arm(
    scope: Option<&Arc<RequestCaptureScope>>,
    kind: CaptureScopeKind,
    turn_index: usize,
    attempt: u32,
    assembly_trace: AssemblyTrace,
) -> Option<String> {
    let scope = scope.cloned()?;
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

pub fn current_audit_sender(
    scope: Option<&Arc<RequestCaptureScope>>,
) -> Option<ProviderAuditSender> {
    let scope = scope.cloned()?;
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

pub fn take_audit_receiver(
    scope: Option<&Arc<RequestCaptureScope>>,
    turn_index: usize,
    attempt: u32,
) -> Option<ProviderAuditReceiver> {
    let scope = scope.cloned()?;
    let state = scope.lock();
    state.audit.as_ref().and_then(|audit| {
        (audit.turn_index == turn_index && audit.attempt == attempt)
            .then(|| Arc::clone(&audit.receiver))
    })
}

pub async fn drain_ready_audit(
    scope: Option<&Arc<RequestCaptureScope>>,
) -> Vec<ProviderAuditObservation> {
    let receiver = {
        let Some(scope) = scope.cloned() else {
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

pub fn auxiliary_output_sink(
    scope: Option<&Arc<RequestCaptureScope>>,
) -> Option<AuxiliaryOutputSink> {
    scope.cloned()?.auxiliary_output_sink.clone()
}

pub async fn emit_auxiliary_output(
    scope: Option<&Arc<RequestCaptureScope>>,
    observation: AuxiliaryOutputObservation,
) -> Result<(), AuxiliaryOutputError> {
    let scope = scope.cloned().ok_or(AuxiliaryOutputError::NoRequestScope)?;
    let sink = scope
        .auxiliary_output_sink
        .as_ref()
        .ok_or(AuxiliaryOutputError::NoCanonicalSink)?
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
                if state.active_auxiliary.is_some() {
                    return Err(AuxiliaryOutputError::AttemptAlreadyActive);
                }
            }
            AuxiliaryOutputEvent::OutputObligationPending { .. }
            | AuxiliaryOutputEvent::FinalText(_)
                if state.active_auxiliary.is_none() =>
            {
                if state.last_auxiliary != Some(identity) {
                    return Err(AuxiliaryOutputError::ReminderChangedSource);
                }
            }
            _ => {
                if state.active_auxiliary != Some(identity) {
                    return Err(AuxiliaryOutputError::OutputChangedSource);
                }
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

pub async fn flush_received_auxiliary_partial(
    scope: Option<&Arc<RequestCaptureScope>>,
) -> Result<bool, AuxiliaryOutputError> {
    let Some(scope) = scope.cloned() else {
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
            .ok_or(AuxiliaryOutputError::NoAuditReceiver)?;
        if !(audit.capture_scope == identity.0.to_string()
            && audit.turn_index == identity.1
            && audit.attempt == identity.2)
        {
            return Err(AuxiliaryOutputError::AuditReceiverChangedSource);
        }
        (identity, Arc::clone(&audit.receiver))
    };
    while let Some(audit) = crate::provider_audit::try_recv_one(&receiver) {
        if (audit.capture_scope, audit.turn, audit.attempt) != identity {
            return Err(AuxiliaryOutputError::QueuedAuditChangedSource);
        }
        emit_auxiliary_output(
            Some(&scope),
            AuxiliaryOutputObservation {
                capture_scope: identity.0,
                turn: identity.1,
                attempt: identity.2,
                event: AuxiliaryOutputEvent::Audit(audit.event),
            },
        )
        .await?;
    }
    emit_auxiliary_output(
        Some(&scope),
        AuxiliaryOutputObservation {
            capture_scope: identity.0,
            turn: identity.1,
            attempt: identity.2,
            event: AuxiliaryOutputEvent::ClosePartial,
        },
    )
    .await?;
    Ok(true)
}

#[derive(Clone, Debug)]
pub enum CaptureClaim {
    Armed(PendingCapture),
    Resend {
        pending: PendingCapture,
        durable_body_fingerprint: Option<[u8; 32]>,
    },
    Unexplained,
}

pub fn claim_pending(
    scope: Option<&Arc<RequestCaptureScope>>,
) -> Option<(Arc<RequestCaptureScope>, CaptureClaim)> {
    let scope = scope.cloned()?;
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

pub fn pending_is_armed(scope: Option<&Arc<RequestCaptureScope>>) -> bool {
    scope
        .cloned()
        .is_some_and(|scope| scope.lock().pending.is_some())
}

pub(crate) fn armed_capture_scope(
    scope: Option<&Arc<RequestCaptureScope>>,
    turn_index: usize,
    attempt: u32,
) -> Option<String> {
    let scope = scope.cloned()?;
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

pub(crate) fn attempt_activity(
    scope: Option<&Arc<RequestCaptureScope>>,
    turn_index: usize,
    attempt: u32,
) -> Option<Arc<crate::provider_activity::ProviderActivity>> {
    let scope = scope.cloned()?;
    let state = scope.lock();
    state
        .pending
        .iter()
        .chain(state.claimed.iter())
        .find(|capture| capture.turn_index == turn_index && capture.attempt == attempt)
        .map(|capture| Arc::clone(&capture.activity))
}

pub fn current_attempt_activity(
    scope: Option<&Arc<RequestCaptureScope>>,
) -> Option<Arc<crate::provider_activity::ProviderActivity>> {
    let scope = scope.cloned()?;
    let state = scope.lock();
    state
        .pending
        .as_ref()
        .or(state.claimed.as_ref())
        .map(|capture| Arc::clone(&capture.activity))
}

/// Arms a capture of `kind` in the capture scope of the call each rendered
/// request is for.
pub fn arming_sink(kind: CaptureScopeKind) -> RenderedRequestSink {
    Arc::new(move |call, turn_index, attempt, _request, assembly_trace| {
        let armed = arm(
            call.capture.as_ref(),
            kind,
            turn_index,
            attempt,
            assembly_trace,
        );
        Box::pin(async move {
            if let Some(capture_scope) = armed {
                tracing::trace!(
                    capture_scope = %capture_scope,
                    turn_index,
                    attempt,
                    "armed rendered-request capture"
                );
            }
            Ok::<(), RenderedRequestError>(())
        })
    })
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureBodyError {
    #[error(transparent)]
    DecodeBody(serde_json::Error),
    #[error(transparent)]
    ParseCaptureScope(gents_protocol::rendered_request::CaptureScopeParseError),
    #[error(transparent)]
    BuildFact(RenderedRequestError),
    #[error(transparent)]
    Persist(RenderedRequestError),
}

impl CaptureBodyError {
    pub fn stage(&self) -> CaptureFailureStage {
        match self {
            Self::DecodeBody(_) => CaptureFailureStage::DecodeBody,
            Self::ParseCaptureScope(_) | Self::BuildFact(_) => CaptureFailureStage::BuildFact,
            Self::Persist(_) => CaptureFailureStage::Persist,
        }
    }
}

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

pub async fn capture_body(
    scope: &RequestCaptureScope,
    admission: Option<&crate::provider_call::admission::AdmissionCallContext>,
    pending: PendingCapture,
    source: super::RenderedRequestSource,
    provider_endpoint: Option<String>,
    provider_route_path_sha256: Option<String>,
    body: &[u8],
) -> Result<(), CaptureBodyError> {
    let request_json: serde_json::Value =
        serde_json::from_slice(body).map_err(CaptureBodyError::DecodeBody)?;
    let components = super::RenderedRequestComponents::from_provider_body(request_json, source);
    let admission_join = (scope.admission_join_lookup)(&pending.capture_scope, admission);
    let capture_kind = pending
        .capture_scope
        .parse::<gents_protocol::rendered_request::CaptureScope>()
        .map_err(CaptureBodyError::ParseCaptureScope)?
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
        source,
        super::ProviderRoute {
            endpoint: provider_endpoint,
            route_path_sha256: provider_route_path_sha256,
        },
        super::RenderedAttempt {
            capture_scope: pending.capture_scope.clone(),
            turn_index: pending.turn_index,
            attempt: pending.attempt,
            assembly_trace: pending.assembly_trace,
            admission_join,
        },
        components,
    )
    .map_err(CaptureBodyError::BuildFact)?;

    let capture_key = rendered.capture_key.clone();
    let request_id = rendered.request_id.clone();
    (scope.sink())(rendered)
        .await
        .map_err(CaptureBodyError::Persist)?;

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

pub fn test_scope(
    context: RenderedRequestContext,
    sink: RenderedRequestCaptureSink,
) -> Arc<RequestCaptureScope> {
    Arc::new(RequestCaptureScope::new(context, sink))
}

pub fn armed_labels(scope: Option<&Arc<RequestCaptureScope>>) -> Vec<String> {
    scope
        .cloned()
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
    use crate::test_support::doubles::{
        empty_trace as trace, noop_capture_sink as noop_sink, test_capture_context as context,
    };
    use futures::FutureExt;

    #[test]
    fn restored_capture_scope_allocation_matches_the_generated_model() {
        gents_lean_vocab::families::backend_wake_capture_scope::replay(|i| {
            next_scope_sequence(i.persisted.iter().copied()).unwrap()
        });
        assert_eq!(next_scope_sequence([u64::MAX]), None);
    }

    #[tokio::test]
    async fn pending_generator_receive_does_not_hold_the_cancellation_drain_lock() {
        let scope = test_scope(context(), noop_sink());
        let scope = Some(&scope);
        arm(scope, CaptureScopeKind::Inference, 0, 0, trace()).unwrap();
        claim_pending(scope).expect("armed attempt");
        let sender = current_audit_sender(scope).expect("claimed audit sender");
        let receiver = take_audit_receiver(scope, 0, 0).expect("audit receiver");
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
            tokio::time::timeout(std::time::Duration::from_secs(2), drain_ready_audit(scope))
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
        let scope = Arc::new(scope);
        let scope = Some(&scope);
        let label = arm(scope, CaptureScopeKind::Compaction, 0, 0, trace()).unwrap();
        claim_pending(scope).expect("armed auxiliary attempt");
        let capture_scope = label.parse().expect("capture scope");
        emit_auxiliary_output(
            scope,
            AuxiliaryOutputObservation {
                capture_scope,
                turn: 0,
                attempt: 0,
                event: AuxiliaryOutputEvent::AttemptStarted,
            },
        )
        .await
        .unwrap();
        let sender = current_audit_sender(scope).expect("claimed audit sender");
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
            flush_received_auxiliary_partial(scope),
        )
        .await
        .expect("bounded auxiliary cancellation drain")
        .unwrap());
        {
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
        }
        assert!(!flush_received_auxiliary_partial(scope).await.unwrap());
    }
}
