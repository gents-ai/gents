//! The per-request capture scope: how the loop's facts reach the transport.
//!
//! The bytes worth capturing exist at the transport seam, but everything that
//! *identifies* them — which request, which completion loop, which turn, which
//! attempt, and the in-memory assembly trace — exists only in the owned loop.
//! A rig completion client is built once per behavior and reused for every
//! request, so those facts cannot be constructor arguments. They ride a
//! task-local instead, exactly as the session id already does for sticky-cache
//! routing (`inference_http::SessionTaggingHttpClient::tag` reads
//! `admission::current_session_id()` at send time, and the admission scope is
//! live across stream polling because `daemon::inference` wraps both the
//! stream's construction and its drain loop in `admission::scope_call*`).
//!
//! ## Arm, then consume
//!
//! The loop *arms* a [`PendingCapture`] immediately before `model.stream`. The
//! transport *takes* it when it is about to post a completion body, writes the
//! durable row, and only then forwards. Two properties follow:
//!
//! * Nothing reaches the network uncaptured while a capture is armed — taking
//!   and writing happen inside the send path, before delegation.
//! * Nothing else consumes the turn's identity by accident: only a request
//!   whose path is a completion path claims the pending capture, so a `/models`
//!   listing or an SSE reconnect issued in the same task passes through
//!   untouched.
//!
//! [`pending_is_armed`] closes the other direction. If a provider response
//! arrives while the arm is still pending, the send bypassed the capturing
//! transport — a mis-wired client stack — and the loop turns that into a
//! terminal error rather than a silently uncaptured call.
//!
//! ## Why the scope label exists
//!
//! One request runs several completion loops, and each starts its turn and
//! attempt counters at zero: the owned inference loop, the per-turn compaction
//! summarizer (guided, plus a strict-JSON fallback), and conversation title
//! generation. Without a discriminator their first calls would all be
//! `(request_id, turn 0, attempt 0)` — one capture key naming several different
//! provider requests, which the sink is required to reject as an integrity
//! violation. Each loop therefore takes a label like `compaction.2` from this
//! scope. Allocation keys off the loop's own first arm — every loop arms
//! `(turn 0, attempt 0)` exactly once, and only on its first call — so the
//! label is stable for the rest of that loop and distinct from every sibling.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::{AssemblyTrace, RenderedRequestCaptureSink, RenderedRequestContext};
use crate::agent::loop_stream::RenderedRequestSink;

/// Which completion loop inside a request is issuing provider calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CaptureScopeKind {
    /// The request's own owned completion loop (`agent/loop_stream.rs`).
    Inference,
    /// The guided per-turn compaction summarizer. Its output is the ephemeral
    /// continuation checkpoint injected straight into provider history and
    /// never written as an `AgentCompactionEntry`, which is the single fact
    /// this whole design exists to make explainable.
    Compaction,
    /// The strict-JSON compaction fallback, taken when guided structured output
    /// exhausts its recovery.
    CompactionFallback,
    /// Conversation title generation.
    Title,
    /// The one-shot runner (`oneshot::run_openai_oneshot_with_tools`).
    OneShot,
}

impl CaptureScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inference => "inference",
            Self::Compaction => "compaction",
            Self::CompactionFallback => "compaction_fallback",
            Self::Title => "title",
            Self::OneShot => "oneshot",
        }
    }
}

impl std::fmt::Display for CaptureScopeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One armed provider attempt, waiting for the transport to supply its body.
#[derive(Clone, Debug)]
pub(crate) struct PendingCapture {
    pub(crate) capture_scope: String,
    pub(crate) turn_index: usize,
    pub(crate) attempt: u32,
    pub(crate) assembly_trace: AssemblyTrace,
}

#[derive(Default)]
struct ScopeState {
    /// How many loops of each kind this request has started.
    allocated: BTreeMap<CaptureScopeKind, u64>,
    /// The label most recently handed to each kind, reused by that loop's later
    /// turns and attempts.
    current: BTreeMap<CaptureScopeKind, String>,
    pending: Option<PendingCapture>,
    /// Every arm this scope has seen, in order. Tests need this because an arm
    /// the transport never claims is silently replaced by the next one, so a
    /// test that only reads `pending` cannot see a loop that armed and then was
    /// superseded — which is exactly the shape the compaction fallback has.
    #[cfg(test)]
    armed_labels: Vec<String>,
}

/// Everything the transport needs to turn a body into a durable row.
pub(crate) struct RequestCaptureScope {
    context: RenderedRequestContext,
    sink: RenderedRequestCaptureSink,
    state: Mutex<ScopeState>,
}

impl RequestCaptureScope {
    pub(crate) fn new(context: RenderedRequestContext, sink: RenderedRequestCaptureSink) -> Self {
        Self {
            context,
            sink,
            state: Mutex::new(ScopeState::default()),
        }
    }

    pub(crate) fn context(&self) -> &RenderedRequestContext {
        &self.context
    }

    pub(crate) fn sink(&self) -> &RenderedRequestCaptureSink {
        &self.sink
    }

    /// The label for a loop of `kind`. A `(0, 0)` arm starts a new loop and
    /// allocates a fresh label; every later turn or attempt reuses it.
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
}

tokio::task_local! {
    static CAPTURE_SCOPE: Arc<RequestCaptureScope>;
}

/// Install `scope` for the duration of `future`.
///
/// Must wrap both the construction of a completion stream and its drain loop:
/// the SSE transports connect lazily on first poll
/// (`rig::http_client::sse::GenericEventSource`), so the HTTP send frequently
/// happens while the loop is polling rather than while it is awaiting the
/// model's stream constructor.
pub(crate) async fn scope_request<T>(
    scope: Arc<RequestCaptureScope>,
    future: impl std::future::Future<Output = T>,
) -> T {
    CAPTURE_SCOPE.scope(scope, future).await
}

pub(crate) fn current_scope() -> Option<Arc<RequestCaptureScope>> {
    CAPTURE_SCOPE.try_with(Arc::clone).ok()
}

/// Arm this attempt's capture. Idempotent per attempt; a second arm before the
/// transport consumes the first replaces it, which is what a pre-stream retry
/// that never reached the network should do.
pub(crate) fn arm(
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
    };
    let replaced = {
        let mut state = scope.lock();
        #[cfg(test)]
        state.armed_labels.push(capture_scope.clone());
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

/// Claim the armed capture. Called by the capturing transport once it knows the
/// outbound request is a completion body.
pub(crate) fn take_pending() -> Option<(Arc<RequestCaptureScope>, PendingCapture)> {
    let scope = current_scope()?;
    let pending = scope.lock().pending.take()?;
    Some((scope, pending))
}

/// Whether an armed capture is still waiting. `true` after a provider response
/// has arrived means the send did not pass through the capturing transport.
pub(crate) fn pending_is_armed() -> bool {
    current_scope().is_some_and(|scope| scope.lock().pending.is_some())
}

/// The `LoopConfig::on_rendered_request` callback every production completion
/// loop installs: arm the ambient scope, never write.
///
/// Returns `Ok(())` when no scope is installed. That is not fail-open — with no
/// scope there is no sink and no capture context, which is the state of a
/// `gents` embedding that never started a request (unit tests, library use).
/// The fail-closed obligation lives where the write does, in
/// `transport::RenderedRequestCapturingHttpClient`.
pub(crate) fn ambient_arming_sink(kind: CaptureScopeKind) -> RenderedRequestSink {
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

/// Build a scope from a context and an optional factory. `None` when capture is
/// not configured.
pub(crate) fn scope_from_factory(
    context: RenderedRequestContext,
    factory: Option<&super::RenderedRequestCaptureFactory>,
) -> Option<Arc<RequestCaptureScope>> {
    let factory = factory?;
    let sink = factory(context.clone());
    Some(Arc::new(RequestCaptureScope::new(context, sink)))
}

/// Run `future` under a capture scope when one can be built, and unchanged
/// otherwise.
pub(crate) async fn scope_request_if_configured<T>(
    context: RenderedRequestContext,
    factory: Option<&super::RenderedRequestCaptureFactory>,
    future: impl std::future::Future<Output = T>,
) -> T {
    match scope_from_factory(context, factory) {
        Some(scope) => scope_request(scope, future).await,
        None => future.await,
    }
}

/// Result of a capture attempt, kept separate from `anyhow` so the transport
/// can log the failing stage without inspecting error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureFailureStage {
    /// The outbound body was not valid JSON, so no fact could be built.
    DecodeBody,
    /// Building the DTO (hashing, key derivation, provenance) failed.
    BuildFact,
    /// The durable write failed or was rejected as an integrity violation.
    Persist,
}

impl CaptureFailureStage {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::DecodeBody => "decode_body",
            Self::BuildFact => "build_fact",
            Self::Persist => "persist",
        }
    }
}

/// Build and persist one capture from a body observed at the transport seam.
///
/// Fail-closed by construction: the caller must not forward the body unless
/// this returns `Ok`.
pub(crate) async fn capture_body(
    scope: &RequestCaptureScope,
    pending: PendingCapture,
    source: super::RenderedRequestSource,
    body: &[u8],
) -> std::result::Result<(), (CaptureFailureStage, anyhow::Error)> {
    let request_json: serde_json::Value = serde_json::from_slice(body)
        .map_err(|error| (CaptureFailureStage::DecodeBody, anyhow::Error::from(error)))?;
    let components = super::RenderedRequestComponents::from_provider_body(request_json, source);
    let rendered = super::build_rendered_completion_request(
        scope.context(),
        &pending.capture_scope,
        source,
        pending.turn_index,
        pending.attempt,
        pending.assembly_trace,
        components,
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

/// Typed error the transport turns into a refusal to send.
pub(crate) fn capture_failure_message(
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
#[cfg(test)]
pub(crate) fn test_scope(
    context: RenderedRequestContext,
    sink: RenderedRequestCaptureSink,
) -> Arc<RequestCaptureScope> {
    Arc::new(RequestCaptureScope::new(context, sink))
}

/// Every scope label armed inside the current scope, in order.
#[cfg(test)]
pub(crate) fn armed_labels() -> Vec<String> {
    current_scope()
        .map(|scope| scope.lock().armed_labels.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rendered_request::{AssemblyBuildPath, AssemblyTrace};

    fn context() -> RenderedRequestContext {
        RenderedRequestContext {
            request_id: "req-1".to_string(),
            agent_did: "did:key:agent".to_string(),
            requester_did: String::new(),
            behavior_id: "behavior".to_string(),
            session_id: "session".to_string(),
            model_name: "test-model".to_string(),
        }
    }

    fn trace() -> AssemblyTrace {
        AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, Vec::new())
    }

    fn noop_sink() -> RenderedRequestCaptureSink {
        Arc::new(|_| Box::pin(async { Ok(()) }))
    }

    /// Every completion loop in a request starts at `(0, 0)`, so a fresh label
    /// per first-arm is the only thing keeping the summarizer's first call and
    /// the request's first turn from being one durable fact.
    #[tokio::test]
    async fn each_loop_of_a_kind_gets_its_own_label() {
        let scope = test_scope(context(), noop_sink());
        scope_request(scope, async {
            assert_eq!(
                arm(CaptureScopeKind::Compaction, 0, 0, trace()).unwrap(),
                "compaction.1"
            );
            let _ = take_pending();
            assert_eq!(
                arm(CaptureScopeKind::Compaction, 0, 0, trace()).unwrap(),
                "compaction.2"
            );
            let _ = take_pending();
            // A retry inside the second loop keeps that loop's label.
            assert_eq!(
                arm(CaptureScopeKind::Compaction, 0, 1, trace()).unwrap(),
                "compaction.2"
            );
            let _ = take_pending();
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
            let _ = take_pending();
            assert_eq!(
                arm(CaptureScopeKind::Title, 0, 0, trace()).unwrap(),
                "title.1"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn take_pending_consumes_exactly_once() {
        let scope = test_scope(context(), noop_sink());
        scope_request(scope, async {
            arm(CaptureScopeKind::Inference, 0, 0, trace()).expect("armed");
            assert!(pending_is_armed());
            assert!(take_pending().is_some());
            assert!(!pending_is_armed());
            assert!(take_pending().is_none());
        })
        .await;
    }

    /// Outside a request there is no sink and no context, so arming is a no-op
    /// rather than an error. The fail-closed obligation is the transport's.
    #[tokio::test]
    async fn arming_without_a_scope_is_a_noop() {
        assert!(arm(CaptureScopeKind::Inference, 0, 0, trace()).is_none());
        assert!(!pending_is_armed());
        assert!(take_pending().is_none());
    }
}
