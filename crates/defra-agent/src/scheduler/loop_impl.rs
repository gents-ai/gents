use super::execution::execute_task_standalone;
use super::*;

impl Scheduler {
    pub async fn run(&mut self, cancel: CancellationToken) -> Result<()> {
        tracing::info!("scheduler started, tick interval = {}s", TICK_INTERVAL_SECS);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!("scheduler shutting down");
                    return Ok(());
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(TICK_INTERVAL_SECS)) => {
                    if let Err(error) = self.tick().await {
                        tracing::error!(error = %error, "scheduler tick failed");
                    }
                }
            }
        }
    }

    async fn tick(&mut self) -> Result<()> {
        let active_snapshot = self.current_snapshot();
        let tasks = self.query_due_tasks().await?;

        if tasks.is_empty() {
            tracing::debug!("scheduler tick: no due tasks");
            return Ok(());
        }

        tracing::info!(count = tasks.len(), "scheduler tick: found due tasks");

        let mut handles: Vec<(ScheduledTask, tokio::task::JoinHandle<Result<()>>)> = Vec::new();

        for task in tasks {
            let behavior = match active_snapshot.behavior(&task.behavior_id) {
                Some(behavior) => behavior.clone(),
                None => {
                    let error_message = active_snapshot
                        .unavailable_reason(&task.behavior_id)
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| {
                            format!("scheduled task behavior {} is not loaded", task.behavior_id)
                        });
                    tracing::warn!(
                        task = %task.name,
                        behavior_id = %task.behavior_id,
                        "skipping task: behavior not loaded"
                    );
                    self.update_task_failure(&task, &error_message).await?;
                    continue;
                }
            };

            if behavior.backend_id.is_none() {
                tracing::error!(
                    task = %task.name,
                    behavior_id = %behavior.name,
                    "skipping task: scheduled behaviors require backend binding"
                );
                self.update_task_failure(
                    &task,
                    &format!(
                        "scheduled task behavior {} requires a backend binding",
                        behavior.name
                    ),
                )
                .await?;
                continue;
            }
            let tool_surface = match active_snapshot.tool_surface(&task.behavior_id) {
                Some(tool_surface) => tool_surface.clone(),
                None => {
                    tracing::warn!(
                        task = %task.name,
                        behavior_id = %task.behavior_id,
                        "skipping task: tool surface not resolved for behavior"
                    );
                    self.update_task_failure(
                        &task,
                        &format!(
                            "scheduled task behavior {} has no resolved tool surface",
                            task.behavior_id
                        ),
                    )
                    .await?;
                    continue;
                }
            };

            let task_clone = task.clone();
            let node = self.node.clone();
            let tool_runtime = self.tool_runtime.clone();
            let backend_tracker = self.backend_tracker.clone();

            let handle = tokio::spawn(async move {
                execute_task_standalone(
                    &task_clone,
                    &behavior,
                    tool_surface.as_ref(),
                    &tool_runtime,
                    &node,
                    backend_tracker,
                )
                .await
            });

            handles.push((task, handle));
        }

        for (task, handle) in handles {
            match handle.await {
                Ok(Ok(())) => {
                    tracing::info!(task = %task.name, "scheduled task completed successfully");
                }
                Ok(Err(error)) => {
                    tracing::error!(task = %task.name, error = %error, "scheduled task failed");
                    if let Err(update_err) =
                        self.update_task_failure(&task, &error.to_string()).await
                    {
                        tracing::error!(
                            task = %task.name,
                            error = %update_err,
                            "failed to update task failure status"
                        );
                    }
                }
                Err(join_err) => {
                    let msg = format!("task panicked: {}", join_err);
                    tracing::error!(task = %task.name, "{}", msg);
                    if let Err(update_err) = self.update_task_failure(&task, &msg).await {
                        tracing::error!(
                            task = %task.name,
                            error = %update_err,
                            "failed to update task failure status after panic"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    async fn query_due_tasks(&self) -> Result<Vec<ScheduledTask>> {
        let items = self.query_scheduled_task_rows().await?;
        let mut tasks = Vec::new();
        for item in &items {
            let task = ScheduledTask::from_value(item)?;
            if task.is_due() {
                tasks.push(task);
            }
        }

        Ok(tasks)
    }

    async fn query_scheduled_task_rows(&self) -> Result<Vec<serde_json::Value>> {
        const BEHAVIOR_QUERY: &str = r#"query { ScheduledTask(filter: {enabled: {_eq: true}}) { _docID task_id name behavior_id prompt interval_secs enabled next_run_at last_status last_error run_count } }"#;

        let behavior_resp = self.node.execute(BEHAVIOR_QUERY).await;
        if behavior_resp.has_errors() {
            let error_text = format!("{:?}", behavior_resp.errors);
            if is_missing_scheduled_task_collection_error(&error_text) {
                tracing::debug!(
                    "ScheduledTask collection not present; skipping scheduled task scan"
                );
                return Ok(Vec::new());
            }
            anyhow::bail!("query ScheduledTask failed: {:?}", behavior_resp.errors);
        }

        Ok(behavior_resp
            .data
            .as_ref()
            .and_then(|d| d.get("ScheduledTask"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default())
    }

    async fn update_task_failure(&self, task: &ScheduledTask, error_msg: &str) -> Result<()> {
        let now = Utc::now();
        let next_run = now + chrono::Duration::seconds(task.interval_secs);
        let now_str = now.to_rfc3339_opts(SecondsFormat::Secs, true);
        let next_str = next_run.to_rfc3339_opts(SecondsFormat::Secs, true);
        let new_run_count = task.run_count + 1;

        let mutation = format!(
            r#"mutation {{ update_ScheduledTask(docID: "{doc_id}", input: {{last_status: "error", last_run_at: "{last_run}", next_run_at: "{next_run}", run_count: {count}, last_error: "{error}"}}) {{ _docID }} }}"#,
            doc_id = task.doc_id,
            last_run = now_str,
            next_run = next_str,
            count = new_run_count,
            error = escape_graphql_string(error_msg),
        );

        let resp = self.node.execute(&mutation).await;
        if resp.has_errors() {
            anyhow::bail!(
                "failed to update task '{}' failure: {:?}",
                task.name,
                resp.errors
            );
        }

        Ok(())
    }
}
