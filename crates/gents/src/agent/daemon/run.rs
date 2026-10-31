use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{mpsc, Mutex};
use tracing::Instrument;

use super::{auxiliary_task_join_result, AgentDaemon};
use crate::agent::worker_capacity::{RequestGuard, WorkerCapacity};
use crate::inference::llm::rig_compat::ProviderModel;
use crate::observe::runtime_trace::RequestTraceAttrs;
use crate::request::watcher::AgentRequest;

impl<M> AgentDaemon<M>
where
    M: ProviderModel,
{
    pub(in crate::agent) async fn run(
        &mut self,
        request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
        capacity: Arc<WorkerCapacity>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        tracing::info!(
            agent_id = %self.agent_config.agent_id,
            did = %self.agent_config.node_did(),
            model = %self.agent_config.model_name,
            context_window = self.agent_config.context_window,
            "gents agent started"
        );

        if self
            .runtime_status
            .readiness()
            .mark_slot_ready(&self.agent_config.agent_id, self.slot_generation)
            .await?
        {
            self.startup_barrier
                .mark_agent_ready(&self.agent_config.agent_id, self.slot_generation)
                .await;
        }
        tracing::info!(
            agent_id = %self.agent_config.agent_id,
            did = %self.agent_config.node_did(),
            "gents agent executor online"
        );

        let auxiliary_tasks = self.auxiliary_tasks.clone();
        let run_result: Result<()> = loop {
            if *shutdown.borrow() {
                break Ok(());
            }
            let request = tokio::select! {
                biased;

                _ = shutdown.changed() => {
                    tracing::info!(agent_id = %self.agent_config.agent_id, "shutdown signal received");
                    break Ok(());
                }

                Some(joined) = auxiliary_tasks.join_next() => {
                    if let Err(error) = auxiliary_task_join_result(joined) {
                        break Err(error);
                    }
                    continue;
                }

                req = async {
                    let mut receiver = request_rx.lock().await;
                    receiver.recv().await
                } => {
                    match req {
                        Some(req) => req,
                        None => break Ok(()),
                    }
                }
            };

            if request.purpose.is_auxiliary() {
                if let Err(error) = self.spawn_auxiliary_request(request, shutdown.clone()) {
                    break Err(error);
                }
                continue;
            }

            let trace_attrs = RequestTraceAttrs::from_request(&request);
            let agent_id = self.agent_config.agent_id.clone();
            let backend_id = self.agent_config.backend_id.clone().unwrap_or_default();

            let cancellation = tokio_util::sync::CancellationToken::new();
            let guard = tokio::select! {
                biased;
                _ = shutdown.changed() => break Ok(()),
                guard = capacity.acquire_unbound(&cancellation) => guard,
            };
            let active_guard = match guard {
                Ok(guard) => Some(RequestGuard::Unbound(guard)),
                Err(error) => {
                    tracing::warn!(agent_id, error = %crate::failure_text::BoundedErrorChain(&error), "request worker capacity admission stopped");
                    break Ok(());
                }
            };

            let process = self
                .process_request_with_capacity(request, active_guard, shutdown.clone())
                .instrument(tracing::info_span!(
                    "agent.request",
                    request_doc_id = %trace_attrs.request_doc_id,
                    request_id = %trace_attrs.request_id,
                    session_id = %trace_attrs.session_id,
                    node_did = %trace_attrs.node_did,
                    agent_id = %agent_id,
                    requested_agent_id = %trace_attrs.requested_agent_id,
                    backend_id = %backend_id,
                    execution_origin = %trace_attrs.execution_origin,
                    persisted_execution_origin = %trace_attrs.execution_origin,
                    deadline_at = %trace_attrs.deadline_at,
                    has_deadline = trace_attrs.has_deadline,
                    request_hop = trace_attrs.request_hop,
                    parent_request_id = %trace_attrs.parent_request_id,
                    parent_tool_call_id = %trace_attrs.parent_tool_call_id,
                    selected_skill_count = trace_attrs.selected_skill_count,
                    workspace_cwd_set = trace_attrs.workspace_cwd_set,
                    claim_outcome = tracing::field::Empty,
                    request_outcome = tracing::field::Empty,
                    failure_class = tracing::field::Empty,
                ));
            if let Err(error) = process.await {
                break Err(error);
            }
        };

        auxiliary_tasks.close();
        let mut auxiliary_error = None;
        while let Some(joined) = auxiliary_tasks.join_next().await {
            if let Err(error) = auxiliary_task_join_result(joined) {
                tracing::error!(error = %crate::failure_text::BoundedErrorChain(error.as_ref()), "owned auxiliary task failed during daemon exit");
                if auxiliary_error.is_none() {
                    auxiliary_error = Some(error);
                }
            }
        }
        match (run_result, auxiliary_error) {
            (Err(error), Some(auxiliary_error)) => Err(error.context(format!(
                "owned auxiliary task also failed: {auxiliary_error:#}"
            ))),
            (Err(error), None) => Err(error),
            (Ok(()), Some(error)) => Err(error),
            (Ok(()), None) => Ok(()),
        }
    }

    /// Verify, through the head's own admission owner, each pending message
    /// that may fold into `request`'s claim; an unverified message keeps its
    /// own claim, where its admission is decided.
    pub(super) async fn verified_fold_admissions(&self, request: &AgentRequest) -> Vec<String> {
        let candidates =
            match crate::request::lifecycle::queue::fold_candidates(&self.node, request).await {
                Ok(candidates) => candidates,
                Err(error) => {
                    tracing::warn!(
                        request_id = %request.request_id,
                        error = %format!("{error:#}"),
                        "could not read queued messages to fold; claiming the request alone"
                    );
                    return Vec::new();
                }
            };
        let mut admitted = Vec::new();
        for candidate in candidates {
            match self
                .request_admission
                .verify_fresh(&candidate, &self.agent_config.agent_id)
                .await
            {
                Ok(verified) => admitted.push(verified.doc_id),
                Err(error) => tracing::debug!(
                    request_id = %request.request_id,
                    candidate_request_id = %candidate.request_id,
                    error = %crate::failure_text::BoundedErrorChain(&error),
                    "queued message admission not verified; it keeps its own turn"
                ),
            }
        }
        admitted
    }
}
