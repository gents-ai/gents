use anyhow::{Context, Result};
use defra_agent_protocol::row::{
    AgentBehaviorRow, AgentPrincipalRow, AgentRequestRow, EventTriggerRow, InferenceBackendRow,
    InferenceProfileRow, ScheduleRow, TaskRow, ToolSelectionRow, ToolServiceRegistryRow,
};

use super::super::mutations::{
    self, CreatedConversation, PeerMutationResult, SubmitRequestOptions, SubmittedRequest,
};
use super::super::schema::subscribed_collection_names;
use super::bootstrap::{
    add_replicator_with_retry_until, branchable_pair_sync_enabled, connect_peer_with_retry_until,
    normalize_required, p2p_pairing_enabled_for_graphql, sync_branchable_collections_with_retry,
    BRANCHABLE_PAIR_SYNC_ENV, REMOTE_P2P_PAIRING_ENV,
};
use super::{ClientCore, ClientPeerStatus, PEER_ADD_OPERATION_TIMEOUT};

impl ClientCore {
    pub async fn create_conversation(
        &self,
        agent_did: &str,
        behavior_id: Option<&str>,
    ) -> Result<CreatedConversation> {
        let snapshot = self.store.snapshot();
        match mutations::create_conversation(
            self.node.as_ref(),
            snapshot.as_ref(),
            agent_did,
            behavior_id,
        )
        .await
        {
            Ok(result) => {
                self.store.set_focused_request_id(None);
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    action = "chat_create",
                    row_id = %result.session_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error("create conversation", error)),
        }
    }

    pub async fn submit_request(
        &self,
        session_id: &str,
        agent_did: &str,
        content: &str,
        behavior_id: Option<&str>,
    ) -> Result<SubmittedRequest> {
        self.submit_request_with_options(
            session_id,
            agent_did,
            content,
            behavior_id,
            SubmitRequestOptions::default(),
        )
        .await
    }

    pub async fn submit_request_with_options(
        &self,
        session_id: &str,
        agent_did: &str,
        content: &str,
        behavior_id: Option<&str>,
        options: SubmitRequestOptions,
    ) -> Result<SubmittedRequest> {
        let snapshot = self.store.snapshot();
        match mutations::submit_request(
            self.node.as_ref(),
            snapshot.as_ref(),
            session_id,
            agent_did,
            content,
            behavior_id,
            options,
        )
        .await
        {
            Ok(result) => {
                self.store
                    .set_focused_request_id(Some(result.request_id.clone()));
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    action = "chat_submit",
                    row_id = %result.request_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error("submit request", error)),
        }
    }

    pub async fn rename_conversation(&self, session_id: &str, title: &str) -> Result<()> {
        let snapshot = self.store.snapshot();
        match mutations::rename_conversation(
            self.node.as_ref(),
            snapshot.as_ref(),
            session_id,
            title,
        )
        .await
        {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    action = "chat_rename",
                    row_id = %session_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("rename conversation", error)),
        }
    }

    pub async fn resend_request(&self, stale_request_id: &str) -> Result<SubmittedRequest> {
        let snapshot = self.store.snapshot();
        match mutations::resend_request(self.node.as_ref(), snapshot.as_ref(), stale_request_id)
            .await
        {
            Ok(result) => {
                self.store
                    .set_focused_request_id(Some(result.request_id.clone()));
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    action = "chat_resend",
                    row_id = %result.request_id,
                    stale_request_id = %stale_request_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error("resend request", error)),
        }
    }

    pub async fn interrupt_request(&self, request_id: &str) -> Result<()> {
        match mutations::interrupt_request(self.node.as_ref(), request_id).await {
            Ok(()) => {
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    action = "chat_interrupt",
                    row_id = %request_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("interrupt request", error)),
        }
    }

    pub async fn retry_request(&self, parent: &AgentRequestRow) -> Result<SubmittedRequest> {
        let snapshot = self.store.snapshot();
        match mutations::retry_request(self.node.as_ref(), snapshot.as_ref(), parent).await {
            Ok(result) => {
                self.store
                    .set_focused_request_id(Some(result.request_id.clone()));
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    action = "chat_retry",
                    row_id = %result.request_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error("retry request", error)),
        }
    }

    pub async fn add_peer(
        &self,
        label: &str,
        addr: &str,
        agent_did: &str,
        graphql: Option<&str>,
    ) -> Result<PeerMutationResult> {
        let label = normalize_required("label", label)?;
        let addr = normalize_required("addr", addr)?;
        let agent_did = normalize_required("agent_did", agent_did)?;
        let graphql = graphql.map(str::trim).filter(|value| !value.is_empty());

        let record = {
            let mut peer_directory = self.peer_directory.write().await;
            peer_directory
                .upsert_saved_peer_with_graphql(label, addr, agent_did, graphql)
                .await?
        };

        let mut warning = None;
        let p2p_pairing_enabled = record
            .graphql
            .as_deref()
            .map(p2p_pairing_enabled_for_graphql)
            .unwrap_or(true);

        let connected = match connect_peer_with_retry_until(
            &self.p2p,
            &record.addr,
            &record.label,
            PEER_ADD_OPERATION_TIMEOUT,
        )
        .await
        {
            Ok(()) => {
                if p2p_pairing_enabled {
                    match add_replicator_with_retry_until(
                        &self.p2p,
                        subscribed_collection_names()
                            .into_iter()
                            .map(str::to_owned)
                            .collect(),
                        &record.addr,
                        &record.label,
                        PEER_ADD_OPERATION_TIMEOUT,
                    )
                    .await
                    {
                        Ok(()) => {}
                        Err(error) => {
                            append_warning(
                                &mut warning,
                                format!(
                                    "deployment connected but replication setup failed: {error}"
                                ),
                            );
                        }
                    }
                } else {
                    tracing::info!(
                        target: "defra_agent_desktop_core::peer",
                        peer_id = %record.peer_id,
                        label = %record.label,
                        env = REMOTE_P2P_PAIRING_ENV,
                        "skipping automatic remote P2P replicator setup for GraphQL-managed peer"
                    );
                }
                true
            }
            Err(error) => {
                append_warning(
                    &mut warning,
                    format!("deployment saved but dial failed: {error}"),
                );
                false
            }
        };

        if let Some(graphql) = record.graphql.as_deref() {
            if p2p_pairing_enabled {
                match super::bootstrap::configure_local_runtime_pairing(&self.p2p, graphql).await {
                    Ok(()) => {
                        if branchable_pair_sync_enabled() {
                            match sync_branchable_collections_with_retry(
                                self.node.as_ref(),
                                &self.p2p,
                                &record.label,
                                PEER_ADD_OPERATION_TIMEOUT,
                            )
                            .await
                            {
                                Ok(synced) => {
                                    tracing::info!(
                                        target: "defra_agent_desktop_core::peer",
                                        peer_id = %record.peer_id,
                                        label = %record.label,
                                        synced_collections = ?synced,
                                        "desktop requested branchable collection sync after peer add"
                                    );
                                }
                                Err(error) => {
                                    append_warning(
                                        &mut warning,
                                        format!(
                                            "deployment paired but existing branchable sync failed: {error}"
                                        ),
                                    );
                                }
                            }
                        } else {
                            tracing::debug!(
                                target: "defra_agent_desktop_core::peer",
                                peer_id = %record.peer_id,
                                label = %record.label,
                                env = BRANCHABLE_PAIR_SYNC_ENV,
                                "skipping opt-in branchable collection sync after peer add"
                            );
                        }
                    }
                    Err(error) => {
                        let prefix = if connected {
                            "deployment connected"
                        } else {
                            "deployment saved"
                        };
                        append_warning(
                            &mut warning,
                            format!("{prefix} but reverse pairing failed: {error}"),
                        );
                    }
                }
            } else {
                tracing::info!(
                    target: "defra_agent_desktop_core::peer",
                    peer_id = %record.peer_id,
                    label = %record.label,
                    graphql,
                    env = REMOTE_P2P_PAIRING_ENV,
                    "skipping automatic reverse P2P pairing for GraphQL-managed peer"
                );
            }
            if let Err(error) = self.refresh_store().await {
                append_warning(
                    &mut warning,
                    format!("deployment saved but remote snapshot refresh failed: {error}"),
                );
            }
        }

        self.update_peer_status(ClientPeerStatus {
            peer_id: record.peer_id.clone(),
            label: record.label.clone(),
            agent_did: record.agent_did.clone(),
            addr: record.addr.clone(),
            dial_succeeded: connected,
            last_error: warning.clone(),
        });
        self.clear_mutation_error();
        if let Some(warning) = warning.as_deref() {
            tracing::warn!(
                target: "defra_agent_desktop_core::peer",
                peer_id = %record.peer_id,
                label = %record.label,
                error = %warning,
                "desktop deployment add warning"
            );
        } else {
            tracing::info!(
                target: "defra_agent_desktop_core::peer",
                peer_id = %record.peer_id,
                label = %record.label,
                "desktop deployment added"
            );
        }

        Ok(PeerMutationResult {
            peer_id: record.peer_id,
            label: record.label,
            addr: record.addr,
            connected,
            warning,
        })
    }

    pub async fn remove_peer(&self, peer_id: &str) -> Result<PeerMutationResult> {
        let peer_id = normalize_required("peer_id", peer_id)?;
        let removed = {
            let mut peer_directory = self.peer_directory.write().await;
            peer_directory.remove(peer_id).await?
        }
        .with_context(|| format!("peer {peer_id} not found"))?;

        let previous_status = {
            let mut statuses = self
                .peer_statuses
                .write()
                .expect("peer status lock poisoned");
            statuses
                .iter()
                .position(|status| status.peer_id == removed.peer_id)
                .map(|index| statuses.remove(index))
        };

        let warning = previous_status
            .filter(|status| status.dial_succeeded)
            .map(|_| {
                "saved deployment removed; any active transport connection remains until restart"
                    .to_string()
            });

        self.clear_mutation_error();
        tracing::info!(
            target: "defra_agent_desktop_core::peer",
            peer_id = %removed.peer_id,
            label = %removed.label,
            "desktop deployment removed"
        );
        Ok(PeerMutationResult {
            peer_id: removed.peer_id,
            label: removed.label,
            addr: removed.addr,
            connected: false,
            warning,
        })
    }

    pub async fn save_behavior(&self, row: &AgentBehaviorRow) -> Result<()> {
        match mutations::upsert_agent_behavior(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    doc_type = "behavior",
                    row_id = %row.behavior_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save behavior", error)),
        }
    }

    pub async fn save_agent_principal(&self, row: &AgentPrincipalRow) -> Result<()> {
        match mutations::upsert_agent_principal(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    doc_type = "agent_principal",
                    row_id = %row.agent_did,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save agent principal", error)),
        }
    }

    pub async fn save_backend(&self, row: &InferenceBackendRow) -> Result<()> {
        match mutations::upsert_inference_backend(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    doc_type = "backend",
                    row_id = %row.backend_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save backend", error)),
        }
    }

    pub async fn save_tool_selection(&self, row: &ToolSelectionRow) -> Result<()> {
        match mutations::upsert_tool_selection(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    doc_type = "tool_selection",
                    row_id = %row.selection_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save tool selection", error)),
        }
    }

    pub async fn save_tool_service_registry(&self, row: &ToolServiceRegistryRow) -> Result<()> {
        match mutations::upsert_tool_service_registry(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    doc_type = "tool_service_registry",
                    row_id = %row.service_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save tool service registry", error)),
        }
    }

    pub async fn save_inference_profile(&self, row: &InferenceProfileRow) -> Result<()> {
        match mutations::upsert_inference_profile(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    doc_type = "inference_profile",
                    row_id = %row.profile_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save inference profile", error)),
        }
    }

    /// Persist a `Task` document.
    ///
    /// Task 51 leaves the mutation body stubbed out (Task 52 wires the
    /// actual upsert); the method is kept on `ClientCore` so call sites
    /// stay linkable.
    pub async fn save_task(&self, row: &TaskRow) -> Result<()> {
        match mutations::upsert_task(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    doc_type = "task",
                    row_id = %row.task_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save task", error)),
        }
    }

    /// Persist a `Schedule` document. See `save_task` for current limits.
    pub async fn save_schedule(&self, row: &ScheduleRow) -> Result<()> {
        match mutations::upsert_schedule(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    doc_type = "schedule",
                    row_id = %row.schedule_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save schedule", error)),
        }
    }

    /// Persist an `EventTrigger` document. Writes ONLY apply-owned
    /// fields; runtime-owned bookkeeping (`last_attempt_at`,
    /// `last_fired_source_doc_id`, `last_status`, `last_error`,
    /// `fire_count`) is never projected into the mutation input.
    pub async fn save_event_trigger(&self, row: &EventTriggerRow) -> Result<()> {
        match mutations::upsert_event_trigger(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    doc_type = "event_trigger",
                    row_id = %row.trigger_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save event trigger", error)),
        }
    }

    /// Fire a task immediately via the shared manual-run helper.
    ///
    /// Returns the new `AgentRequest`'s `_docID` on success. The row lands
    /// at `lifecycle_state = "pending"` with manual lineage so the agent's
    /// normal intake picks it up.
    pub async fn fire_task_now(
        &self,
        task_row: &TaskRow,
        args: serde_json::Value,
    ) -> Result<String> {
        match mutations::fire_task_now(self.node.as_ref(), task_row, args).await {
            Ok(doc_id) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    doc_type = "manual_run",
                    task_id = %task_row.task_id,
                    request_doc_id = %doc_id,
                    "desktop manual task run enqueued"
                );
                Ok(doc_id)
            }
            Err(error) => Err(self.record_mutation_error("fire task", error)),
        }
    }

    /// Force a `Schedule`'s task to fire now.
    ///
    /// Delegates to `fire_task_now` on the schedule's `task_id` with
    /// empty args, so the operator override produces an
    /// `AgentRequest` with manual lineage (`caused_by_trigger_kind =
    /// "manual"`, not `"schedule"`). Returns the new request's
    /// `_docID` on success.
    pub async fn fire_schedule_now(&self, row: &ScheduleRow) -> Result<String> {
        match mutations::fire_schedule_now(self.node.as_ref(), row).await {
            Ok(doc_id) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    doc_type = "schedule",
                    row_id = %row.schedule_id,
                    action = "run_now",
                    request_doc_id = %doc_id,
                    "desktop manual schedule run enqueued"
                );
                Ok(doc_id)
            }
            Err(error) => Err(self.record_mutation_error("fire schedule now", error)),
        }
    }

    fn update_peer_status(&self, status: ClientPeerStatus) {
        let mut statuses = self
            .peer_statuses
            .write()
            .expect("peer status lock poisoned");
        if let Some(existing) = statuses
            .iter_mut()
            .find(|existing| existing.peer_id == status.peer_id)
        {
            *existing = status;
        } else {
            statuses.push(status);
            statuses.sort_by(|left, right| {
                left.label
                    .to_lowercase()
                    .cmp(&right.label.to_lowercase())
                    .then_with(|| left.peer_id.cmp(&right.peer_id))
            });
        }
    }

    fn clear_mutation_error(&self) {
        *self
            .last_mutation_error
            .write()
            .expect("mutation error lock poisoned") = None;
    }

    fn record_mutation_error(&self, operation: &str, error: anyhow::Error) -> anyhow::Error {
        let message = format!("{operation} failed: {error}");
        *self
            .last_mutation_error
            .write()
            .expect("mutation error lock poisoned") = Some(message);
        error
    }
}

fn append_warning(warning: &mut Option<String>, message: String) {
    match warning {
        Some(existing) => {
            existing.push_str("; ");
            existing.push_str(&message);
        }
        None => *warning = Some(message),
    }
}
