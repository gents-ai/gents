use super::*;
use crate::tools::background::agent_identity_watch::AgentIdentityWatch;

pub(crate) async fn run_background_completion_observer(
    node: Arc<EmbeddedNode>,
    local_did: String,
    background_executions: crate::tools::call::hook::BackgroundExecutionRegistry,
    job_driver: Arc<crate::tools::background::job::driver::JobDriver>,
    agents: Box<dyn AgentIdentityWatch>,
    cancel: CancellationToken,
) -> Result<()> {
    let mut observer = BackgroundCompletionObserver::new(
        node,
        local_did,
        background_executions,
        job_driver,
        agents,
        cancel,
    )?;
    observer.run().await
}

struct BackgroundCompletionObserver {
    node: Arc<EmbeddedNode>,
    local_did: String,
    background_executions: crate::tools::call::hook::BackgroundExecutionRegistry,
    job_driver: Arc<crate::tools::background::job::driver::JobDriver>,
    agents: Box<dyn AgentIdentityWatch>,
    cancel: CancellationToken,
    subscription: events::DocumentChangeSubscription,
    mailbox_cursor: Option<String>,
}

impl BackgroundCompletionObserver {
    fn new(
        node: Arc<EmbeddedNode>,
        local_did: String,
        background_executions: crate::tools::call::hook::BackgroundExecutionRegistry,
        job_driver: Arc<crate::tools::background::job::driver::JobDriver>,
        agents: Box<dyn AgentIdentityWatch>,
        cancel: CancellationToken,
    ) -> Result<Self> {
        let subscription = crate::subscribe_collections(&node, [AGENT_REQUEST_COLLECTION])?;
        Ok(Self {
            node,
            local_did,
            background_executions,
            job_driver,
            agents,
            cancel,
            subscription,
            mailbox_cursor: None,
        })
    }

    async fn run(&mut self) -> Result<()> {
        self.run_reconcilers().await?;
        let mut reconciler_tick = tokio::time::interval(Duration::from_secs(5));
        reconciler_tick.reset();
        loop {
            let batch = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Ok(()),
                _ = reconciler_tick.tick() => {
                    self.run_reconcilers().await?;
                    // Slow sweeps must leave time to drain changes rather than
                    // repeatedly winning this biased select with overdue ticks.
                    reconciler_tick.reset();
                    continue;
                }
                batch = self.subscription.recv() => {
                    match batch {
                        Some(batch) => batch,
                        None => anyhow::bail!("background completion subscription channel closed"),
                    }
                }
            };

            if batch.resync_required {
                tracing::warn!(
                    updates = batch.updates,
                    "background completion observer change capacity exceeded; running the recovery sweeps"
                );
                self.run_reconcilers().await?;
                reconciler_tick.reset();
                continue;
            }

            for change in batch.changes {
                for (actor, access) in self.selected_agents() {
                    if let Err(error) = super::session_message::settle_rows_after_request_update(
                        &self.node,
                        &access,
                        &self.local_did,
                        &change.doc_id,
                    )
                    .await
                    {
                        tracing::warn!(request_doc_id = %change.doc_id, agent_did = %actor, %error, "session-message settlement failed; the periodic scan retries");
                    }
                    if let Err(error) =
                        crate::session::recover_agent_session_relationships_with_access(
                            &access,
                            &self.local_did,
                            &actor,
                        )
                        .await
                    {
                        tracing::warn!(agent_did = %actor, %error, "session relationship recovery failed; will retry");
                    }
                    if let Err(error) =
                        crate::tools::background::job::recovery::wake_jobs_after_request_update(
                            &access,
                            &self.job_driver,
                            &change.doc_id,
                        )
                        .await
                    {
                        tracing::warn!(
                            request_doc_id = %change.doc_id,
                            agent_did = %actor,
                            error = %format!("{error:#}"),
                            "job wake from a request update failed"
                        );
                    }
                }
            }
        }
    }

    async fn run_reconcilers(&mut self) -> Result<()> {
        let identities = self.agents.watcher_identities()?;
        match crate::session::mailbox::sweep_open_mailbox_items(
            &self.node,
            &self.local_did,
            &identities,
            self.mailbox_cursor.as_deref(),
        )
        .await
        {
            Ok(page) => {
                self.mailbox_cursor = page.next_cursor;
                let mailbox = page.report;
                if mailbox.acted > 0
                    || mailbox.expired > 0
                    || mailbox.skipped_unsupported > 0
                    || mailbox.skipped_errors > 0
                {
                    tracing::debug!(?mailbox, "mailbox close sweep applied");
                }
            }
            Err(error) => {
                // A failed read/transition must leave rows open and retry on
                // the next ordinary reconciler tick; it must not take down
                // the other background reconcilers.
                tracing::warn!(%error, "mailbox close sweep failed; will retry");
            }
        }
        for run in crate::runtime::periodic_recovery::run_periodic_recovery_sweeps_with_identities(
            &self.node,
            &self.local_did,
            &identities,
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
        for (actor, access) in self.selected_agents() {
            if let Err(error) = crate::session::recover_agent_session_relationships_with_access(
                &access,
                &self.local_did,
                &actor,
            )
            .await
            {
                tracing::warn!(agent_did = %actor, %error, "session relationship recovery failed; will retry");
            }
            if let Err(error) =
                crate::store::config_client::session_children::recover_agent_session_child_relationships_with_access(
                    &access,
                    &self.local_did,
                    &actor,
                )
                .await
            {
                tracing::warn!(agent_did = %actor, %error, "session child relationship recovery failed; will retry");
            }
            match crate::tools::background::job::recovery::redrive_running_jobs(
                &self.job_driver,
                &actor,
            )
            .await
            {
                Ok(0) => {}
                Ok(ticked) => {
                    tracing::debug!(ticked, "plugin job recovery sweep ticked running jobs")
                }
                Err(error) => {
                    tracing::warn!(agent_did = %actor, error = %format!("{error:#}"), "plugin job recovery sweep failed; will retry");
                }
            }
        }
        let wake_redrive =
            crate::RequestLifecycle::redrive_failed_background_wakeups_with_identities(
                &self.node,
                &self.local_did,
                &identities,
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
        let redrive = crate::RequestLifecycle::redrive_terminal_convergence_with_identities(
            &self.node,
            &self.local_did,
            &identities,
        )
        .await?;
        if !redrive.is_noop() {
            tracing::debug!(
                reasserted = redrive.reasserted,
                acknowledged = redrive.acknowledged,
                scanned = redrive.scanned,
                "re-drove terminal request convergence to replicas"
            );
        }
        Ok(())
    }

    fn selected_agents(&self) -> Vec<(identity::Did, crate::ConfigAccess)> {
        let identities = match self.agents.watcher_identities() {
            Ok(identities) => identities,
            Err(error) => {
                tracing::warn!(%error, "agent recovery identities are inconsistent");
                return Vec::new();
            }
        };
        identities.into_iter().filter_map(|identity| {
            let did = identity.did();
            if did == self.local_did {
                tracing::error!(agent_did = %did, "runtime identity cannot execute agent work");
                return None;
            }
            let result = identity::Did::new(did).map_err(anyhow::Error::from).and_then(|actor| {
                let access = crate::ConfigAccess::local_managed_agent(self.node.clone(), actor.clone(), identity.as_ref())?;
                Ok((actor, access))
            });
            match result {
                Ok(selected) => Some(selected),
                Err(error) => { tracing::warn!(agent_did = %did, %error, "agent recovery signer is invalid"); None }
            }
        }).collect()
    }
}
