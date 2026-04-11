use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rig::agent::Agent;
use rig::completion::CompletionModel;
use tokio::sync::{mpsc, Mutex};

mod inference;
mod request;

use super::runtime::StartupBarrier;
use crate::backend_registry::BackendTracker;
use crate::compaction::{CompactionOptions, DefraCompactor};
use crate::config::BehaviorConfig;
use crate::hook::FailurePolicy;
use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
use crate::prompt::LayeredPromptBuilder;
use crate::retry::RetryPolicy;
use crate::streaming::DefraStreamWriter;
use crate::watcher::AgentRequest;

pub(super) struct BehaviorDaemon<M: CompletionModel> {
    node: Arc<defra_node::EmbeddedNode>,
    behavior: Arc<BehaviorConfig>,
    agent: Agent<M>,
    backend_tracker: Arc<BackendTracker>,
    prompt_builder: LayeredPromptBuilder,
    stream_writer: DefraStreamWriter,
    compactor: DefraCompactor<M>,
    compaction_options: CompactionOptions,
    retry_policy: RetryPolicy,
    hook_failure_policy: FailurePolicy,
    startup_barrier: Arc<StartupBarrier>,
}

enum HandleRequestOutcome {
    Completed,
    FailedAfterResponse(anyhow::Error),
}

impl<M: CompletionModel + 'static> BehaviorDaemon<M> {
    pub(super) fn new(
        node: Arc<defra_node::EmbeddedNode>,
        behavior: Arc<BehaviorConfig>,
        agent: Agent<M>,
        prompt_builder: LayeredPromptBuilder,
        backend_tracker: Arc<BackendTracker>,
        retry_policy: RetryPolicy,
        hook_failure_policy: FailurePolicy,
        startup_barrier: Arc<StartupBarrier>,
    ) -> Self {
        let stream_writer = DefraStreamWriter::new(
            node.clone(),
            behavior.did(),
            Duration::from_millis(behavior.stream_batch_ms),
        );
        let compactor = DefraCompactor::new(agent.clone());
        let compaction_options = CompactionOptions {
            threshold: behavior.compaction_threshold,
            strategy: behavior.compaction_strategy.clone(),
            ..Default::default()
        };

        Self {
            node,
            behavior,
            agent,
            backend_tracker,
            prompt_builder,
            stream_writer,
            compactor,
            compaction_options,
            retry_policy,
            hook_failure_policy,
            startup_barrier,
        }
    }

    pub(super) async fn run(
        &mut self,
        request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        tracing::info!(
            behavior_id = %self.behavior.name,
            did = %self.behavior.did(),
            model = %self.behavior.model_name,
            context_window = self.behavior.context_window,
            "defra-agent behavior started"
        );

        self.startup_barrier
            .mark_behavior_ready(&self.behavior.name)
            .await;
        tracing::info!(
            behavior_id = %self.behavior.name,
            did = %self.behavior.did(),
            "defra-agent behavior executor online"
        );

        loop {
            let request = tokio::select! {
                biased;

                _ = shutdown.changed() => {
                    tracing::info!(behavior_id = %self.behavior.name, "shutdown signal received");
                    return Ok(());
                }

                req = async {
                    let mut receiver = request_rx.lock().await;
                    receiver.recv().await
                } => {
                    match req {
                        Some(req) => req,
                        None => return Ok(()),
                    }
                }
            };

            let mut lifecycle = RequestLifecycle::new_with_execution_binding(
                self.node.clone(),
                &self.behavior.name,
                self.behavior.did(),
                request.clone(),
                self.behavior.deadline_duration.as_secs(),
                crate::lifecycle::ExecutionOrigin::Interactive,
                self.behavior.backend_id.clone().unwrap_or_default(),
            );

            match lifecycle.claim_with_identity().await {
                Ok(ClaimOutcome::Claimed) => {}
                Ok(ClaimOutcome::Superseded) => {
                    tracing::info!(
                        behavior_id = %self.behavior.name,
                        request_id = %request.request_id,
                        session_id = %request.session_id,
                        "request superseded by an earlier non-terminal request"
                    );
                    continue;
                }
                Err(error) => {
                    tracing::warn!(
                        behavior_id = %self.behavior.name,
                        request_id = %request.request_id,
                        error = %error,
                        "failed to claim request"
                    );
                    continue;
                }
            }

            if let Some(requested_behavior_id) = request
                .behavior_id
                .as_deref()
                .map(str::trim)
                .filter(|behavior_id| !behavior_id.is_empty())
            {
                if requested_behavior_id != self.behavior.name {
                    let error = anyhow::anyhow!(
                        "request targets behavior {} but runtime is serving behavior {}",
                        requested_behavior_id,
                        self.behavior.name
                    );
                    tracing::warn!(
                        behavior_id = %self.behavior.name,
                        request_id = %request.request_id,
                        session_id = %request.session_id,
                        requested_behavior_id = %requested_behavior_id,
                        "rejecting request for unroutable behavior"
                    );
                    let _ = lifecycle.record_failure_reason(&error.to_string()).await;
                    let _ = lifecycle.fail().await;
                    if !lifecycle.response_exists().await.unwrap_or(false) {
                        if let Err(stream_error) = self
                            .write_error_response(&request, lifecycle.behavior_id(), &error)
                            .await
                        {
                            tracing::error!(
                                behavior_id = %self.behavior.name,
                                error = %stream_error,
                                "failed to write behavior-mismatch response"
                            );
                        }
                    }
                    continue;
                }
            }

            if let Err(error) = lifecycle.prepare_session_with_identity().await {
                tracing::error!(
                    behavior_id = %self.behavior.name,
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    behavior_id = %lifecycle.behavior_id(),
                    error = %error,
                    "failed to prepare behavior-pinned session"
                );
                let _ = lifecycle.record_failure_reason(&error.to_string()).await;
                let _ = lifecycle.fail().await;
                if !lifecycle.response_exists().await.unwrap_or(false) {
                    if let Err(stream_error) = self
                        .write_error_response(&request, lifecycle.behavior_id(), &error)
                        .await
                    {
                        tracing::error!(
                            behavior_id = %self.behavior.name,
                            error = %stream_error,
                            "failed to write session-preparation response"
                        );
                    }
                }
                continue;
            }

            match self.handle_request(&mut lifecycle).await {
                Ok(HandleRequestOutcome::Completed) => {
                    let _ = lifecycle.complete().await;
                }
                Ok(HandleRequestOutcome::FailedAfterResponse(error)) => {
                    tracing::error!(
                        behavior_id = %self.behavior.name,
                        request_id = %request.request_id,
                        error = %error,
                        "request failed after response started"
                    );
                    let _ = lifecycle.record_failure_reason(&error.to_string()).await;
                    let _ = lifecycle.fail().await;
                }
                Err(error) => {
                    tracing::error!(
                        behavior_id = %self.behavior.name,
                        request_id = %request.request_id,
                        error = %error,
                        "request handling failed"
                    );
                    let _ = lifecycle.record_failure_reason(&error.to_string()).await;
                    let _ = lifecycle.fail().await;
                    if !lifecycle.response_exists().await.unwrap_or(false) {
                        if let Err(stream_error) = self
                            .write_error_response(&request, lifecycle.behavior_id(), &error)
                            .await
                        {
                            tracing::error!(
                                behavior_id = %self.behavior.name,
                                error = %stream_error,
                                "failed to write error response"
                            );
                        }
                    }
                }
            }
        }
    }
}
