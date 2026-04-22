use anyhow::{Context, Result};
use defra_agent_protocol::row::{
    AgentBehaviorRow, AgentRequestRow, EventTriggerRow, InferenceBackendRow, InferenceProfileRow,
    ScheduleRow, TaskRow, ToolSelectionRow,
};

use super::super::mutations::{self, CreatedConversation, PeerMutationResult, SubmittedRequest};
use super::super::schema::subscribed_collection_names;
use super::bootstrap::{
    add_replicator_with_retry_until, connect_peer_with_retry_until, normalize_required,
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
                    target: "defra_agent_desktop::writes",
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
        let snapshot = self.store.snapshot();
        match mutations::submit_request(
            self.node.as_ref(),
            snapshot.as_ref(),
            session_id,
            agent_did,
            content,
            behavior_id,
        )
        .await
        {
            Ok(result) => {
                self.store
                    .set_focused_request_id(Some(result.request_id.clone()));
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop::writes",
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
                    target: "defra_agent_desktop::writes",
                    action = "chat_rename",
                    row_id = %session_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("rename conversation", error)),
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
                    target: "defra_agent_desktop::writes",
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
    ) -> Result<PeerMutationResult> {
        let label = normalize_required("label", label)?;
        let addr = normalize_required("addr", addr)?;
        let agent_did = normalize_required("agent_did", agent_did)?;

        let record = {
            let mut peer_directory = self.peer_directory.write().await;
            peer_directory
                .upsert_saved_peer(label, addr, agent_did)
                .await?
        };

        let (connected, warning) = match connect_peer_with_retry_until(
            &self.p2p,
            &record.addr,
            &record.label,
            PEER_ADD_OPERATION_TIMEOUT,
        )
        .await
        {
            Ok(()) => match add_replicator_with_retry_until(
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
                Ok(()) => (true, None),
                Err(error) => (
                    true,
                    Some(format!(
                        "deployment connected but replication setup failed: {error}"
                    )),
                ),
            },
            Err(error) => (
                false,
                Some(format!("deployment saved but dial failed: {error}")),
            ),
        };

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
                target: "defra_agent_desktop::peer",
                peer_id = %record.peer_id,
                label = %record.label,
                error = %warning,
                "desktop deployment add warning"
            );
        } else {
            tracing::info!(
                target: "defra_agent_desktop::peer",
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
            target: "defra_agent_desktop::peer",
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
                    target: "defra_agent_desktop::writes",
                    doc_type = "behavior",
                    row_id = %row.behavior_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save behavior", error)),
        }
    }

    pub async fn save_backend(&self, row: &InferenceBackendRow) -> Result<()> {
        match mutations::upsert_inference_backend(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop::writes",
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
                    target: "defra_agent_desktop::writes",
                    doc_type = "tool_selection",
                    row_id = %row.selection_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save tool selection", error)),
        }
    }

    pub async fn save_inference_profile(&self, row: &InferenceProfileRow) -> Result<()> {
        match mutations::upsert_inference_profile(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop::writes",
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
                    target: "defra_agent_desktop::writes",
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
                    target: "defra_agent_desktop::writes",
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
                    target: "defra_agent_desktop::writes",
                    doc_type = "event_trigger",
                    row_id = %row.trigger_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save event trigger", error)),
        }
    }

    /// Force a `Schedule` to fire now. Stubbed in Task 51; Task 52 wires
    /// the real mutation.
    pub async fn fire_schedule_now(&self, row: &ScheduleRow) -> Result<()> {
        match mutations::fire_schedule_now(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop::writes",
                    doc_type = "schedule",
                    row_id = %row.schedule_id,
                    action = "run_now",
                    "desktop write saved"
                );
                Ok(())
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
