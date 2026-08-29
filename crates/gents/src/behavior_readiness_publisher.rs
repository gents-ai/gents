//! Single ordered owner of durable runtime behavior readiness.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use gents_protocol::row::{
    project_behavior_readiness_source, BehaviorReadinessProcessState, BehaviorReadinessSnapshot,
    BehaviorReadinessSourceEntry, BehaviorReadinessUnavailableReason,
};
use tokio::sync::{mpsc, oneshot, watch};

use crate::agent::ProcessLifecycleState;
use crate::graphql::escape_graphql_string;
use crate::runtime_snapshot::ActiveRuntimeSnapshot;
use crate::session::execute_mutation_with_retry;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BehaviorAdmissionObservation {
    source_generation: u64,
    demotions: BTreeMap<String, String>,
}

impl BehaviorAdmissionObservation {
    pub(crate) fn demotion_reason(&self, behavior_id: &str) -> Option<&str> {
        self.demotions.get(behavior_id).map(String::as_str)
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
            source_generation,
            demotions: demotions.into_iter().collect(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct BehaviorReadinessPublisherHandle {
    commands: mpsc::Sender<Command>,
    observation: watch::Receiver<BehaviorAdmissionObservation>,
}

pub(crate) struct BehaviorReadinessPublisherOwner {
    commands: mpsc::Sender<Command>,
    task: tokio::task::JoinHandle<Result<()>>,
}

#[async_trait::async_trait]
pub(crate) trait BehaviorReadinessWriter: Send + Sync {
    async fn upsert(
        &self,
        agent_did: &str,
        snapshot: &BehaviorReadinessSnapshot,
        updated_at: &str,
    ) -> Result<()>;
}

struct DefraBehaviorReadinessWriter {
    node: Arc<defra_node::EmbeddedNode>,
}

#[async_trait::async_trait]
impl BehaviorReadinessWriter for DefraBehaviorReadinessWriter {
    async fn upsert(
        &self,
        agent_did: &str,
        snapshot: &BehaviorReadinessSnapshot,
        updated_at: &str,
    ) -> Result<()> {
        upsert_behavior_readiness(self.node.as_ref(), agent_did, snapshot, updated_at).await
    }
}

#[derive(Clone)]
struct ReadinessSource {
    active_generation: u64,
    default_behavior_id: String,
    entries: BTreeMap<String, BehaviorReadinessSourceEntry>,
    slot_generations: BTreeMap<String, u64>,
}

#[derive(Clone)]
struct PublisherState {
    agent_did: String,
    process_state: BehaviorReadinessProcessState,
    router_generation: u64,
    source: Option<ReadinessSource>,
    registered_slots: BTreeSet<(String, u64)>,
    demotions: BTreeMap<(String, u64), String>,
    persisted: Option<BehaviorReadinessSnapshot>,
    updated_at: String,
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct FatalBehaviorReadinessWrite;

#[cfg(test)]
impl std::fmt::Display for FatalBehaviorReadinessWrite {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("injected fatal behavior readiness write")
    }
}

#[cfg(test)]
impl std::error::Error for FatalBehaviorReadinessWrite {}

enum Command {
    Initialize {
        default_behavior_id: String,
        ack: oneshot::Sender<Result<()>>,
    },
    PublishSource {
        source: ReadinessSource,
        ack: oneshot::Sender<Result<()>>,
    },
    SetProcess {
        state: BehaviorReadinessProcessState,
        ack: oneshot::Sender<Result<()>>,
    },
    SetRouterGeneration {
        generation: u64,
        ack: oneshot::Sender<Result<()>>,
    },
    RegisterSlot {
        behavior_id: String,
        generation: u64,
        ack: oneshot::Sender<Result<()>>,
    },
    MarkSlotReady {
        behavior_id: String,
        generation: u64,
        ack: oneshot::Sender<Result<bool>>,
    },
    DemoteSlot {
        behavior_id: String,
        generation: u64,
        diagnostic: String,
        ack: oneshot::Sender<Result<bool>>,
    },
    RetireSlot {
        behavior_id: String,
        generation: u64,
        ack: oneshot::Sender<Result<bool>>,
    },
    Close {
        ack: oneshot::Sender<Result<()>>,
    },
}

impl BehaviorReadinessPublisherHandle {
    pub(crate) fn start(
        node: Arc<defra_node::EmbeddedNode>,
        agent_did: impl Into<String>,
    ) -> (BehaviorReadinessPublisherOwner, Self) {
        Self::start_with_writer(
            Arc::new(DefraBehaviorReadinessWriter { node }),
            agent_did,
            Duration::from_secs(1),
        )
    }

    pub(crate) fn start_with_writer(
        writer: Arc<dyn BehaviorReadinessWriter>,
        agent_did: impl Into<String>,
        retry_delay: Duration,
    ) -> (BehaviorReadinessPublisherOwner, Self) {
        let (commands, receiver) = mpsc::channel(64);
        let (observation_tx, observation) = watch::channel(BehaviorAdmissionObservation::default());
        let task = tokio::spawn(run_publisher(
            writer,
            PublisherState {
                agent_did: agent_did.into(),
                process_state: BehaviorReadinessProcessState::Uninitialized,
                router_generation: 0,
                source: None,
                registered_slots: BTreeSet::new(),
                demotions: BTreeMap::new(),
                persisted: None,
                updated_at: Utc::now().to_rfc3339(),
            },
            receiver,
            observation_tx,
            retry_delay,
        ));
        (
            BehaviorReadinessPublisherOwner {
                commands: commands.clone(),
                task,
            },
            Self {
                commands,
                observation,
            },
        )
    }

    pub(crate) fn observation(&self) -> BehaviorAdmissionObservation {
        self.observation.borrow().clone()
    }

    pub(crate) fn subscribe_observation(&self) -> watch::Receiver<BehaviorAdmissionObservation> {
        self.observation.clone()
    }

    pub(crate) async fn initialize(&self, default_behavior_id: &str) -> Result<()> {
        self.send(|ack| Command::Initialize {
            default_behavior_id: default_behavior_id.to_string(),
            ack,
        })
        .await
    }

    pub(crate) async fn publish_snapshot(&self, snapshot: &ActiveRuntimeSnapshot) -> Result<()> {
        let behavior_ids = snapshot
            .dispatchers
            .keys()
            .chain(snapshot.unavailable_behaviors.keys())
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let entries = behavior_ids
            .into_iter()
            .map(|behavior_id| {
                let entry = BehaviorReadinessSourceEntry {
                    behavior_id: behavior_id.clone(),
                    dispatcher_present: snapshot.dispatchers.contains_key(&behavior_id),
                    unavailable_reason: snapshot
                        .unavailable_behaviors
                        .get(&behavior_id)
                        .map(|unavailable| unavailable.public_reason),
                    startup_demoted: false,
                };
                (behavior_id, entry)
            })
            .collect();
        self.send(|ack| Command::PublishSource {
            source: ReadinessSource {
                active_generation: snapshot.generation,
                default_behavior_id: snapshot.default_behavior_id.clone(),
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

    pub(crate) async fn register_slot(&self, behavior_id: &str, generation: u64) -> Result<()> {
        self.send(|ack| Command::RegisterSlot {
            behavior_id: behavior_id.to_string(),
            generation,
            ack,
        })
        .await
    }

    pub(crate) async fn mark_slot_ready(&self, behavior_id: &str, generation: u64) -> Result<bool> {
        self.send(|ack| Command::MarkSlotReady {
            behavior_id: behavior_id.to_string(),
            generation,
            ack,
        })
        .await
    }

    pub(crate) async fn demote_slot(
        &self,
        behavior_id: &str,
        generation: u64,
        diagnostic: String,
    ) -> Result<bool> {
        self.send(|ack| Command::DemoteSlot {
            behavior_id: behavior_id.to_string(),
            generation,
            diagnostic,
            ack,
        })
        .await
    }

    pub(crate) async fn retire_slot(&self, behavior_id: &str, generation: u64) -> Result<bool> {
        self.send(|ack| Command::RetireSlot {
            behavior_id: behavior_id.to_string(),
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
        self.commands
            .send(command(ack))
            .await
            .map_err(|_| anyhow!("behavior readiness publisher stopped"))?;
        result
            .await
            .map_err(|_| anyhow!("behavior readiness publisher dropped acknowledgement"))?
    }
}

impl BehaviorReadinessPublisherOwner {
    pub(crate) async fn close(self) -> Result<()> {
        let (ack, result) = oneshot::channel();
        self.commands
            .send(Command::Close { ack })
            .await
            .map_err(|_| anyhow!("behavior readiness publisher stopped before close"))?;
        result
            .await
            .map_err(|_| anyhow!("behavior readiness publisher dropped close acknowledgement"))??;
        self.task
            .await
            .context("join behavior readiness publisher")?
    }
}

async fn run_publisher(
    writer: Arc<dyn BehaviorReadinessWriter>,
    mut state: PublisherState,
    mut commands: mpsc::Receiver<Command>,
    observation: watch::Sender<BehaviorAdmissionObservation>,
    retry_delay: Duration,
) -> Result<()> {
    while let Some(command) = commands.recv().await {
        let close = matches!(&command, Command::Close { .. });
        match command {
            Command::Initialize {
                default_behavior_id,
                ack,
            } => {
                let mut candidate = state.clone();
                candidate.process_state = BehaviorReadinessProcessState::Recovering;
                candidate.router_generation = 0;
                candidate.source = Some(ReadinessSource {
                    active_generation: 0,
                    default_behavior_id: default_behavior_id.clone(),
                    entries: BTreeMap::from([(
                        default_behavior_id.clone(),
                        BehaviorReadinessSourceEntry {
                            behavior_id: default_behavior_id,
                            dispatcher_present: false,
                            unavailable_reason: Some(
                                BehaviorReadinessUnavailableReason::RuntimeConfigurationInvalid,
                            ),
                            startup_demoted: false,
                        },
                    )]),
                    slot_generations: BTreeMap::new(),
                });
                let _ = ack.send(
                    commit_candidate(&writer, &mut state, candidate, &observation, retry_delay)
                        .await,
                );
            }
            Command::PublishSource { mut source, ack } => {
                let mut candidate = state.clone();
                source.slot_generations = source
                    .entries
                    .iter()
                    .filter(|(_, entry)| entry.dispatcher_present)
                    .filter_map(|(behavior_id, _)| {
                        candidate
                            .registered_slots
                            .iter()
                            .filter(|(registered_id, _)| registered_id == behavior_id)
                            .map(|(_, generation)| *generation)
                            .max()
                            .map(|generation| (behavior_id.clone(), generation))
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
                    commit_candidate(&writer, &mut state, candidate, &observation, retry_delay)
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
                    commit_candidate(&writer, &mut state, candidate, &observation, retry_delay)
                        .await,
                );
            }
            Command::SetRouterGeneration { generation, ack } => {
                let mut candidate = state.clone();
                candidate.router_generation = generation;
                let _ = ack.send(
                    commit_candidate(&writer, &mut state, candidate, &observation, retry_delay)
                        .await,
                );
            }
            Command::RegisterSlot {
                behavior_id,
                generation,
                ack,
            } => {
                let mut candidate = state.clone();
                candidate.registered_slots.insert((behavior_id, generation));
                let _ = ack.send(
                    commit_candidate(&writer, &mut state, candidate, &observation, retry_delay)
                        .await,
                );
            }
            Command::MarkSlotReady {
                behavior_id,
                generation,
                ack,
            } => {
                let applied = state
                    .registered_slots
                    .contains(&(behavior_id.clone(), generation));
                let mut candidate = state.clone();
                if applied {
                    candidate
                        .demotions
                        .remove(&(behavior_id.clone(), generation));
                }
                let result = if applied {
                    commit_candidate(&writer, &mut state, candidate, &observation, retry_delay)
                        .await
                        .map(|()| true)
                } else {
                    Ok(false)
                };
                let _ = ack.send(result);
            }
            Command::DemoteSlot {
                behavior_id,
                generation,
                diagnostic,
                ack,
            } => {
                let applied = state
                    .registered_slots
                    .contains(&(behavior_id.clone(), generation));
                let mut candidate = state.clone();
                if applied {
                    candidate
                        .demotions
                        .insert((behavior_id, generation), diagnostic);
                }
                let result = if applied {
                    commit_candidate(&writer, &mut state, candidate, &observation, retry_delay)
                        .await
                        .map(|()| true)
                } else {
                    Ok(false)
                };
                let _ = ack.send(result);
            }
            Command::RetireSlot {
                behavior_id,
                generation,
                ack,
            } => {
                let applied = state
                    .registered_slots
                    .contains(&(behavior_id.clone(), generation));
                let mut candidate = state.clone();
                if applied {
                    candidate
                        .registered_slots
                        .remove(&(behavior_id.clone(), generation));
                    let source_uses_slot = candidate.source.as_ref().is_some_and(|source| {
                        source.slot_generations.get(&behavior_id) == Some(&generation)
                    });
                    if !source_uses_slot {
                        candidate.demotions.remove(&(behavior_id, generation));
                    }
                }
                let result = if applied {
                    commit_candidate(&writer, &mut state, candidate, &observation, retry_delay)
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
    Err(anyhow!(
        "behavior readiness publisher command channel closed"
    ))
}

async fn commit_candidate(
    writer: &Arc<dyn BehaviorReadinessWriter>,
    state: &mut PublisherState,
    mut candidate: PublisherState,
    observation: &watch::Sender<BehaviorAdmissionObservation>,
    retry_delay: Duration,
) -> Result<()> {
    let next_observation = persist_candidate(writer, &mut candidate, retry_delay).await?;
    *state = candidate;
    if *observation.borrow() != next_observation {
        observation.send_replace(next_observation);
    }
    Ok(())
}

async fn persist_candidate(
    writer: &Arc<dyn BehaviorReadinessWriter>,
    state: &mut PublisherState,
    retry_delay: Duration,
) -> Result<BehaviorAdmissionObservation> {
    let source = state
        .source
        .as_ref()
        .ok_or_else(|| anyhow!("behavior readiness source is not initialized"))?;
    let sources = source.entries.values().cloned().map(|mut entry| {
        entry.startup_demoted =
            source
                .slot_generations
                .get(&entry.behavior_id)
                .is_some_and(|slot_generation| {
                    state
                        .demotions
                        .contains_key(&(entry.behavior_id.clone(), *slot_generation))
                });
        entry
    });
    let snapshot = project_behavior_readiness_source(
        state.process_state,
        source.active_generation,
        state.router_generation,
        source.default_behavior_id.clone(),
        sources,
    )
    .map_err(anyhow::Error::msg)?;
    if state.persisted.as_ref() != Some(&snapshot) {
        state.updated_at = Utc::now().to_rfc3339();
        loop {
            match writer
                .upsert(&state.agent_did, &snapshot, &state.updated_at)
                .await
            {
                Ok(()) => break,
                Err(error) => {
                    if is_fatal_behavior_readiness_write(&error) {
                        return Err(error);
                    }
                    tracing::warn!(
                        agent_did = %state.agent_did,
                        error = %error,
                        "behavior readiness persistence failed; ordered owner will retry"
                    );
                    tokio::time::sleep(retry_delay).await;
                }
            }
        }
        state.persisted = Some(snapshot);
    }
    Ok(BehaviorAdmissionObservation {
        source_generation: source.active_generation,
        demotions: source
            .slot_generations
            .iter()
            .filter_map(|(behavior_id, generation)| {
                state
                    .demotions
                    .get(&(behavior_id.clone(), *generation))
                    .map(|diagnostic| (behavior_id.clone(), diagnostic.clone()))
            })
            .collect(),
    })
}

fn is_fatal_behavior_readiness_write(error: &anyhow::Error) -> bool {
    #[cfg(test)]
    {
        error
            .downcast_ref::<FatalBehaviorReadinessWrite>()
            .is_some()
    }
    #[cfg(not(test))]
    {
        let _ = error;
        false
    }
}

async fn upsert_behavior_readiness(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
    snapshot: &BehaviorReadinessSnapshot,
    updated_at: &str,
) -> Result<()> {
    let snapshot_json = serde_json::to_string(snapshot)?;
    let mutation = format!(
        r#"mutation {{
            upsert_AgentBehaviorReadiness(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                add: {{
                    agent_did: "{agent_did}",
                    snapshot_json: "{snapshot_json}",
                    updated_at: "{updated_at}"
                }},
                update: {{
                    snapshot_json: "{snapshot_json}",
                    updated_at: "{updated_at}"
                }}
            ) {{ _docID }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
        snapshot_json = escape_graphql_string(&snapshot_json),
        updated_at = escape_graphql_string(updated_at),
    );
    let response =
        execute_mutation_with_retry(node, &mutation, "upsert_behavior_readiness").await?;
    if response.has_errors() {
        anyhow::bail!(
            "upsert AgentBehaviorReadiness failed: {:?}",
            response.errors
        );
    }
    Ok(())
}

impl From<ProcessLifecycleState> for BehaviorReadinessProcessState {
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
#[path = "behavior_readiness_publisher/tests.rs"]
mod tests;
