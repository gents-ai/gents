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

pub use super::CaptureScopeKind;

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

pub type AdmissionJoinLookup = Arc<dyn Fn(&str) -> Option<AdmissionJoin> + Send + Sync>;

fn no_admission_join(_capture_scope: &str) -> Option<AdmissionJoin> {
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

tokio::task_local! {
    static CAPTURE_SCOPE: Arc<RequestCaptureScope>;
}

/// Keep the task-local scope active through stream construction and polling;
/// provider SSE clients connect lazily on first poll.
pub async fn scope_request<T>(
    scope: Arc<RequestCaptureScope>,
    future: impl std::future::Future<Output = T>,
) -> T {
    CAPTURE_SCOPE.scope(scope, future).await
}

pub fn current_scope() -> Option<Arc<RequestCaptureScope>> {
    CAPTURE_SCOPE.try_with(Arc::clone).ok()
}

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

pub fn take_audit_receiver(turn_index: usize, attempt: u32) -> Option<ProviderAuditReceiver> {
    let scope = current_scope()?;
    let state = scope.lock();
    state.audit.as_ref().and_then(|audit| {
        (audit.turn_index == turn_index && audit.attempt == attempt)
            .then(|| Arc::clone(&audit.receiver))
    })
}

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

#[derive(Clone, Debug)]
pub enum CaptureClaim {
    Armed(PendingCapture),
    Resend {
        pending: PendingCapture,
        durable_body_fingerprint: Option<[u8; 32]>,
    },
    Unexplained,
}

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

pub fn current_attempt_activity() -> Option<Arc<crate::provider_activity::ProviderActivity>> {
    let scope = current_scope()?;
    let state = scope.lock();
    state
        .pending
        .as_ref()
        .or(state.claimed.as_ref())
        .map(|capture| Arc::clone(&capture.activity))
}

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

pub fn unexplained_send_message(context: &RenderedRequestContext, path: &str) -> String {
    format!(
        "a completion request to {path} reached the provider transport for request {} \
         with no armed rendered-request capture; the completion loop that issued it does not \
         install `LoopConfig::on_rendered_request`, so the call cannot be made durable and was \
         not issued",
        context.request_id
    )
}

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

pub fn test_scope(
    context: RenderedRequestContext,
    sink: RenderedRequestCaptureSink,
) -> Arc<RequestCaptureScope> {
    Arc::new(RequestCaptureScope::new(context, sink))
}

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
            assert!(!flush_received_auxiliary_partial().await.unwrap());
        })
        .await;
    }
}
