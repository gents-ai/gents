use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rig::agent::Agent;
use rig::completion::CompletionModel;

mod inference;
mod request;

use super::runtime::StartupBarrier;
use crate::backend_registry::BackendTracker;
use crate::compaction::{CompactionOptions, DefraCompactor};
use crate::config::ProfileConfig;
use crate::hook::FailurePolicy;
use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
use crate::prompt::LayeredPromptBuilder;
use crate::retry::RetryPolicy;
use crate::streaming::DefraStreamWriter;
use crate::watcher::{DefraWatcher, Watcher};

pub(super) struct ProfileDaemon<M: CompletionModel> {
    node: Arc<defra_node::EmbeddedNode>,
    profile: Arc<ProfileConfig>,
    agent: Agent<M>,
    watcher: DefraWatcher,
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

impl<M: CompletionModel + 'static> ProfileDaemon<M> {
    pub(super) fn new(
        node: Arc<defra_node::EmbeddedNode>,
        profile: Arc<ProfileConfig>,
        agent: Agent<M>,
        backend_tracker: Arc<BackendTracker>,
        retry_policy: RetryPolicy,
        hook_failure_policy: FailurePolicy,
        startup_barrier: Arc<StartupBarrier>,
    ) -> Self {
        let watcher = DefraWatcher::new(node.clone(), profile.did());
        let prompt_builder = LayeredPromptBuilder::from_profile(profile.as_ref());
        let stream_writer = DefraStreamWriter::new(
            node.clone(),
            profile.did(),
            Duration::from_millis(profile.stream_batch_ms),
        );
        let compactor = DefraCompactor::new(agent.clone());
        let compaction_options = CompactionOptions {
            threshold: profile.compaction_threshold,
            strategy: profile.compaction_strategy.clone(),
            ..Default::default()
        };

        Self {
            node,
            profile,
            agent,
            watcher,
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
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        tracing::info!(
            profile = %self.profile.name,
            did = %self.profile.did(),
            model = %self.profile.model_name,
            context_window = self.profile.context_window,
            "defra-agent profile started"
        );

        match RequestLifecycle::recover_all(&self.node, self.profile.did()).await {
            Ok(report) => {
                if report.requests_recovered > 0 {
                    tracing::info!(profile = %self.profile.name, count = report.requests_recovered, "recovered stuck requests");
                }
                if report.responses_recovered > 0 {
                    tracing::info!(profile = %self.profile.name, count = report.responses_recovered, "recovered stuck responses");
                }
                if report.conversations_recovered > 0 {
                    tracing::info!(profile = %self.profile.name, count = report.conversations_recovered, "recovered stuck conversations");
                }
            }
            Err(error) => {
                tracing::warn!(profile = %self.profile.name, error = %error, "startup recovery failed");
            }
        }

        self.startup_barrier
            .mark_profile_ready(&self.profile.name)
            .await;
        tracing::info!(
            profile = %self.profile.name,
            did = %self.profile.did(),
            "defra-agent profile ready"
        );

        loop {
            let request = tokio::select! {
                biased;

                _ = shutdown.changed() => {
                    tracing::info!(profile = %self.profile.name, "shutdown signal received");
                    return Ok(());
                }

                req = self.watcher.next_request() => {
                    match req {
                        Some(Ok(req)) => req,
                        Some(Err(error)) => {
                            tracing::error!(profile = %self.profile.name, error = %error, "watcher error, retrying");
                            continue;
                        }
                        None => return Ok(()),
                    }
                }
            };

            let mut lifecycle = RequestLifecycle::new_with_execution_binding(
                self.node.clone(),
                &self.profile.name,
                self.profile.did(),
                request.clone(),
                self.profile.deadline_duration.as_secs(),
                crate::lifecycle::ExecutionOrigin::Interactive,
                self.profile.backend_id.clone().unwrap_or_default(),
            );

            match lifecycle.claim_with_identity().await {
                Ok(ClaimOutcome::Claimed) => {}
                Ok(ClaimOutcome::Superseded) => {
                    tracing::info!(
                        profile = %self.profile.name,
                        request_id = %request.request_id,
                        session_id = %request.session_id,
                        "request superseded by an earlier non-terminal request"
                    );
                    continue;
                }
                Err(error) => {
                    tracing::warn!(
                        profile = %self.profile.name,
                        request_id = %request.request_id,
                        error = %error,
                        "failed to claim request"
                    );
                    continue;
                }
            }

            match self.handle_request(&mut lifecycle).await {
                Ok(HandleRequestOutcome::Completed) => {
                    let _ = lifecycle.complete().await;
                }
                Ok(HandleRequestOutcome::FailedAfterResponse(error)) => {
                    tracing::error!(
                        profile = %self.profile.name,
                        request_id = %request.request_id,
                        error = %error,
                        "request failed after response started"
                    );
                    let _ = lifecycle.fail().await;
                }
                Err(error) => {
                    tracing::error!(
                        profile = %self.profile.name,
                        request_id = %request.request_id,
                        error = %error,
                        "request handling failed"
                    );
                    let _ = lifecycle.fail().await;
                    if !lifecycle.response_exists().await.unwrap_or(false) {
                        if let Err(stream_error) = self.write_error_response(&request, &error).await
                        {
                            tracing::error!(
                                profile = %self.profile.name,
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
