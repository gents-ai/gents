use super::ops::{
    log_followup_consumption, verify_ops_report_written, warn_if_missing_findings,
};
use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_task_standalone(
    task: &ScheduledTask,
    profile: &ProfileConfig,
    node: &Arc<EmbeddedNode>,
    mcp_pool: &McpPool,
    health_map: &ServiceHealthMap,
    local_hostname: &str,
    local_subnet: Option<&str>,
    ops_graphql_endpoint: &str,
    backend_tracker: Arc<BackendTracker>,
) -> Result<()> {
    let backend_id = profile
        .backend_id
        .as_deref()
        .ok_or_else(|| anyhow!("scheduled profiles require backend binding"))?;
    let runtime_context = format!(
        "Current time: {}\nHost: {}\nTask: {} (run #{})\n\n",
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        local_hostname,
        task.name,
        task.run_count + 1,
    );
    let full_prompt = format!("{}{}", runtime_context, task.prompt);
    let mut lifecycle = RequestLifecycle::materialize_claimed_with_execution_binding(
        node.clone(),
        &profile.name,
        profile.did(),
        &full_prompt,
        profile.deadline_duration.as_secs(),
        ExecutionOrigin::Scheduled,
        backend_id,
    )
    .await?;

    let timed = tokio::time::timeout(
        Duration::from_secs(TASK_TIMEOUT_SECS),
        execute_materialized_task(
            task,
            profile,
            node,
            mcp_pool,
            health_map,
            local_hostname,
            local_subnet,
            ops_graphql_endpoint,
            backend_tracker,
            &mut lifecycle,
            &full_prompt,
        ),
    )
    .await;

    match timed {
        Ok(Ok(())) => {
            lifecycle.complete().await?;
            close_scheduled_session(node, lifecycle.request().session_id.as_str()).await?;
            update_task_success_standalone(task, node).await?;
            Ok(())
        }
        Ok(Err(error)) => {
            finalize_scheduled_failure(task, node, profile, &mut lifecycle, &error.to_string())
                .await?;
            Err(error)
        }
        Err(_) => {
            let error = anyhow!("wall-clock timeout exceeded ({}s)", TASK_TIMEOUT_SECS);
            finalize_scheduled_failure(task, node, profile, &mut lifecycle, &error.to_string())
                .await?;
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_materialized_task(
    task: &ScheduledTask,
    profile: &ProfileConfig,
    node: &Arc<EmbeddedNode>,
    mcp_pool: &McpPool,
    health_map: &ServiceHealthMap,
    local_hostname: &str,
    local_subnet: Option<&str>,
    ops_graphql_endpoint: &str,
    backend_tracker: Arc<BackendTracker>,
    lifecycle: &mut RequestLifecycle,
    full_prompt: &str,
) -> Result<()> {
    let task_started_at = Utc::now();
    let api_key = std::env::var("AGENT_DAEMON_API_KEY").unwrap_or_else(|_| "no-key".to_string());
    let openai_client: rig::providers::openai::CompletionsClient =
        rig::providers::openai::CompletionsClient::builder()
            .api_key(&api_key)
            .base_url(&profile.model_endpoint)
            .build()?;
    let prompt_builder = LayeredPromptBuilder::from_profile(profile);
    let preamble = prompt_builder.preamble().to_string();

    let mut tools = profile.native_tools.build_native_tools()?;
    tools.push(build_delegate_tool(node.clone()));
    tools.extend(build_meta_tools(
        node.clone(),
        mcp_pool.clone(),
        health_map.clone(),
        local_hostname.to_string(),
        local_subnet.map(str::to_string),
    ));

    let agent = openai_client
        .agent(&profile.model_name)
        .preamble(&preamble)
        .default_max_turns(profile.max_turns)
        .tools(tools)
        .build();

    let stream_writer = DefraStreamWriter::new(
        node.clone(),
        profile.did(),
        Duration::from_millis(profile.stream_batch_ms),
    );
    let _backend_permit = acquire_backend_permit(task, profile, node, backend_tracker, lifecycle)
        .await?;
    lifecycle.begin_execution().await?;

    let doc_id = stream_writer
        .begin(
            lifecycle.request().session_id.as_str(),
            lifecycle.request().request_id.as_str(),
        )
        .await?;
    lifecycle.set_response_doc_id(&doc_id);
    lifecycle.advance().await?;

    let response_text = prompt_scheduled_task(
        task,
        profile,
        node,
        &prompt_builder,
        &agent,
        lifecycle.request().session_id.as_str(),
        full_prompt,
    )
    .await?;

    if !response_text.is_empty() {
        let _ = stream_writer.write_tokens(&doc_id, &response_text).await?;
        lifecycle.advance().await?;
    }

    let report_status =
        verify_ops_report_written(task_started_at, &task.name, ops_graphql_endpoint).await?;
    warn_if_missing_findings(
        task_started_at,
        &task.name,
        &report_status,
        ops_graphql_endpoint,
    )
    .await;
    log_followup_consumption(task_started_at, &task.name, ops_graphql_endpoint).await;

    stream_writer.finalize(&doc_id, StreamStatus::Complete).await?;
    Ok(())
}

async fn acquire_backend_permit(
    task: &ScheduledTask,
    profile: &ProfileConfig,
    node: &Arc<EmbeddedNode>,
    backend_tracker: Arc<BackendTracker>,
    lifecycle: &mut RequestLifecycle,
) -> Result<BackendPermit> {
    let backend_id = lifecycle.backend_id();
    let deadline = tokio::time::Instant::now() + profile.deadline_duration;

    loop {
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "task '{}' timed out waiting for backend {} capacity",
                task.name,
                backend_id
            );
        }

        let backend = backend_registry::lookup_backend(node, backend_id)
            .await?
            .ok_or_else(|| {
                anyhow!(
                    "backend {} not found for scheduled profile {}",
                    backend_id,
                    profile.name
                )
            })?;

        if backend.is_available() {
            if let Some(permit) =
                backend_tracker.try_acquire_permit(backend_id, backend.max_concurrent)
            {
                lifecycle.mark_slot_acquired().await?;
                return Ok(permit);
            }
        }

        tokio::time::sleep(Duration::from_millis(BACKEND_WAIT_POLL_MS)).await;
    }
}

async fn prompt_scheduled_task<M: rig::completion::CompletionModel>(
    task: &ScheduledTask,
    profile: &ProfileConfig,
    node: &Arc<EmbeddedNode>,
    prompt_builder: &LayeredPromptBuilder,
    agent: &rig::agent::Agent<M>,
    session_id: &str,
    full_prompt: &str,
) -> Result<String> {
    let mut history = prompt_builder.build(&[], &[]).await?.messages;
    let hook = DefraSessionHook::resume_or_create_with_identity_policy(
        node.clone(),
        session_id,
        &profile.name,
        profile.did(),
        FailurePolicy::FailClosed,
    )
    .await?;

    match agent
        .prompt(full_prompt)
        .with_history(&mut history)
        .with_hook(hook)
        .await
    {
        Ok(response) => Ok(response.to_string()),
        Err(error) if error.to_string().contains("empty") => {
            tracing::warn!(
                task = %task.name,
                error = %error,
                "empty completion response, retrying after 2s"
            );
            tokio::time::sleep(Duration::from_secs(2)).await;

            let retry_hook = DefraSessionHook::resume_or_create_with_identity_policy(
                node.clone(),
                session_id,
                &profile.name,
                profile.did(),
                FailurePolicy::FailClosed,
            )
            .await?;
            let mut retry_history = prompt_builder.build(&[], &[]).await?.messages;

            agent.prompt(full_prompt)
                .with_history(&mut retry_history)
                .with_hook(retry_hook)
                .await
                .map(|response| response.to_string())
                .map_err(|retry_error| anyhow!("scheduled task inference failed on retry: {retry_error}"))
        }
        Err(error) => Err(anyhow!("scheduled task inference failed: {error}")),
    }
}

async fn finalize_scheduled_failure(
    task: &ScheduledTask,
    node: &Arc<EmbeddedNode>,
    profile: &ProfileConfig,
    lifecycle: &mut RequestLifecycle,
    error_message: &str,
) -> Result<()> {
    let stream_writer = DefraStreamWriter::new(
        node.clone(),
        profile.did(),
        Duration::from_millis(profile.stream_batch_ms),
    );
    let response_doc_id = match lifecycle.response_doc_id() {
        Some(doc_id) => doc_id.to_string(),
        None => {
            let doc_id = stream_writer
                .begin(
                    lifecycle.request().session_id.as_str(),
                    lifecycle.request().request_id.as_str(),
                )
                .await?;
            lifecycle.set_response_doc_id(&doc_id);
            doc_id
        }
    };
    let error_text = format!("Error: {}", error_message);

    let _ = stream_writer
        .write_tokens(&response_doc_id, &error_text)
        .await?;
    stream_writer
        .finalize(&response_doc_id, StreamStatus::Error)
        .await?;
    lifecycle.fail().await?;

    if let Err(close_error) = close_scheduled_session(node, lifecycle.request().session_id.as_str()).await {
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
    let now = Utc::now();
    let next_run = now + chrono::Duration::seconds(task.interval_secs);
    let now_str = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let next_str = next_run.to_rfc3339_opts(SecondsFormat::Secs, true);
    let new_run_count = task.run_count + 1;

    let mutation = format!(
        r#"mutation {{ update_ScheduledTask(docID: "{doc_id}", input: {{last_status: "success", last_run_at: "{last_run}", next_run_at: "{next_run}", run_count: {count}, last_error: ""}}) {{ _docID }} }}"#,
        doc_id = task.doc_id,
        last_run = now_str,
        next_run = next_str,
        count = new_run_count,
    );

    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!(
            "failed to update task '{}' success: {:?}",
            task.name,
            resp.errors
        );
    }

    Ok(())
}
