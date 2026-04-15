use super::*;
use crate::admission::{self, AdmissionCallContext, AdmissionRegistry, CallKind};
use crate::backend_provider::BackendProviderKind;
use crate::completion_factory::build_admitted_agent;
use crate::config::BehaviorConfig;
use crate::tool_surface::ToolSurface;
use anyhow::Context;

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_task_standalone(
    task: &ScheduledTask,
    behavior: &BehaviorConfig,
    tool_surface: &ToolSurface,
    tool_runtime: &ToolRuntimeContext,
    node: &Arc<EmbeddedNode>,
    admission_registry: AdmissionRegistry,
    cancel: CancellationToken,
) -> Result<()> {
    let backend_id = behavior
        .backend_id
        .as_deref()
        .ok_or_else(|| anyhow!("scheduled behaviors require backend binding"))?;
    let runtime_context = format!(
        "Current time: {}\nHost: {}\nTask: {} (run #{})\n\n",
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        tool_runtime.local_hostname(),
        task.name,
        task.run_count + 1,
    );
    let full_prompt = format!("{}{}", runtime_context, task.prompt);
    let mut lifecycle = RequestLifecycle::materialize_claimed_with_execution_binding(
        node.clone(),
        &behavior.name,
        behavior.did(),
        &full_prompt,
        behavior.deadline_duration.as_secs(),
        ExecutionOrigin::Scheduled,
        backend_id,
    )
    .await?;

    let timed = tokio::select! {
        _ = cancel.cancelled() => {
            return Err(anyhow!("scheduled task cancelled during shutdown"));
        }
        timed = tokio::time::timeout(
            Duration::from_secs(TASK_TIMEOUT_SECS),
            execute_materialized_task(
                task,
                behavior,
                tool_surface,
                tool_runtime,
                node,
                admission_registry,
                &mut lifecycle,
                &full_prompt,
                &cancel,
            ),
        ) => timed
    };

    match timed {
        Ok(Ok(())) => {
            lifecycle.complete().await?;
            close_scheduled_session(node, lifecycle.request().session_id.as_str()).await?;
            update_task_success_standalone(task, node).await?;
            Ok(())
        }
        Ok(Err(error)) => {
            finalize_scheduled_failure(task, node, behavior, &mut lifecycle, &error.to_string())
                .await?;
            Err(error)
        }
        Err(_) => {
            let error = anyhow!("wall-clock timeout exceeded ({}s)", TASK_TIMEOUT_SECS);
            finalize_scheduled_failure(task, node, behavior, &mut lifecycle, &error.to_string())
                .await?;
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_materialized_task(
    task: &ScheduledTask,
    behavior: &BehaviorConfig,
    tool_surface: &ToolSurface,
    tool_runtime: &ToolRuntimeContext,
    node: &Arc<EmbeddedNode>,
    admission_registry: AdmissionRegistry,
    lifecycle: &mut RequestLifecycle,
    full_prompt: &str,
    cancel: &CancellationToken,
) -> Result<()> {
    let api_key = behavior.completion_client_api_key()?;
    let prompt_builder = LayeredPromptBuilder::new(behavior, tool_surface);
    let preamble = prompt_builder.preamble().to_string();
    let tools = tool_surface.build_tools(tool_runtime)?;

    let stream_writer = DefraStreamWriter::new(
        node.clone(),
        behavior.did(),
        Duration::from_millis(behavior.stream_batch_ms),
    );
    lifecycle.begin_execution().await?;

    let doc_id = stream_writer
        .begin(
            lifecycle.request().session_id.as_str(),
            lifecycle.request().request_id.as_str(),
            lifecycle.behavior_id(),
        )
        .await?;
    lifecycle.set_response_doc_id(&doc_id);
    lifecycle.advance().await?;

    let response_text = match behavior.backend_provider_kind {
        BackendProviderKind::OpenAiCompatible => {
            let build_context = format!(
                "building OpenAI-compatible completion client for behavior {} against {}",
                behavior.name, behavior.backend_endpoint
            );
            let client: rig::providers::openai::CompletionsClient =
                rig::providers::openai::CompletionsClient::builder()
                    .api_key(&api_key)
                    .base_url(&behavior.backend_endpoint)
                    .build()
                    .with_context(|| build_context.clone())?;
            let agent = build_admitted_agent(
                client,
                admission_registry.clone(),
                behavior,
                &preamble,
                tools,
            );
            let admission_context = AdmissionCallContext::for_request(
                lifecycle.request(),
                lifecycle.behavior_id(),
                lifecycle.backend_id(),
            );
            admission::scope_request(admission_context, async {
                prompt_scheduled_task(
                    task,
                    behavior,
                    node,
                    &prompt_builder,
                    &agent,
                    lifecycle.request().session_id.as_str(),
                    full_prompt,
                    cancel,
                )
                .await
            })
            .await?
        }
        BackendProviderKind::OpenRouter => {
            let build_context = format!(
                "building OpenRouter completion client for behavior {} against {}",
                behavior.name, behavior.backend_endpoint
            );
            let client: rig::providers::openrouter::Client =
                rig::providers::openrouter::Client::builder()
                    .api_key(&api_key)
                    .base_url(&behavior.backend_endpoint)
                    .build()
                    .with_context(|| build_context.clone())?;
            let agent = build_admitted_agent(
                client,
                admission_registry.clone(),
                behavior,
                &preamble,
                tools,
            );
            let admission_context = AdmissionCallContext::for_request(
                lifecycle.request(),
                lifecycle.behavior_id(),
                lifecycle.backend_id(),
            );
            admission::scope_request(admission_context, async {
                prompt_scheduled_task(
                    task,
                    behavior,
                    node,
                    &prompt_builder,
                    &agent,
                    lifecycle.request().session_id.as_str(),
                    full_prompt,
                    cancel,
                )
                .await
            })
            .await?
        }
    };

    if !response_text.is_empty() {
        let _ = stream_writer.write_tokens(&doc_id, &response_text).await?;
        lifecycle.advance().await?;
    }

    stream_writer
        .finalize(&doc_id, StreamStatus::Complete)
        .await?;
    Ok(())
}

async fn prompt_scheduled_task<M: rig::completion::CompletionModel>(
    task: &ScheduledTask,
    behavior: &BehaviorConfig,
    node: &Arc<EmbeddedNode>,
    prompt_builder: &LayeredPromptBuilder,
    agent: &rig::agent::Agent<M>,
    session_id: &str,
    full_prompt: &str,
    cancel: &CancellationToken,
) -> Result<String> {
    let mut history = prompt_builder.build(&[], &[]).await?.messages;
    let hook = DefraSessionHook::resume_or_create_with_identity_policy(
        node.clone(),
        session_id,
        &behavior.name,
        behavior.did(),
        FailurePolicy::FailClosed,
    )
    .await?;

    match admission::scope_call(CallKind::Scheduled, 1, async {
        tokio::select! {
            _ = cancel.cancelled() => {
                Err(anyhow!("scheduled task '{}' cancelled during shutdown", task.name))
            }
            response = agent
                .prompt(full_prompt)
                .with_history(&mut history)
                .with_hook(hook) => response.map_err(anyhow::Error::from)
        }
    })
    .await
    {
        Ok(response) => Ok(response.to_string()),
        Err(error) if error.to_string().contains("empty") => {
            tracing::warn!(
                task = %task.name,
                error = %error,
                "empty completion response, retrying after 2s"
            );
            tokio::select! {
                _ = cancel.cancelled() => {
                    return Err(anyhow!("scheduled task '{}' cancelled during shutdown", task.name));
                }
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            }

            let retry_hook = DefraSessionHook::resume_or_create_with_identity_policy(
                node.clone(),
                session_id,
                &behavior.name,
                behavior.did(),
                FailurePolicy::FailClosed,
            )
            .await?;
            let mut retry_history = prompt_builder.build(&[], &[]).await?.messages;

            admission::scope_call(CallKind::Scheduled, 2, async {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        Err(anyhow!("scheduled task '{}' cancelled during shutdown", task.name))
                    }
                    response = agent
                        .prompt(full_prompt)
                        .with_history(&mut retry_history)
                        .with_hook(retry_hook) => response
                        .map(|response| response.to_string())
                        .map_err(|retry_error| {
                            anyhow!("scheduled task inference failed on retry: {retry_error}")
                        })
                }
            })
            .await
        }
        Err(error) => Err(anyhow!("scheduled task inference failed: {error}")),
    }
}

async fn finalize_scheduled_failure(
    task: &ScheduledTask,
    node: &Arc<EmbeddedNode>,
    behavior: &BehaviorConfig,
    lifecycle: &mut RequestLifecycle,
    error_message: &str,
) -> Result<()> {
    let stream_writer = DefraStreamWriter::new(
        node.clone(),
        behavior.did(),
        Duration::from_millis(behavior.stream_batch_ms),
    );
    let response_doc_id = match lifecycle.response_doc_id() {
        Some(doc_id) => doc_id.to_string(),
        None => {
            let doc_id = stream_writer
                .begin(
                    lifecycle.request().session_id.as_str(),
                    lifecycle.request().request_id.as_str(),
                    lifecycle.behavior_id(),
                )
                .await?;
            lifecycle.set_response_doc_id(&doc_id);
            doc_id
        }
    };
    let error_text = format!("Error: {}", error_message);
    let _ = lifecycle.record_failure_reason(error_message).await;
    stream_writer
        .set_error_message(&response_doc_id, error_message)
        .await?;

    let _ = stream_writer
        .write_tokens(&response_doc_id, &error_text)
        .await?;
    stream_writer
        .finalize(&response_doc_id, StreamStatus::Error)
        .await?;
    lifecycle.fail().await?;

    if let Err(close_error) =
        close_scheduled_session(node, lifecycle.request().session_id.as_str()).await
    {
        tracing::error!(
            task = %task.name,
            session_id = %lifecycle.request().session_id,
            error = %close_error,
            "failed to close scheduled session after error"
        );
    }

    Ok(())
}

async fn close_scheduled_session(node: &Arc<EmbeddedNode>, session_id: &str) -> Result<()> {
    session::close_session(node.as_ref(), session_id).await
}

async fn update_task_success_standalone(
    task: &ScheduledTask,
    node: &Arc<EmbeddedNode>,
) -> Result<()> {
    super::update_task_runtime_state(node, task, "success", None).await
}
