use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use defra_node::Node as EmbeddedNode;

use crate::triggers::event_delivery::{EventDeliveryRuntimeContract, EventDeliverySourceContract};
use crate::UpdateSubscriptionSource;
use gents_protocol::row::AgentRequest;

mod cooldown;
mod query;
#[cfg(test)]
mod tests;

pub(crate) use cooldown::GOSSIP_FALLBACK_POLL;
use cooldown::{
    prune_processed_requests, take_next_eligible_pending_request, PROCESSED_REQUEST_COOLDOWN,
};
pub(crate) use query::agent_request_from_mutation_response;

pub trait Watcher: Send + Sync {
    fn set_agent_identities(
        &mut self,
        _agents: Vec<Arc<dyn crate::store::identity::NodeIdentity>>,
    ) -> Result<()> {
        Ok(())
    }

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
    agents: Vec<Arc<dyn crate::store::identity::NodeIdentity>>,
    scan_failed: bool,
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
        let subscription =
            subs.subscribe_collection_changes(crate::collection_ids(&node, ["AgentRequest"])?);
        Ok(Self {
            node,
            node_did: node_did.to_string(),
            subscription,
            processed_request_ids: RapidHashMap::new(),
            agents: Vec::new(),
            scan_failed: false,
        })
    }

    pub fn for_agents(
        node: Arc<EmbeddedNode>,
        node_did: &str,
        agents: Vec<Arc<dyn crate::store::identity::NodeIdentity>>,
    ) -> Result<Self> {
        let mut watcher = Self::new(node, node_did)?;
        watcher.set_agent_identities(agents)?;
        Ok(watcher)
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
        mut agents: Vec<Arc<dyn crate::store::identity::NodeIdentity>>,
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

            match self.pending_requests().await {
                Ok(requests) => {
                    let pending_count = requests.len();
                    if let Some(request) = take_next_eligible_pending_request(
                        &mut self.processed_request_ids,
                        requests,
                        now,
                    ) {
                        return Some(Ok(request));
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

            let batch = match wait_for_request_change(&mut self.subscription).await {
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
