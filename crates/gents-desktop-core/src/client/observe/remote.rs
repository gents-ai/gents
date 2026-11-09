use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gents::store::config_client::{ConfigAccess, GraphqlEndpoint, TransactionFailure};
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};
use tokio::time::Instant;

use super::{ObservedStore, ObserverMetrics, RuntimeConnectionEvent};
use gents::store::config_client::SubscriptionEnd;
use crate::client::query::OBSERVED_COLLECTIONS;
use crate::client::PeerRecord;

/// One event of a source task (`RuntimeConnectionObservation.armedEvent` or
/// `streamEndEvent`).
pub(super) struct Notice {
    pub node: String,
    pub generation: u64,
    pub event: RuntimeConnectionEvent,
}

/// `RuntimeConnectionObservation.armedEvent`.
pub(super) fn armed_event(renewal: bool) -> RuntimeConnectionEvent {
    if renewal {
        RuntimeConnectionEvent::Renewed
    } else {
        RuntimeConnectionEvent::SubscriptionsReady
    }
}

/// `RuntimeConnectionObservation.armedAfter`.
pub(super) fn armed_after(was_armed: bool, event: RuntimeConnectionEvent) -> bool {
    match event {
        RuntimeConnectionEvent::SubscriptionsReady | RuntimeConnectionEvent::Renewed => true,
        RuntimeConnectionEvent::TransportFailed | RuntimeConnectionEvent::Refused => false,
        RuntimeConnectionEvent::SourceStarted
        | RuntimeConnectionEvent::SnapshotSucceeded
        | RuntimeConnectionEvent::EndpointMissing => was_armed,
    }
}

/// `RuntimeConnectionObservation.SnapshotOutcome` of a read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SnapshotOutcome {
    Succeeded,
    Refused,
    Unreachable,
}

/// A failed read is `refused` exactly when the access owner classifies it
/// so; an omitted root is not a failure (`load_node_snapshot_on`).
pub(super) fn snapshot_outcome(error: &anyhow::Error) -> SnapshotOutcome {
    if error.permission_denied() {
        SnapshotOutcome::Refused
    } else {
        SnapshotOutcome::Unreachable
    }
}

/// `RuntimeConnectionObservation.snapshotEvent`.
pub(super) fn snapshot_event(outcome: SnapshotOutcome) -> RuntimeConnectionEvent {
    match outcome {
        SnapshotOutcome::Succeeded => RuntimeConnectionEvent::SnapshotSucceeded,
        SnapshotOutcome::Refused => RuntimeConnectionEvent::Refused,
        SnapshotOutcome::Unreachable => RuntimeConnectionEvent::TransportFailed,
    }
}

/// `RuntimeConnectionObservation.renewalStartMs`: a full period after the arm
/// that began at `started` completed.
pub(super) fn renewal_start(started: Instant, arm: Duration, period: Duration) -> Instant {
    started + arm + period
}

pub(super) struct Source {
    pub endpoint: GraphqlEndpoint,
    pub generation: u64,
    /// When the ended source reconnects at the same endpoint; `None` while
    /// its task runs.
    reconnect_at: Option<Instant>,
    abort: AbortHandle,
}

impl Source {
    /// The source ended and waits for its reconnect; its notices and loads
    /// are no longer current.
    pub(super) fn ended(&self) -> bool {
        self.reconnect_at.is_some()
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

#[derive(Default)]
pub(super) struct Sources {
    pub entries: BTreeMap<String, Source>,
    /// Consecutive reconnects per node (`RuntimeConnectionObservation.nextAttempts`).
    attempts: BTreeMap<String, u32>,
    /// Nodes whose due reconnect dropped their source; the next reconcile
    /// restarts them at this endpoint without resetting the shown status.
    reconnecting: BTreeMap<String, String>,
    generation: u64,
    /// Every node this owner last published a connection status for.
    observed: BTreeSet<String>,
}

const RECONNECT_BASE_MS: u64 = 250;
const RECONNECT_FLAT_ATTEMPTS: u32 = 5;
const RECONNECT_CAP_MS: u64 = 30_000;

/// `RuntimeConnectionObservation.reconnectDelayMs`: the one delay of every
/// reconnect, after a transport failure or a refusal.
pub(super) fn reconnect_delay(attempt: u32) -> Duration {
    if attempt < RECONNECT_FLAT_ATTEMPTS {
        return Duration::from_millis(RECONNECT_BASE_MS);
    }
    Duration::from_millis(
        1_u64
            .checked_shl(attempt + 1 - RECONNECT_FLAT_ATTEMPTS)
            .and_then(|factor| factor.checked_mul(RECONNECT_BASE_MS))
            .map_or(RECONNECT_CAP_MS, |delay| delay.min(RECONNECT_CAP_MS)),
    )
}

/// `RuntimeConnectionObservation.reconnects`.
pub(super) fn reconnects(event: RuntimeConnectionEvent) -> bool {
    matches!(
        event,
        RuntimeConnectionEvent::TransportFailed | RuntimeConnectionEvent::Refused
    )
}

/// `RuntimeConnectionObservation.nextAttempts`.
pub(super) fn next_attempts(attempts: u32, event: RuntimeConnectionEvent) -> u32 {
    match event {
        RuntimeConnectionEvent::TransportFailed | RuntimeConnectionEvent::Refused => {
            attempts.saturating_add(1)
        }
        RuntimeConnectionEvent::SnapshotSucceeded => 0,
        RuntimeConnectionEvent::SourceStarted
        | RuntimeConnectionEvent::SubscriptionsReady
        | RuntimeConnectionEvent::Renewed
        | RuntimeConnectionEvent::EndpointMissing => attempts,
    }
}

impl Sources {
    /// Publishes `event` for `node` and advances its reconnect schedule. An
    /// event that reconnects stops the source and schedules its restart at
    /// the same endpoint after `reconnect_delay` of its consecutive count.
    pub(super) fn observe(
        &mut self,
        node: &str,
        event: RuntimeConnectionEvent,
        store: &ObservedStore,
        now: Instant,
        metrics: &ObserverMetrics,
    ) {
        match event {
            RuntimeConnectionEvent::TransportFailed => {
                metrics.transport_failures.fetch_add(1, Ordering::Relaxed);
            }
            RuntimeConnectionEvent::Renewed => {
                metrics.subscription_renewals.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        store.observe_runtime_connection(node, event);
        let attempts = self.attempts.get(node).copied().unwrap_or_default();
        if reconnects(event) {
            if let Some(source) = self.entries.get_mut(node) {
                metrics.reconnects.fetch_add(1, Ordering::Relaxed);
                source.reconnect_at = Some(now + reconnect_delay(attempts));
                source.abort.abort();
            }
        }
        self.attempts
            .insert(node.to_owned(), next_attempts(attempts, event));
    }

    pub(super) fn next_reconnect(&self) -> Option<Instant> {
        self.entries
            .values()
            .filter_map(|source| source.reconnect_at)
            .min()
    }

    /// Drops ended sources whose reconnect is due so the next reconcile
    /// restarts them; returns whether any was dropped.
    pub(super) fn take_due_reconnects(&mut self, now: Instant) -> bool {
        let due: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, source)| source.reconnect_at.is_some_and(|due| due <= now))
            .map(|(node, source)| (node.clone(), source.endpoint.to_string()))
            .collect();
        for (node, endpoint) in &due {
            self.entries.remove(node);
            self.reconnecting.insert(node.clone(), endpoint.clone());
        }
        !due.is_empty()
    }

    /// Starts, keeps or drops each peer's source and owns every node's
    /// connection status: a peer without an operator endpoint shows
    /// `noEndpoint`, and a node no longer in the directory has no status.
    pub(super) fn reconcile(
        &mut self,
        peers: &[PeerRecord],
        capability: &gents::store::config_client::ClientSigningCapability,
        notices: &mpsc::UnboundedSender<Notice>,
        store: &Arc<ObservedStore>,
        metrics: &Arc<ObserverMetrics>,
    ) {
        let mut unavailable = BTreeSet::new();
        let endpoints: BTreeMap<_, _> = peers
            .iter()
            .filter_map(|peer| {
                let endpoint = crate::local_runtime::operator_endpoint(peer, capability);
                if endpoint.is_none() {
                    unavailable.insert(peer.node_did.clone());
                }
                endpoint.map(|endpoint| (peer.node_did.clone(), endpoint))
            })
            .collect();
        let observed: BTreeSet<String> =
            endpoints.keys().chain(&unavailable).cloned().collect();
        for removed in self.observed.difference(&observed) {
            store.set_runtime_connection(removed, None);
        }
        for node in unavailable.iter().filter(|node| !endpoints.contains_key(*node)) {
            store.observe_runtime_connection(node, RuntimeConnectionEvent::EndpointMissing);
        }
        self.observed = observed;
        self.entries.retain(|node, source| {
            endpoints
                .get(node)
                .is_some_and(|endpoint| endpoint.to_string() == source.endpoint.to_string())
                && (source.ended() || !source.abort.is_finished())
        });
        self.attempts.retain(|node, _| endpoints.contains_key(node));
        let reconnecting = std::mem::take(&mut self.reconnecting);
        for (node, endpoint) in endpoints {
            if self.entries.contains_key(&node) {
                continue;
            }
            // A reconnect keeps the shown status: an outage stays
            // `disconnected` until the runtime answers.
            if reconnecting.get(&node) != Some(&endpoint.to_string()) {
                store.observe_runtime_connection(&node, RuntimeConnectionEvent::SourceStarted);
            }
            self.generation += 1;
            let current = self.generation;
            let task = tokio::spawn(observe(
                node.clone(),
                endpoint.clone(),
                current,
                notices.clone(),
                store.clone(),
                metrics.clone(),
            ));
            self.entries.insert(
                node,
                Source {
                    endpoint,
                    generation: current,
                    reconnect_at: None,
                    abort: task.abort_handle(),
                },
            );
        }
    }
}

/// How a source's subscriptions ended (`RuntimeConnectionObservation.StreamEnd`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StreamEnd {
    Ended(SubscriptionEnd),
    /// `refused` is the access owner's classification of the failure.
    Failed { refused: bool },
}

/// `RuntimeConnectionObservation.streamEndEvent`.
pub(super) fn stream_end_event(end: StreamEnd) -> Option<RuntimeConnectionEvent> {
    match end {
        StreamEnd::Ended(SubscriptionEnd::RenewalDue) => None,
        StreamEnd::Ended(SubscriptionEnd::Completed) | StreamEnd::Failed { refused: false } => {
            Some(RuntimeConnectionEvent::TransportFailed)
        }
        StreamEnd::Failed { refused: true } => Some(RuntimeConnectionEvent::Refused),
    }
}

/// One armed subscription set. Dropping it aborts every stream.
struct Armed {
    tasks: JoinSet<Result<SubscriptionEnd>>,
    renew_at: Option<Instant>,
}

fn stream_end(
    node: &str,
    joined: Option<std::result::Result<Result<SubscriptionEnd>, tokio::task::JoinError>>,
) -> StreamEnd {
    match joined {
        None => StreamEnd::Ended(SubscriptionEnd::Completed),
        Some(Ok(Ok(end))) => StreamEnd::Ended(end),
        Some(Ok(Err(error))) => failed(node, &error),
        Some(Err(error)) => failed(node, &anyhow::Error::new(error)),
    }
}

fn failed(node: &str, error: &anyhow::Error) -> StreamEnd {
    tracing::warn!(node_did = %node, error = %error, "runtime subscription disconnected");
    StreamEnd::Failed {
        refused: error.permission_denied(),
    }
}

/// Every renewal period the source arms a replacement set while the current
/// set keeps streaming, then drops the old set and reports `renewed`
/// (make-before-break), so no change goes undelivered across a renewal. Any
/// other end reports its `streamEndEvent` and ends the task; the observer
/// reconnects it after `reconnect_delay`.
async fn observe(
    node: String,
    endpoint: GraphqlEndpoint,
    generation: u64,
    notices: mpsc::UnboundedSender<Notice>,
    store: Arc<ObservedStore>,
    metrics: Arc<ObserverMetrics>,
) {
    let send = |event| {
        notices
            .send(Notice {
                node: node.clone(),
                generation,
                event,
            })
            .is_ok()
    };
    let mut current: Option<Armed> = None;
    loop {
        let renewal = current.is_some();
        let armed = match current.as_mut() {
            None => arm(&node, &endpoint, &store, &metrics).await,
            Some(old) => tokio::select! {
                armed = arm(&node, &endpoint, &store, &metrics) => armed,
                joined = old.tasks.join_next() => Err(stream_end(&node, joined)),
            },
        };
        let ended = match armed {
            Ok(armed) => {
                let set = current.insert(armed);
                if !send(armed_event(renewal)) {
                    return;
                }
                match set.renew_at {
                    Some(deadline) => tokio::select! {
                        joined = set.tasks.join_next() => stream_end(&node, joined),
                        () = tokio::time::sleep_until(deadline) => continue,
                    },
                    None => stream_end(&node, set.tasks.join_next().await),
                }
            }
            Err(end) => end,
        };
        current = None;
        // Changes committed while no subscription is armed are not delivered.
        store.observe_request_event(None);
        let Some(event) = stream_end_event(ended) else {
            continue;
        };
        send(event);
        return;
    }
}

/// DefraDB emits changes only after subscription response headers; arm every
/// root before allowing a snapshot. The source takes each stream's renewal
/// cadence so the set is renewed as a whole, a full period after it is armed.
async fn arm(
    node: &str,
    endpoint: &GraphqlEndpoint,
    store: &Arc<ObservedStore>,
    metrics: &Arc<ObserverMetrics>,
) -> std::result::Result<Armed, StreamEnd> {
    let access = ConfigAccess::Graphql(endpoint.clone());
    let started = Instant::now();
    let mut period = None;
    let mut streams = Vec::with_capacity(OBSERVED_COLLECTIONS.len() + 2);
    for collection in OBSERVED_COLLECTIONS
        .iter()
        .copied()
        .chain(["AgentToolControl", "AgentToolControlClaim"])
    {
        let query =
            format!("subscription {{ {collection}(showDeleted: true) {{ _docID _deleted }} }}");
        let mut stream = access
            .subscribe(&query)
            .await
            .map_err(|error| failed(node, &anyhow::Error::new(error)))?;
        period = stream.take_renewal().or(period);
        streams.push((collection, stream));
    }
    let renew_at = period.map(|period| renewal_start(started, started.elapsed(), period));
    let mut tasks = JoinSet::new();
    for (collection, mut stream) in streams {
        let node = node.to_owned();
        let store = store.clone();
        let metrics = metrics.clone();
        tasks.spawn(async move {
            while let Some(envelope) = stream.recv().await? {
                super::delivery::deliver(&store, &metrics, &node, collection, &envelope)?;
            }
            Ok::<_, anyhow::Error>(stream.end().unwrap_or(SubscriptionEnd::Completed))
        });
    }
    Ok(Armed { tasks, renew_at })
}
