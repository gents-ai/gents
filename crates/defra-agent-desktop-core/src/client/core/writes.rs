use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use defra_agent_protocol::row::{
    AgentBehaviorRow, AgentPrincipalRow, AgentRequestRow, EventTriggerRow, InferenceBackendRow,
    InferenceProfileRow, ScheduleRow, SkillRow, TaskRow, ToolSelectionRow, ToolServiceRegistryRow,
};
use defra_p2p_adapter::P2POperations as P2POps;

use super::super::mutations::{
    self, CreatedConversation, PeerMutationResult, SubmitRequestOptions, SubmittedRequest,
};
use super::super::observe::ObservedStore;
use super::super::peer_directory::PeerRecord;
use super::super::query::load_chat_patch_from_graphql;
use super::super::schema::subscribed_collection_names;
use super::super::store::{ClientStore, ClientStoreRows};
use super::bootstrap::{
    add_replicator_with_retry_until, branchable_pair_sync_enabled, connect_peer_with_retry_until,
    normalize_required, p2p_pairing_enabled_for_graphql, sync_branchable_collections_with_retry,
    BRANCHABLE_PAIR_SYNC_ENV, REMOTE_P2P_PAIRING_ENV,
};
use super::p2p_ops;
use super::p2p_ops::{p2p_disconnect_peer, p2p_remove_replicator};
use super::{ClientCore, ClientPeerStatus, PEER_ADD_OPERATION_TIMEOUT};

const REMOTE_REQUEST_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const REMOTE_REQUEST_REFRESH_TIMEOUT: Duration = Duration::from_secs(30 * 60);

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

    pub async fn submit_remote_graphql_request_with_options(
        &self,
        graphql: &str,
        session_id: &str,
        agent_did: &str,
        content: &str,
        behavior_id: Option<&str>,
        options: SubmitRequestOptions,
    ) -> Result<SubmittedRequest> {
        let snapshot = self.store.snapshot();
        let peer_record = self.peer_record_for_agent(agent_did).await;
        match mutations::submit_request_to_graphql(
            graphql,
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
                self.spawn_remote_request_refresh(
                    graphql.to_string(),
                    agent_did.to_string(),
                    result.request_id.clone(),
                    session_id.to_string(),
                    peer_record.clone(),
                );
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    action = "chat_submit_remote_graphql",
                    row_id = %result.request_id,
                    agent_did,
                    session_id,
                    peer_id = %peer_record.as_ref().map(|record| record.peer_id.as_str()).unwrap_or(""),
                    peer_label = %peer_record.as_ref().map(|record| record.label.as_str()).unwrap_or(""),
                    peer_addr = %peer_record.as_ref().map(|record| record.addr.as_str()).unwrap_or(""),
                    graphql,
                    "desktop remote write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error("submit remote GraphQL request", error)),
        }
    }

    fn spawn_remote_request_refresh(
        &self,
        graphql: String,
        agent_did: String,
        request_id: String,
        session_id: String,
        peer_record: Option<PeerRecord>,
    ) {
        let store = Arc::clone(&self.store);

        tokio::spawn(async move {
            let peer_id = peer_record
                .as_ref()
                .map(|record| record.peer_id.clone())
                .unwrap_or_default();
            let peer_label = peer_record
                .as_ref()
                .map(|record| record.label.clone())
                .unwrap_or_default();
            let peer_addr = peer_record
                .as_ref()
                .map(|record| record.addr.clone())
                .unwrap_or_default();
            let started = Instant::now();
            let mut last_patch_signature: Option<(usize, usize, u64)> = None;
            loop {
                if started.elapsed() >= REMOTE_REQUEST_REFRESH_TIMEOUT {
                    tracing::warn!(
                        target: "defra_agent_desktop_core::writes",
                        request_id = %request_id,
                        agent_did = %agent_did,
                        session_id = %session_id,
                        peer_id = %peer_id,
                        peer_label = %peer_label,
                        peer_addr = %peer_addr,
                        graphql = %graphql,
                        "desktop remote request refresh timed out before terminal state"
                    );
                    break;
                }

                tokio::time::sleep(REMOTE_REQUEST_REFRESH_INTERVAL).await;
                match load_chat_patch_from_graphql(&graphql, &request_id).await {
                    Ok(mut patch) => {
                        patch.stamp_source_agent_did(&agent_did);
                        let terminal = patch.request_row(&request_id).is_some_and(|row| {
                            is_terminal_lifecycle_state(row.lifecycle_state.as_deref())
                        });
                        let patch_signature = chat_patch_signature(&patch);
                        let (rows, bytes, _hash) = patch_signature;

                        if !terminal && last_patch_signature == Some(patch_signature) {
                            tracing::debug!(
                                target: "defra_agent_desktop_core::writes",
                                request_id = %request_id,
                                agent_did = %agent_did,
                                session_id = %session_id,
                                peer_id = %peer_id,
                                peer_label = %peer_label,
                                peer_addr = %peer_addr,
                                graphql = %graphql,
                                rows,
                                bytes,
                                "desktop remote request patch unchanged"
                            );
                            continue;
                        }

                        last_patch_signature = Some(patch_signature);
                        let version = store.merge_chat_patch(patch);
                        tracing::info!(
                            target: "defra_agent_desktop_core::writes",
                            request_id = %request_id,
                            agent_did = %agent_did,
                            session_id = %session_id,
                            peer_id = %peer_id,
                            peer_label = %peer_label,
                            peer_addr = %peer_addr,
                            graphql = %graphql,
                            version,
                            rows,
                            bytes,
                            terminal,
                            "desktop remote request patch merged"
                        );
                        if terminal {
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "defra_agent_desktop_core::writes",
                            request_id = %request_id,
                            agent_did = %agent_did,
                            session_id = %session_id,
                            peer_id = %peer_id,
                            peer_label = %peer_label,
                            peer_addr = %peer_addr,
                            graphql = %graphql,
                            error = %error,
                            "desktop could not load remote request patch"
                        );
                    }
                }
            }
        });
    }

    /// Fork a conversation at a user turn, routed like every per-agent
    /// operation. The caller principal must own the source session; the
    /// runtime's fork machinery enforces that and the busy/turn-range rules.
    pub async fn fork_session(
        &self,
        agent_did: &str,
        source_session_id: &str,
        at_user_turn: u32,
        target_behavior_id: Option<&str>,
    ) -> Result<defra_agent::ForkOutcome> {
        let agent_did = normalize_required("agent_did", agent_did)?;
        let source_session_id = normalize_required("source_session_id", source_session_id)?;
        let params = defra_agent::ForkParams {
            source_session_id,
            fork_at_user_turn: at_user_turn,
            caller_agent_did: agent_did,
            target_behavior_id,
        };
        let result = match self.graphql_for_agent(agent_did).await {
            Some(graphql) => defra_agent::fork_via_http(&graphql, params).await,
            None => defra_agent::fork(self.node(), params).await,
        };
        match result {
            Ok(outcome) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    action = "session_fork",
                    row_id = %outcome.session_id,
                    source_session_id,
                    "desktop write saved"
                );
                Ok(outcome)
            }
            Err(error) => {
                Err(self.record_mutation_error("fork session", anyhow::Error::from(error)))
            }
        }
    }

    /// Reconstruct a request's persisted event timeline, routed like every
    /// per-agent operation: remote GraphQL when the peer registers an
    /// endpoint, the local node otherwise. Bounded so a dead peer fails the
    /// panel instead of hanging it.
    pub async fn request_timeline(
        &self,
        agent_did: &str,
        request_id: &str,
    ) -> Result<defra_agent::run_timeline::RunTimeline> {
        let agent_did = normalize_required("agent_did", agent_did)?;
        let request_id = normalize_required("request_id", request_id)?;
        let access = match self.graphql_for_agent(agent_did).await {
            Some(graphql) => defra_agent::config_client::ConfigAccess::Graphql(graphql),
            None => defra_agent::config_client::ConfigAccess::Local(self.node_arc()),
        };
        let timeline = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            defra_agent::run_timeline_fetch::load_run_timeline(&access, request_id),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out loading timeline for {request_id}"))?
        // The GraphQL transport appends CLI-flavored operator hints
        // ("run `defra-agent init`", "Retry with `--graphql ...`") that are
        // meaningless inside the desktop app.
        .map_err(|error| anyhow::anyhow!("{}", strip_cli_operator_hints(&error.to_string())))?;
        Ok(timeline)
    }

    /// Live P2P/network state for the visibility panel. Each probe fails
    /// independently — a dead subsystem reports its error instead of hiding
    /// the healthy ones.
    pub async fn network_status(&self) -> NetworkStatus {
        let local_peer_id = p2p_ops::p2p_local_peer_id(&self.p2p).await;
        let listen_addresses = p2p_ops::p2p_listen_addresses(&self.p2p).await;
        let connected_peers = p2p_ops::p2p_connected_peers(&self.p2p).await;
        let replicators = p2p_ops::p2p_get_replicators(&self.p2p).await;
        let saved_peers = self.peer_directory.read().await.records().to_vec();

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

    pub async fn graphql_for_agent(&self, agent_did: &str) -> Option<String> {
        self.peer_record_for_agent(agent_did)
            .await
            .and_then(|record| {
                record
                    .graphql
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
    }

    pub async fn peer_record_for_agent(&self, agent_did: &str) -> Option<PeerRecord> {
        let agent_did = agent_did.trim();
        if agent_did.is_empty() {
            return None;
        }
        let peer_directory = self.peer_directory.read().await;
        peer_directory
            .records()
            .iter()
            .find(|record| record.agent_did == agent_did)
            .cloned()
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

    pub async fn delete_skill(&self, skill_id: &str, source_agent_did: &str) -> Result<()> {
        let skill_id = normalize_required("skill_id", skill_id)?;
        let source_agent_did = normalize_required("source_agent_did", source_agent_did)?;
        let remote_graphql = self.graphql_for_agent(source_agent_did).await;
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
            let deleted = match remote_graphql.as_deref() {
                Some(graphql) => {
                    mutations::delete_skill_from_graphql(graphql, source_agent_did, skill_id)
                        .await?
                }
                None => {
                    mutations::delete_skill(self.node.as_ref(), source_agent_did, skill_id).await?
                }
            };
            if deleted == 0 {
                bail!("no Skill document with skill_id {skill_id:?} for {source_agent_did}");
            }
            for behavior in affected_behaviors {
                match remote_graphql.as_deref() {
                    Some(graphql) => {
                        mutations::upsert_agent_behavior_to_graphql(graphql, &behavior).await?
                    }
                    None => mutations::upsert_agent_behavior(self.node.as_ref(), &behavior).await?,
                }
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
        let remote_graphql = self.graphql_for_agent(source_agent_did).await;
        let snapshot = self.store.snapshot();
        if !snapshot.tasks.iter().enumerate().any(|(index, row)| {
            row.task_id == task_id
                && row_matches_source(
                    &snapshot.task_source_agent_dids,
                    index,
                    source_agent_did,
                    remote_graphql.is_some(),
                )
        }) {
            bail!("no Task document with task_id {task_id:?}");
        }
        // Dependents block deletion: silently cascading into automation that
        // still fires would be worse than asking the operator to detach it.
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
                        remote_graphql.is_some(),
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
                        remote_graphql.is_some(),
                    )
            })
            .count();
        if schedule_refs + trigger_refs > 0 {
            bail!(
                "task {task_id:?} is referenced by {schedule_refs} schedule(s) and {trigger_refs} event trigger(s); delete or detach those first"
            );
        }

        let result = async {
            let deleted = match remote_graphql.as_deref() {
                Some(graphql) => mutations::delete_task_from_graphql(graphql, task_id).await?,
                None => mutations::delete_task(self.node.as_ref(), task_id).await?,
            };
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
                    remote_graphql.is_some(),
                    |row| row.task_id == task_id,
                );
            },
        )
        .await
    }

    pub async fn delete_schedule(&self, schedule_id: &str, source_agent_did: &str) -> Result<()> {
        let schedule_id = normalize_required("schedule_id", schedule_id)?;
        let source_agent_did = normalize_required("source_agent_did", source_agent_did)?;
        let remote_graphql = self.graphql_for_agent(source_agent_did).await;
        let snapshot = self.store.snapshot();
        if !snapshot.schedules.iter().enumerate().any(|(index, row)| {
            row.schedule_id == schedule_id
                && row_matches_source(
                    &snapshot.schedule_source_agent_dids,
                    index,
                    source_agent_did,
                    remote_graphql.is_some(),
                )
        }) {
            bail!("no Schedule document with schedule_id {schedule_id:?}");
        }

        let result = async {
            let deleted = match remote_graphql.as_deref() {
                Some(graphql) => {
                    mutations::delete_schedule_from_graphql(graphql, schedule_id).await?
                }
                None => mutations::delete_schedule(self.node.as_ref(), schedule_id).await?,
            };
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
                    remote_graphql.is_some(),
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
        let remote_graphql = self.graphql_for_agent(source_agent_did).await;
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
                        remote_graphql.is_some(),
                    )
            })
        {
            bail!("no EventTrigger document with trigger_id {trigger_id:?}");
        }

        let result = async {
            let deleted = match remote_graphql.as_deref() {
                Some(graphql) => {
                    mutations::delete_event_trigger_from_graphql(graphql, trigger_id).await?
                }
                None => mutations::delete_event_trigger(self.node.as_ref(), trigger_id).await?,
            };
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
                    remote_graphql.is_some(),
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
        let remote_graphql = self.graphql_for_agent(source_agent_did).await;
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
                        remote_graphql.is_some(),
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
            let deleted = match remote_graphql.as_deref() {
                Some(graphql) => {
                    mutations::delete_inference_backend_from_graphql(graphql, backend_id).await?
                }
                None => mutations::delete_inference_backend(self.node.as_ref(), backend_id).await?,
            };
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
                    remote_graphql.is_some(),
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
        let remote_graphql = self.graphql_for_agent(source_agent_did).await;
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
                        remote_graphql.is_some(),
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
            let deleted = match remote_graphql.as_deref() {
                Some(graphql) => {
                    mutations::delete_inference_profile_from_graphql(graphql, profile_id).await?
                }
                None => mutations::delete_inference_profile(self.node.as_ref(), profile_id).await?,
            };
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
                    remote_graphql.is_some(),
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
        let remote_graphql = self.graphql_for_agent(source_agent_did).await;
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
            let deleted = match remote_graphql.as_deref() {
                Some(graphql) => {
                    mutations::delete_tool_selection_from_graphql(
                        graphql,
                        source_agent_did,
                        selection_id,
                    )
                    .await?
                }
                None => {
                    mutations::delete_tool_selection(
                        self.node.as_ref(),
                        source_agent_did,
                        selection_id,
                    )
                    .await?
                }
            };
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
        let remote_graphql = self.graphql_for_agent(source_agent_did).await;
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
                        remote_graphql.is_some(),
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
            let deleted = match remote_graphql.as_deref() {
                Some(graphql) => {
                    mutations::delete_tool_service_registry_from_graphql(graphql, service_id)
                        .await?
                }
                None => {
                    mutations::delete_tool_service_registry(self.node.as_ref(), service_id).await?
                }
            };
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
                    remote_graphql.is_some(),
                    |row| row.service_id == service_id,
                );
            },
        )
        .await
    }

    pub async fn delete_behavior(&self, behavior_id: &str, source_agent_did: &str) -> Result<()> {
        let behavior_id = normalize_required("behavior_id", behavior_id)?;
        let source_agent_did = normalize_required("source_agent_did", source_agent_did)?;
        let remote_graphql = self.graphql_for_agent(source_agent_did).await;
        let snapshot = self.store.snapshot();
        if !snapshot.behaviors.iter().any(|row| {
            row.behavior_id == behavior_id && row.agent_did.as_deref() == Some(source_agent_did)
        }) {
            bail!("no AgentBehavior document with behavior_id {behavior_id:?}");
        }
        // The default behavior is the request fallback; deleting it strands
        // every request that names no behavior.
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
                        remote_graphql.is_some(),
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
            let deleted = match remote_graphql.as_deref() {
                Some(graphql) => {
                    mutations::delete_agent_behavior_from_graphql(
                        graphql,
                        source_agent_did,
                        behavior_id,
                    )
                    .await?
                }
                None => {
                    mutations::delete_agent_behavior(
                        self.node.as_ref(),
                        source_agent_did,
                        behavior_id,
                    )
                    .await?
                }
            };
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

    /// Shared tail for automation-document deletes: refresh, prune the row
    /// locally so the UI reflects the delete immediately, log, and record or
    /// clear the mutation error.
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
        match self.refresh_remote_agent(source_agent_did).await? {
            Some(version) => Ok(version),
            None => self.refresh_store().await,
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

    pub async fn rename_peer(&self, peer_id: &str, label: &str) -> Result<()> {
        let peer_id = normalize_required("peer_id", peer_id)?;
        let label = normalize_required("label", label)?;
        let mut peer_directory = self.peer_directory.write().await;
        let record = peer_directory
            .records()
            .iter()
            .find(|record| record.peer_id == peer_id)
            .cloned()
            .with_context(|| format!("peer {peer_id} not found"))?;
        let mut record = record;
        record.label = label.to_string();
        peer_directory.upsert(record).await?;
        self.clear_mutation_error();
        Ok(())
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
                match super::bootstrap::configure_local_runtime_pairing(self.node.as_ref(), &record)
                    .await
                {
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
            let refresh_result = if record
                .graphql
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            {
                self.refresh_remote_peer_record(&record).await.map(|_| ())
            } else {
                self.refresh_store().await.map(|_| ())
            };
            if let Err(error) = refresh_result {
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
            pairing: Vec::new(),
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
        let record = {
            let peer_directory = self.peer_directory.read().await;
            let record = peer_directory
                .records()
                .iter()
                .find(|record| record.peer_id == peer_id)
                .cloned()
                .with_context(|| format!("peer {peer_id} not found"))?;
            if record.source.as_deref() == Some("local-standard") {
                // The local runtime is this machine, not a saved peer.
                anyhow::bail!("the local runtime deployment cannot be removed");
            }
            record
        };

        if let Err(error) = cleanup_saved_peer_p2p(&self.p2p, &record).await {
            tracing::warn!(
                target: "defra_agent_desktop_core::peer",
                peer_id = %record.peer_id,
                label = %record.label,
                error = %error,
                "desktop deployment P2P cleanup failed; deployment retained"
            );
            return Err(self.record_mutation_error("remove deployment", error));
        }

        let removed_result = {
            let mut peer_directory = self.peer_directory.write().await;
            let remove_result = peer_directory
                .remove(peer_id)
                .await
                .with_context(|| {
                    format!(
                        "P2P cleanup succeeded but removing peer {peer_id} from saved deployments failed"
                    )
                })
                .and_then(|removed| {
                    removed.with_context(|| format!("peer {peer_id} not found after P2P cleanup"))
                });
            match remove_result {
                Ok(removed) => Ok(removed),
                Err(remove_error) => match peer_directory.upsert(record.clone()).await {
                    Ok(()) => Err(anyhow::anyhow!(
                        "{remove_error}; saved deployment restored and retry is safe"
                    )),
                    Err(restore_error) => Err(anyhow::anyhow!(
                        "{remove_error}; restoring saved deployment also failed: {restore_error}"
                    )),
                },
            }
        };
        let removed = match removed_result {
            Ok(removed) => removed,
            Err(error) => return Err(self.record_mutation_error("remove deployment", error)),
        };

        if let Err(desired_error) =
            delete_peer_pairing_desired(self.node.as_ref(), &record.peer_id).await
        {
            let restore_result = {
                let mut peer_directory = self.peer_directory.write().await;
                peer_directory.upsert(record.clone()).await
            };
            let error = match restore_result {
                Ok(()) => anyhow::anyhow!(
                    "P2P teardown succeeded but pairing desired-state deletion failed: {desired_error}; saved deployment restored and retry is safe"
                ),
                Err(restore_error) => anyhow::anyhow!(
                    "P2P teardown succeeded but pairing desired-state deletion failed: {desired_error}; restoring saved deployment also failed: {restore_error}"
                ),
            };
            tracing::warn!(
                target: "defra_agent_desktop_core::peer",
                peer_id = %record.peer_id,
                label = %record.label,
                error = %error,
                "desktop pairing desired-state deletion failed after P2P cleanup"
            );
            return Err(self.record_mutation_error("remove deployment", error));
        }

        {
            let mut statuses = self
                .peer_statuses
                .write()
                .expect("peer status lock poisoned");
            if let Some(index) = statuses
                .iter()
                .position(|status| status.peer_id == removed.peer_id)
            {
                statuses.remove(index);
            }
        }

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
            warning: None,
        })
    }

    pub async fn save_behavior(&self, row: &AgentBehaviorRow) -> Result<()> {
        let remote_graphql = match row.agent_did.as_deref() {
            Some(agent_did) => self.graphql_for_agent(agent_did).await,
            None => None,
        };
        let result = match remote_graphql.as_deref() {
            Some(graphql) => mutations::upsert_agent_behavior_to_graphql(graphql, row).await,
            None => mutations::upsert_agent_behavior(self.node.as_ref(), row).await,
        };
        match result {
            Ok(()) => {
                if let Some(agent_did) = row.agent_did.as_deref() {
                    if remote_graphql.is_some() {
                        self.refresh_remote_agent(agent_did).await?;
                    } else {
                        self.refresh_store().await?;
                    }
                } else {
                    self.refresh_store().await?;
                }
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
        let remote_graphql = self.graphql_for_agent(&row.agent_did).await;
        let result = match remote_graphql.as_deref() {
            Some(graphql) => mutations::upsert_agent_principal_to_graphql(graphql, row).await,
            None => mutations::upsert_agent_principal(self.node.as_ref(), row).await,
        };
        match result {
            Ok(()) => {
                if remote_graphql.is_some() {
                    self.refresh_remote_agent(&row.agent_did).await?;
                } else {
                    self.refresh_store().await?;
                }
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
        let remote_graphql = match row.agent_did.as_deref() {
            Some(agent_did) => self.graphql_for_agent(agent_did).await,
            None => None,
        };
        let result = match remote_graphql.as_deref() {
            Some(graphql) => mutations::upsert_tool_selection_to_graphql(graphql, row).await,
            None => mutations::upsert_tool_selection(self.node.as_ref(), row).await,
        };
        match result {
            Ok(()) => {
                if let Some(agent_did) = row.agent_did.as_deref() {
                    if remote_graphql.is_some() {
                        self.refresh_remote_agent(agent_did).await?;
                    } else {
                        self.refresh_store().await?;
                    }
                } else {
                    self.refresh_store().await?;
                }
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

    /// Persist a `Skill` document. Skills are apply-owned and globally
    /// addressed by `skill_id`; every field is operator-authored, so the
    /// upsert projects all of them (the runtime never writes skills back).
    pub async fn save_skill(&self, row: &SkillRow) -> Result<()> {
        match mutations::upsert_skill(self.node.as_ref(), row).await {
            Ok(()) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "defra_agent_desktop_core::writes",
                    doc_type = "skill",
                    row_id = %row.skill_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save skill", error)),
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
                target: "defra_agent_desktop_core::writes",
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
        target: "defra_agent_desktop_core::writes",
        action = %action,
        row_id = %row_id,
        "desktop write saved"
    );
}

pub(super) async fn cleanup_saved_peer_p2p(
    p2p: &Arc<dyn P2POps>,
    record: &PeerRecord,
) -> Result<()> {
    let collections = subscribed_collection_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let replicator_result = p2p_remove_replicator(p2p, collections, &record.addr).await;
    // The pinned transport defines disconnect as idempotent for absent peers,
    // so always attempt it even when the deployment never connected or the
    // replicator cleanup failed.
    let disconnect_result = p2p_disconnect_peer(p2p, &record.addr).await;

    match (replicator_result, disconnect_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(replicator_error), Ok(())) => anyhow::bail!(
            "transport disconnected but replicator cleanup failed for {} at {}: {}; saved deployment retained",
            record.label,
            record.addr,
            replicator_error
        ),
        (Ok(()), Err(disconnect_error)) => anyhow::bail!(
            "replicator removed but transport disconnect failed for {} at {}: {}; saved deployment retained",
            record.label,
            record.addr,
            disconnect_error
        ),
        (Err(replicator_error), Err(disconnect_error)) => anyhow::bail!(
            "replicator cleanup failed for {} at {}: {}; transport disconnect also failed: {}; saved deployment retained",
            record.label,
            record.addr,
            replicator_error,
            disconnect_error
        ),
    }
}

async fn delete_peer_pairing_desired(
    node: &defra_node::EmbeddedNode,
    peer_id: &str,
) -> Result<bool> {
    use defra_agent_protocol::graphql::escape_graphql_string;

    let peer_id = escape_graphql_string(peer_id);
    let mutation = format!(
        r#"mutation {{
            delete_PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{
                _docID
            }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!(
            "delete PeerPairingDesired failed; saved deployment retained: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }

    let deleted = response
        .data
        .as_ref()
        .and_then(|data| data.get("delete_PeerPairingDesired"))
        .and_then(|rows| rows.as_array())
        .context("delete PeerPairingDesired returned no result rows")?;
    Ok(!deleted.is_empty())
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

#[cfg(test)]
mod delete_source_tests {
    use super::*;
    use anyhow::anyhow;
    use serde_json::json;

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

/// Drop advice lines that only make sense at a CLI prompt.
fn strip_cli_operator_hints(message: &str) -> String {
    message
        .lines()
        .filter(|line| {
            !line.contains("defra-agent init")
                && !line.contains("defra-agent server")
                && !line.contains("--graphql")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Independent probe results; `Err` carries the probe's failure message.
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
