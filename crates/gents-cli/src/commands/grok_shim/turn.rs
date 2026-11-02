mod observe;
mod prompt;
mod request;
pub(super) mod sender;
mod stream;
pub(super) mod wire;

use rapidhash::{HashMapExt, RapidHashMap};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use defra_node::EmbeddedNode;
use serde_json::Value;
use tokio::sync::{oneshot, Mutex};

use super::projection::{RequestCursor, UpdateTimestamps};

const MAX_TRANSIENT_GRAPHQL_READ_RETRIES: usize = 4;

fn transient_graphql_read_retry_delay(retry: usize) -> Duration {
    Duration::from_millis(50 * retry.max(1) as u64)
}

/// Collections whose changes can produce novel projected session events.
const PROJECTED_COLLECTIONS: [&str; 10] = [
    "AgentRequest",
    "AgentSession",
    "AgentToolCall",
    "AgentMessage",
    "AgentOutputSegment",
    "InferenceCall",
    "CompactionEntry",
    "ProviderContextReduction",
    "Goal",
    "Trigger",
];

/// Goal panels read through owners whose rows lie outside
/// [`PROJECTED_COLLECTIONS`]; this bounds how stale they can become.
const PROJECTION_FALLBACK: Duration = Duration::from_secs(2);

fn subscribe_projected_changes(node: &EmbeddedNode) -> Option<events::DocumentChangeSubscription> {
    gents::subscribe_collections(node, PROJECTED_COLLECTIONS)
        .inspect_err(|error| {
            tracing::warn!(%error, "Grok shim projection polls without a change feed");
        })
        .ok()
}

/// Wait for a projected change, at most [`PROJECTION_FALLBACK`]. A closed
/// feed degrades to fallback polling.
async fn wait_for_projected_change(changes: &mut Option<events::DocumentChangeSubscription>) {
    match changes.as_mut() {
        Some(subscription) => {
            if let Ok(None) = tokio::time::timeout(PROJECTION_FALLBACK, subscription.recv()).await {
                *changes = None;
            }
        }
        None => tokio::time::sleep(PROJECTION_FALLBACK).await,
    }
}

struct RequestUpdateTiming {
    turn_start_ms: i64,
    active_segment: Option<String>,
    starts: BTreeMap<String, i64>,
    last_start_ms: Option<i64>,
}

impl RequestUpdateTiming {
    fn new(turn_start_ms: i64) -> Self {
        Self {
            turn_start_ms,
            active_segment: None,
            starts: BTreeMap::new(),
            last_start_ms: None,
        }
    }

    fn resolve(
        &mut self,
        timing: Option<&super::projection::ProjectionEventTiming>,
        now_ms: i64,
    ) -> UpdateTimestamps {
        if let Some(timing) = timing {
            let start = if let Some(start) = self.starts.get(&timing.segment_key) {
                *start
            } else {
                let candidate = timing.stream_start_candidate_ms.unwrap_or(now_ms);
                let start = self.last_start_ms.map_or(candidate, |previous| {
                    candidate.max(previous.saturating_add(1))
                });
                self.starts.insert(timing.segment_key.clone(), start);
                self.last_start_ms = Some(start);
                start
            };
            self.active_segment = Some(timing.segment_key.clone());
            let agent_timestamp_ms = timing
                .agent_timestamp_candidate_ms
                .unwrap_or(now_ms)
                .max(start);
            return UpdateTimestamps {
                agent_timestamp_ms: Some(agent_timestamp_ms),
                stream_start_ms: Some(start),
                turn_start_ms: Some(self.turn_start_ms),
            };
        }
        UpdateTimestamps {
            agent_timestamp_ms: Some(now_ms),
            stream_start_ms: self
                .active_segment
                .as_ref()
                .and_then(|segment| self.starts.get(segment).copied()),
            turn_start_ms: Some(self.turn_start_ms),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct TurnManagerConfig {
    pub actor: identity::Did,
    pub node_did: String,
    pub agent_id: String,
    pub graphql: String,
}

#[derive(Debug, Default)]
struct CancelBeforeIdLatch {
    cancelled: bool,
}

impl CancelBeforeIdLatch {
    fn cancel(&mut self) -> bool {
        let was_cancelled = self.cancelled;
        self.cancelled = true;
        !was_cancelled
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

struct PendingPrompt {
    response_tx: Option<oneshot::Sender<Result<Value>>>,
    request: Option<gents_protocol::row::AgentRequestRow>,
    cancel_before_id: CancelLatch,
    drained: bool,
}

impl PendingPrompt {
    fn resolve(&mut self, result: Result<Value>) {
        if let Some(tx) = self.response_tx.take() {
            let _ = tx.send(result);
        }
        self.drained = true;
    }
}

/// A pending prompt's session id and prompt id.
type PromptKey = (String, String);
type CancelLatch = Arc<Mutex<CancelBeforeIdLatch>>;

#[derive(Default)]
struct ConnectionState {
    closed: bool,
    entries: RapidHashMap<PromptKey, PendingPrompt>,
}

struct ObservedRequest {
    prompt_id: Option<String>,
    cursor: Mutex<RequestCursor>,
    timing: RequestUpdateTiming,
    completion_sent: bool,
    echo_sent: bool,
}

impl ObservedRequest {
    fn new(prompt_id: String, started_at: i64, foreground: bool) -> Self {
        Self {
            prompt_id: (!foreground).then_some(prompt_id),
            cursor: Mutex::new(RequestCursor::new()),
            timing: RequestUpdateTiming::new(started_at),
            completion_sent: foreground,
            echo_sent: true,
        }
    }
}

type ObservedRequests = BTreeMap<(String, String), Arc<Mutex<ObservedRequest>>>;

type ForegroundDeliveries = Arc<std::sync::Mutex<RapidHashMap<String, usize>>>;

pub(super) struct ForegroundDelivery {
    sessions: ForegroundDeliveries,
    session_id: String,
}

impl Drop for ForegroundDelivery {
    fn drop(&mut self) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(count) = sessions.get_mut(&self.session_id) {
            *count -= 1;
            if *count == 0 {
                sessions.remove(&self.session_id);
            }
        }
    }
}

impl ConnectionState {
    fn is_closed(&self) -> bool {
        self.closed
    }
}

pub(super) struct TurnManager {
    node: Arc<EmbeddedNode>,
    config: TurnManagerConfig,
    state: Mutex<ConnectionState>,
    delivery_gate: Mutex<()>,
    foreground_deliveries: ForegroundDeliveries,
    autonomous_delivery: Mutex<RapidHashMap<String, String>>,
    observed: Mutex<ObservedRequests>,
    observers: Mutex<RapidHashMap<String, tokio::task::JoinHandle<()>>>,
    #[cfg(test)]
    insertion_gate: Mutex<Option<TestGate>>,
    #[cfg(test)]
    disconnect_gate: Mutex<Option<TestGate>>,
    #[cfg(test)]
    cancel_drain_gate: Mutex<Option<TestGate>>,
    #[cfg(test)]
    cancel_selection_gate: Mutex<Option<TestGate>>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct TestGate {
    arrived: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl TurnManager {
    pub(super) fn new(node: Arc<EmbeddedNode>, config: TurnManagerConfig) -> Self {
        TurnManager {
            node,
            config,
            state: Mutex::new(ConnectionState::default()),
            delivery_gate: Mutex::new(()),
            foreground_deliveries: Arc::new(std::sync::Mutex::new(RapidHashMap::new())),
            autonomous_delivery: Mutex::new(RapidHashMap::new()),
            observed: Mutex::new(BTreeMap::new()),
            observers: Mutex::new(RapidHashMap::new()),
            #[cfg(test)]
            insertion_gate: Mutex::new(None),
            #[cfg(test)]
            disconnect_gate: Mutex::new(None),
            #[cfg(test)]
            cancel_drain_gate: Mutex::new(None),
            #[cfg(test)]
            cancel_selection_gate: Mutex::new(None),
        }
    }

    pub(super) async fn begin_foreground_delivery(&self, session_id: &str) -> ForegroundDelivery {
        let _delivery = self.delivery_gate.lock().await;
        *self
            .foreground_deliveries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(session_id.to_owned())
            .or_default() += 1;
        ForegroundDelivery {
            sessions: self.foreground_deliveries.clone(),
            session_id: session_id.to_owned(),
        }
    }
}

fn register_transient_read_retry(error: &anyhow::Error, consecutive: &mut usize) -> bool {
    if !gents_protocol::graphql::graphql_transport_error_in_chain(error.as_ref())
        || *consecutive >= MAX_TRANSIENT_GRAPHQL_READ_RETRIES
    {
        return false;
    }
    *consecutive += 1;
    true
}

#[cfg(test)]
mod tests;
