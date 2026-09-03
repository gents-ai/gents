use std::collections::{hash_map::DefaultHasher, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, RwLock as StdRwLock};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents::identity::AgentIdentity;
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::row::{
    AgentBehaviorRow, AgentPrincipalRow, AgentRequestRow, EventTriggerRow, InferenceBackendRow,
    InferenceProfileRow, ScheduleRow, SkillRow, TaskRow, ToolSelectionRow, ToolServiceRegistryRow,
};
use gents_protocol::session_hydration::{
    decode_manifest_json, SessionHydrationDocumentKey, SessionHydrationReceipt,
    SESSION_HYDRATION_RECEIPT_VERSION,
};
use serde::Deserialize;

use super::super::mutations::{self, PeerMutationResult, SubmitRequestOptions, SubmittedRequest};
use super::super::observe::ObservedStore;
use super::super::peer_directory::PeerRecord;
use super::super::query::load_chat_patch;
use super::super::store::{ClientStore, ClientStoreRows};
use super::bootstrap::normalize_required;
use super::p2p_ops;
use super::ClientCore;
#[cfg(test)]
use super::ClientPeerStatus;

const REQUEST_PATCH_SIGNATURE_CAPACITY: usize = 2_048;

fn required_peer_generation<'a>(name: &str, value: Option<&'a str>) -> Result<&'a str> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{name} is missing from the current peer authority"))
}

fn is_terminal_lifecycle_state(value: Option<&str>) -> bool {
    matches!(
        value,
        Some("completed" | "failed" | "superseded" | "dead" | "interrupted")
    )
}

fn row_matches_source(
    sources: &[Option<String>],
    index: usize,
    source_agent_did: &str,
    is_remote_source: bool,
) -> bool {
    match sources.get(index).and_then(|source| source.as_deref()) {
        Some(source) => source == source_agent_did,
        None => !is_remote_source,
    }
}

fn retain_sourced_rows<T>(
    rows: &mut Vec<T>,
    sources: &mut Vec<Option<String>>,
    source_agent_did: &str,
    is_remote_source: bool,
    should_delete: impl Fn(&T) -> bool,
) {
    let previous_rows = std::mem::take(rows);
    let previous_sources = std::mem::take(sources);

    for (index, row) in previous_rows.into_iter().enumerate() {
        if should_delete(&row)
            && row_matches_source(&previous_sources, index, source_agent_did, is_remote_source)
        {
            continue;
        }
        rows.push(row);
        sources.push(previous_sources.get(index).cloned().unwrap_or_default());
    }
}

fn chat_patch_signature(patch: &ClientStore) -> (usize, usize, u64) {
    let rows = patch.row_count();
    match serde_json::to_vec(&patch.to_rows()) {
        Ok(bytes) => {
            let mut hasher = DefaultHasher::new();
            bytes.hash(&mut hasher);
            (rows, bytes.len(), hasher.finish())
        }
        Err(_) => (rows, 0, 0),
    }
}

fn behavior_id_for_write(requested_behavior_id: Option<&str>) -> Option<String> {
    requested_behavior_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn ensure_peer_chat_ready_at(
    agent_did: &str,
    peer_record: Option<&PeerRecord>,
    now: DateTime<Utc>,
) -> Result<()> {
    match peer_record {
        Some(record) if record.is_chat_ready_at(now) => Ok(()),
        Some(_) => bail!(
            "the selected deployment route is not ready; no request was saved (wait for pairing repair or inspect pairing status)"
        ),
        None => bail!(
            "no saved deployment route owns agent {agent_did}; no request was saved"
        ),
    }
}

fn peer_record_owning_agent_at(
    records: &[PeerRecord],
    agent_did: &str,
    now: DateTime<Utc>,
) -> Option<PeerRecord> {
    records
        .iter()
        .find(|record| record.agent_did == agent_did)
        .filter(|record| record.is_chat_ready_at(now))
        .cloned()
}

impl ClientCore {
    pub async fn refresh_local_standard_peer(
        &self,
        agent_home: &Path,
        label: &str,
    ) -> Result<PeerRecord> {
        let discovery = crate::local_runtime::discover_standard_runtime(agent_home).await?;
        self.persist_local_standard_peer(
            label,
            &discovery.p2p_listen_address,
            &discovery.agent_did,
            &discovery.graphql,
            &agent_home.display().to_string(),
        )
        .await
    }

    pub async fn persist_local_standard_peer(
        &self,
        label: &str,
        addr: &str,
        agent_did: &str,
        graphql: &str,
        agent_home: &str,
    ) -> Result<PeerRecord> {
        self.sync_state
            .upsert_local_standard_peer(
                normalize_required("label", label)?,
                normalize_required("addr", addr)?,
                normalize_required("agent_did", agent_did)?,
                normalize_required("graphql", graphql)?,
                normalize_required("agent_home", agent_home)?,
            )
            .await
    }

    /// Dismiss an open mailbox item as the authenticated local principal.
    pub async fn dismiss_mailbox_item(&self, doc_id: &str) -> Result<gents::mailbox::MailboxItem> {
        let result =
            gents::mailbox::dismiss_mailbox_item(self.node.as_ref(), doc_id, self.principal.did())
                .await;
        match result {
            Ok(item) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                Ok(item)
            }
            Err(error) => Err(self.record_mutation_error("dismiss mailbox item", error)),
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
        let peer_record = self
            .peer_record_for_chat_write(agent_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(agent_did, peer_record.as_ref(), Utc::now())?;
        let (signer, admission, requester_did) = self
            .request_authority(agent_did, peer_record.as_ref())
            .await?;
        let behavior_id = behavior_id_for_write(behavior_id);
        match mutations::submit_request(
            self.node.as_ref(),
            snapshot.as_ref(),
            session_id,
            agent_did,
            &requester_did,
            signer.as_ref(),
            admission,
            content,
            behavior_id.as_deref(),
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
                    target: "gents_desktop_core::writes",
                    action = "chat_submit",
                    row_id = %result.request_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error("submit request", error)),
        }
    }

    /// Reconstruct a request's persisted event timeline from the local P2P
    /// replica. Bounded so an unavailable replica fails the panel instead of
    /// hanging it.
    pub async fn request_timeline(
        &self,
        agent_did: &str,
        request_id: &str,
    ) -> Result<gents::run_timeline::RunTimeline> {
        normalize_required("agent_did", agent_did)?;
        let request_id = normalize_required("request_id", request_id)?;
        let access = gents::config_client::ConfigAccess::Local(self.node_arc());
        let timeline = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            gents::run_timeline_fetch::load_run_timeline(&access, request_id),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out loading timeline for {request_id}"))?
        .map_err(|error| anyhow::anyhow!("{}", strip_cli_operator_hints(&error.to_string())))?;
        Ok(timeline)
    }

    pub async fn list_tool_call_holds(
        &self,
        agent_did: &str,
    ) -> Result<Vec<gents::config_client::HeldToolCall>> {
        let agent_did = normalize_required("agent_did", agent_did)?;
        let access = gents::config_client::ConfigAccess::Local(self.node_arc());
        let held = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            gents::config_client::list_held_tool_calls(&access, Some(agent_did)),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out listing tool-call holds for {agent_did}"))?
        .map_err(|error| anyhow::anyhow!("{}", strip_cli_operator_hints(&error.to_string())))?;
        Ok(held)
    }

    pub async fn resolve_tool_call_hold(
        &self,
        agent_did: &str,
        tool_call_id: &str,
        approve: bool,
        reason: Option<String>,
    ) -> Result<String> {
        let agent_did = normalize_required("agent_did", agent_did)?;
        let peer_record = self
            .peer_record_for_chat_write(&agent_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(&agent_did, peer_record.as_ref(), Utc::now())?;
        let tool_call_id = normalize_required("tool_call_id", tool_call_id)?;
        let approval_id = match self
            .resolve_tool_call_hold_inner(agent_did, tool_call_id, approve, reason)
            .await
        {
            Ok(approval_id) => approval_id,
            Err(error) => return Err(self.record_mutation_error("resolve tool-call hold", error)),
        };
        self.clear_mutation_error();
        tracing::info!(
            target: "gents_desktop_core::writes",
            action = "resolve_tool_call_hold",
            row_id = %tool_call_id,
            approve,
            "desktop write saved"
        );
        Ok(approval_id)
    }

    async fn resolve_tool_call_hold_inner(
        &self,
        agent_did: &str,
        tool_call_id: &str,
        approve: bool,
        reason: Option<String>,
    ) -> Result<String> {
        let access = gents::config_client::ConfigAccess::Local(self.node_arc());
        let held = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            gents::config_client::list_held_tool_calls(&access, Some(agent_did)),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out listing tool-call holds for {agent_did}"))?
        .map_err(|error| anyhow::anyhow!("{}", strip_cli_operator_hints(&error.to_string())))?;
        let mut targets = held.iter().filter(|call| call.tool_call_id == tool_call_id);
        let target = targets
            .next()
            .ok_or_else(|| anyhow::anyhow!("tool call {tool_call_id} is not awaiting approval"))?;
        if targets.next().is_some() {
            anyhow::bail!(
                "tool call {tool_call_id} is ambiguous across multiple held AgentToolCall documents"
            );
        }
        let verdict = gents::config_client::ToolApprovalVerdict {
            tool_call_doc_id: target.tool_call_doc_id.clone(),
            tool_call_id: tool_call_id.to_string(),
            agent_did: agent_did.to_string(),
            request_id: target.request_id.clone(),
            approve,
            approver_did: self.principal().did().to_string(),
            reason,
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            gents::config_client::write_tool_approval(&access, &verdict),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out writing approval decision for {tool_call_id}"))?
        .map_err(|error| anyhow::anyhow!("{}", strip_cli_operator_hints(&error.to_string())))
    }

    pub async fn network_status(&self) -> NetworkStatus {
        let local_peer_id = p2p_ops::p2p_local_peer_id(&self.p2p).await;
        let listen_addresses = p2p_ops::p2p_listen_addresses(&self.p2p).await;
        let connected_peers = p2p_ops::p2p_connected_peers(&self.p2p).await;
        let replicators = p2p_ops::p2p_get_replicators(&self.p2p).await;
        let saved_peers = self.sync_state.records();

        NetworkStatus {
            local_peer_id: local_peer_id.map_err(|error| error.to_string()),
            listen_addresses: listen_addresses.map_err(|error| error.to_string()),
            connected_peers: connected_peers.map_err(|error| error.to_string()),
            replicators: replicators
                .map(|rows| {
                    rows.into_iter()
                        .map(|info| NetworkReplicator {
                            peer_id: info.id,
                            address: info.address,
                            collections: info.collections,
                            status: info.status,
                            last_status_change: info.last_status_change,
                        })
                        .collect()
                })
                .map_err(|error| error.to_string()),
            saved_peers,
        }
    }

    /// Reload the current directory generation and synchronously fence a chat
    /// mutation against its signed enrollment lease. Expired readiness is
    /// durably demoted before the caller can create an AgentRequest.
    async fn peer_record_for_chat_write(
        &self,
        agent_did: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<PeerRecord>> {
        let records = self.sync_state.records();
        let record = peer_record_owning_agent_at(&records, agent_did.trim(), now);
        if record.is_some() {
            return Ok(record);
        }

        let stale = records
            .iter()
            .find(|record| record.agent_did == agent_did.trim())
            .cloned();
        if let Some(stale) = stale {
            if stale.is_enrollment() && stale.pairing_ready {
                self.sync_state
                    .set_pairing_ready(&stale, false)
                    .await
                    .context("persisting expired enrollment write fence")?;
            }
            ensure_peer_chat_ready_at(agent_did, Some(&stale), now)?;
        }
        Ok(None)
    }

    async fn request_authority(
        &self,
        agent_did: &str,
        record: Option<&PeerRecord>,
    ) -> Result<(
        Arc<dyn gents::identity::AgentIdentity>,
        AgentRequestAdmissionRecord,
        String,
    )> {
        let record = record.context("chat target has no current owned peer route")?;
        if record.is_enrollment() {
            let request_id = required_peer_generation(
                "enrollment_request_id",
                record.enrollment_request_id.as_deref(),
            )?;
            let digest = required_peer_generation(
                "enrollment_request_digest",
                record.enrollment_request_digest.as_deref(),
            )?;
            let admin_did = required_peer_generation(
                "enrollment_admin_did",
                record.enrollment_admin_did.as_deref(),
            )?;
            let sequence = record
                .enrollment_authorization_sequence
                .filter(|value| *value > 0)
                .context("enrollment authorization sequence is missing")?;
            let expires_at = required_peer_generation(
                "enrollment_authorization_expires_at",
                record.enrollment_authorization_expires_at.as_deref(),
            )?;
            let signer: Arc<dyn gents::identity::AgentIdentity> = Arc::new(self.principal.clone());
            let admission = AgentRequestAdmissionRecord::enrollment(
                self.principal.did(),
                request_id,
                digest,
                admin_did,
                sequence,
                expires_at,
            );
            return Ok((signer, admission, self.principal.did().to_string()));
        }
        anyhow::ensure!(
            record.source.as_deref() == Some("local-standard") && record.agent_did == agent_did,
            "chat target is not owned by enrollment or a local standard runtime"
        );
        let signer: Arc<dyn gents::identity::AgentIdentity> =
            match gents::identity::RegisteredIdentity::from_registered_did(agent_did, None) {
                Ok(identity) => Arc::new(identity),
                Err(_) => {
                    let home = required_peer_generation(
                        "local_agent_home",
                        record.local_agent_home.as_deref(),
                    )?;
                    crate::local_runtime::load_standard_runtime_identity(Path::new(home))?
                }
            };
        anyhow::ensure!(
            signer.did() == agent_did,
            "local request signer does not own target agent"
        );
        Ok((
            signer,
            AgentRequestAdmissionRecord::local_self(agent_did),
            agent_did.to_string(),
        ))
    }

    pub async fn refresh_local_request(
        &self,
        agent_did: &str,
        request_id: &str,
    ) -> Result<Option<u64>> {
        let agent_did = agent_did.trim();
        let request_id = request_id.trim();
        if agent_did.is_empty() || request_id.is_empty() {
            return Ok(None);
        }

        let patch = load_chat_patch(self.node.as_ref(), request_id).await?;
        let rows = patch.row_count();
        if rows == 0 {
            return Ok(None);
        }
        // This patch came from the embedded replica, just like the observer's
        // baseline snapshot. Keep its source untagged so both paths address a
        // durable document by the same identity.
        let signature = chat_patch_signature(&patch);
        let cache_key = format!("local\0{agent_did}\0{request_id}");
        {
            let mut signatures = self.request_patch_signatures.lock().await;
            if signatures.get(&cache_key) == Some(&signature) {
                return Ok(None);
            }
            if signatures.len() >= REQUEST_PATCH_SIGNATURE_CAPACITY {
                signatures.clear();
            }
            signatures.insert(cache_key, signature);
        }

        let (_rows, bytes, _hash) = signature;
        let terminal = patch
            .request_row(request_id)
            .is_some_and(|row| is_terminal_lifecycle_state(row.lifecycle_state.as_deref()));
        let version = self.store.merge_chat_patch(patch);
        tracing::debug!(
            target: "gents_desktop_core::replication",
            request_id,
            agent_did,
            version,
            rows,
            bytes,
            terminal,
            "desktop selected local request patch merged"
        );
        Ok(Some(version))
    }

    pub async fn rename_conversation(
        &self,
        agent_did: &str,
        session_id: &str,
        title: &str,
    ) -> Result<()> {
        let snapshot = self.store.snapshot();
        let result = mutations::rename_conversation(
            self.node.as_ref(),
            snapshot.as_ref(),
            agent_did,
            self.principal.did(),
            session_id,
            title,
        )
        .await;
        match result {
            Ok(()) => {
                if self.refresh_agent(agent_did).await?.is_none() {
                    self.refresh_store().await?;
                }
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    action = "chat_rename",
                    row_id = %session_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("rename conversation", error)),
        }
    }

    pub async fn delete_skill(&self, skill_id: &str, source_agent_did: &str) -> Result<()> {
        let skill_id = normalize_required("skill_id", skill_id)?;
        let source_agent_did = normalize_required("source_agent_did", source_agent_did)?;
        let snapshot = self.store.snapshot();
        if !snapshot.skills.iter().any(|row| {
            row.skill_id == skill_id && row.agent_did.as_deref() == Some(source_agent_did)
        }) {
            bail!("no Skill document with skill_id {skill_id:?} for {source_agent_did}");
        }

        let affected_behaviors = snapshot
            .behaviors
            .iter()
            .filter(|row| row.agent_did.as_deref() == Some(source_agent_did))
            .filter_map(|row| {
                let mut next = row.clone();
                let refs_before = next.skill_refs.len();
                let excludes_before = next.skill_excludes.len();
                next.skill_refs.retain(|id| id != skill_id);
                next.skill_excludes.retain(|id| id != skill_id);
                (next.skill_refs.len() != refs_before
                    || next.skill_excludes.len() != excludes_before)
                    .then_some(next)
            })
            .collect::<Vec<_>>();

        let result = async {
            let deleted =
                mutations::delete_skill(self.node.as_ref(), source_agent_did, skill_id).await?;
            if deleted == 0 {
                bail!("no Skill document with skill_id {skill_id:?} for {source_agent_did}");
            }
            for behavior in affected_behaviors {
                mutations::upsert_agent_behavior(self.node.as_ref(), &behavior).await?;
            }
            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                let refresh_result = self.refresh_config_source(source_agent_did).await;
                complete_confirmed_delete(
                    self.store.as_ref(),
                    &self.last_mutation_error,
                    refresh_result,
                    "delete skill",
                    "config_skill_delete",
                    skill_id,
                    |rows| prune_deleted_skill_rows(rows, source_agent_did, skill_id),
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("delete skill", error)),
        }
    }

    pub async fn delete_task(&self, task_id: &str, source_agent_did: &str) -> Result<()> {
        let task_id = normalize_required("task_id", task_id)?;
        let source_agent_did = normalize_required("source_agent_did", source_agent_did)?;
        let snapshot = self.store.snapshot();
        if !snapshot.tasks.iter().enumerate().any(|(index, row)| {
            row.task_id == task_id
                && row_matches_source(
                    &snapshot.task_source_agent_dids,
                    index,
                    source_agent_did,
                    false,
                )
        }) {
            bail!("no Task document with task_id {task_id:?}");
        }
        let schedule_refs = snapshot
            .schedules
            .iter()
            .enumerate()
            .filter(|(index, row)| {
                row.task_id.as_deref() == Some(task_id)
                    && row_matches_source(
                        &snapshot.schedule_source_agent_dids,
                        *index,
                        source_agent_did,
                        false,
                    )
            })
            .count();
        let trigger_refs = snapshot
            .event_triggers
            .iter()
            .enumerate()
            .filter(|(index, row)| {
                row.task_id.as_deref() == Some(task_id)
                    && row_matches_source(
                        &snapshot.event_trigger_source_agent_dids,
                        *index,
                        source_agent_did,
                        false,
                    )
            })
            .count();
        if schedule_refs + trigger_refs > 0 {
            bail!(
                "task {task_id:?} is referenced by {schedule_refs} schedule(s) and {trigger_refs} event trigger(s); delete or detach those first"
            );
        }

        let result = async {
            let deleted = mutations::delete_task(self.node.as_ref(), task_id).await?;
            if deleted == 0 {
                bail!("no Task document with task_id {task_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete task",
            "config_task_delete",
            task_id,
            source_agent_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.tasks,
                    &mut rows.task_source_agent_dids,
                    source_agent_did,
                    false,
                    |row| row.task_id == task_id,
                );
            },
        )
        .await
    }

    pub async fn delete_schedule(&self, schedule_id: &str, source_agent_did: &str) -> Result<()> {
        let schedule_id = normalize_required("schedule_id", schedule_id)?;
        let source_agent_did = normalize_required("source_agent_did", source_agent_did)?;
        let snapshot = self.store.snapshot();
        if !snapshot.schedules.iter().enumerate().any(|(index, row)| {
            row.schedule_id == schedule_id
                && row_matches_source(
                    &snapshot.schedule_source_agent_dids,
                    index,
                    source_agent_did,
                    false,
                )
        }) {
            bail!("no Schedule document with schedule_id {schedule_id:?}");
        }

        let result = async {
            let deleted = mutations::delete_schedule(self.node.as_ref(), schedule_id).await?;
            if deleted == 0 {
                bail!("no Schedule document with schedule_id {schedule_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete schedule",
            "config_schedule_delete",
            schedule_id,
            source_agent_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.schedules,
                    &mut rows.schedule_source_agent_dids,
                    source_agent_did,
                    false,
                    |row| row.schedule_id == schedule_id,
                );
            },
        )
        .await
    }

    pub async fn delete_event_trigger(
        &self,
        trigger_id: &str,
        source_agent_did: &str,
    ) -> Result<()> {
        let trigger_id = normalize_required("trigger_id", trigger_id)?;
        let source_agent_did = normalize_required("source_agent_did", source_agent_did)?;
        let snapshot = self.store.snapshot();
        if !snapshot
            .event_triggers
            .iter()
            .enumerate()
            .any(|(index, row)| {
                row.trigger_id == trigger_id
                    && row_matches_source(
                        &snapshot.event_trigger_source_agent_dids,
                        index,
                        source_agent_did,
                        false,
                    )
            })
        {
            bail!("no EventTrigger document with trigger_id {trigger_id:?}");
        }

        let result = async {
            let deleted = mutations::delete_event_trigger(self.node.as_ref(), trigger_id).await?;
            if deleted == 0 {
                bail!("no EventTrigger document with trigger_id {trigger_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete event trigger",
            "config_event_trigger_delete",
            trigger_id,
            source_agent_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.event_triggers,
                    &mut rows.event_trigger_source_agent_dids,
                    source_agent_did,
                    false,
                    |row| row.trigger_id == trigger_id,
                );
            },
        )
        .await
    }

    pub async fn delete_inference_backend(
        &self,
        backend_id: &str,
        source_agent_did: &str,
    ) -> Result<()> {
        let backend_id = normalize_required("backend_id", backend_id)?;
        let source_agent_did = normalize_required("source_agent_did", source_agent_did)?;
        let snapshot = self.store.snapshot();
        if !snapshot
            .inference_backends
            .iter()
            .enumerate()
            .any(|(index, row)| {
                row.backend_id == backend_id
                    && row_matches_source(
                        &snapshot.inference_backend_source_agent_dids,
                        index,
                        source_agent_did,
                        false,
                    )
            })
        {
            bail!("no InferenceBackend document with backend_id {backend_id:?}");
        }
        let referencing = snapshot
            .behaviors
            .iter()
            .filter(|row| {
                row.agent_did.as_deref() == Some(source_agent_did)
                    && row.backend_id.as_deref() == Some(backend_id)
            })
            .map(|row| row.behavior_id.clone())
            .collect::<Vec<_>>();
        if !referencing.is_empty() {
            bail!(
                "backend {backend_id:?} is referenced by behavior(s) {}; point them elsewhere first",
                referencing.join(", ")
            );
        }

        let result = async {
            let deleted =
                mutations::delete_inference_backend(self.node.as_ref(), backend_id).await?;
            if deleted == 0 {
                bail!("no InferenceBackend document with backend_id {backend_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete inference backend",
            "config_backend_delete",
            backend_id,
            source_agent_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.inference_backends,
                    &mut rows.inference_backend_source_agent_dids,
                    source_agent_did,
                    false,
                    |row| row.backend_id == backend_id,
                );
            },
        )
        .await
    }

    pub async fn delete_inference_profile(
        &self,
        profile_id: &str,
        source_agent_did: &str,
    ) -> Result<()> {
        let profile_id = normalize_required("profile_id", profile_id)?;
        let source_agent_did = normalize_required("source_agent_did", source_agent_did)?;
        let snapshot = self.store.snapshot();
        if !snapshot
            .inference_profiles
            .iter()
            .enumerate()
            .any(|(index, row)| {
                row.profile_id == profile_id
                    && row_matches_source(
                        &snapshot.inference_profile_source_agent_dids,
                        index,
                        source_agent_did,
                        false,
                    )
            })
        {
            bail!("no InferenceProfile document with profile_id {profile_id:?}");
        }
        let referencing = snapshot
            .behaviors
            .iter()
            .filter(|row| {
                row.agent_did.as_deref() == Some(source_agent_did)
                    && row.inference_profile_id.as_deref() == Some(profile_id)
            })
            .map(|row| row.behavior_id.clone())
            .collect::<Vec<_>>();
        if !referencing.is_empty() {
            bail!(
                "profile {profile_id:?} is referenced by behavior(s) {}; point them elsewhere first",
                referencing.join(", ")
            );
        }

        let result = async {
            let deleted =
                mutations::delete_inference_profile(self.node.as_ref(), profile_id).await?;
            if deleted == 0 {
                bail!("no InferenceProfile document with profile_id {profile_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete inference profile",
            "config_profile_delete",
            profile_id,
            source_agent_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.inference_profiles,
                    &mut rows.inference_profile_source_agent_dids,
                    source_agent_did,
                    false,
                    |row| row.profile_id == profile_id,
                );
            },
        )
        .await
    }

    pub async fn delete_tool_selection(
        &self,
        selection_id: &str,
        source_agent_did: &str,
    ) -> Result<()> {
        let selection_id = normalize_required("selection_id", selection_id)?;
        let source_agent_did = normalize_required("source_agent_did", source_agent_did)?;
        let snapshot = self.store.snapshot();
        if !snapshot.tool_selections.iter().any(|row| {
            row.selection_id == selection_id && row.agent_did.as_deref() == Some(source_agent_did)
        }) {
            bail!("no ToolSelection document with selection_id {selection_id:?}");
        }
        let referencing = snapshot
            .behaviors
            .iter()
            .filter(|row| {
                row.agent_did.as_deref() == Some(source_agent_did)
                    && row.tool_selection_id.as_deref() == Some(selection_id)
            })
            .map(|row| row.behavior_id.clone())
            .collect::<Vec<_>>();
        if !referencing.is_empty() {
            bail!(
                "tool selection {selection_id:?} is referenced by behavior(s) {}; point them elsewhere first",
                referencing.join(", ")
            );
        }

        let result = async {
            let deleted = mutations::delete_tool_selection(
                self.node.as_ref(),
                source_agent_did,
                selection_id,
            )
            .await?;
            if deleted == 0 {
                bail!("no ToolSelection document with selection_id {selection_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete tool selection",
            "config_tool_selection_delete",
            selection_id,
            source_agent_did,
            |rows| {
                rows.tool_selections.retain(|row| {
                    row.selection_id != selection_id
                        || row.agent_did.as_deref() != Some(source_agent_did)
                });
            },
        )
        .await
    }

    pub async fn delete_tool_service(
        &self,
        service_id: &str,
        source_agent_did: &str,
    ) -> Result<()> {
        let service_id = normalize_required("service_id", service_id)?;
        let source_agent_did = normalize_required("source_agent_did", source_agent_did)?;
        let snapshot = self.store.snapshot();
        if !snapshot
            .tool_service_registries
            .iter()
            .enumerate()
            .any(|(index, row)| {
                row.service_id == service_id
                    && row_matches_source(
                        &snapshot.tool_service_registry_source_agent_dids,
                        index,
                        source_agent_did,
                        false,
                    )
            })
        {
            bail!("no ToolServiceRegistry document with service_id {service_id:?}");
        }
        let referencing = snapshot
            .tool_selections
            .iter()
            .filter(|row| {
                row.agent_did.as_deref() == Some(source_agent_did)
                    && row
                        .allowed_mcp_service_ids
                        .iter()
                        .any(|id| id == service_id)
            })
            .map(|row| row.selection_id.clone())
            .collect::<Vec<_>>();
        if !referencing.is_empty() {
            bail!(
                "tool service {service_id:?} is allowed by tool selection(s) {}; remove it there first",
                referencing.join(", ")
            );
        }

        let result = async {
            let deleted =
                mutations::delete_tool_service_registry(self.node.as_ref(), service_id).await?;
            if deleted == 0 {
                bail!("no ToolServiceRegistry document with service_id {service_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete tool service",
            "config_tool_service_delete",
            service_id,
            source_agent_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.tool_service_registries,
                    &mut rows.tool_service_registry_source_agent_dids,
                    source_agent_did,
                    false,
                    |row| row.service_id == service_id,
                );
            },
        )
        .await
    }

    pub async fn delete_behavior(&self, behavior_id: &str, source_agent_did: &str) -> Result<()> {
        let behavior_id = normalize_required("behavior_id", behavior_id)?;
        let source_agent_did = normalize_required("source_agent_did", source_agent_did)?;
        let snapshot = self.store.snapshot();
        if !snapshot.behaviors.iter().any(|row| {
            row.behavior_id == behavior_id && row.agent_did.as_deref() == Some(source_agent_did)
        }) {
            bail!("no AgentBehavior document with behavior_id {behavior_id:?}");
        }
        let is_default = snapshot.agent_principals.iter().any(|principal| {
            principal.agent_did == source_agent_did
                && principal.default_behavior_id.as_deref() == Some(behavior_id)
        });
        if is_default {
            bail!(
                "behavior {behavior_id:?} is the agent's default behavior; make another behavior the default first"
            );
        }
        let referencing = snapshot
            .tasks
            .iter()
            .enumerate()
            .filter(|(index, task)| {
                task.behavior_id.as_deref() == Some(behavior_id)
                    && row_matches_source(
                        &snapshot.task_source_agent_dids,
                        *index,
                        source_agent_did,
                        false,
                    )
            })
            .map(|(_index, task)| task.task_id.clone())
            .collect::<Vec<_>>();
        if !referencing.is_empty() {
            bail!(
                "behavior {behavior_id:?} is referenced by task(s) {}; repoint or delete those first",
                referencing.join(", ")
            );
        }
        let subagent_referencing = tool_selections_referencing_behavior(
            &snapshot.tool_selections,
            source_agent_did,
            behavior_id,
        );
        if !subagent_referencing.is_empty() {
            bail!(
                "behavior {behavior_id:?} is a subagent target of tool selection(s) {}; remove it there first",
                subagent_referencing.join(", ")
            );
        }

        let result = async {
            let deleted =
                mutations::delete_agent_behavior(self.node.as_ref(), source_agent_did, behavior_id)
                    .await?;
            if deleted == 0 {
                bail!("no AgentBehavior document with behavior_id {behavior_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete behavior",
            "config_behavior_delete",
            behavior_id,
            source_agent_did,
            |rows| {
                rows.behaviors.retain(|row| {
                    row.behavior_id != behavior_id
                        || row.agent_did.as_deref() != Some(source_agent_did)
                });
            },
        )
        .await
    }

    async fn finish_automation_delete(
        &self,
        result: Result<()>,
        action_label: &str,
        action: &str,
        row_id: &str,
        source_agent_did: &str,
        prune: impl FnOnce(&mut ClientStoreRows),
    ) -> Result<()> {
        match result {
            Ok(()) => {
                let refresh_result = self.refresh_config_source(source_agent_did).await;
                complete_confirmed_delete(
                    self.store.as_ref(),
                    &self.last_mutation_error,
                    refresh_result,
                    action_label,
                    action,
                    row_id,
                    prune,
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error(action_label, error)),
        }
    }

    async fn refresh_config_source(&self, source_agent_did: &str) -> Result<u64> {
        match self.refresh_agent(source_agent_did).await? {
            Some(version) => Ok(version),
            None => self.refresh_store().await,
        }
    }

    pub async fn resend_request(&self, stale_request_id: &str) -> Result<SubmittedRequest> {
        let snapshot = self.store.snapshot();
        let selected_agent_did = self.selected_agent_did();
        let mut candidates = snapshot
            .requests
            .iter()
            .filter(|row| row.request_id == stale_request_id)
            .filter(|row| {
                selected_agent_did
                    .as_deref()
                    .is_none_or(|did| row.agent_did.as_deref() == Some(did))
            });
        let stale = candidates
            .next()
            .ok_or_else(|| anyhow::anyhow!("request {stale_request_id} not found"))?;
        if candidates.next().is_some() {
            bail!("request {stale_request_id} is ambiguous across the selected agent scope");
        }
        let agent_did = stale
            .agent_did
            .as_deref()
            .context("stale request has no agent_did")?;
        let peer_record = self
            .peer_record_for_chat_write(agent_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(agent_did, peer_record.as_ref(), Utc::now())?;
        let (signer, admission, requester_did) = self
            .request_authority(agent_did, peer_record.as_ref())
            .await?;
        let result = mutations::resend_request(
            self.node.as_ref(),
            snapshot.as_ref(),
            stale_request_id,
            agent_did,
            &requester_did,
            signer.as_ref(),
            admission,
        )
        .await;
        match result {
            Ok(result) => {
                self.store
                    .set_focused_request_id(Some(result.request_id.clone()));
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
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
        let snapshot = self.store.snapshot();
        let selected_agent_did = self.selected_agent_did();
        let mut requests = snapshot
            .requests
            .iter()
            .filter(|request| request.request_id == request_id)
            .filter(|request| {
                selected_agent_did
                    .as_deref()
                    .is_none_or(|selected| request.agent_did.as_deref() == Some(selected))
            });
        let request = requests
            .next()
            .with_context(|| format!("request {request_id} is absent from the selected agent"))?;
        anyhow::ensure!(
            requests.next().is_none(),
            "request {request_id} is ambiguous across the selected agent scope"
        );
        let agent_did = request
            .agent_did
            .as_deref()
            .context("request has no agent_did")?;
        let peer_record = self
            .peer_record_for_chat_write(agent_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(agent_did, peer_record.as_ref(), Utc::now())?;
        match mutations::interrupt_request(self.node.as_ref(), request_id).await {
            Ok(()) => {
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
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
        let agent_did = parent
            .agent_did
            .as_deref()
            .context("retry parent has no agent_did")?;
        let peer_record = self
            .peer_record_for_chat_write(agent_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(agent_did, peer_record.as_ref(), Utc::now())?;
        let (signer, admission, requester_did) = self
            .request_authority(agent_did, peer_record.as_ref())
            .await?;
        let result = mutations::retry_request(
            self.node.as_ref(),
            snapshot.as_ref(),
            parent,
            &requester_did,
            signer.as_ref(),
            admission,
        )
        .await;
        match result {
            Ok(result) => {
                self.store
                    .set_focused_request_id(Some(result.request_id.clone()));
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    action = "chat_retry",
                    row_id = %result.request_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error("retry request", error)),
        }
    }

    pub async fn rename_peer(&self, peer_id: &str, label: &str) -> Result<()> {
        let peer_id = normalize_required("peer_id", peer_id)?;
        let label = normalize_required("label", label)?;
        let record = self
            .sync_state
            .records()
            .iter()
            .find(|record| record.peer_id == peer_id)
            .cloned()
            .with_context(|| format!("peer {peer_id} not found"))?;
        let mut renamed = record.clone();
        renamed.label = label.to_string();
        self.sync_state
            .replace_record(&record, renamed)
            .await?
            .with_context(|| format!("peer {peer_id} changed while it was being renamed"))?;
        self.clear_mutation_error();
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn update_peer_status(&self, status: ClientPeerStatus) {
        if let Some(expected) = self
            .sync_state
            .records()
            .into_iter()
            .find(|record| record.peer_id == status.peer_id)
        {
            self.sync_state.replace_peer(&expected, status);
        }
    }

    pub async fn remove_peer(&self, peer_id: &str) -> Result<PeerMutationResult> {
        let peer_id = normalize_required("peer_id", peer_id)?;
        let removal = self
            .route_manager
            .remove_peer(&self.sync_state, peer_id)
            .await?;
        let removed = removal.record;
        let warning = match removal.cleanup_error {
            None => None,
            Some(error) => {
                let warning = format!(
                    "deployment hidden locally; route teardown is pending and will retry: {error}"
                );
                tracing::warn!(
                    target: "gents_desktop_core::peer",
                    peer_id = %removed.peer_id,
                    label = %removed.label,
                    error = %error,
                    "desktop deployment removal queued for retry"
                );
                Some(warning)
            }
        };

        self.clear_mutation_error();
        tracing::info!(
            target: "gents_desktop_core::peer",
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
        let result = mutations::upsert_agent_behavior(self.node.as_ref(), row).await;
        match result {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
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
        let result = mutations::upsert_agent_principal(self.node.as_ref(), row).await;
        match result {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
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
                    target: "gents_desktop_core::writes",
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
        let result = mutations::upsert_tool_selection(self.node.as_ref(), row).await;
        match result {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
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
                    target: "gents_desktop_core::writes",
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
                    target: "gents_desktop_core::writes",
                    doc_type = "inference_profile",
                    row_id = %row.profile_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save inference profile", error)),
        }
    }

    pub async fn save_task(&self, row: &TaskRow) -> Result<()> {
        match mutations::upsert_task(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "task",
                    row_id = %row.task_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save task", error)),
        }
    }

    pub async fn save_skill(&self, row: &SkillRow) -> Result<()> {
        match mutations::upsert_skill(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "skill",
                    row_id = %row.skill_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save skill", error)),
        }
    }

    pub async fn save_schedule(&self, row: &ScheduleRow) -> Result<()> {
        match mutations::upsert_schedule(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "schedule",
                    row_id = %row.schedule_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save schedule", error)),
        }
    }

    pub async fn save_event_trigger(&self, row: &EventTriggerRow) -> Result<()> {
        match mutations::upsert_event_trigger(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "event_trigger",
                    row_id = %row.trigger_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save event trigger", error)),
        }
    }

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
                    target: "gents_desktop_core::writes",
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

    pub async fn fire_schedule_now(&self, row: &ScheduleRow) -> Result<String> {
        match mutations::fire_schedule_now(self.node.as_ref(), row).await {
            Ok(doc_id) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
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

    /// Observe hydration for a selected session and start its first request.
    ///
    /// A rejected request is terminal here. Retrying it is an explicit user
    /// action owned by [`Self::retry_session_hydration`].
    pub async fn ensure_session_hydration_started(
        &self,
        session_id: &str,
        agent_did: &str,
    ) -> Result<()> {
        let session_id = normalize_required("session_id", session_id)?;
        let agent_did = normalize_required("agent_did", agent_did)?;
        let _transition = self.hydration_transition.lock().await;
        let progress = self
            .session_hydration_progress(&session_id, &agent_did)
            .await?;
        if !should_start_session_hydration_request(&progress, &session_id, &agent_did) {
            return Ok(());
        }
        let evidence = load_local_hydration_start_evidence(
            self.node.as_ref(),
            self.principal.did(),
            &session_id,
            &agent_did,
        )
        .await?;
        if !hydration_start_evidence_is_ready(&progress, &evidence) {
            return Ok(());
        }
        self.request_session_hydration(&session_id, &agent_did)
            .await
    }

    /// Derive receiver progress from durable rows for one exact target.
    pub async fn session_hydration_progress(
        &self,
        session_id: &str,
        agent_did: &str,
    ) -> Result<gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress> {
        let session_id = normalize_required("session_id", session_id)?;
        let agent_did = normalize_required("agent_did", agent_did)?;
        self.load_hydration_progress(&session_id, &agent_did).await
    }

    /// Explicitly restart a failed hydration attempt for one session.
    pub async fn retry_session_hydration(&self, session_id: &str, agent_did: &str) -> Result<()> {
        let session_id = normalize_required("session_id", session_id)?;
        let agent_did = normalize_required("agent_did", agent_did)?;
        let _transition = self.hydration_transition.lock().await;
        let progress = self
            .load_hydration_progress(&session_id, &agent_did)
            .await?;
        if !gents::agent::p2p_reconcile::session_hydration::can_retry_hydration(
            &progress,
            &session_id,
            &agent_did,
        ) {
            bail!("session hydration retry requires a failed attempt for the selected session");
        }
        self.request_session_hydration(&session_id, &agent_did)
            .await
    }

    async fn request_session_hydration(&self, session_id: &str, agent_did: &str) -> Result<()> {
        let session_id = normalize_required("session_id", session_id)?;
        let agent_did = normalize_required("agent_did", agent_did)?;
        let peer_record = self
            .peer_record_for_chat_write(&agent_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(&agent_did, peer_record.as_ref(), Utc::now())?;
        let request_key = format!("{}:{session_id}", self.local_peer_id());
        let requester_did = gents::graphql::escape_graphql_string(self.principal.did());
        let agent_did_gql = gents::graphql::escape_graphql_string(&agent_did);
        let session_id_gql = gents::graphql::escape_graphql_string(&session_id);
        let request_key_gql = gents::graphql::escape_graphql_string(&request_key);
        let now = gents::graphql::escape_graphql_string(
            &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        let mutation = format!(
            r#"mutation {{
                upsert_SessionHydrationRequest(
                    filter: {{ request_key: {{ _eq: "{request_key_gql}" }} }},
                    add: {{
                        request_key: "{request_key_gql}",
                        requester_did: "{requester_did}",
                        agent_did: "{agent_did_gql}",
                        session_id: "{session_id_gql}",
                        created_at: "{now}",
                        status: "pending",
                        status_detail: "",
                        served_doc_count: 0
                    }},
                    update: {{
                        status: "pending",
                        status_detail: "",
                        served_doc_count: 0,
                        served_manifest_json: "",
                        outcome_signer_did: "",
                        outcome_signature: "",
                        processed_at: null
                    }}
                ) {{ _docID }}
            }}"#
        );
        let response = self.node.execute(&mutation).await;
        if !response.errors.is_empty() {
            anyhow::bail!("upsert SessionHydrationRequest: {:?}", response.errors);
        }
        Ok(())
    }

    async fn load_hydration_progress(
        &self,
        session_id: &str,
        agent_did: &str,
    ) -> Result<gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress> {
        let merged = load_local_hydration_documents(
            self.node.as_ref(),
            self.principal.did(),
            session_id,
            agent_did,
        )
        .await?;
        let request = load_hydration_server_state(
            self.node.as_ref(),
            self.local_peer_id(),
            self.principal.did(),
            session_id,
            agent_did,
            &self.principal,
        )
        .await?;
        Ok(
            gents::agent::p2p_reconcile::session_hydration::project_durable_hydration_progress(
                session_id, agent_did, merged, request,
            ),
        )
    }

    pub(super) fn clear_mutation_error(&self) {
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

#[derive(Deserialize)]
struct HydrationDocIdRow {
    #[serde(rename = "_docID")]
    doc_id: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct LocalHydrationStartEvidence {
    owned_session_present: bool,
    nonterminal_request_present: bool,
}

#[derive(Deserialize)]
struct HydrationLifecycleRow {
    lifecycle_state: Option<String>,
}

fn should_start_session_hydration_request(
    progress: &gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress,
    session_id: &str,
    agent_did: &str,
) -> bool {
    use gents::agent::p2p_reconcile::session_hydration::ClientHydrationPhase;
    progress.session_id == session_id
        && progress.agent_did == agent_did
        && progress.phase == ClientHydrationPhase::Idle
}

fn hydration_start_evidence_is_ready(
    progress: &gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress,
    evidence: &LocalHydrationStartEvidence,
) -> bool {
    (evidence.owned_session_present || progress.merged_count > 0)
        && !evidence.nonterminal_request_present
}

async fn load_local_hydration_start_evidence(
    node: &EmbeddedNode,
    requester_did: &str,
    session_id: &str,
    agent_did: &str,
) -> Result<LocalHydrationStartEvidence> {
    let requester_did = gents::graphql::escape_graphql_string(requester_did);
    let session_id = gents::graphql::escape_graphql_string(session_id);
    let agent_did = gents::graphql::escape_graphql_string(agent_did);
    let scope = format!(
        "requester_did: {{ _eq: \"{requester_did}\" }}, agent_did: {{ _eq: \"{agent_did}\" }}, session_id: {{ _eq: \"{session_id}\" }}"
    );
    let response = node
        .execute(&format!(
            r#"{{
                AgentSession(filter: {{ {scope} }}, limit: 1) {{ _docID }}
                AgentRequest(filter: {{ {scope} }}) {{ lifecycle_state }}
            }}"#
        ))
        .await;
    gents::graphql::ensure_no_errors(&response, "query local hydration start evidence")?;
    let owned_session_present =
        !gents::graphql::rows::<HydrationDocIdRow>(&response, "AgentSession")?.is_empty();
    let nonterminal_request_present =
        gents::graphql::rows::<HydrationLifecycleRow>(&response, "AgentRequest")?
            .iter()
            .any(|row| !is_terminal_lifecycle_state(row.lifecycle_state.as_deref()));
    Ok(LocalHydrationStartEvidence {
        owned_session_present,
        nonterminal_request_present,
    })
}

async fn load_local_hydration_documents(
    node: &EmbeddedNode,
    requester_did: &str,
    session_id: &str,
    agent_did: &str,
) -> Result<BTreeSet<SessionHydrationDocumentKey>> {
    let query = local_hydration_query(requester_did, session_id, agent_did);
    let response = node.execute(&query).await;
    gents::graphql::ensure_no_errors(&response, "query local session hydration documents")?;
    local_hydration_documents_from_response(&response)
}

fn local_hydration_query(requester_did: &str, session_id: &str, agent_did: &str) -> String {
    let requester_did = gents::graphql::escape_graphql_string(requester_did);
    let session_id = gents::graphql::escape_graphql_string(session_id);
    let agent_did = gents::graphql::escape_graphql_string(agent_did);
    let scope = format!(
        "requester_did: {{ _eq: \"{requester_did}\" }}, agent_did: {{ _eq: \"{agent_did}\" }}, session_id: {{ _eq: \"{session_id}\" }}"
    );
    format!(
        r#"{{
            AgentRequest(filter: {{ {scope} }}) {{ _docID }}
            AgentResponse(filter: {{ {scope} }}) {{ _docID }}
            AgentMessage(filter: {{ {scope} }}) {{ _docID }}
            AgentToolCall(filter: {{ {scope} }}) {{ _docID }}
            AgentToolResult(filter: {{ {scope} }}) {{ _docID }}
            CompactionEntry(filter: {{ {scope} }}) {{ _docID }}
        }}"#
    )
}

fn local_hydration_documents_from_response(
    response: &defra_node::QueryResponse,
) -> Result<BTreeSet<SessionHydrationDocumentKey>> {
    let mut ids = BTreeSet::new();
    for collection in [
        "AgentRequest",
        "AgentResponse",
        "AgentMessage",
        "AgentToolCall",
        "AgentToolResult",
        "CompactionEntry",
    ] {
        for row in gents::graphql::rows::<HydrationDocIdRow>(response, collection)? {
            if let Some(doc_id) = row.doc_id.filter(|value| !value.is_empty()) {
                ids.insert(SessionHydrationDocumentKey {
                    collection: collection.to_string(),
                    doc_id,
                });
            }
        }
    }
    Ok(ids)
}

async fn load_hydration_server_state(
    node: &EmbeddedNode,
    peer_id: &str,
    requester_did: &str,
    session_id: &str,
    agent_did: &str,
    principal: &super::super::principal_identity::PrincipalIdentity,
) -> Result<gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState> {
    use gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState;
    let expected_request_key = format!("{peer_id}:{session_id}");
    let request_key = gents::graphql::escape_graphql_string(&expected_request_key);
    let query = format!(
        r#"{{ SessionHydrationRequest(filter: {{ request_key: {{ _eq: "{request_key}" }} }}) {{
            request_key requester_did agent_did session_id status status_detail
            served_doc_count served_manifest_json processed_at
            outcome_signer_did outcome_signature
        }} }}"#
    );
    let response = node.execute(&query).await;
    gents::graphql::ensure_no_errors(&response, "query session hydration request")?;
    let Some(data) = response.data else {
        return Ok(ClientHydrationRequestState::Missing);
    };
    let rows = data
        .get("SessionHydrationRequest")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    if rows.len() > 1 {
        bail!("session hydration request key resolved to multiple rows");
    }
    let Some(row) = rows.first() else {
        return Ok(ClientHydrationRequestState::Missing);
    };
    for (field, expected) in [
        ("request_key", expected_request_key.as_str()),
        ("requester_did", requester_did),
        ("agent_did", agent_did),
        ("session_id", session_id),
    ] {
        if row.get(field).and_then(|value| value.as_str()) != Some(expected) {
            bail!("session hydration request {field} does not match the selected target");
        }
    }
    let status = row
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if status == "pending" {
        return Ok(ClientHydrationRequestState::Pending);
    }
    if !matches!(status, "served" | "rejected") {
        bail!("unknown session hydration request status {status:?}");
    }
    let served_count = row
        .get("served_doc_count")
        .and_then(|value| value.as_i64())
        .map(usize::try_from)
        .transpose()
        .context("session hydration served_doc_count must be non-negative")?;
    let manifest = decode_manifest_json(
        row.get("served_manifest_json")
            .and_then(|value| value.as_str())
            .context("terminal hydration request is missing served_manifest_json")?,
    )?;
    if served_count != Some(manifest.len()) {
        bail!("session hydration served_doc_count does not match signed manifest");
    }
    let signature = bs58::decode(
        row.get("outcome_signature")
            .and_then(|value| value.as_str())
            .context("terminal hydration request is missing outcome_signature")?,
    )
    .into_vec()
    .context("decode session hydration receipt signature")?;
    let receipt = SessionHydrationReceipt {
        version: SESSION_HYDRATION_RECEIPT_VERSION,
        request_key: expected_request_key,
        requester_did: requester_did.to_string(),
        agent_did: agent_did.to_string(),
        session_id: session_id.to_string(),
        status: status.to_string(),
        status_detail: row
            .get("status_detail")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        served_manifest: manifest,
        processed_at: row
            .get("processed_at")
            .and_then(|value| value.as_str())
            .context("terminal hydration request is missing processed_at")?
            .to_string(),
        signer_did: row
            .get("outcome_signer_did")
            .and_then(|value| value.as_str())
            .context("terminal hydration request is missing outcome_signer_did")?
            .to_string(),
        signature,
    };
    receipt.validate_shape()?;
    if !principal
        .verify(agent_did, &receipt.signing_payload()?, &receipt.signature)
        .await?
    {
        bail!("session hydration receipt signature is invalid");
    }
    let documents = receipt.served_manifest.into_iter().collect::<BTreeSet<_>>();
    match status {
        "served" => Ok(ClientHydrationRequestState::Served(documents)),
        "rejected" => Ok(ClientHydrationRequestState::Rejected(Some(documents))),
        _ => unreachable!(),
    }
}

fn retain_rows_with_sources<T>(
    rows: &mut Vec<T>,
    sources: &mut Vec<Option<String>>,
    mut keep: impl FnMut(&T) -> bool,
) {
    let mut kept_rows = Vec::with_capacity(rows.len());
    let mut kept_sources = Vec::with_capacity(rows.len());

    for (index, row) in rows.drain(..).enumerate() {
        if keep(&row) {
            kept_rows.push(row);
            kept_sources.push(sources.get(index).cloned().unwrap_or_default());
        }
    }

    *rows = kept_rows;
    *sources = kept_sources;
}

fn prune_deleted_skill_rows(rows: &mut ClientStoreRows, agent_did: &str, skill_id: &str) {
    retain_rows_with_sources(&mut rows.skills, &mut rows.skill_source_agent_dids, |row| {
        !(row.skill_id == skill_id && row.agent_did.as_deref() == Some(agent_did))
    });

    for behavior in rows
        .behaviors
        .iter_mut()
        .filter(|row| row.agent_did.as_deref() == Some(agent_did))
    {
        behavior.skill_refs.retain(|id| id != skill_id);
        behavior.skill_excludes.retain(|id| id != skill_id);
    }
}

fn tool_selections_referencing_behavior(
    selections: &[ToolSelectionRow],
    agent_did: &str,
    behavior_id: &str,
) -> Vec<String> {
    let mut referencing = selections
        .iter()
        .filter(|selection| selection.agent_did.as_deref() == Some(agent_did))
        .filter(|selection| {
            selection.subagent_targets.iter().any(|entry| {
                let Ok(target) = serde_json::from_str::<serde_json::Value>(entry) else {
                    return false;
                };
                target.get("agent_did").and_then(serde_json::Value::as_str) == Some(agent_did)
                    && target
                        .get("behavior_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(behavior_id)
            })
        })
        .map(|selection| selection.selection_id.clone())
        .collect::<Vec<_>>();
    referencing.sort();
    referencing.dedup();
    referencing
}

fn complete_confirmed_delete(
    store: &ObservedStore,
    last_mutation_error: &StdRwLock<Option<String>>,
    refresh_result: Result<u64>,
    action_label: &str,
    action: &str,
    row_id: &str,
    prune: impl FnOnce(&mut ClientStoreRows),
) {
    match refresh_result {
        Ok(_) => {
            *last_mutation_error
                .write()
                .expect("mutation error lock poisoned") = None;
        }
        Err(error) => {
            let warning = format!(
                "{action_label} succeeded, but refreshing the source snapshot failed: {error}"
            );
            *last_mutation_error
                .write()
                .expect("mutation error lock poisoned") = Some(warning);
            tracing::warn!(
                target: "gents_desktop_core::writes",
                action = %action,
                row_id = %row_id,
                error = %error,
                "desktop write saved, but refreshing the source snapshot failed"
            );
        }
    }

    let mut rows = store.snapshot().to_rows();
    prune(&mut rows);
    store.replace_snapshot(ClientStore::from_rows(rows));

    tracing::info!(
        target: "gents_desktop_core::writes",
        action = %action,
        row_id = %row_id,
        "desktop write saved"
    );
}

#[cfg(test)]
mod delete_source_tests {
    use super::*;
    use anyhow::anyhow;
    use serde_json::json;

    #[test]
    fn hydration_count_matches_all_client_routable_server_collections() {
        let response = defra_node::QueryResponse::success(json!({
            "AgentRequest": [{ "_docID": "request-1" }],
            "AgentResponse": [{ "_docID": "response-1" }],
            "AgentMessage": [{ "_docID": "message-1" }],
            "AgentToolCall": [{ "_docID": "tool-call-1" }],
            "AgentToolResult": [{ "_docID": "tool-result-1" }],
            "CompactionEntry": [{ "_docID": "compaction-1" }]
        }));

        assert_eq!(
            local_hydration_documents_from_response(&response)
                .unwrap()
                .len(),
            6
        );
    }

    #[test]
    fn initial_hydration_request_is_written_only_for_observed_idle_target() {
        use gents::agent::p2p_reconcile::session_hydration::{
            ClientHydrationPhase, ClientHydrationProgress,
        };

        let serving = ClientHydrationProgress {
            session_id: "session-1".into(),
            agent_did: "did:agent".into(),
            phase: ClientHydrationPhase::Serving,
            merged_count: 3,
            served_count: Some(8),
            ..ClientHydrationProgress::default()
        };
        assert!(!should_start_session_hydration_request(
            &serving,
            "session-1",
            "did:agent",
        ));
        assert!(!should_start_session_hydration_request(
            &serving,
            "session-2",
            "did:agent",
        ));

        let failed = ClientHydrationProgress {
            phase: ClientHydrationPhase::Failed,
            ..serving.clone()
        };
        assert!(!should_start_session_hydration_request(
            &failed,
            "session-1",
            "did:agent",
        ));

        let complete = ClientHydrationProgress {
            phase: ClientHydrationPhase::Complete,
            merged_count: 8,
            served_count: Some(8),
            ..serving
        };
        assert!(!should_start_session_hydration_request(
            &complete,
            "session-1",
            "did:agent",
        ));
        let idle = ClientHydrationProgress {
            session_id: "session-1".into(),
            agent_did: "did:agent".into(),
            ..ClientHydrationProgress::default()
        };
        assert!(should_start_session_hydration_request(
            &idle,
            "session-1",
            "did:agent",
        ));
        assert!(!hydration_start_evidence_is_ready(
            &idle,
            &LocalHydrationStartEvidence::default(),
        ));
        assert!(hydration_start_evidence_is_ready(
            &ClientHydrationProgress {
                merged_count: 1,
                ..idle.clone()
            },
            &LocalHydrationStartEvidence::default(),
        ));
        assert!(hydration_start_evidence_is_ready(
            &idle,
            &LocalHydrationStartEvidence {
                owned_session_present: true,
                nonterminal_request_present: false,
            },
        ));
        assert!(!hydration_start_evidence_is_ready(
            &ClientHydrationProgress {
                merged_count: 1,
                ..idle
            },
            &LocalHydrationStartEvidence {
                owned_session_present: true,
                nonterminal_request_present: true,
            },
        ));
    }

    #[test]
    fn hydration_query_escapes_every_scope_value() {
        let query = local_hydration_query(
            "did:key:requester\"escaped",
            "session\"escaped",
            "did:key:agent\"escaped",
        );
        assert!(query.contains(r#"requester_did: { _eq: "did:key:requester\"escaped" }"#));
        assert!(query.contains(r#"session_id: { _eq: "session\"escaped" }"#));
        assert!(query.contains(r#"agent_did: { _eq: "did:key:agent\"escaped" }"#));
    }

    fn task(task_id: &str) -> TaskRow {
        serde_json::from_value(json!({ "task_id": task_id })).expect("task row")
    }

    fn tool_selection(
        selection_id: &str,
        agent_did: &str,
        subagent_targets: Vec<String>,
    ) -> ToolSelectionRow {
        serde_json::from_value(json!({
            "selection_id": selection_id,
            "agent_did": agent_did,
            "subagent_targets": subagent_targets,
        }))
        .expect("tool selection row")
    }

    fn peer_record(source: Option<&str>) -> PeerRecord {
        let mut record = PeerRecord::new("Amy", "endpoint-amy", "did:key:amy");
        record.source = source.map(str::to_owned);
        if source == Some("enrollment") {
            record.pairing_network_id = Some("network-a".into());
            record.enrollment_request_digest = Some("digest-a".into());
            record.enrollment_authorization_sequence = Some(1);
            record.enrollment_authorization_expires_at = Some("2099-09-29T00:00:00Z".into());
        }
        record
    }

    #[test]
    fn only_the_requested_behavior_is_forwarded_to_the_db_binding() {
        let mut peer = peer_record(Some("enrollment"));
        peer.pairing_ready = true;

        ensure_peer_chat_ready_at(&peer.agent_did, Some(&peer), Utc::now()).unwrap();
        assert_eq!(behavior_id_for_write(None), None);
        assert_eq!(
            behavior_id_for_write(Some(" review ")).as_deref(),
            Some("review")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn expired_enrollment_cannot_admit_a_request_without_waiting_for_the_sweep() {
        let mut peer = peer_record(Some("enrollment"));
        peer.pairing_ready = true;
        peer.enrollment_authorization_expires_at = Some("2026-08-30T12:00:01Z".into());
        let before = "2026-08-30T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let expired = "2026-08-30T12:00:01Z".parse::<DateTime<Utc>>().unwrap();

        assert!(ensure_peer_chat_ready_at(&peer.agent_did, Some(&peer), before).is_ok());
        assert!(ensure_peer_chat_ready_at(&peer.agent_did, Some(&peer), expired).is_err());
        assert!(
            peer_record_owning_agent_at(std::slice::from_ref(&peer), &peer.agent_did, expired,)
                .is_none()
        );
    }

    #[test]
    fn machine_pairing_never_claims_an_unlisted_child_agent() {
        let mut machine = peer_record(Some("enrollment"));
        machine.pairing_template = Some("machine".to_string());
        machine.pairing_ready = true;
        assert!(peer_record_owning_agent_at(&[machine], "did:key:child", Utc::now()).is_none());
    }

    #[test]
    fn pending_enrollment_peer_rejects_chat_writes() {
        let peer = peer_record(Some("enrollment"));

        assert!(
            ensure_peer_chat_ready_at(&peer.agent_did, Some(&peer), Utc::now())
                .unwrap_err()
                .to_string()
                .contains("route is not ready")
        );
    }

    #[test]
    fn malformed_source_and_missing_owner_reject_chat_writes() {
        let peer = peer_record(None);
        assert!(ensure_peer_chat_ready_at(&peer.agent_did, Some(&peer), Utc::now()).is_err());
        assert!(
            ensure_peer_chat_ready_at("did:key:missing", None, Utc::now())
                .unwrap_err()
                .to_string()
                .contains("no saved deployment route")
        );
    }

    #[test]
    fn local_standard_is_explicitly_exempt_from_route_readiness() {
        let peer = peer_record(Some("local-standard"));
        ensure_peer_chat_ready_at(&peer.agent_did, Some(&peer), Utc::now()).unwrap();
    }

    #[test]
    fn source_matching_distinguishes_remote_rows_from_local_rows() {
        let sources = vec![None, Some("did:remote".to_string())];

        assert!(row_matches_source(&sources, 0, "did:local", false));
        assert!(!row_matches_source(&sources, 0, "did:remote", true));
        assert!(row_matches_source(&sources, 1, "did:remote", true));
        assert!(!row_matches_source(&sources, 1, "did:other", true));
    }

    #[test]
    fn sourced_pruning_preserves_same_id_rows_and_parallel_attribution() {
        let mut rows = vec!["shared", "shared", "other"];
        let mut sources = vec![
            None,
            Some("did:remote".to_string()),
            Some("did:remote".to_string()),
        ];

        retain_sourced_rows(&mut rows, &mut sources, "did:remote", true, |row| {
            *row == "shared"
        });

        assert_eq!(rows, vec!["shared", "other"]);
        assert_eq!(sources, vec![None, Some("did:remote".to_string())]);
    }

    #[test]
    fn confirmed_delete_prunes_locally_and_warns_when_refresh_fails() {
        let rows = ClientStoreRows {
            tasks: vec![task("deleted"), task("retained")],
            task_source_agent_dids: vec![None, Some("did:key:remote".to_string())],
            ..ClientStoreRows::default()
        };
        let (store, _version_rx) = ObservedStore::new(ClientStore::from_rows(rows));
        let last_mutation_error = StdRwLock::new(None);

        complete_confirmed_delete(
            store.as_ref(),
            &last_mutation_error,
            Err(anyhow!("replica unavailable")),
            "delete task",
            "config_task_delete",
            "deleted",
            |rows| {
                retain_rows_with_sources(
                    &mut rows.tasks,
                    &mut rows.task_source_agent_dids,
                    |row| row.task_id != "deleted",
                );
            },
        );

        let snapshot = store.snapshot();
        assert_eq!(snapshot.tasks.len(), 1);
        assert_eq!(snapshot.tasks[0].task_id, "retained");
        assert_eq!(
            snapshot.task_source_agent_dids,
            vec![Some("did:key:remote".to_string())]
        );
        assert_eq!(
            last_mutation_error
                .read()
                .expect("mutation error lock poisoned")
                .as_deref(),
            Some(
                "delete task succeeded, but refreshing the source snapshot failed: replica unavailable"
            )
        );
    }

    #[test]
    fn subagent_behavior_references_are_scoped_to_the_owning_agent() {
        let local_target = json!({
            "name": "local",
            "agent_did": "did:key:alpha",
            "behavior_id": "research",
        })
        .to_string();
        let remote_target = json!({
            "name": "remote",
            "agent_did": "did:key:beta",
            "behavior_id": "research",
        })
        .to_string();
        let selections = vec![
            tool_selection("alpha-local", "did:key:alpha", vec![local_target.clone()]),
            tool_selection("alpha-remote", "did:key:alpha", vec![remote_target]),
            tool_selection("beta-local", "did:key:beta", vec![local_target]),
        ];

        assert_eq!(
            tool_selections_referencing_behavior(&selections, "did:key:alpha", "research"),
            vec!["alpha-local"]
        );
        assert!(
            tool_selections_referencing_behavior(&selections, "did:key:alpha", "writer").is_empty()
        );
    }
}

fn strip_cli_operator_hints(message: &str) -> String {
    message
        .lines()
        .filter(|line| {
            !line.contains("gents init")
                && !line.contains("gents server")
                && !line.contains("--graphql")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

#[derive(Debug, Clone)]
pub struct NetworkStatus {
    pub local_peer_id: std::result::Result<String, String>,
    pub listen_addresses: std::result::Result<Vec<String>, String>,
    pub connected_peers: std::result::Result<Vec<String>, String>,
    pub replicators: std::result::Result<Vec<NetworkReplicator>, String>,
    pub saved_peers: Vec<super::super::peer_directory::PeerRecord>,
}

#[derive(Debug, Clone)]
pub struct NetworkReplicator {
    pub peer_id: Option<String>,
    pub address: Option<String>,
    pub collections: Vec<String>,
    pub status: Option<u8>,
    pub last_status_change: Option<String>,
}
