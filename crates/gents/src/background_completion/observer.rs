use super::*;

pub(crate) async fn run_background_completion_observer(
    node: Arc<EmbeddedNode>,
    local_did: String,
    background_executions: crate::hook::BackgroundExecutionRegistry,
    cancel: CancellationToken,
) -> Result<()> {
    let mut observer =
        BackgroundCompletionObserver::new(node, local_did, background_executions, cancel);
    observer.run().await
}

struct BackgroundCompletionObserver {
    node: Arc<EmbeddedNode>,
    local_did: String,
    background_executions: crate::hook::BackgroundExecutionRegistry,
    cancel: CancellationToken,
    subscription: events::Subscription,
    collection_id_to_name: HashMap<String, String>,
    processed_child_request_ids: HashSet<String>,
}

impl BackgroundCompletionObserver {
    fn new(
        node: Arc<EmbeddedNode>,
        local_did: String,
        background_executions: crate::hook::BackgroundExecutionRegistry,
        cancel: CancellationToken,
    ) -> Self {
        let subscription = node.subscribe(&[EventName::Update]);
        Self {
            node,
            local_did,
            background_executions,
            cancel,
            subscription,
            collection_id_to_name: HashMap::new(),
            processed_child_request_ids: HashSet::new(),
        }
    }

    async fn run(&mut self) -> Result<()> {
        self.project_ready_children().await?;
        self.run_reconcilers().await?;
        let mut reconciler_tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            let message = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Ok(()),
                _ = reconciler_tick.tick() => {
                    self.project_ready_children().await?;
                    self.run_reconcilers().await?;
                    continue;
                }
                msg = self.subscription.recv() => {
                    match msg {
                        Some(message) => message,
                        None => anyhow::bail!("subagent completion subscription channel closed"),
                    }
                }
            };

            let dropped = self.subscription.check_and_reset_dropped();
            if dropped > 0 {
                tracing::warn!(
                    dropped,
                    "subagent completion observer dropped messages; scanning terminal children"
                );
                self.project_ready_children().await?;
                self.run_reconcilers().await?;
            }

            let Some(update) = message.as_update() else {
                continue;
            };
            let Some(collection_name) = self.resolve_collection_name(&update.collection_id).await
            else {
                continue;
            };
            if collection_name != AGENT_REQUEST_COLLECTION {
                continue;
            }

            let Some(child_request_id) =
                load_request_id_by_doc_id(self.node.as_ref(), &update.doc_id).await?
            else {
                continue;
            };
            self.project_child_if_needed(child_request_id).await;
        }
    }

    async fn project_ready_children(&mut self) -> Result<()> {
        for child_request_id in load_terminal_child_request_ids(self.node.as_ref()).await? {
            self.project_child_if_needed(child_request_id).await;
        }
        Ok(())
    }

    async fn run_reconcilers(&mut self) -> Result<()> {
        match crate::mailbox::sweep_open_mailbox_items(self.node.as_ref()).await {
            Ok(mailbox)
                if mailbox.acted > 0
                    || mailbox.expired > 0
                    || mailbox.skipped_unsupported > 0
                    || mailbox.skipped_errors > 0 =>
            {
                tracing::debug!(?mailbox, "mailbox close sweep applied");
            }
            Ok(_) => {}
            Err(error) => {
                // A failed read/transition must leave rows open and retry on
                // the next ordinary reconciler tick; it must not take down
                // the other background reconcilers.
                tracing::warn!(%error, "mailbox close sweep failed; will retry");
            }
        }
        for run in crate::periodic_recovery::run_periodic_recovery_sweeps(
            self.node.as_ref(),
            &self.local_did,
            &self.background_executions,
        )
        .await?
        {
            if !run.is_noop() {
                tracing::debug!(
                    sweep_ids = ?run.metadata.sweep_ids,
                    rust_function = run.metadata.rust_function,
                    outcome = ?run.outcome,
                    "periodic recovery sweep applied"
                );
            }
        }
        let wake_redrive = crate::RequestLifecycle::redrive_failed_background_wakeups(
            self.node.as_ref(),
            &self.local_did,
        )
        .await?;
        if !wake_redrive.is_noop() {
            tracing::debug!(
                redriven = wake_redrive.redriven,
                deferred = wake_redrive.deferred,
                already_redriven = wake_redrive.already_redriven,
                coalesced = wake_redrive.coalesced,
                ineligible = wake_redrive.ineligible,
                failed = wake_redrive.failed,
                scanned = wake_redrive.scanned,
                "redrove failed background-completion wakes"
            );
        }
        let unclaimed =
            reconcile_unclaimed_cross_deployment_spawns(self.node.clone(), &self.local_did).await?;
        if !unclaimed.is_empty() {
            tracing::debug!(
                count = unclaimed.len(),
                "reconciled unclaimed subagent spawns"
            );
        }
        let cancel_ack = observe_cancel_cascade_ack(self.node.clone(), &self.local_did).await?;
        if !cancel_ack.is_empty() {
            tracing::debug!(
                count = cancel_ack.len(),
                "observed cross-deployment cancel acks"
            );
        }

        // Owner-scoped terminal-convergence re-drive (#664): re-assert the
        // terminal state of recently-terminalized own-requests so the terminal
        // delta reaches replicas that missed the one-shot PushLog.
        let redrive = crate::RequestLifecycle::redrive_terminal_convergence(
            self.node.as_ref(),
            &self.local_did,
        )
        .await?;
        if !redrive.is_noop() {
            tracing::debug!(
                reasserted = redrive.reasserted,
                scanned = redrive.scanned,
                "re-drove terminal request convergence to replicas"
            );
        }
        Ok(())
    }

    async fn project_child_if_needed(&mut self, child_request_id: String) {
        if self.processed_child_request_ids.contains(&child_request_id) {
            return;
        }

        match project_background_subagent_completion(
            self.node.clone(),
            &child_request_id,
            &self.local_did,
        )
        .await
        {
            Ok(BackgroundCompletionOutcome::Projected { .. })
            | Ok(BackgroundCompletionOutcome::AlreadyProjected)
            | Ok(BackgroundCompletionOutcome::NotLocalOwner) => {
                self.processed_child_request_ids.insert(child_request_id);
            }
            Ok(
                BackgroundCompletionOutcome::NotTerminal
                | BackgroundCompletionOutcome::NotBackground
                | BackgroundCompletionOutcome::MissingFinalResponse
                | BackgroundCompletionOutcome::Unlinked,
            ) => {}
            Err(error) => {
                tracing::warn!(
                    child_request_id = %child_request_id,
                    error = %error,
                    "failed to project background subagent completion"
                );
            }
        }
    }

    async fn resolve_collection_name(&mut self, collection_id: &str) -> Option<String> {
        if let Some(name) = self.collection_id_to_name.get(collection_id) {
            return Some(name.clone());
        }

        let names = match self.node.list_collections() {
            Ok(names) => names,
            Err(error) => {
                tracing::warn!(
                    collection_id = %collection_id,
                    %error,
                    "subagent completion observer failed to list collections"
                );
                return None;
            }
        };

        for name in names {
            let def = match self.node.get_collection(&name) {
                Ok(Some(def)) => def,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(
                        collection_name = %name,
                        %error,
                        "subagent completion observer failed to fetch collection definition",
                    );
                    continue;
                }
            };
            self.collection_id_to_name
                .insert(def.collection_id.clone(), def.name.clone());
        }

        self.collection_id_to_name.get(collection_id).cloned()
    }
}
