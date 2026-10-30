use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use super::controller::PendingCallMetadata;
use super::permit::AdmissionPermit;
use super::AdmissionRegistry;
use crate::request::watcher::AgentRequest;

const CANCELLED_BY_INTERRUPT_MSG: &str = "inference cancelled by request interrupt";

/// Admission refused, cancelled, or could not record a provider call.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub(crate) struct AdmissionError(pub(crate) String);

/// A provider call that did not complete under admission.
#[derive(Debug)]
pub(crate) enum AdmittedCallError<E> {
    Admission(AdmissionError),
    Provider(E),
}

/// A provider model admitted through the backend it was built for.
/// `connection` attributes calls only; after a connection change, calls
/// through this model still share the backend's pool and finish on that
/// connection.
#[derive(Clone)]
pub(crate) struct AdmittedCompletionModel<M> {
    pub(crate) inner: M,
    admission: AdmissionRegistry,
    connection: Arc<str>,
    pub(crate) model_name: Option<String>,
    pub(crate) profile_id: Option<String>,
    pub(crate) separate_reasoning: bool,
}

impl<M> AdmittedCompletionModel<M> {
    pub(crate) fn new(
        inner: M,
        admission: AdmissionRegistry,
        connection: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            inner,
            admission,
            connection: connection.into(),
            model_name: None,
            profile_id: None,
            separate_reasoning: false,
        }
    }

    /// Run one provider call under an admission permit, racing the request's
    /// interrupt token. On success the caller owns the permit's terminal.
    pub(crate) async fn admit<T, E>(
        &self,
        observe_provider_activity: bool,
        call: impl Future<Output = Result<T, E>>,
    ) -> Result<(T, AdmissionPermit), AdmittedCallError<E>>
    where
        E: std::fmt::Display,
    {
        let mut permit = self
            .admission
            .acquire_current_call(
                &self.connection,
                self.model_name.as_deref(),
                self.profile_id.as_deref(),
            )
            .await
            .map_err(AdmittedCallError::Admission)?;
        if observe_provider_activity {
            permit.observe_provider_activity(
                crate::inference::rendered_request::scope::current_attempt_activity(),
            );
        }
        let result = match current_context().ok().and_then(|c| c.inference_token) {
            Some(token) => {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        permit.mark_interrupted();
                        return Err(AdmittedCallError::Admission(AdmissionError(
                            CANCELLED_BY_INTERRUPT_MSG.into(),
                        )));
                    }
                    result = call => result,
                }
            }
            None => call.await,
        };
        match result {
            Ok(value) => Ok((value, permit)),
            Err(error) => {
                let _ = permit.finish_failure(&error.to_string()).await;
                Err(AdmittedCallError::Provider(error))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallKind {
    Inference,
    Compaction,
    #[cfg_attr(not(test), allow(dead_code))]
    Scheduled,
    OneOff,
}

impl CallKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Inference => "inference",
            Self::Compaction => "compaction",
            Self::Scheduled => "scheduled",
            Self::OneOff => "oneoff",
        }
    }
}

/// The identity of the admission call currently in flight on this task:
/// exactly what `next_call` minted for it.
#[derive(Clone, Debug)]
pub(crate) struct CurrentCallJoin {
    pub(crate) call_id: String,
    pub(crate) call_seq: i64,
    pub(crate) call_kind: CallKind,
}

#[derive(Clone)]
pub(crate) struct AdmissionCallContext {
    pub(super) request_id: String,
    pub(super) request_doc_id: String,
    pub(super) backend_id: String,
    pub(super) agent_id: String,
    pub(super) node_did: String,
    pub(super) session_id: String,
    pub(super) call_kind: CallKind,
    pub(super) attempt: i64,
    pub(super) call_seq: Arc<AtomicU64>,
    /// Written by `next_call`, read by `current_call_join()` at the transport
    /// seam. Every call scope installs its own slot so concurrent calls
    /// cannot replace each other's joins.
    pub(super) current_call: Arc<Mutex<Option<CurrentCallJoin>>>,
    pub(super) inference_token: Option<CancellationToken>,
    pub(super) terminal_failure_reason: Option<TerminalFailureReasonObserver>,
}

impl AdmissionCallContext {
    pub(crate) fn for_request(
        request: &AgentRequest,
        agent_id: impl Into<String>,
        backend_id: impl Into<String>,
    ) -> Self {
        Self {
            request_id: request.request_id.clone(),
            request_doc_id: request.doc_id.clone(),
            backend_id: backend_id.into(),
            agent_id: agent_id.into(),
            node_did: request.node_did.clone(),
            session_id: request.session_id.clone(),
            call_kind: CallKind::Inference,
            attempt: 1,
            call_seq: Arc::new(AtomicU64::new(0)),
            current_call: Arc::new(Mutex::new(None)),
            inference_token: None,
            terminal_failure_reason: None,
        }
    }

    pub(super) fn next_call(
        &self,
        runtime_instance_id: &str,
        model_name: Option<&str>,
        profile_id: Option<&str>,
    ) -> PendingCallMetadata {
        let call_seq = self.call_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let metadata = PendingCallMetadata {
            call_id: uuid::Uuid::new_v4().to_string(),
            runtime_instance_id: runtime_instance_id.to_string(),
            request_id: self.request_id.clone(),
            request_doc_id: self.request_doc_id.clone(),
            session_id: self.session_id.clone(),
            model_name: model_name.map(str::to_owned),
            profile_id: profile_id.map(str::to_owned),
            call_seq,
            backend_id: self.backend_id.clone(),
            agent_id: self.agent_id.clone(),
            node_did: self.node_did.clone(),
            call_kind: self.call_kind,
            attempt: self.attempt,
        };
        let join = CurrentCallJoin {
            call_id: metadata.call_id.clone(),
            call_seq: i64::try_from(call_seq).unwrap_or(i64::MAX),
            call_kind: self.call_kind,
        };
        match self.current_call.lock() {
            Ok(mut slot) => *slot = Some(join),
            Err(poisoned) => *poisoned.into_inner() = Some(join),
        }
        metadata
    }
}

tokio::task_local! {
    static ADMISSION_CALL_CONTEXT: AdmissionCallContext;
}

pub(crate) async fn scope_request<T>(
    context: AdmissionCallContext,
    future: impl Future<Output = T>,
) -> T {
    ADMISSION_CALL_CONTEXT.scope(context, future).await
}

/// Select an internal call's backend without changing the enclosing scope's
/// physical request identity, cancellation, call sequence or join slot.
pub(crate) async fn scope_backend<T>(backend_id: &str, future: impl Future<Output = T>) -> T {
    let mut context =
        current_context().expect("backend selection requires request admission context");
    context.backend_id = backend_id.to_owned();
    ADMISSION_CALL_CONTEXT.scope(context, future).await
}

pub(crate) async fn scope_call<T>(
    call_kind: CallKind,
    attempt: i64,
    future: impl Future<Output = T>,
) -> T {
    let context = context_for_call(call_kind, attempt);
    ADMISSION_CALL_CONTEXT.scope(context, future).await
}

pub(crate) async fn scope_call_with_join<T>(
    call_kind: CallKind,
    attempt: i64,
    future: impl Future<Output = T>,
) -> (T, Option<CurrentCallJoin>) {
    let context = context_for_call(call_kind, attempt);
    let current_call = context.current_call.clone();
    ADMISSION_CALL_CONTEXT
        .scope(context, async move {
            let output = future.await;
            let join = match current_call.lock() {
                Ok(slot) => slot.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            };
            (output, join)
        })
        .await
}

fn context_for_call(call_kind: CallKind, attempt: i64) -> AdmissionCallContext {
    let mut context = current_context().expect("admission call scope requires request context");
    context.call_kind = call_kind;
    context.attempt = attempt;
    context.current_call = Arc::new(Mutex::new(None));
    context
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn scope_call_with_token<T>(
    call_kind: CallKind,
    attempt: i64,
    token: CancellationToken,
    future: impl Future<Output = T>,
) -> T {
    let mut context = context_for_call(call_kind, attempt);
    context.inference_token = Some(token);
    ADMISSION_CALL_CONTEXT.scope(context, future).await
}

pub(crate) type TerminalFailureReasonObserver = Arc<Mutex<Option<String>>>;

pub(crate) fn terminal_failure_reason_observer() -> TerminalFailureReasonObserver {
    Arc::new(Mutex::new(None))
}

pub(crate) fn set_terminal_failure_reason(
    observer: &TerminalFailureReasonObserver,
    reason: impl Into<String>,
) {
    let reason = reason.into();
    match observer.lock() {
        Ok(mut slot) => *slot = Some(reason),
        Err(poisoned) => *poisoned.into_inner() = Some(reason),
    }
}

pub(crate) async fn scope_call_with_token_and_failure_reason<T>(
    call_kind: CallKind,
    attempt: i64,
    token: CancellationToken,
    terminal_failure_reason: TerminalFailureReasonObserver,
    future: impl Future<Output = T>,
) -> T {
    let mut context = context_for_call(call_kind, attempt);
    context.inference_token = Some(token);
    context.terminal_failure_reason = Some(terminal_failure_reason);
    ADMISSION_CALL_CONTEXT.scope(context, future).await
}

pub(super) fn current_context() -> Result<AdmissionCallContext, AdmissionError> {
    ADMISSION_CALL_CONTEXT
        .try_with(Clone::clone)
        .map_err(|_| AdmissionError("missing inference admission context".into()))
}

pub(crate) fn current_session_id() -> Option<String> {
    ADMISSION_CALL_CONTEXT
        .try_with(|context| context.session_id.clone())
        .ok()
}

/// The admission identity of the call currently in flight on this task, if
/// one has been admitted. `None` outside an admission scope and before the
/// first `next_call` of a scope.
pub(crate) fn current_call_join() -> Option<CurrentCallJoin> {
    ADMISSION_CALL_CONTEXT
        .try_with(|context| match context.current_call.lock() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        })
        .ok()
        .flatten()
}
