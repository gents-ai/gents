use std::sync::Arc;

use crate::llm::message::Message;
use crate::llm::tool::ToolDyn;
use anyhow::{anyhow, Context, Result};
use defra_node::EmbeddedNode;
use rig::client::CompletionClient;
use rig::completion::CompletionModel;

use crate::agent::stream_processor::{StreamAction, StreamProcessor};
use crate::completion_factory::loop_config;
use crate::config::AgentBehavior;
use crate::hook::{BackgroundToolRegistry, DefraSessionHook, FailurePolicy};
use crate::lifecycle::TerminalizeResult;
use crate::lifecycle::{ExecutionOrigin, RequestLifecycle, RequestTerminalOutcome, TriggerLineage};
use crate::prompt::{LayeredPromptBuilder, PromptBuilder};
use crate::streaming::DefraStreamWriter;
use crate::tool_surface::{self, ToolRuntimeContext};
use futures::StreamExt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneshotRunResult {
    pub session_id: String,
    pub response_text: String,
}

pub async fn run_openai_oneshot(
    node: Arc<EmbeddedNode>,
    behavior: &AgentBehavior,
    prompt: &str,
) -> Result<OneshotRunResult> {
    run_openai_oneshot_with_tools(node, behavior, Vec::new(), prompt).await
}

pub async fn run_openai_oneshot_with_tools(
    node: Arc<EmbeddedNode>,
    behavior: &AgentBehavior,
    extra_tools: Vec<Box<dyn ToolDyn>>,
    prompt: &str,
) -> Result<OneshotRunResult> {
    crate::migration::ensure_all_runtime_migrations(node.clone()).await?;

    let api_key = behavior.completion_client_api_key()?;
    let tool_runtime =
        ToolRuntimeContext::oneshot_with_agent_did(node.clone(), behavior.agent_did());
    let tool_surface = behavior.tools.resolve(node.as_ref()).await?;
    let allowed_targets = tool_surface::resolve_subagent_target_descriptions(&tool_surface);
    let prompt_builder = LayeredPromptBuilder::new(behavior, &tool_surface, &allowed_targets);
    let output_obligations = tool_surface.output_obligations();

    let lsp_pool = tool_runtime.lsp_pool.clone();
    let mut tools = tool_surface.build_tools(&tool_runtime)?;
    tools.extend(extra_tools);
    let tools = Arc::new(tools);
    // Background executions run through `call_tool_managed`, which owns the
    // deadline/cancellation envelope — no per-tool wrapper needed.
    let background_tool_registry = BackgroundToolRegistry::from_tools(
        tool_surface.build_tools(&tool_runtime)?,
        &tool_surface.background_tools().allowlist,
    );

    let client = crate::llm::backend_client::build_backend_client(
        node.clone(),
        behavior,
        &api_key,
        crate::startup_readiness::StartupReadinessOptions::default().build_timeout,
    )
    .await?;

    crate::llm::backend_client::with_backend_client!(client, |client| {
        run_oneshot_with_completion_client(
            node,
            behavior,
            prompt,
            prompt_builder,
            &output_obligations,
            tools,
            background_tool_registry,
            lsp_pool.clone(),
            client,
        )
        .await
    })
}

async fn run_oneshot_with_completion_client<C>(
    node: Arc<EmbeddedNode>,
    behavior: &AgentBehavior,
    prompt: &str,
    prompt_builder: LayeredPromptBuilder,
    output_obligations: &[(String, crate::document_config::WriteToolOutputObligation)],
    tools: Arc<Vec<Box<dyn ToolDyn>>>,
    background_tool_registry: BackgroundToolRegistry,
    lsp_pool: crate::toolset::lsp::LspPool,
    client: C,
) -> Result<OneshotRunResult>
where
    C: CompletionClient,
    C::CompletionModel: 'static,
    <C::CompletionModel as CompletionModel>::StreamingResponse: 'static,
{
    // exemption: one-shot does not wrap `model` in the daemon's
    // `AdmissionRegistry`. Admission enforces a per-backend concurrency
    // ceiling (`BackendAdmissionConfig`, built from the backend document's
    // `max_concurrent`/`max_queue_depth`/`probe_status`) so multiple daemon
    // slots sharing one backend stay bounded; it requires a registry that has
    // been `reconcile()`-d with that config, which only the daemon's runtime
    // reconciler drives. `AgentBehavior` here carries no such fields (by
    // design — one-shot is a single ad hoc call, not a slot pool with
    // contention to bound), so plugging in a fresh, never-reconciled registry
    // would make every completion fail immediately with "BackendGone: backend
    // admission controller is not active" rather than skip the ceiling.
    // Pinned by `oneshot_completes_without_backend_admission_reconciliation`
    // in `tests/misc/oneshot_admission_exemption.rs`.
    let model = client.completion_model(&behavior.model_name);
    let config = loop_config(
        behavior,
        prompt_builder.preamble().to_owned(),
        tools.len(),
        crate::rendered_request::CaptureScopeKind::OneShot,
    );
    run_oneshot_owned(
        node,
        behavior,
        &prompt_builder,
        model,
        prompt,
        tools,
        config,
        output_obligations,
        background_tool_registry,
        lsp_pool,
    )
    .await
}

async fn terminalize_oneshot_setup_failure(
    lifecycle: &mut RequestLifecycle,
    lsp_pool: &crate::toolset::lsp::LspPool,
    error: anyhow::Error,
) -> anyhow::Error {
    let reason = error.to_string();
    let persistence_error = persist_oneshot_failure(lifecycle, &reason).await.err();
    lsp_pool.shutdown().await;
    match persistence_error {
        None => error,
        Some(persistence_error) => anyhow!(
            "one-shot setup failed: {error}; additionally failed to persist its terminal response: {persistence_error}"
        ),
    }
}

async fn persist_oneshot_failure(lifecycle: &mut RequestLifecycle, reason: &str) -> Result<()> {
    lifecycle
        .terminalize_owned_without_stream(RequestTerminalOutcome::Failed, Some(reason))
        .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_oneshot_owned<M: CompletionModel + 'static>(
    node: Arc<EmbeddedNode>,
    behavior: &AgentBehavior,
    prompt_builder: &LayeredPromptBuilder,
    model: M,
    prompt: &str,
    tools: Arc<Vec<Box<dyn ToolDyn>>>,
    mut config: crate::agent::loop_stream::LoopConfig,
    output_obligations: &[(String, crate::document_config::WriteToolOutputObligation)],
    background_tool_registry: BackgroundToolRegistry,
    lsp_pool: crate::toolset::lsp::LspPool,
) -> Result<OneshotRunResult>
where
    M::StreamingResponse: 'static,
{
    let mut lifecycle = RequestLifecycle::materialize_pending_with_execution_binding(
        node.clone(),
        &behavior.behavior_id,
        behavior.principal_identity().clone(),
        prompt,
        behavior.deadline_duration.as_secs(),
        ExecutionOrigin::Interactive,
        behavior.backend_id.as_deref().unwrap_or_default(),
        TriggerLineage::default(),
    )
    .await?;
    lifecycle.set_execution_lease_duration(behavior.stream_liveness_timeout);
    anyhow::ensure!(
        matches!(
            lifecycle.claim_with_identity().await?,
            crate::lifecycle::ClaimOutcome::Claimed
        ),
        "new one-shot request was not claimed"
    );
    let request = lifecycle.request().clone();
    let stream_writer = DefraStreamWriter::new(
        node.clone(),
        behavior.agent_did(),
        std::time::Duration::ZERO,
    );
    let response_doc_id = match lifecycle.begin_owned_execution(&stream_writer).await {
        Ok(doc_id) => doc_id,
        Err(error) => {
            return Err(terminalize_oneshot_setup_failure(&mut lifecycle, &lsp_pool, error).await);
        }
    };
    let request_commit_cid = match lifecycle.request_commit_cid() {
        Some(cid) => cid.to_string(),
        None => {
            let error = anyhow!("one-shot request has no commit CID");
            return Err(terminalize_oneshot_setup_failure(&mut lifecycle, &lsp_pool, error).await);
        }
    };
    config.deadline = request
        .deadline
        .as_deref()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&chrono::Utc));
    config.output_obligation_gate =
        match crate::agent::output_obligation::OutputObligationGate::for_request(
            node.clone(),
            &request,
            output_obligations,
        )
        .await
        {
            Ok(gate) => gate,
            Err(error) => {
                return Err(
                    terminalize_oneshot_setup_failure(&mut lifecycle, &lsp_pool, error).await,
                );
            }
        };
    let capture_scope = crate::rendered_request::scope::scope_from_factory(
        crate::rendered_request::RenderedRequestContext {
            request_doc_id: request.doc_id.clone(),
            request_commit_cid,
            request_id: request.request_id.clone(),
            agent_did: behavior.agent_did().to_string(),
            requester_did: String::new(),
            behavior_id: behavior.behavior_id.clone(),
            session_id: request.session_id.clone(),
            model_name: behavior.model_name.clone(),
        },
        Some(&crate::rendered_request::defra_rendered_request_capture_factory(node.clone())),
    );

    let history = match prompt_builder.build(&[], &[]).await {
        Ok(prompt) => prompt.messages,
        Err(error) => {
            return Err(terminalize_oneshot_setup_failure(&mut lifecycle, &lsp_pool, error).await);
        }
    };

    let hook = match DefraSessionHook::resume_with_identity_policy(
        node.clone(),
        &request.session_id,
        &behavior.behavior_id,
        behavior.agent_did(),
        FailurePolicy::default(),
    )
    .await
    {
        Ok(hook) => hook
            .with_output_obligation_gate(config.output_obligation_gate.clone())
            .with_background_tool_registry(background_tool_registry)
            .with_goal_tool_authority(
                behavior.tools.goal_tools_requested(),
                behavior.tools.goal_creation_requested(),
            ),
        Err(error) => {
            return Err(terminalize_oneshot_setup_failure(&mut lifecycle, &lsp_pool, error).await);
        }
    };
    hook.set_active_request_binding(
        Some(request.request_id.clone()),
        Some(request.doc_id.clone()),
        request.requester_did.clone(),
    )
    .await;
    hook.set_request_deadline_at(config.deadline).await;
    // Both entry points consume the owned stream through the same durable
    // processor, so text, reasoning and tool progress renew the same lease.
    let inference = async {
        let mut stream = Box::pin(crate::agent::loop_stream::run_loop_stream(
            model,
            Some(hook.clone()),
            Message::user(prompt),
            history,
            tools,
            config,
        ));
        let mut processor =
            StreamProcessor::new(&hook, &stream_writer, &mut lifecycle, &response_doc_id);
        let mut lease_poll = tokio::time::interval(std::time::Duration::from_secs(1));
        lease_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let item = tokio::select! {
                biased;
                _ = lease_poll.tick() => { processor.validate_execution().await?; lease_poll.reset(); continue; },
                item = stream.next() => item,
            };
            let Some(item) = item else {
                break;
            };
            match processor.process_item(item).await? {
                StreamAction::Continue => {}
                StreamAction::Done => {
                    return processor
                        .final_text
                        .take()
                        .context("one-shot final response missing text")
                }
                StreamAction::Error(error) => {
                    drop(stream);
                    processor
                        .persist_partial_turn("persist failed one-shot assistant turn")
                        .await?;
                    return Err(anyhow!("one-shot inference failed: {error}"));
                }
            }
        }
        processor
            .persist_partial_turn("persist truncated one-shot assistant turn")
            .await?;
        Err(anyhow!(
            "provider stream ended without an explicit terminal response"
        ))
    };
    let response = match capture_scope {
        Some(scope) => crate::rendered_request::scope::scope_request(scope, inference).await,
        None => inference.await,
    };

    let session_id = hook.session_id().await;
    match response {
        Ok(response_text) => {
            let lifecycle_result = lifecycle
                .terminalize_owned(&stream_writer, RequestTerminalOutcome::Completed, None)
                .await;
            let close_result = if matches!(lifecycle_result, Ok(TerminalizeResult::Won)) {
                hook.close().await
            } else {
                Ok(())
            };
            if let Some(id) = session_id.as_deref() {
                lsp_pool.close_session(id).await;
            } else {
                lsp_pool.shutdown().await;
            }

            let session_id = session_id.context("one-shot run did not create a session")?;
            anyhow::ensure!(
                lifecycle_result? != TerminalizeResult::Lost,
                "one-shot execution lost terminal ownership"
            );
            close_result.with_context(|| format!("closing one-shot session {session_id}"))?;

            Ok(OneshotRunResult {
                session_id,
                response_text,
            })
        }
        Err(error) => {
            let lifecycle_result = lifecycle
                .terminalize_owned(
                    &stream_writer,
                    RequestTerminalOutcome::Failed,
                    Some(&error.to_string()),
                )
                .await;
            if !matches!(
                lifecycle_result,
                Ok(TerminalizeResult::Won | TerminalizeResult::AlreadySame)
            ) {
                // A one-shot process has no periodic daemon sweep. Reuse the
                // recovery owner before returning an expired execution to its caller.
                RequestLifecycle::recover_all(&node, behavior.agent_did()).await?;
            }
            let close_result = if matches!(lifecycle_result, Ok(TerminalizeResult::Won)) {
                hook.close().await
            } else {
                Ok(())
            };
            if let Some(id) = session_id.as_deref() {
                lsp_pool.close_session(id).await;
            } else {
                lsp_pool.shutdown().await;
            }

            if let Err(lifecycle_error) = lifecycle_result {
                return Err(anyhow!(
                    "agent prompt failed: {error}; additionally failed to terminalize request: {lifecycle_error}"
                ));
            }
            if let Some(session_id) = session_id {
                if let Err(close_error) = close_result {
                    return Err(anyhow!(
                        "agent prompt failed: {error}; additionally failed to close session {session_id}: {close_error}"
                    ));
                }
            } else if let Err(close_error) = close_result {
                return Err(anyhow!(
                    "agent prompt failed: {error}; additionally failed to close one-shot hook: {close_error}"
                ));
            }

            Err(error)
        }
    }
}

#[cfg(test)]
mod tests;
