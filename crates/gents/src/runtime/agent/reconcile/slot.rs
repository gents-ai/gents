use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use anyhow::Result;
use futures::FutureExt;
use tokio::sync::{mpsc, watch, Mutex};
use tokio::task::JoinSet;
use tokio_util::task::AbortOnDropHandle;

use crate::runtime::agent::retry::RetryPolicy;
use crate::runtime::agent::worker_capacity::WorkerCapacity;
use crate::runtime::startup_readiness::{BuildOutcome, BuildStanding};
use gents_automation::triggers::runtime_snapshot::{AgentDispatcher, ResolvedRuntimeSnapshot};
use gents_protocol::row::AgentRequest;
use gents_tools::surface::AgentWithTools;
use gents_tools::surface::ToolSurface;

use rapidhash::{HashMapExt, RapidHashMap};

/// Bounds requests buffered for one worker level and applies backpressure to
/// dispatch.
const AGENT_EXECUTOR_QUEUE_CAPACITY: usize = 32;

/// An Agent's worker levels and the most workers of each (Lean
/// `WorkerLevels.levels`, `WorkerLevels.levelWorkers`), never sized from a
/// backend: model calls take their backend's admission permit per call. A
/// level starts one worker and builds another while all of its workers run
/// requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WorkerLayout {
    pub(super) levels: usize,
    pub(super) level_workers: usize,
}

impl WorkerLayout {
    pub(super) const AGENT: Self = Self {
        levels: 3,
        level_workers: 8,
    };

    fn workers(self) -> usize {
        self.levels * self.level_workers
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AgentSlotState {
    Active,
    Retiring,
}

pub(super) struct AgentSlot {
    pub(super) dispatcher: AgentDispatcher,
    pub(super) state_tx: watch::Sender<AgentSlotState>,
    pub(super) handle: AbortOnDropHandle<Result<()>>,
    pub(super) agent_fingerprint: String,
    pub(super) tool_surface_fingerprint: String,
    pub(super) executor_capacity: usize,
    #[cfg(test)]
    pub(super) worker_task_count: usize,
    pub(super) queue_capacity: usize,
    pub(super) generation: u64,
    standing: Arc<std::sync::Mutex<BuildStanding>>,
}

impl AgentSlot {
    /// Whether the slot exhausted its build budget and runs nothing.
    pub(super) fn parked(&self) -> bool {
        match self.standing.lock() {
            Ok(standing) => *standing == BuildStanding::Demoted,
            Err(_) => true,
        }
    }

    pub(super) fn matches(
        &self,
        agent_config: &AgentWithTools,
        tool_surface: &Arc<ToolSurface>,
    ) -> bool {
        self.agent_fingerprint == agent_config.slot_fingerprint()
            && self.tool_surface_fingerprint == format!("{tool_surface:?}")
    }
}

#[async_trait::async_trait]
pub(crate) trait SlotFailurePolicy: Send + Sync {
    fn build_failure_budget(&self) -> u32;
    fn on_build_failure(&self, _agent_did: &str, _failure_number: u32, _error: &str) {}
    async fn on_slot_created(&self, agent_did: &str, generation: u64) -> Result<()>;
    async fn try_demote(&self, agent_did: &str, generation: u64, error: &str) -> Result<bool>;
    /// A demoted slot is parked; `publication_busy` says whether its last
    /// build failed only because an installer held the plugin store.
    fn on_slot_parked(&self, _publication_busy: bool) {}
    async fn on_slot_retired(&self, agent_did: &str, generation: u64, recreated: bool);
}

async fn handle_slot_failure(
    agent_did: &str,
    generation: u64,
    failure: &anyhow::Error,
    failure_policy: Option<&dyn SlotFailurePolicy>,
    standing: &Arc<std::sync::Mutex<BuildStanding>>,
    shutdown: &mut watch::Receiver<bool>,
    state_rx: &mut watch::Receiver<AgentSlotState>,
) -> bool {
    let Some(policy) = failure_policy else {
        return false;
    };
    let error = &format!(
        "{}",
        gents_protocol::error_chain::ErrorChain(failure.as_ref())
    );
    enum Verdict {
        Restart,
        AlreadyDemoted,
        Transitioned,
        StillPending,
    }
    let (verdict, failure_number) = match standing.lock() {
        Err(_) => (Verdict::Restart, None),
        Ok(mut standing) => {
            if standing.released() {
                if *standing == BuildStanding::Demoted {
                    (Verdict::AlreadyDemoted, None)
                } else {
                    (Verdict::Restart, None)
                }
            } else {
                let next = standing.step(policy.build_failure_budget(), BuildOutcome::Failed);
                *standing = next;
                if next == BuildStanding::Demoted {
                    (Verdict::Transitioned, Some(policy.build_failure_budget()))
                } else if let BuildStanding::Pending { failures } = next {
                    (Verdict::StillPending, Some(failures))
                } else {
                    (Verdict::Restart, None)
                }
            }
        }
    };
    if let Some(failure_number) = failure_number {
        policy.on_build_failure(agent_did, failure_number, error);
    }
    match verdict {
        Verdict::Restart | Verdict::StillPending => return false,
        Verdict::AlreadyDemoted => {
            park_until_retired(shutdown, state_rx).await;
            return true;
        }
        Verdict::Transitioned => {}
    }
    match policy.try_demote(agent_did, generation, error).await {
        Ok(true) => policy.on_slot_parked(
            gents_pack::plugin::store::PublishedPluginReadError::error_chain_is_busy(
                failure.as_ref(),
            ),
        ),
        Ok(false) => tracing::warn!(
            agent_did,
            generation,
            "startup demotion was already stale; keeping the exhausted slot fail closed"
        ),
        Err(error) => tracing::error!(
            agent_did,
            generation,
            error = %error,
            "failed to persist startup demotion; keeping the exhausted slot fail closed"
        ),
    }
    park_until_retired(shutdown, state_rx).await;
    true
}

async fn park_until_retired(
    shutdown: &mut watch::Receiver<bool>,
    state_rx: &mut watch::Receiver<AgentSlotState>,
) {
    loop {
        if *shutdown.borrow() || *state_rx.borrow() == AgentSlotState::Retiring {
            return;
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            changed = state_rx.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

#[derive(Clone)]
pub(super) struct SlotConfig {
    pub(super) agent_config: AgentWithTools,
    pub(super) tool_surface: Arc<ToolSurface>,
    pub(super) generation: u64,
    pub(super) retry_policy: RetryPolicy,
    pub(super) failure_policy: Option<Arc<dyn SlotFailurePolicy>>,
}

#[derive(Clone)]
struct SlotRuntime {
    request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
    capacity: Arc<WorkerCapacity>,
    shutdown: watch::Receiver<bool>,
    state_rx: watch::Receiver<AgentSlotState>,
    standing: Arc<std::sync::Mutex<BuildStanding>>,
}

pub(super) fn spawn_slots<F, Fut>(
    resolved_snapshot: &ResolvedRuntimeSnapshot,
    generation: u64,
    retry_policy: RetryPolicy,
    runner: F,
    shutdown: watch::Receiver<bool>,
    failure_policy: Option<Arc<dyn SlotFailurePolicy>>,
) -> RapidHashMap<String, AgentSlot>
where
    F: Fn(
            AgentWithTools,
            Arc<ToolSurface>,
            Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
            Arc<WorkerCapacity>,
            u64,
            watch::Receiver<bool>,
        ) -> Fut
        + Send
        + Sync
        + Clone
        + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let mut slots = RapidHashMap::with_capacity(resolved_snapshot.agents.len());
    for agent_did in resolved_snapshot.agents.keys() {
        let tool_surface = resolved_snapshot
            .tool_surfaces
            .get(agent_did)
            .cloned()
            .expect("resolved snapshot should include tool surfaces for runnable agents");
        let agent_config = &resolved_snapshot
            .agent_with_tools(agent_did)
            .expect("resolved snapshot should include tool configurations for runnable agents");
        slots.insert(
            agent_did.clone(),
            spawn_slot_with_layout(
                SlotConfig {
                    agent_config: agent_config.clone(),
                    tool_surface,
                    generation,
                    retry_policy: retry_policy.clone(),
                    failure_policy: failure_policy.clone(),
                },
                WorkerLayout::AGENT,
                runner.clone(),
                shutdown.clone(),
            ),
        );
    }
    slots
}

#[cfg(test)]
pub(super) fn spawn_slot<F, Fut>(
    agent_config: AgentWithTools,
    tool_surface: Arc<ToolSurface>,
    retry_policy: RetryPolicy,
    runner: F,
    shutdown: watch::Receiver<bool>,
) -> AgentSlot
where
    F: Fn(
            AgentWithTools,
            Arc<ToolSurface>,
            Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
            Arc<WorkerCapacity>,
            u64,
            watch::Receiver<bool>,
        ) -> Fut
        + Send
        + Sync
        + Clone
        + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    spawn_slot_with_layout(
        SlotConfig {
            agent_config,
            tool_surface,
            generation: 1,
            retry_policy,
            failure_policy: None,
        },
        WorkerLayout {
            levels: 1,
            level_workers: 1,
        },
        runner,
        shutdown,
    )
}

pub(super) fn spawn_slot_with_layout<F, Fut>(
    slot: SlotConfig,
    layout: WorkerLayout,
    runner: F,
    shutdown: watch::Receiver<bool>,
) -> AgentSlot
where
    F: Fn(
            AgentWithTools,
            Arc<ToolSurface>,
            Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
            Arc<WorkerCapacity>,
            u64,
            watch::Receiver<bool>,
        ) -> Fut
        + Send
        + Sync
        + Clone
        + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let SlotConfig {
        agent_config,
        tool_surface,
        generation,
        retry_policy,
        failure_policy,
    } = slot;
    let (mut senders, receivers): (Vec<_>, Vec<_>) = (0..=layout.levels)
        .map(|_| mpsc::channel(AGENT_EXECUTOR_QUEUE_CAPACITY))
        .unzip();
    let auxiliary = senders
        .pop()
        .expect("the auxiliary lane follows the levels");
    let dispatcher = AgentDispatcher::new(senders, auxiliary);
    let queue_capacity = dispatcher.max_capacity();
    let (state_tx, state_rx) = watch::channel(AgentSlotState::Active);
    let agent_fingerprint = agent_config.slot_fingerprint();
    let tool_surface_fingerprint = format!("{tool_surface:?}");

    let standing = Arc::new(std::sync::Mutex::new(BuildStanding::seeded()));
    let lanes = receivers
        .into_iter()
        .map(|request_rx| SlotRuntime {
            request_rx: Arc::new(Mutex::new(request_rx)),
            capacity: WorkerCapacity::new(layout.level_workers),
            shutdown: shutdown.clone(),
            state_rx: state_rx.clone(),
            standing: standing.clone(),
        })
        .collect();
    let handle = AbortOnDropHandle::new(tokio::spawn(run_slot_workers(
        SlotConfig {
            agent_config,
            tool_surface,
            generation,
            retry_policy,
            failure_policy,
        },
        lanes,
        layout,
        runner,
    )));

    AgentSlot {
        dispatcher,
        state_tx,
        handle,
        agent_fingerprint,
        tool_surface_fingerprint,
        executor_capacity: layout.workers(),
        #[cfg(test)]
        worker_task_count: layout.levels + 1,
        standing,
        queue_capacity,
        generation,
    }
}

#[cfg(test)]
pub(super) fn retire_slot(slot: AgentSlot) {
    let _ = slot.state_tx.send(AgentSlotState::Retiring);
    drop(slot.dispatcher);
    tokio::spawn(async move {
        match slot.handle.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::error!(error = %error, "retired agent slot failed"),
            Err(error) if !error.is_cancelled() => {
                tracing::error!(error = %error, "retired agent slot join failed");
            }
            Err(_) => {}
        }
    });
}

async fn run_slot_loop<F, Fut>(slot: SlotConfig, runtime: SlotRuntime, runner: F) -> Result<()>
where
    F: Fn(
            AgentWithTools,
            Arc<ToolSurface>,
            Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
            Arc<WorkerCapacity>,
            u64,
            watch::Receiver<bool>,
        ) -> Fut
        + Send
        + Sync
        + Clone
        + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let SlotConfig {
        agent_config,
        tool_surface,
        generation,
        retry_policy,
        failure_policy,
    } = slot;
    let SlotRuntime {
        request_rx,
        capacity,
        mut shutdown,
        mut state_rx,
        standing,
    } = runtime;
    let mut failure_count = 0u32;
    loop {
        if *shutdown.borrow() || *state_rx.borrow() == AgentSlotState::Retiring {
            return Ok(());
        }
        if standing
            .lock()
            .map(|standing| *standing == BuildStanding::Demoted)
            .unwrap_or(false)
        {
            park_until_retired(&mut shutdown, &mut state_rx).await;
            return Ok(());
        }

        let outcome = AssertUnwindSafe(runner(
            agent_config.clone(),
            tool_surface.clone(),
            request_rx.clone(),
            capacity.clone(),
            generation,
            shutdown.clone(),
        ))
        .catch_unwind()
        .await;

        if *shutdown.borrow() {
            return match outcome {
                Ok(Err(error)) => Err(error),
                Err(_) => anyhow::bail!("agent runner panicked during shutdown"),
                Ok(Ok(())) => Ok(()),
            };
        }

        match outcome {
            Ok(Ok(())) if *state_rx.borrow() == AgentSlotState::Retiring => return Ok(()),
            Ok(Ok(())) => {
                if let Ok(mut standing) = standing.lock() {
                    *standing = standing.step(u32::MAX, BuildOutcome::Started);
                }
                let delay = retry_policy.delay_for_attempt(failure_count);
                failure_count += 1;
                tracing::warn!(
                    agent_did = %agent_config.agent.agent_did,
                    delay_ms = delay.as_millis() as u64,
                    "agent slot exited unexpectedly, scheduling restart"
                );
                if !wait_for_restart(delay, &mut shutdown).await {
                    return Ok(());
                }
            }
            Ok(Err(error)) => {
                if handle_slot_failure(
                    &agent_config.agent.agent_did,
                    generation,
                    &error,
                    failure_policy.as_deref(),
                    &standing,
                    &mut shutdown,
                    &mut state_rx,
                )
                .await
                {
                    return Ok(());
                }
                let delay = retry_policy.delay_for_attempt(failure_count);
                failure_count += 1;
                tracing::error!(
                    agent_did = %agent_config.agent.agent_did,
                    error = %gents_protocol::error_chain::ErrorChain(error.as_ref()),
                    delay_ms = delay.as_millis() as u64,
                    "agent slot failed, scheduling restart"
                );
                if !wait_for_restart(delay, &mut shutdown).await {
                    return Ok(());
                }
            }
            Err(_) => {
                if handle_slot_failure(
                    &agent_config.agent.agent_did,
                    generation,
                    &anyhow::anyhow!("agent runner panicked"),
                    failure_policy.as_deref(),
                    &standing,
                    &mut shutdown,
                    &mut state_rx,
                )
                .await
                {
                    return Ok(());
                }
                let delay = retry_policy.delay_for_attempt(failure_count);
                failure_count += 1;
                tracing::error!(
                    agent_did = %agent_config.agent.agent_did,
                    delay_ms = delay.as_millis() as u64,
                    "agent slot panicked, scheduling restart"
                );
                if !wait_for_restart(delay, &mut shutdown).await {
                    return Ok(());
                }
            }
        }
    }
}

/// `lanes` holds one runtime per worker level and, last, the auxiliary lane,
/// whose one worker only starts auxiliary requests and never grows.
async fn run_slot_workers<F, Fut>(
    slot: SlotConfig,
    lanes: Vec<SlotRuntime>,
    layout: WorkerLayout,
    runner: F,
) -> Result<()>
where
    F: Fn(
            AgentWithTools,
            Arc<ToolSurface>,
            Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
            Arc<WorkerCapacity>,
            u64,
            watch::Receiver<bool>,
        ) -> Fut
        + Send
        + Sync
        + Clone
        + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let agent_did = slot.agent_config.agent.agent_did.clone();
    tracing::info!(
        agent_did = %agent_did,
        levels = layout.levels,
        level_workers = layout.level_workers,
        queue_capacity = AGENT_EXECUTOR_QUEUE_CAPACITY,
        "agent executor worker pool starting"
    );
    let mut workers = JoinSet::new();
    let mut spawned = vec![0; lanes.len()];
    let spawn = |workers: &mut JoinSet<Result<()>>, spawned: &mut [usize], lane: usize| {
        workers.spawn(run_slot_loop(
            slot.clone(),
            lanes[lane].clone(),
            runner.clone(),
        ));
        spawned[lane] += 1;
        tracing::debug!(
            agent_did = %agent_did,
            lane,
            workers = spawned[lane],
            "agent executor worker spawned"
        );
    };
    for lane in 0..lanes.len() {
        spawn(&mut workers, &mut spawned, lane);
    }
    let mut first_error = None;
    loop {
        let growing = lanes[..layout.levels]
            .iter()
            .zip(&spawned)
            .enumerate()
            .filter(|(_, (_, spawned))| **spawned < layout.level_workers)
            .map(|(level, (runtime, spawned))| {
                let capacity = runtime.capacity.clone();
                let spawned = *spawned;
                Box::pin(async move {
                    capacity.saturated(spawned).await;
                    level
                })
            })
            .collect::<Vec<_>>();
        let saturated = async {
            if growing.is_empty() {
                std::future::pending().await
            } else {
                futures::future::select_all(growing).await.0
            }
        };
        tokio::select! {
            level = saturated => spawn(&mut workers, &mut spawned, level),
            joined = workers.join_next() => {
                let Some(joined) = joined else {
                    break;
                };
                let error = match joined {
                    Ok(Ok(())) => continue,
                    Ok(Err(error)) => error,
                    Err(error) => anyhow::anyhow!("agent executor worker task join failed: {error}"),
                };
                tracing::error!(agent_did = %agent_did, error = %error, "agent executor worker failed");
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

async fn wait_for_restart(
    delay: std::time::Duration,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => true,
        _ = shutdown.changed() => false,
    }
}
