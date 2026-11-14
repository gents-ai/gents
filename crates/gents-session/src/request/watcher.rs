use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use defra_node::Node as EmbeddedNode;

use crate::request::event_delivery::{EventDeliveryRuntimeContract, EventDeliverySourceContract};
use gents_protocol::row::AgentRequest;
use gents_store::subscription_source::UpdateSubscriptionSource;

pub mod cooldown;
mod query;
#[cfg(test)]
mod tests;

pub(crate) use cooldown::GOSSIP_FALLBACK_POLL;
use cooldown::{
    prune_processed_requests, take_next_eligible_pending_request, PROCESSED_REQUEST_COOLDOWN,
};
pub use query::{agent_request_from_mutation_response, PendingRequestScan, WatcherScanError};

pub trait Watcher: Send + Sync {
    fn set_agent_identities(
        &mut self,
        _agents: Vec<Arc<dyn gents_store::identity::NodeIdentity>>,
    ) -> Result<()> {
        Ok(())
    }

    /// Lean `EventDelivery.Watcher.skippedActors`: the scan actors whose
    /// Agent is disabled.
    fn set_disabled_agents(&mut self, _disabled: RapidHashSet<String>) {}

    fn retains_agent(&self, _agent_did: &str) -> bool {
        true
    }

    fn next_request(
        &mut self,
    ) -> impl std::future::Future<Output = Option<Result<AgentRequest>>> + Send;
}

pub struct DefraWatcher {
    node: Arc<EmbeddedNode>,
    node_did: String,
    subscription: events::DocumentChangeSubscription,
    processed_request_ids: cooldown::ProcessedRequests,
    agents: Vec<Arc<dyn gents_store::identity::NodeIdentity>>,
    disabled: RapidHashSet<String>,
    scan_failed: bool,
    backend_health: Option<gents_inference::backends::health::BackendHealthMap>,
    backend_observations: Option<tokio::sync::watch::Receiver<()>>,
    has_parked: std::sync::atomic::AtomicBool,
    parked_wait_ms: std::sync::atomic::AtomicU64,
}

impl DefraWatcher {
    pub fn new(node: Arc<EmbeddedNode>, node_did: &str) -> Result<Self> {
        Self::with_subscription_source(node.clone(), node, node_did)
    }

    pub fn with_subscription_source(
        subs: Arc<dyn UpdateSubscriptionSource>,
        node: Arc<EmbeddedNode>,
        node_did: &str,
    ) -> Result<Self> {
        let subscription = subs.subscribe_collection_changes(
            gents_store::subscription_source::collection_ids(&node, ["AgentRequest"])?,
        );
        Ok(Self {
            node,
            node_did: node_did.to_string(),
            subscription,
            processed_request_ids: RapidHashMap::new(),
            agents: Vec::new(),
            disabled: RapidHashSet::new(),
            scan_failed: false,
            backend_health: None,
            backend_observations: None,
            has_parked: std::sync::atomic::AtomicBool::new(false),
            parked_wait_ms: std::sync::atomic::AtomicU64::new(
                GOSSIP_FALLBACK_POLL.as_millis() as u64
            ),
        })
    }

    pub fn for_agents(
        node: Arc<EmbeddedNode>,
        node_did: &str,
        agents: Vec<Arc<dyn gents_store::identity::NodeIdentity>>,
    ) -> Result<Self> {
        let mut watcher = Self::new(node, node_did)?;
        watcher.set_agent_identities(agents)?;
        Ok(watcher)
    }

    pub fn with_backend_health(
        mut self,
        health: gents_inference::backends::health::BackendHealthMap,
    ) -> Self {
        self.backend_observations = Some(health.subscribe_observations());
        self.backend_health = Some(health);
        self
    }
}

impl EventDeliveryRuntimeContract for DefraWatcher {
    const EVENT_DELIVERY_CONTRACT: EventDeliverySourceContract = EventDeliverySourceContract {
        name: "Watcher",
        dedupe_policy: "ttl_cooldown",
        rescan_bounded_by: 1,
        deviation: None,
    };
}

pub(crate) async fn wait_for_request_change(
    subscription: &mut events::DocumentChangeSubscription,
) -> Result<Option<events::DocumentChangeBatch>, tokio::time::error::Elapsed> {
    tokio::time::timeout(GOSSIP_FALLBACK_POLL, subscription.recv()).await
}

pub(crate) async fn wait_for_change_or_resubscribe<F>(
    subscription: &mut events::DocumentChangeSubscription,
    reopen: F,
) -> anyhow::Result<()>
where
    F: FnOnce() -> anyhow::Result<events::DocumentChangeSubscription>,
{
    if matches!(wait_for_request_change(subscription).await, Ok(None)) {
        tokio::time::sleep(GOSSIP_FALLBACK_POLL).await;
        *subscription = reopen()?;
    }
    Ok(())
}

impl Watcher for DefraWatcher {
    fn set_agent_identities(
        &mut self,
        mut agents: Vec<Arc<dyn gents_store::identity::NodeIdentity>>,
    ) -> Result<()> {
        for agent in &agents {
            anyhow::ensure!(
                agent.did() != self.node_did,
                "request watcher requires distinct agent identities"
            );
            identity::Did::new(agent.did())?;
        }
        agents.sort_by(|a, b| a.did().cmp(b.did()));
        agents.dedup_by(|a, b| a.did() == b.did());
        let unchanged = self.agents.len() == agents.len()
            && self
                .agents
                .iter()
                .zip(&agents)
                .all(|(a, b)| a.did() == b.did());
        if !unchanged {
            self.scan_failed = false;
        }
        self.agents = agents;
        Ok(())
    }

    fn set_disabled_agents(&mut self, disabled: RapidHashSet<String>) {
        if self.disabled != disabled {
            self.scan_failed = false;
        }
        self.disabled = disabled;
    }

    fn retains_agent(&self, agent_did: &str) -> bool {
        self.agents.iter().any(|agent| agent.did() == agent_did)
    }

    async fn next_request(&mut self) -> Option<Result<AgentRequest>> {
        loop {
            if self.scan_failed {
                match wait_for_request_change(&mut self.subscription).await {
                    Ok(None) => return None,
                    Ok(Some(_)) | Err(_) => self.scan_failed = false,
                }
            }
            let now = Instant::now();
            prune_processed_requests(&mut self.processed_request_ids, now);

            match self.scan_pending_requests().await {
                Ok(scan) => {
                    let needs_rescan = scan.needs_rescan();
                    let PendingRequestScan { requests, failures } = scan;
                    let pending_count = requests.len();
                    if let Some(request) = take_next_eligible_pending_request(
                        &mut self.processed_request_ids,
                        requests,
                        now,
                    ) {
                        return Some(Ok(request));
                    }
                    if let (true, Some(failure)) = (needs_rescan, failures.into_iter().next()) {
                        self.scan_failed = true;
                        return Some(Err(failure.into()));
                    }

                    if pending_count > 0 {
                        tracing::debug!(
                            pending_count,
                            cooldown_secs = PROCESSED_REQUEST_COOLDOWN.as_secs(),
                            "all pending requests are cooling down"
                        );
                    }
                }
                Err(e) => {
                    self.scan_failed = true;
                    return Some(Err(e));
                }
            }

            let watch_health = self.has_parked.load(std::sync::atomic::Ordering::Relaxed);
            let parked_wait = Duration::from_millis(
                self.parked_wait_ms
                    .load(std::sync::atomic::Ordering::Relaxed),
            );
            let batch = match tokio::select! {
                result = wait_for_request_change(&mut self.subscription) => result,
                _ = tokio::time::sleep(parked_wait), if watch_health => continue,
                result = async {
                    match self.backend_observations.as_mut() {
                        Some(observations) if watch_health => observations.changed().await,
                        _ => std::future::pending().await,
                    }
                } => {
                    if result.is_err() {
                        self.backend_observations = None;
                    }
                    continue;
                }
            } {
                Ok(Some(batch)) => batch,
                Ok(None) => return None,
                Err(_timeout) => {
                    tracing::trace!("gossip quiet, polling for pending requests");
                    continue;
                }
            };

            if batch.resync_required {
                tracing::warn!(
                    updates = batch.updates,
                    "request watcher document-change capacity exceeded; durable queue scan remains authoritative"
                );
            } else {
                tracing::trace!(
                    updates = batch.updates,
                    changed_documents = batch.changes.len(),
                    "coalesced DefraDB document changes into one request wakeup"
                );
            }

            continue;
        }
    }
}
