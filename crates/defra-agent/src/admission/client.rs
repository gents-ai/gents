use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rig::client::CompletionClient;
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::streaming::StreamingCompletionResponse;
use tokio_util::sync::CancellationToken;

use super::controller::PendingCallMetadata;
use super::stream_guard::hold_stream_guard;
use super::AdmissionRegistry;
use crate::watcher::AgentRequest;

#[derive(Clone)]
pub(crate) struct AdmittedCompletionClient<C> {
    inner: C,
    admission: AdmissionRegistry,
}

impl<C> AdmittedCompletionClient<C> {
    pub(crate) fn new(inner: C, admission: AdmissionRegistry) -> Self {
        Self { inner, admission }
    }
}

impl<C> CompletionClient for AdmittedCompletionClient<C>
where
    C: CompletionClient,
    C::CompletionModel: 'static,
    <C::CompletionModel as CompletionModel>::Response: 'static,
    <C::CompletionModel as CompletionModel>::StreamingResponse: 'static,
{
    type CompletionModel = AdmittedCompletionModel<C::CompletionModel>;
}

#[derive(Clone)]
pub(crate) struct AdmittedCompletionModel<M> {
    inner: M,
    admission: AdmissionRegistry,
}

impl<M> CompletionModel for AdmittedCompletionModel<M>
where
    M: CompletionModel + 'static,
    M::Response: 'static,
    M::StreamingResponse: 'static,
{
    type Response = M::Response;
    type StreamingResponse = M::StreamingResponse;
    type Client = AdmittedCompletionClient<M::Client>;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        Self {
            inner: M::make(&client.inner, model),
            admission: client.admission.clone(),
        }
    }

    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        let mut permit = self.admission.acquire_current_call().await?;
        let token = current_context().ok().and_then(|c| c.inference_token);
        match token {
            Some(token) => {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        permit.mark_interrupted();
                        Err(CompletionError::ProviderError(
                            "inference cancelled by request interrupt".into(),
                        ))
                    }
                    result = self.inner.completion(request) => match result {
                        Ok(response) => {
                            permit.finish_success(Some(response.usage)).await;
                            Ok(response)
                        }
                        Err(error) => {
                            permit.finish_failure(&error.to_string()).await;
                            Err(error)
                        }
                    }
                }
            }
            None => match self.inner.completion(request).await {
                Ok(response) => {
                    permit.finish_success(Some(response.usage)).await;
                    Ok(response)
                }
                Err(error) => {
                    permit.finish_failure(&error.to_string()).await;
                    Err(error)
                }
            },
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        let mut permit = self.admission.acquire_current_call().await?;
        let token = current_context().ok().and_then(|c| c.inference_token);
        // NOTE: the token here only covers *pre-stream* cancellation (i.e.
        // cancellation observed before the HTTP request returns and the
        // stream handle is produced). Once `hold_stream_guard` takes
        // ownership of the permit below, mid-stream interrupts are handled
        // at the daemon level (see `run_inference`'s select arms): the
        // daemon drops the stream, which fires the permit's `Drop` with
        // the default "failed" terminal. Promoting mid-stream interrupts
        // to `cancelled` would require teaching the stream guard itself
        // to observe this token and is out of scope for Task 9.
        match token {
            Some(token) => {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        permit.mark_interrupted();
                        Err(CompletionError::ProviderError(
                            "inference cancelled by request interrupt".into(),
                        ))
                    }
                    result = self.inner.stream(request) => match result {
                        Ok(stream) => Ok(hold_stream_guard(stream, permit)),
                        Err(error) => {
                            permit.finish_failure(&error.to_string()).await;
                            Err(error)
                        }
                    }
                }
            }
            None => match self.inner.stream(request).await {
                Ok(stream) => Ok(hold_stream_guard(stream, permit)),
                Err(error) => {
                    permit.finish_failure(&error.to_string()).await;
                    Err(error)
                }
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallKind {
    Inference,
    Compaction,
    Scheduled,
}

impl CallKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Inference => "inference",
            Self::Compaction => "compaction",
            Self::Scheduled => "scheduled",
        }
    }
}

#[derive(Clone)]
pub(crate) struct AdmissionCallContext {
    pub(super) request_id: String,
    pub(super) backend_id: String,
    pub(super) behavior_id: String,
    pub(super) agent_did: String,
    pub(super) call_kind: CallKind,
    pub(super) attempt: i64,
    pub(super) call_seq: Arc<AtomicU64>,
    /// Cancellation token tied to the request's lifecycle. When cancelled,
    /// the `AdmittedCompletionModel` races the inner call against it and
    /// calls `permit.mark_interrupted()` before returning a cancelled
    /// error. `None` means no cancellation observation (e.g. one-shot CLI
    /// calls without a daemon-side interrupt observer).
    pub(super) inference_token: Option<CancellationToken>,
}

impl AdmissionCallContext {
    pub(crate) fn for_request(
        request: &AgentRequest,
        behavior_id: impl Into<String>,
        backend_id: impl Into<String>,
    ) -> Self {
        Self {
            request_id: request.request_id.clone(),
            backend_id: backend_id.into(),
            behavior_id: behavior_id.into(),
            agent_did: request.agent_did.clone(),
            call_kind: CallKind::Inference,
            attempt: 1,
            call_seq: Arc::new(AtomicU64::new(0)),
            inference_token: None,
        }
    }

    pub(super) fn next_call(&self, runtime_instance_id: &str) -> PendingCallMetadata {
        let call_seq = self.call_seq.fetch_add(1, Ordering::SeqCst) + 1;
        PendingCallMetadata {
            call_id: uuid::Uuid::new_v4().to_string(),
            runtime_instance_id: runtime_instance_id.to_string(),
            request_id: self.request_id.clone(),
            call_seq,
            backend_id: self.backend_id.clone(),
            behavior_id: self.behavior_id.clone(),
            agent_did: self.agent_did.clone(),
            call_kind: self.call_kind,
            attempt: self.attempt,
        }
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

pub(crate) async fn scope_call<T>(
    call_kind: CallKind,
    attempt: i64,
    future: impl Future<Output = T>,
) -> T {
    let mut context = current_context().expect("admission call scope requires request context");
    context.call_kind = call_kind;
    context.attempt = attempt;
    ADMISSION_CALL_CONTEXT.scope(context, future).await
}

/// Like `scope_call`, but also attaches a cancellation token that the
/// `AdmittedCompletionModel` observes during the inner completion/stream
/// call. When the token cancels, the permit is marked as interrupted so
/// the `InferenceCall` row lands as `cancelled` rather than `failed`.
pub(crate) async fn scope_call_with_token<T>(
    call_kind: CallKind,
    attempt: i64,
    token: CancellationToken,
    future: impl Future<Output = T>,
) -> T {
    let mut context = current_context().expect("admission call scope requires request context");
    context.call_kind = call_kind;
    context.attempt = attempt;
    context.inference_token = Some(token);
    ADMISSION_CALL_CONTEXT.scope(context, future).await
}

pub(super) fn current_context() -> Result<AdmissionCallContext, CompletionError> {
    ADMISSION_CALL_CONTEXT
        .try_with(Clone::clone)
        .map_err(|_| CompletionError::ProviderError("missing inference admission context".into()))
}
