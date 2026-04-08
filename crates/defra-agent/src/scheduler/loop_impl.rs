use super::execution::execute_task_standalone;
use super::*;

impl Scheduler {
    pub async fn run(&self, cancel: CancellationToken) -> Result<()> {
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

    async fn tick(&self) -> Result<()> {
        let profile_names: Vec<&str> = self.profiles.iter().map(|p| p.name.as_str()).collect();
        let tasks = self.query_due_tasks(&profile_names).await?;

        if tasks.is_empty() {
            tracing::debug!("scheduler tick: no due tasks");
            return Ok(());
        }

        tracing::info!(count = tasks.len(), "scheduler tick: found due tasks");

        let mut handles: Vec<(ScheduledTask, tokio::task::JoinHandle<Result<()>>)> = Vec::new();

        for task in tasks {
            let profile = match self.profiles.iter().find(|p| p.name == task.profile_name) {
                Some(p) => p.clone(),
                None => {
                    tracing::warn!(
                        task = %task.name,
                        profile = %task.profile_name,
                        "skipping task: profile not found"
                    );
                    continue;
                }
            };

            if profile.backend_id.is_none() {
                tracing::error!(
                    task = %task.name,
                    profile = %profile.name,
                    "skipping task: scheduled profiles require backend binding"
                );
                continue;
            }

            let task_clone = task.clone();
            let node = self.node.clone();
            let mcp_pool = self.mcp_pool.clone();
            let health_map = self.health_map.clone();
            let local_hostname = self.local_hostname.clone();
            let local_subnet = self.local_subnet.clone();
            let ops_graphql_endpoint = self.ops_graphql_endpoint.clone();
            let backend_tracker = self.backend_tracker.clone();

            let handle = tokio::spawn(async move {
                execute_task_standalone(
                    &task_clone,
                    &profile,
                    &node,
                    &mcp_pool,
                    &health_map,
                    &local_hostname,
                    local_subnet.as_deref(),
                    &ops_graphql_endpoint,
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

    async fn query_due_tasks(&self, profile_names: &[&str]) -> Result<Vec<ScheduledTask>> {
        let query = r#"query { ScheduledTask(filter: {enabled: {_eq: true}}) { _docID task_id name profile_name prompt interval_secs enabled next_run_at last_status last_error run_count } }"#;

        let resp = self.node.execute(query).await;
        if resp.has_errors() {
            anyhow::bail!("query ScheduledTask failed: {:?}", resp.errors);
        }

        let items = resp
            .data
            .as_ref()
            .and_then(|d| d.get("ScheduledTask"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let tasks: Vec<ScheduledTask> = items
            .iter()
            .filter_map(ScheduledTask::from_value)
            .filter(|t| profile_names.contains(&t.profile_name.as_str()))
            .filter(|t| t.is_due())
            .collect();

        Ok(tasks)
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
