use gents_protocol::error_chain::{find_source, ErrorChain};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::output::TerminalOutput;
use gents_protocol::request_admission::RequestPurpose;
use tokio::sync::watch;

use super::{AgentDaemon, ShutdownDrainFailure};
use crate::admission::{self, AdmissionCallContext, CallKind};
use crate::lifecycle::{ClaimOutcome, RequestLifecycle, RequestTerminalOutcome, TerminalizeResult};
use crate::session;
use crate::streaming::DefraStreamWriter;
use crate::watcher::AgentRequest;

pub(super) mod review;

const RECENT_TITLE_LIMIT: usize = 5;
const TITLE_GENERATION_MAX_ATTEMPTS: i64 = 2;
const TITLE_GENERATION_TIMEOUT_SECS: u64 = 10;
const TITLE_VISIBLE_MAX_TOKENS: u64 = 24;

async fn flush_received_title_partial(context: &str) -> Result<()> {
    super::request::flush_received_preparation_partial(&format!("title {context}")).await
}

#[derive(Clone)]
pub(super) struct AuxiliaryTask<M>
where
    M: crate::llm::rig_compat::ProviderModel,
{
    node: Arc<EmbeddedNode>,
    agent_config: Arc<crate::config::ResolvedAgent>,
    provider_family: Option<String>,
    model: Arc<M>,
    verifier: crate::request_admission::AgentRequestAdmissionVerifier,
    capture_factory: Option<crate::rendered_request::RenderedRequestCaptureFactory>,
}

enum AuxiliaryResult {
    Generated {
        title: String,
        parent_requester_did: Option<String>,
    },
    Reviewed(String),
    Skipped,
    Interrupted,
}

impl<M> AgentDaemon<M>
where
    M: crate::llm::rig_compat::ProviderModel,
{
    pub(super) fn auxiliary_task(&self) -> AuxiliaryTask<M> {
        AuxiliaryTask {
            node: Arc::clone(&self.node),
            agent_config: Arc::clone(&self.agent_config),
            provider_family: self.provider_family.clone(),
            model: Arc::clone(&self.model),
            verifier: self.request_admission.clone(),
            capture_factory: self.rendered_request_capture_factory.clone(),
        }
    }

    pub(super) fn spawn_conversation_title_generation(&self, request: &AgentRequest) {
        if !title_generation_allowed(request.max_total_tokens) {
            tracing::debug!(
                request_id = %request.request_id,
                "skipping generated title for an aggregate-token-budgeted request"
            );
            return;
        }
        let node = Arc::clone(&self.node);
        let parent = request.clone();
        tokio::spawn(Box::pin(async move {
            let result: Result<()> = async {
                if !session::session_needs_generated_title(
                    node.as_ref(),
                    &parent.node_did,
                    parent.requester_did.as_deref(),
                    &parent.session_id,
                )
                .await?
                {
                    return Ok(());
                }
                crate::lifecycle::materialize::write_pending_auxiliary_request(
                    &node,
                    &parent,
                    RequestPurpose::TitleAudit,
                    parent.content.clone(),
                    None,
                )
                .await?;
                Ok(())
            }
            .await;
            if let Err(error) = result {
                tracing::warn!(
                    error = %error,
                    "failed to create owned conversation title request"
                );
            }
        }));
    }

    pub(super) fn spawn_auxiliary_request(
        &self,
        request: AgentRequest,
        shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        let task = self.auxiliary_task();
        self.auxiliary_tasks
            .spawn(async move { task.run(request, shutdown).await })?;
        Ok(())
    }
}

fn title_generation_allowed(max_total_tokens: Option<i64>) -> bool {
    max_total_tokens.is_none()
}

/// Reasoning-capable titles keep the configured output budget, so a reasoning
/// run is not truncated into a reasoning-only result by the visible-title cap.
fn title_max_tokens(
    additional_params: Option<&serde_json::Value>,
    configured: Option<u64>,
) -> Option<u64> {
    if crate::completion_factory::params_disable_reasoning(additional_params) {
        Some(TITLE_VISIBLE_MAX_TOKENS)
    } else {
        configured
    }
}

#[derive(Debug, thiserror::Error)]
enum AuxiliaryRequestError {
    #[error("expected access review")]
    ExpectedAccessReview,
    #[error("expected auxiliary request")]
    ExpectedAuxiliary,
    #[error("title inference timed out after {TITLE_GENERATION_TIMEOUT_SECS}s")]
    TitleTimedOut,
    #[error("auxiliary request lacks parent document")]
    ParentDocumentMissing,
    #[error("request parent request disappeared")]
    ParentDisappeared,
    #[error("request parent receipt does not match signed provenance")]
    ParentReceiptMismatch,
}

fn title_attempt_is_retryable(error: &(dyn std::error::Error + 'static)) -> bool {
    find_source::<crate::agent::loop_stream::OneShotError>(error).is_some_and(|error| {
        matches!(
            error,
            crate::agent::loop_stream::OneShotError::ProviderFailure(_)
                | crate::agent::loop_stream::OneShotError::NoVisibleOutput(_)
        )
    })
}

struct TitleGeneration<'a> {
    request_id: &'a str,
    fallback_source: &'a str,
    prompt: &'a str,
    config: crate::agent::loop_stream::LoopConfig,
    parent_requester_did: Option<String>,
}

impl<M> AuxiliaryTask<M>
where
    M: crate::llm::rig_compat::ProviderModel,
{
    async fn run(self, request: AgentRequest, shutdown: watch::Receiver<bool>) -> Result<()> {
        self.run_owned(request, shutdown, None).await.map(|_| ())
    }

    /// Interrupting a caller must cancel its audit worker without dropping
    /// received output. The daemon retains the worker's join through shutdown.
    pub(super) async fn run_review(
        self,
        request: AgentRequest,
        mut execution: review::ReviewExecution,
        shutdown: watch::Receiver<bool>,
        tasks: &super::auxiliary_tasks::AuxiliaryTasks,
    ) -> Result<Option<String>> {
        if request.purpose != RequestPurpose::AccessReview {
            return Err(AuxiliaryRequestError::ExpectedAccessReview.into());
        }
        let cancellation = execution.cancellation.child_token();
        let guard = cancellation.clone().drop_guard();
        execution.cancellation = cancellation;
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        tasks.spawn(async move {
            let result = self
                .run_owned(request, shutdown, Some(execution))
                .await
                .map_err(|error| review::ReviewFailure(Arc::new(error)));
            let reply = match &result {
                Ok(reply) => Ok(reply.clone()),
                Err(error) => Err(error.clone()),
            };
            let _ = reply_tx.send(reply);
            result.map(|_| ()).map_err(anyhow::Error::new)
        })?;
        let result = reply_rx
            .await
            .map_err(|error| ShutdownDrainFailure {
                reason: format!(
                    "access review worker stopped before confirming output retention: {error}"
                ),
            })?
            .map_err(anyhow::Error::new);
        guard.disarm();
        result
    }

    async fn run_owned(
        self,
        request: AgentRequest,
        shutdown: watch::Receiver<bool>,
        review: Option<review::ReviewExecution>,
    ) -> Result<Option<String>> {
        if !request.purpose.is_auxiliary() {
            return Err(AuxiliaryRequestError::ExpectedAuxiliary.into());
        }
        let Some(request) = super::verify_request_at_claim_boundary(
            &self.verifier,
            Arc::clone(&self.node),
            &self.agent_config.agent_id,
            request,
        )
        .await
        else {
            return Ok(None);
        };
        let origin =
            crate::lifecycle::ExecutionOrigin::from_persisted(request.execution_origin.as_deref())
                .context("admitted auxiliary request is missing execution origin")?;
        let mut lifecycle = RequestLifecycle::new_with_execution_binding(
            Arc::clone(&self.node),
            &self.agent_config.agent_id,
            self.agent_config.node_did(),
            request.clone(),
            self.agent_config.deadline_duration.as_secs(),
            origin,
            self.agent_config.backend_id.clone().unwrap_or_default(),
        );
        lifecycle.set_execution_lease_duration(self.agent_config.stream_liveness_timeout);
        lifecycle.set_configured_max_total_tokens(self.agent_config.max_total_tokens);
        match lifecycle.claim_with_identity().await {
            Ok(ClaimOutcome::Claimed) => {}
            Ok(ClaimOutcome::Queued | ClaimOutcome::Interrupted | ClaimOutcome::Expired) => {
                return Ok(None)
            }
            Err(error) if crate::lifecycle::is_claim_admission_error(&error) => {
                lifecycle.reject_admission(&error.to_string()).await?;
                return Ok(None);
            }
            Err(error) => {
                return Err(anyhow::Error::from(error).context("claiming auxiliary request"))
            }
        }
        let writer = DefraStreamWriter::new(
            Arc::clone(&self.node),
            self.agent_config.node_did(),
            Duration::from_millis(self.agent_config.stream_batch_ms),
        );
        let title_request = lifecycle.request().clone();
        let (interrupt_tx, interrupt_rx) = watch::channel(None);
        let _observer = tokio_util::task::AbortOnDropHandle::new(
            crate::interrupt::spawn_request_interrupt_observer(
                self.node.clone(),
                title_request.doc_id.clone(),
                interrupt_tx,
                shutdown.clone(),
            ),
        );
        let result = self
            .execute(
                &mut lifecycle,
                &writer,
                title_request.clone(),
                shutdown,
                interrupt_rx,
                review,
            )
            .await;
        if result
            .as_ref()
            .err()
            .is_some_and(|error| ShutdownDrainFailure::occurs_in(error.as_ref()))
        {
            return result.map(|_| None);
        }
        let (outcome, title, reply, reason) = match result {
            Ok(AuxiliaryResult::Generated {
                title,
                parent_requester_did,
            }) => (
                RequestTerminalOutcome::Completed,
                Some((title, parent_requester_did)),
                None,
                None,
            ),
            Ok(AuxiliaryResult::Reviewed(reply)) => {
                (RequestTerminalOutcome::Completed, None, Some(reply), None)
            }
            Ok(AuxiliaryResult::Skipped) => (RequestTerminalOutcome::Completed, None, None, None),
            Ok(AuxiliaryResult::Interrupted) => {
                (RequestTerminalOutcome::Interrupted, None, None, None)
            }
            Err(error) => (RequestTerminalOutcome::Failed, None, None, Some(error)),
        };
        let reason_text = reason.as_ref().map(ToString::to_string);
        let terminal = lifecycle
            .terminalize_owned(outcome, TerminalOutput::NoMessage, reason_text.as_deref())
            .await;
        let terminal = match terminal {
            Ok(value) => value,
            Err(terminal_error) => {
                if let Some(error) = reason {
                    return Err(anyhow::Error::from(terminal_error).context(format!(
                        "auxiliary request failed first: {}",
                        ErrorChain(error.as_ref())
                    )));
                }
                return Err(terminal_error.into());
            }
        };
        if let Some(error) = reason {
            return Err(error);
        }
        if terminal == TerminalizeResult::Won {
            if let Some((title, parent_requester_did)) = title {
                if let Err(error) = session::update_session_title_with_source(
                    &self.node,
                    &title_request.node_did,
                    parent_requester_did.as_deref(),
                    &title_request.session_id,
                    &title,
                    gents_protocol::session::SessionTitleSource::Generated,
                )
                .await
                {
                    tracing::warn!(%error, request_id = %title_request.request_id, "title audit completed but optional session title update failed");
                }
            }
        }
        Ok((terminal == TerminalizeResult::Won)
            .then_some(reply)
            .flatten())
    }

    async fn execute(
        &self,
        lifecycle: &mut RequestLifecycle,
        writer: &DefraStreamWriter,
        request: AgentRequest,
        shutdown: watch::Receiver<bool>,
        mut interrupt_rx: watch::Receiver<Option<crate::interrupt::InterruptIntent>>,
        review: Option<review::ReviewExecution>,
    ) -> Result<AuxiliaryResult> {
        let Some(parent) = super::request::prepare_unless_interrupted(
            &mut interrupt_rx,
            load_auxiliary_parent(&self.node, &request),
        )
        .await
        else {
            return Ok(AuxiliaryResult::Interrupted);
        };
        let parent = parent?;
        let commit_cid = lifecycle
            .request_commit_cid()
            .context("claimed auxiliary request has no exact commit CID")?;
        let generation = lifecycle.execution_generation()?.to_owned();
        let capture_context = crate::rendered_request::context_for_claimed_request(
            &request,
            commit_cid,
            self.agent_config.model_name.clone(),
            self.provider_family.clone(),
        );
        let mut capture_scope = crate::rendered_request::scope_from_factory(
            capture_context,
            self.capture_factory.as_ref(),
        )
        .context("auxiliary request has no durable provider capture authority")?;
        Arc::get_mut(&mut capture_scope)
            .context("new auxiliary capture scope is already shared")?
            .set_auxiliary_output_sink(writer.auxiliary_output_sink(
                request.clone(),
                generation,
                crate::provider_input::ProviderInputProfile::resolve(
                    self.agent_config.backend_provider_kind,
                    self.agent_config.openai_wire_api,
                ),
            ));
        let admission_context = AdmissionCallContext::for_request(
            &request,
            &self.agent_config.agent_id,
            self.agent_config.backend_id.clone().unwrap_or_default(),
        );
        admission::scope_request(
            admission_context,
            crate::rendered_request::scope::scope_request(capture_scope, async {
                lifecycle.begin_owned_execution(writer).await?;
                if request.purpose == RequestPurpose::AccessReview {
                    return self
                        .execute_review(&request, &parent, review, shutdown, &mut interrupt_rx)
                        .await;
                }
                let preparation = async {
                    if !session::session_needs_generated_title(
                        self.node.as_ref(),
                        &request.node_did,
                        parent.requester_did.as_deref(),
                        &request.session_id,
                    )
                    .await?
                    {
                        return Ok(None);
                    }
                    let recent = session::load_recent_titles_for_agent(
                        self.node.as_ref(),
                        &request.node_did,
                        &request.session_id,
                        RECENT_TITLE_LIMIT,
                    )
                    .await
                    .unwrap_or_default();
                    let source = crate::session::title_text::generation_input(
                        &parent.content,
                        &parent.input.captured_context,
                    );
                    let prompt = title_generation_prompt(&source, &recent);
                    let mut config = crate::completion_factory::loop_config(
                        &self.agent_config,
                        title_generation_preamble(),
                        0,
                        crate::rendered_request::CaptureScopeKind::Title,
                    );
                    config.temperature = Some(0.0);
                    config.max_tokens =
                        title_max_tokens(config.additional_params.as_ref(), config.max_tokens);
                    config.max_turns = 1;
                    config.retry_policy =
                        crate::agent::completion_retry::CompletionRetryPolicy::no_retry();
                    config.aggregate_token_budget =
                        crate::completion_factory::aggregate_token_budget_for_request(
                            self.node.as_ref(),
                            &request,
                        )
                        .await?;
                    Ok::<_, anyhow::Error>(Some((prompt, config, source)))
                };
                let Some(prepared) =
                    super::request::prepare_unless_interrupted(&mut interrupt_rx, preparation)
                        .await
                else {
                    return Ok(AuxiliaryResult::Interrupted);
                };
                let Some((prompt, config, source)) = prepared? else {
                    return Ok(AuxiliaryResult::Skipped);
                };
                self.generate_with_fallback(
                    TitleGeneration {
                        request_id: &request.request_id,
                        fallback_source: &source,
                        prompt: &prompt,
                        config,
                        parent_requester_did: parent.requester_did,
                    },
                    shutdown,
                    &mut interrupt_rx,
                )
                .await
            }),
        )
        .await
    }

    async fn generate_with_fallback(
        &self,
        generation: TitleGeneration<'_>,
        mut shutdown: watch::Receiver<bool>,
        interrupt_rx: &mut watch::Receiver<Option<crate::interrupt::InterruptIntent>>,
    ) -> Result<AuxiliaryResult> {
        let TitleGeneration {
            request_id,
            fallback_source,
            prompt,
            config,
            parent_requester_did,
        } = generation;
        let mut last_error = None;
        for attempt in 1..=TITLE_GENERATION_MAX_ATTEMPTS {
            if *shutdown.borrow() {
                return Ok(AuxiliaryResult::Interrupted);
            }
            let run = admission::scope_call(
                CallKind::OneOff,
                attempt,
                Box::pin(crate::agent::loop_stream::run_loop_to_text::<M>(
                    (*self.model).clone(),
                    crate::llm::message::Message::user(prompt.to_string()),
                    Vec::new(),
                    Arc::new(Vec::new()),
                    config.clone(),
                )),
            );
            let result = tokio::select! {
                biased;
                _ = async {
                    let _ = shutdown.wait_for(|value| *value).await;
                } => {
                    flush_received_title_partial("shutdown").await?;
                    return Ok(AuxiliaryResult::Interrupted);
                }
                result = super::request::prepare_unless_interrupted(
                    interrupt_rx,
                    tokio::time::timeout(Duration::from_secs(TITLE_GENERATION_TIMEOUT_SECS), run),
                ) => match result {
                    Some(result) => result,
                    None => {
                        flush_received_title_partial("interrupt").await?;
                        return Ok(AuxiliaryResult::Interrupted);
                    }
                },
            };
            match result {
                Ok(Ok(raw)) => {
                    return Ok(AuxiliaryResult::Generated {
                        title: sanitize_generated_title(&raw, fallback_source),
                        parent_requester_did,
                    })
                }
                Ok(Err(error)) => {
                    flush_received_title_partial("provider error").await?;
                    let error = anyhow::Error::new(error);
                    if !title_attempt_is_retryable(error.as_ref()) {
                        return Err(error.context("title audit provider/output invariant failed"));
                    }
                    last_error = Some(error);
                }
                Err(_) => {
                    flush_received_title_partial("provider timeout").await?;
                    last_error = Some(AuxiliaryRequestError::TitleTimedOut.into());
                }
            }
            tracing::warn!(request_id, attempt, error = %last_error.as_ref().expect("failed attempt has error"), "title inference failed");
        }
        let fallback = sanitize_generated_title("", fallback_source);
        tracing::info!(request_id, title = %fallback, error = ?last_error.map(|error| error.to_string()), "using fallback conversation title after provider failures");
        Ok(AuxiliaryResult::Generated {
            title: fallback,
            parent_requester_did,
        })
    }
}

async fn load_auxiliary_parent(
    node: &EmbeddedNode,
    request: &AgentRequest,
) -> Result<AgentRequest> {
    let parent_doc_id = request
        .caused_by_parent_request_doc_id
        .as_deref()
        .ok_or(AuxiliaryRequestError::ParentDocumentMissing)?;
    let escaped = crate::graphql::escape_graphql_string(parent_doc_id);
    let query = format!(
        "{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{escaped}\" }} }}, limit: 1) {{ {} }} }}",
        crate::request_admission::SIGNED_REQUEST_FIELDS
    );
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "load auxiliary parent receipt",
    )
    .await?;
    let row: gents_protocol::row::AgentRequestRow =
        crate::graphql::first_row(&response, "AgentRequest")?
            .ok_or(AuxiliaryRequestError::ParentDisappeared)?;
    crate::request_admission::verify_request_receipt_signature(&row)?;
    let parent = AgentRequest::try_from(row)?;
    if !(parent.purpose == RequestPurpose::Normal
        && parent.doc_id == parent_doc_id
        && request.caused_by_parent_request_id.as_deref() == Some(parent.request_id.as_str())
        && parent.node_did == request.node_did
        && parent.session_id == request.session_id
        && parent.agent_id == request.agent_id)
    {
        return Err(AuxiliaryRequestError::ParentReceiptMismatch.into());
    }
    Ok(parent)
}

pub(super) fn title_generation_preamble() -> String {
    crate::session::title_text::GENERATION_PREAMBLE.to_owned()
}

fn title_generation_prompt(request_content: &str, recent_titles: &[String]) -> String {
    crate::session::title_text::generation_prompt(request_content, recent_titles)
}

fn sanitize_generated_title(raw_title: &str, fallback_source: &str) -> String {
    crate::session::title_text::generated_title(raw_title, fallback_source)
}

#[cfg(test)]
mod owned_tests;
