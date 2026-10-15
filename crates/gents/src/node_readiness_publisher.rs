//! Single ordered owner of durable runtime agent readiness.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use gents_protocol::row::{
    project_node_readiness_source, AgentReadinessSourceEntry, AgentReadinessUnavailableReason,
    NodeReadinessProcessState, NodeReadinessSnapshot,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::agent::ProcessLifecycleState;
use crate::graphql::escape_graphql_string;
use crate::runtime_snapshot::ActiveRuntimeSnapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentAdmissionObservation {
    process_state: NodeReadinessProcessState,
    source_generation: u64,
    demotions: BTreeMap<String, String>,
}

impl AgentAdmissionObservation {
    pub(crate) fn process_state(&self) -> NodeReadinessProcessState {
        self.process_state
    }

    pub(crate) fn demotion_reason(&self, agent_id: &str) -> Option<&str> {
        self.demotions.get(agent_id).map(String::as_str)
    }

    pub(crate) fn source_generation(&self) -> u64 {
        self.source_generation
    }

    pub(crate) fn demotions(&self) -> &BTreeMap<String, String> {
        &self.demotions
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        source_generation: u64,
        demotions: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        Self {
            process_state: NodeReadinessProcessState::Ready,
            source_generation,
            demotions: demotions.into_iter().collect(),
        }
    }
}

impl Default for AgentAdmissionObservation {
    fn default() -> Self {
        Self {
            process_state: NodeReadinessProcessState::Uninitialized,
            source_generation: 0,
            demotions: BTreeMap::new(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct NodeReadinessPublisherHandle {
    commands: mpsc::Sender<Command>,
    observation: watch::Receiver<AgentAdmissionObservation>,
    cancel: CancellationToken,
    command_timeout: Option<Duration>,
}

pub(crate) struct NodeReadinessPublisherOwner {
    commands: mpsc::Sender<Command>,
    cancel: CancellationToken,
    close_timeout: Option<Duration>,
    task: tokio::task::JoinHandle<Result<()>>,
}

const MAX_PERSIST_ATTEMPTS: usize = 5;
const MIN_PERSIST_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(250);
const PRODUCTION_PERSIST_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);

#[async_trait::async_trait]
pub(crate) trait NodeReadinessWriter: Send + Sync {
    async fn upsert(
        &self,
        node_did: &str,
        snapshot: &NodeReadinessSnapshot,
        updated_at: &str,
    ) -> Result<()>;
}

struct DefraNodeReadinessWriter {
    node: Arc<defra_node::EmbeddedNode>,
}

#[async_trait::async_trait]
impl NodeReadinessWriter for DefraNodeReadinessWriter {
    async fn upsert(
        &self,
        node_did: &str,
        snapshot: &NodeReadinessSnapshot,
        updated_at: &str,
    ) -> Result<()> {
        upsert_node_readiness(self.node.as_ref(), node_did, snapshot, updated_at).await
    }
}

#[derive(Clone)]
struct ReadinessSource {
    active_generation: u64,
    default_agent_id: String,
    entries: BTreeMap<String, AgentReadinessSourceEntry>,
    slot_generations: BTreeMap<String, u64>,
}

#[derive(Clone)]
struct PublisherState {
    node_did: String,
    process_state: NodeReadinessProcessState,
    router_generation: u64,
    source: Option<ReadinessSource>,
    registered_slots: BTreeSet<(String, u64)>,
    demotions: BTreeMap<(String, u64), String>,
    persisted: Option<NodeReadinessSnapshot>,
    updated_at: String,
    persist_attempt_timeout: Option<Duration>,
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct FatalNodeReadinessWrite;

#[cfg(test)]
impl std::fmt::Display for FatalNodeReadinessWrite {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("injected fatal agent readiness write")
    }
}

#[cfg(test)]
impl std::error::Error for FatalNodeReadinessWrite {}

enum Command {
    Initialize {
        default_agent_id: String,
        ack: oneshot::Sender<Result<()>>,
    },
    PublishSource {
        source: ReadinessSource,
        ack: oneshot::Sender<Result<()>>,
    },
    SetProcess {
        state: NodeReadinessProcessState,
        ack: oneshot::Sender<Result<()>>,
    },
    SetRouterGeneration {
        generation: u64,
        ack: oneshot::Sender<Result<()>>,
    },
    RegisterSlot {
        agent_id: String,
        generation: u64,
        ack: oneshot::Sender<Result<()>>,
    },
    MarkSlotReady {
        agent_id: String,
        generation: u64,
        ack: oneshot::Sender<Result<bool>>,
    },
    DemoteSlot {
        agent_id: String,
        generation: u64,
        diagnostic: String,
        ack: oneshot::Sender<Result<bool>>,
    },
    RetireSlot {
        agent_id: String,
        generation: u64,
        ack: oneshot::Sender<Result<bool>>,
    },
    Close {
        ack: oneshot::Sender<Result<()>>,
    },
}

impl NodeReadinessPublisherHandle {
    pub(crate) fn start(
        node: Arc<defra_node::EmbeddedNode>,
        node_did: impl Into<String>,
    ) -> (NodeReadinessPublisherOwner, Self) {
        Self::start_with_writer_and_timeout(
            Arc::new(DefraNodeReadinessWriter { node }),
            node_did,
            Duration::from_secs(1),
            Some(PRODUCTION_PERSIST_ATTEMPT_TIMEOUT),
        )
    }

    #[cfg(test)]
    pub(crate) fn start_with_writer(
        writer: Arc<dyn NodeReadinessWriter>,
        node_did: impl Into<String>,
        retry_delay: Duration,
    ) -> (NodeReadinessPublisherOwner, Self) {
        let persist_attempt_timeout = retry_delay
            .saturating_mul(MAX_PERSIST_ATTEMPTS as u32)
            .max(MIN_PERSIST_ATTEMPT_TIMEOUT);
        Self::start_with_writer_and_timeout(
            writer,
            node_did,
            retry_delay,
            Some(persist_attempt_timeout),
        )
    }

    #[cfg(test)]
    pub(crate) fn start_with_unbounded_test_clock(
        node: Arc<defra_node::EmbeddedNode>,
        node_did: impl Into<String>,
    ) -> (NodeReadinessPublisherOwner, Self) {
        Self::start_with_writer_and_timeout(
            Arc::new(DefraNodeReadinessWriter { node }),
            node_did,
            Duration::from_secs(1),
            None,
        )
    }

    fn start_with_writer_and_timeout(
        writer: Arc<dyn NodeReadinessWriter>,
        node_did: impl Into<String>,
        retry_delay: Duration,
        persist_attempt_timeout: Option<Duration>,
    ) -> (NodeReadinessPublisherOwner, Self) {
        let (commands, receiver) = mpsc::channel(64);
        let (observation_tx, observation) = watch::channel(AgentAdmissionObservation::default());
        let cancel = CancellationToken::new();
        let command_timeout = persist_attempt_timeout.map(|attempt_timeout| {
            attempt_timeout
                .saturating_add(retry_delay)
                .saturating_mul(MAX_PERSIST_ATTEMPTS as u32)
                .saturating_add(MIN_PERSIST_ATTEMPT_TIMEOUT)
        });
        let task = tokio::spawn(run_publisher(
            writer,
            PublisherState {
                node_did: node_did.into(),
                process_state: NodeReadinessProcessState::Uninitialized,
                router_generation: 0,
                source: None,
                registered_slots: BTreeSet::new(),
                demotions: BTreeMap::new(),
                persisted: None,
                updated_at: Utc::now().to_rfc3339(),
                persist_attempt_timeout,
            },
            receiver,
            observation_tx,
            retry_delay,
            cancel.clone(),
        ));
        (
            NodeReadinessPublisherOwner {
                commands: commands.clone(),
                cancel: cancel.clone(),
                close_timeout: command_timeout,
                task,
            },
            Self {
                commands,
                observation,
                cancel,
                command_timeout,
            },
        )
    }

    pub(crate) fn observation(&self) -> AgentAdmissionObservation {
        self.observation.borrow().clone()
    }

    pub(crate) fn subscribe_observation(&self) -> watch::Receiver<AgentAdmissionObservation> {
        self.observation.clone()
    }

    pub(crate) async fn initialize(&self, default_agent_id: &str) -> Result<()> {
        self.send(|ack| Command::Initialize {
            default_agent_id: default_agent_id.to_string(),
            ack,
        })
        .await
    }

    pub(crate) async fn publish_snapshot(&self, snapshot: &ActiveRuntimeSnapshot) -> Result<()> {
        let agent_ids = snapshot
            .dispatchers
            .keys()
            .chain(snapshot.unavailable_agents.keys())
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let entries = agent_ids
            .into_iter()
            .map(|agent_id| {
                let entry = AgentReadinessSourceEntry {
                    agent_id: agent_id.clone(),
                    dispatcher_present: snapshot.dispatchers.contains_key(&agent_id),
                    unavailable_reason: snapshot
                        .unavailable_agents
                        .get(&agent_id)
                        .map(|unavailable| unavailable.public_reason),
                    startup_demoted: false,
                };
                (agent_id, entry)
            })
            .collect();
        self.send(|ack| Command::PublishSource {
            source: ReadinessSource {
                active_generation: snapshot.generation,
                default_agent_id: snapshot.default_agent_id.clone(),
                entries,
                slot_generations: BTreeMap::new(),
            },
            ack,
        })
        .await
    }

    pub(crate) async fn set_process_state(&self, state: ProcessLifecycleState) -> Result<()> {
        self.send(|ack| Command::SetProcess {
            state: state.into(),
            ack,
        })
        .await
    }

    pub(crate) async fn set_router_generation(&self, generation: u64) -> Result<()> {
        self.send(|ack| Command::SetRouterGeneration { generation, ack })
            .await
    }

    pub(crate) async fn register_slot(&self, agent_id: &str, generation: u64) -> Result<()> {
        self.send(|ack| Command::RegisterSlot {
            agent_id: agent_id.to_string(),
            generation,
            ack,
        })
        .await
    }

    pub(crate) async fn mark_slot_ready(&self, agent_id: &str, generation: u64) -> Result<bool> {
        self.send(|ack| Command::MarkSlotReady {
            agent_id: agent_id.to_string(),
            generation,
            ack,
        })
        .await
    }

    pub(crate) async fn demote_slot(
        &self,
        agent_id: &str,
        generation: u64,
        diagnostic: String,
    ) -> Result<bool> {
        self.send(|ack| Command::DemoteSlot {
            agent_id: agent_id.to_string(),
            generation,
            diagnostic,
            ack,
        })
        .await
    }

    pub(crate) async fn retire_slot(&self, agent_id: &str, generation: u64) -> Result<bool> {
        self.send(|ack| Command::RetireSlot {
            agent_id: agent_id.to_string(),
            generation,
            ack,
        })
        .await
    }

    async fn send<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<T>>) -> Command,
    ) -> Result<T> {
        let (ack, result) = oneshot::channel();
        if self.cancel.is_cancelled() {
            return Err(anyhow!("agent readiness publisher stopped"));
        }
        let enqueue = async { self.commands.send(command(ack)).await };
        let enqueue_result = match self.command_timeout {
            Some(timeout) => match tokio::time::timeout(timeout, enqueue).await {
                Ok(result) => result,
                Err(_) => {
                    self.cancel.cancel();
                    return Err(anyhow!(
                        "agent readiness publisher command enqueue timed out"
                    ));
                }
            },
            None => enqueue.await,
        };
        match enqueue_result {
            Ok(()) => {}
            Err(_) => return Err(anyhow!("agent readiness publisher stopped")),
        }
        let acknowledgement = async { result.await };
        let acknowledgement_result = match self.command_timeout {
            Some(timeout) => match tokio::time::timeout(timeout, acknowledgement).await {
                Ok(result) => result,
                Err(_) => {
                    self.cancel.cancel();
                    return Err(anyhow!(
                        "agent readiness publisher command acknowledgement timed out"
                    ));
                }
            },
            None => acknowledgement.await,
        };
        match acknowledgement_result {
            Ok(result) => result,
            Err(_) => Err(anyhow!("agent readiness publisher dropped acknowledgement")),
        }
    }
}

impl NodeReadinessPublisherOwner {
    pub(crate) async fn close(self) -> Result<()> {
        let (ack, result) = oneshot::channel();
        let close = async {
            self.commands
                .send(Command::Close { ack })
                .await
                .map_err(|_| anyhow!("agent readiness publisher stopped before close"))?;
            result
                .await
                .map_err(|_| anyhow!("agent readiness publisher dropped close acknowledgement"))?
        };
        let close_result = match self.close_timeout {
            Some(timeout) => match tokio::time::timeout(timeout, close).await {
                Ok(result) => result,
                Err(_) => Err(anyhow!("agent readiness publisher close timed out")),
            },
            None => close.await,
        };
        if close_result.is_err() {
            self.cancel.cancel();
        }
        let mut task = self.task;
        let joined = match self.close_timeout {
            Some(timeout) => match tokio::time::timeout(timeout, &mut task).await {
                Ok(joined) => joined.context("join agent readiness publisher")?,
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    return Err(close_result.err().unwrap_or_else(|| {
                        anyhow!("agent readiness publisher task shutdown timed out")
                    }));
                }
            },
            None => task.await.context("join agent readiness publisher")?,
        };
        close_result?;
        joined
    }
}

async fn run_publisher(
    writer: Arc<dyn NodeReadinessWriter>,
    mut state: PublisherState,
    mut commands: mpsc::Receiver<Command>,
    observation: watch::Sender<AgentAdmissionObservation>,
    retry_delay: Duration,
    cancel: CancellationToken,
) -> Result<()> {
    loop {
        let command = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            command = commands.recv() => match command {
                Some(command) => command,
                None => return Err(anyhow!(
                    "agent readiness publisher command channel closed"
                )),
            },
        };
        let close = matches!(&command, Command::Close { .. });
        match command {
            Command::Initialize {
                default_agent_id,
                ack,
            } => {
                let mut candidate = state.clone();
                candidate.process_state = NodeReadinessProcessState::Recovering;
                candidate.router_generation = 0;
                candidate.source = Some(ReadinessSource {
                    active_generation: 0,
                    default_agent_id: default_agent_id.clone(),
                    entries: BTreeMap::from([(
                        default_agent_id.clone(),
                        AgentReadinessSourceEntry {
                            agent_id: default_agent_id,
                            dispatcher_present: false,
                            unavailable_reason: Some(
                                AgentReadinessUnavailableReason::RuntimeConfigurationInvalid,
                            ),
                            startup_demoted: false,
                        },
                    )]),
                    slot_generations: BTreeMap::new(),
                });
                let _ = ack.send(
                    commit_candidate(
                        &writer,
                        &mut state,
                        candidate,
                        &observation,
                        retry_delay,
                        &cancel,
                    )
                    .await,
                );
            }
            Command::PublishSource { mut source, ack } => {
                let mut candidate = state.clone();
                source.slot_generations = source
                    .entries
                    .iter()
                    .filter(|(_, entry)| entry.dispatcher_present)
                    .filter_map(|(agent_id, _)| {
                        candidate
                            .registered_slots
                            .iter()
                            .filter(|(registered_id, _)| registered_id == agent_id)
                            .map(|(_, generation)| *generation)
                            .max()
                            .map(|generation| (agent_id.clone(), generation))
                    })
                    .collect();
                candidate.source = Some(source);
                candidate.demotions.retain(|slot, _| {
                    candidate.registered_slots.contains(slot)
                        || candidate.source.as_ref().is_some_and(|source| {
                            source.slot_generations.get(&slot.0) == Some(&slot.1)
                        })
                });
                let _ = ack.send(
                    commit_candidate(
                        &writer,
                        &mut state,
                        candidate,
                        &observation,
                        retry_delay,
                        &cancel,
                    )
                    .await,
                );
            }
            Command::SetProcess {
                state: process,
                ack,
            } => {
                let mut candidate = state.clone();
                candidate.process_state = process;
                let _ = ack.send(
                    commit_candidate(
                        &writer,
                        &mut state,
                        candidate,
                        &observation,
                        retry_delay,
                        &cancel,
                    )
                    .await,
                );
            }
            Command::SetRouterGeneration { generation, ack } => {
                let mut candidate = state.clone();
                candidate.router_generation = generation;
                let _ = ack.send(
                    commit_candidate(
                        &writer,
                        &mut state,
                        candidate,
                        &observation,
                        retry_delay,
                        &cancel,
                    )
                    .await,
                );
            }
            Command::RegisterSlot {
                agent_id,
                generation,
                ack,
            } => {
                let mut candidate = state.clone();
                candidate.registered_slots.insert((agent_id, generation));
                let _ = ack.send(
                    commit_candidate(
                        &writer,
                        &mut state,
                        candidate,
                        &observation,
                        retry_delay,
                        &cancel,
                    )
                    .await,
                );
            }
            Command::MarkSlotReady {
                agent_id,
                generation,
                ack,
            } => {
                let applied = state
                    .registered_slots
                    .contains(&(agent_id.clone(), generation));
                let mut candidate = state.clone();
                if applied {
                    candidate.demotions.remove(&(agent_id.clone(), generation));
                }
                let result = if applied {
                    commit_candidate(
                        &writer,
                        &mut state,
                        candidate,
                        &observation,
                        retry_delay,
                        &cancel,
                    )
                    .await
                    .map(|()| true)
                } else {
                    Ok(false)
                };
                let _ = ack.send(result);
            }
            Command::DemoteSlot {
                agent_id,
                generation,
                diagnostic,
                ack,
            } => {
                let applied = state
                    .registered_slots
                    .contains(&(agent_id.clone(), generation));
                let mut candidate = state.clone();
                if applied {
                    candidate
                        .demotions
                        .insert((agent_id, generation), diagnostic);
                }
                let result = if applied {
                    commit_candidate(
                        &writer,
                        &mut state,
                        candidate,
                        &observation,
                        retry_delay,
                        &cancel,
                    )
                    .await
                    .map(|()| true)
                } else {
                    Ok(false)
                };
                let _ = ack.send(result);
            }
            Command::RetireSlot {
                agent_id,
                generation,
                ack,
            } => {
                let applied = state
                    .registered_slots
                    .contains(&(agent_id.clone(), generation));
                let mut candidate = state.clone();
                if applied {
                    candidate
                        .registered_slots
                        .remove(&(agent_id.clone(), generation));
                    let source_uses_slot = candidate.source.as_ref().is_some_and(|source| {
                        source.slot_generations.get(&agent_id) == Some(&generation)
                    });
                    if source_uses_slot {
                        let source = candidate
                            .source
                            .as_mut()
                            .expect("source slot mapping implies initialized source");
                        source.slot_generations.remove(&agent_id);
                        if let Some(entry) = source.entries.get_mut(&agent_id) {
                            entry.dispatcher_present = false;
                            entry.unavailable_reason =
                                Some(AgentReadinessUnavailableReason::ExecutorStartFailed);
                        }
                    }
                    candidate.demotions.remove(&(agent_id, generation));
                }
                let result = if applied {
                    commit_candidate(
                        &writer,
                        &mut state,
                        candidate,
                        &observation,
                        retry_delay,
                        &cancel,
                    )
                    .await
                    .map(|()| true)
                } else {
                    Ok(false)
                };
                let _ = ack.send(result);
            }
            Command::Close { ack } => {
                let _ = ack.send(Ok(()));
            }
        }
        if close {
            return Ok(());
        }
    }
}

async fn commit_candidate(
    writer: &Arc<dyn NodeReadinessWriter>,
    state: &mut PublisherState,
    mut candidate: PublisherState,
    observation: &watch::Sender<AgentAdmissionObservation>,
    retry_delay: Duration,
    cancel: &CancellationToken,
) -> Result<()> {
    let next_observation = persist_candidate(writer, &mut candidate, retry_delay, cancel).await?;
    *state = candidate;
    if *observation.borrow() != next_observation {
        observation.send_replace(next_observation);
    }
    Ok(())
}

async fn persist_candidate(
    writer: &Arc<dyn NodeReadinessWriter>,
    state: &mut PublisherState,
    retry_delay: Duration,
    cancel: &CancellationToken,
) -> Result<AgentAdmissionObservation> {
    let source = state
        .source
        .as_ref()
        .ok_or_else(|| anyhow!("agent readiness source is not initialized"))?;
    let sources = source.entries.values().cloned().map(|mut entry| {
        entry.startup_demoted =
            source
                .slot_generations
                .get(&entry.agent_id)
                .is_some_and(|slot_generation| {
                    state
                        .demotions
                        .contains_key(&(entry.agent_id.clone(), *slot_generation))
                });
        entry
    });
    let snapshot = project_node_readiness_source(
        state.process_state,
        source.active_generation,
        state.router_generation,
        source.default_agent_id.clone(),
        sources,
    )
    .map_err(anyhow::Error::msg)?;
    if state.persisted.as_ref() != Some(&snapshot) {
        state.updated_at = Utc::now().to_rfc3339();
        for attempt in 1..=MAX_PERSIST_ATTEMPTS {
            let write = tokio::select! {
                _ = cancel.cancelled() => {
                    return Err(anyhow!("agent readiness persistence cancelled"));
                }
                result = async {
                    match state.persist_attempt_timeout {
                        Some(timeout) => tokio::time::timeout(
                            timeout,
                            writer.upsert(&state.node_did, &snapshot, &state.updated_at),
                        )
                        .await
                        .map_err(|_| anyhow!("agent readiness persistence attempt timed out"))?,
                        None => writer.upsert(&state.node_did, &snapshot, &state.updated_at).await,
                    }
                } => result,
            };
            match write {
                Ok(()) => break,
                Err(error) => {
                    if is_fatal_node_readiness_write(&error) || attempt == MAX_PERSIST_ATTEMPTS {
                        return Err(error);
                    }
                    tracing::warn!(
                        node_did = %state.node_did,
                        error = %error,
                        "agent readiness persistence failed; ordered owner will retry"
                    );
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            return Err(anyhow!("agent readiness persistence cancelled"));
                        }
                        _ = tokio::time::sleep(retry_delay) => {}
                    }
                }
            }
        }
        state.persisted = Some(snapshot);
    }
    Ok(AgentAdmissionObservation {
        process_state: state.process_state,
        source_generation: source.active_generation,
        demotions: source
            .slot_generations
            .iter()
            .filter_map(|(agent_id, generation)| {
                state
                    .demotions
                    .get(&(agent_id.clone(), *generation))
                    .map(|diagnostic| (agent_id.clone(), diagnostic.clone()))
            })
            .collect(),
    })
}

fn is_fatal_node_readiness_write(error: &anyhow::Error) -> bool {
    #[cfg(test)]
    {
        error.downcast_ref::<FatalNodeReadinessWrite>().is_some()
    }
    #[cfg(not(test))]
    {
        let _ = error;
        false
    }
}

pub(crate) async fn upsert_node_readiness(
    node: &defra_node::EmbeddedNode,
    node_did: &str,
    snapshot: &NodeReadinessSnapshot,
    updated_at: &str,
) -> Result<()> {
    let snapshot_json = serde_json::to_string(snapshot)?;
    let mutation = format!(
        r#"mutation {{
            upsert_NodeReadiness(
                filter: {{ node_did: {{ _eq: "{node_did}" }} }},
                add: {{
                    node_did: "{node_did}",
                    snapshot_json: "{snapshot_json}",
                    updated_at: "{updated_at}"
                }},
                update: {{
                    snapshot_json: "{snapshot_json}",
                    updated_at: "{updated_at}"
                }}
            ) {{ _docID }}
        }}"#,
        node_did = escape_graphql_string(node_did),
        snapshot_json = escape_graphql_string(&snapshot_json),
        updated_at = escape_graphql_string(updated_at),
    );
    let response = crate::config_client::ConfigAccess::write_local_response(
        node,
        "upsert_node_readiness",
        &mutation,
    )
    .await?;
    if response.has_errors() {
        anyhow::bail!("upsert NodeReadiness failed: {:?}", response.errors);
    }
    Ok(())
}

impl From<ProcessLifecycleState> for NodeReadinessProcessState {
    fn from(value: ProcessLifecycleState) -> Self {
        match value {
            ProcessLifecycleState::Uninitialized => Self::Uninitialized,
            ProcessLifecycleState::Recovering => Self::Recovering,
            ProcessLifecycleState::Ready => Self::Ready,
            ProcessLifecycleState::ShuttingDown => Self::ShuttingDown,
            ProcessLifecycleState::Shutdown => Self::Shutdown,
        }
    }
}

#[cfg(test)]
#[path = "node_readiness_publisher/tests.rs"]
mod tests;
