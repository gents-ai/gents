//! Trigger engine scaffold.
//!
//! Unifies how schedules, event triggers, and manual requests are turned into
//! `AgentRequest` materializations. The full engine is built up across
//! Tasks 27-33; this module currently defines only the public types that
//! downstream tasks will consume.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{watch, Mutex};
use tokio_util::sync::CancellationToken;

use crate::runtime_snapshot::ActiveRuntimeSnapshot;

#[cfg(test)]
mod tests;

/// Kind of trigger that produced a fire intent.
///
/// Schedule and Event triggers both drive the engine from stored trigger
/// documents. Manual is reserved for direct fire requests (e.g. CLI / API
/// invocations) that do not have a persisted trigger document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TriggerKind {
    Schedule,
    Event,
    Manual,
}

impl TriggerKind {
    /// Lowercase string representation used in persisted fields (e.g.
    /// `AgentRequest.trigger_kind`) and log/metric labels.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            TriggerKind::Schedule => "schedule",
            TriggerKind::Event => "event",
            TriggerKind::Manual => "manual",
        }
    }
}

/// A single fire attempt produced by a `TriggerSource`.
///
/// Carries everything the engine needs to render the prompt and materialize a
/// request: the resolved task, the concurrency policy, the template variable
/// bags, and a one-shot `on_result` callback so the source can react (e.g.
/// write back `last_status` bookkeeping on the trigger document).
pub(crate) struct FireIntent {
    pub(crate) trigger_id: Option<String>,
    pub(crate) trigger_kind: TriggerKind,
    pub(crate) task: crate::runtime_snapshot::ResolvedTask,
    pub(crate) concurrency: crate::runtime_snapshot::ConcurrencyMode,
    pub(crate) event_vars: serde_json::Value,
    pub(crate) doc_vars: Option<serde_json::Value>,
    pub(crate) args_vars: Option<serde_json::Value>,
    pub(crate) on_result: Box<dyn FnOnce(FireResult) + Send>,
}

/// Outcome of a dispatched `FireIntent`.
///
/// `Fired` means the engine successfully materialized an `AgentRequest` and
/// carries the new request id. `Skipped` is a policy outcome (e.g. serial
/// concurrency found an in-flight run) and is not an error. `Errored` is an
/// unexpected failure that the source should record and, in most cases, retry
/// on a later tick.
#[derive(Debug, Clone)]
pub(crate) enum FireResult {
    Fired { request_id: String },
    Skipped { reason: String },
    Errored { error: String },
}

/// Persisted-status projection of a `FireResult`.
///
/// `FireResult` carries the per-outcome payload (request id / reason / error);
/// `FireAttemptStatus` is the compact enum form that lands in the
/// `last_status` field on `Schedule` and `EventTrigger` documents via the
/// source's `on_result` callback. Keeping the two apart lets the engine stay
/// free of persistence-layer wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FireAttemptStatus {
    Fired,
    Skipped,
    Errored,
}

impl FireAttemptStatus {
    /// String form matching the GraphQL-persisted `last_status` values.
    ///
    /// Note: `Errored` serializes to `"error"` (not `"errored"`) to match the
    /// existing schema vocabulary on `Schedule.last_status` /
    /// `EventTrigger.last_status`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            FireAttemptStatus::Fired => "fired",
            FireAttemptStatus::Skipped => "skipped",
            FireAttemptStatus::Errored => "error",
        }
    }
}

/// Stream of fire intents produced by a source (e.g. the schedule clock, an
/// event-queue poller, a manual-fire inbox).
///
/// Sources are polled by the engine's main loop; returning `None` indicates
/// the source is exhausted and should be dropped.
pub(crate) trait TriggerSource: Send + Sync {
    fn next_fire(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<FireIntent>> + Send + '_>>;
}

/// Abstraction over the request-materialization + trigger-aware bookkeeping
/// the engine needs at fire time.
///
/// Production wiring (Task 30+) will point this at `crate::lifecycle`'s
/// materialize entry points; tests can provide a lightweight spy. Keeping
/// this as a trait avoids the engine having to depend directly on the
/// lifecycle module while the two layers are still being wired together.
pub(crate) trait MaterializerHandle: Send + Sync {
    /// Create a new `AgentRequest` for `task` with the rendered prompt, using
    /// `trigger_id` / `trigger_kind` as the provenance recorded on the
    /// materialized document. Returns the new request id.
    fn materialize(
        &self,
        task: &crate::runtime_snapshot::ResolvedTask,
        trigger_id: Option<&str>,
        trigger_kind: TriggerKind,
        rendered_prompt: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send + '_>>;

    /// Check whether any non-terminal `AgentRequest` is currently bound to
    /// this trigger. Used by the concurrency gate to decide whether a new
    /// fire should skip or supersede.
    fn has_nonterminal_request_for_trigger(
        &self,
        trigger_id: &str,
        trigger_kind: TriggerKind,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<bool>> + Send + '_>>;

    /// Supersede any non-terminal requests bound to this trigger. Returns the
    /// number of requests transitioned. Invoked by `LatestOnly` concurrency
    /// before materializing the new fire.
    fn supersede_nonterminal_requests_for_trigger(
        &self,
        trigger_id: &str,
        trigger_kind: TriggerKind,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<usize>> + Send + '_>>;
}

/// Scaffolding for the trigger engine.
///
/// The engine owns a read handle onto the active runtime snapshot (to look up
/// behaviors / concurrency / enabled gates at fire time) and a materializer
/// handle (to create requests). Per-trigger mutexes serialize dispatches that
/// share a trigger id so the concurrency gate and request materialization
/// land atomically with respect to each other.
pub(crate) struct TriggerEngine {
    #[allow(dead_code)]
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    #[allow(dead_code)]
    materializer: Arc<dyn MaterializerHandle>,
    #[allow(dead_code)]
    per_trigger_locks: Mutex<HashMap<(String, TriggerKind), Arc<Mutex<()>>>>,
}

impl TriggerEngine {
    pub(crate) fn new(
        snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
        materializer: Arc<dyn MaterializerHandle>,
    ) -> Self {
        Self {
            snapshot_rx,
            materializer,
            per_trigger_locks: Mutex::new(HashMap::new()),
        }
    }

    /// Run the engine until `cancel` is triggered.
    ///
    /// Scaffold only: Tasks 30-33 will drive `sources` via
    /// `FuturesUnordered` / `select_all`, funnel each yielded `FireIntent`
    /// into `dispatch`, and honor `cancel` for graceful shutdown.
    pub(crate) async fn run(
        self,
        mut sources: Vec<Box<dyn TriggerSource>>,
        cancel: CancellationToken,
    ) {
        // TODO(Task 30-33): drive sources via FuturesUnordered / select_all,
        // funnel into `dispatch`.
        let _ = (&mut sources, &cancel);
        tracing::warn!("TriggerEngine::run is a scaffold; no sources are driven yet");
    }

    /// Dispatch a single `FireIntent`.
    ///
    /// Current scope (Tasks 30-33): enabled gate against the active snapshot,
    /// then render the task's prompt template (render failures return
    /// `Errored` without materializing), then apply the intent's concurrency
    /// mode (`Parallel` fires unconditionally; `Serial` skips when a
    /// non-terminal request already exists for this trigger tuple;
    /// `LatestOnly` supersedes any prior non-terminal request under a
    /// per-trigger lock), then hand the rendered prompt to the materializer.
    #[allow(dead_code)]
    async fn dispatch(&self, intent: FireIntent) -> FireResult {
        let snapshot = self.snapshot_rx.borrow().clone();

        // 1. Enabled gate.
        match intent.trigger_kind {
            TriggerKind::Schedule => {
                let Some(trigger_id) = intent.trigger_id.as_deref() else {
                    let result = FireResult::Errored {
                        error: "Schedule trigger missing trigger_id".to_string(),
                    };
                    (intent.on_result)(result.clone());
                    return result;
                };
                if snapshot.active_schedules().get(trigger_id).is_none() {
                    let result = FireResult::Skipped {
                        reason: "trigger disabled".to_string(),
                    };
                    (intent.on_result)(result.clone());
                    return result;
                }
            }
            TriggerKind::Event => {
                let Some(trigger_id) = intent.trigger_id.as_deref() else {
                    let result = FireResult::Errored {
                        error: "Event trigger missing trigger_id".to_string(),
                    };
                    (intent.on_result)(result.clone());
                    return result;
                };
                if snapshot.active_event_triggers().get(trigger_id).is_none() {
                    let result = FireResult::Skipped {
                        reason: "trigger disabled".to_string(),
                    };
                    (intent.on_result)(result.clone());
                    return result;
                }
            }
            TriggerKind::Manual => {
                // Manual runs bypass the enabled gate (operator-initiated).
            }
        }

        // 2. Render the prompt template against the intent's scope.
        let scope = crate::template::TemplateScope {
            event: intent.event_vars.clone(),
            doc: intent.doc_vars.clone(),
            args: intent.args_vars.clone(),
        };
        let rendered = match crate::template::render_template(&intent.task.prompt_template, &scope)
        {
            Ok(s) => s,
            Err(e) => {
                // Render failure is reported as `Errored` with a `template:`
                // prefix so upstream sources can route the result to
                // `last_status = "error"` bookkeeping without having to parse
                // the underlying minijinja message. The callback runs before
                // return so the dispatched source observes the same value the
                // caller will.
                let result = FireResult::Errored {
                    error: format!("template: {e}"),
                };
                (intent.on_result)(result.clone());
                return result;
            }
        };

        // 3. Concurrency gate. `Parallel` skips the check entirely. `Serial`
        // queries the materializer for a non-terminal request bound to the
        // same `(trigger_id, trigger_kind)` tuple — matching on the tuple is
        // load-bearing because trigger_id alone is not unique across kinds.
        // `LatestOnly` lands in Task 32.
        use crate::runtime_snapshot::ConcurrencyMode;
        match intent.concurrency {
            ConcurrencyMode::Parallel => {
                // No coordination; proceed to materialize.
            }
            ConcurrencyMode::Serial => {
                // Manual mode has trigger_id = None and isn't really
                // Serial-vs-Parallel in practice; bypass the in-flight check
                // if there's no trigger id to key on.
                if let Some(trigger_id) = intent.trigger_id.as_deref() {
                    match self
                        .materializer
                        .has_nonterminal_request_for_trigger(trigger_id, intent.trigger_kind)
                        .await
                    {
                        Ok(true) => {
                            let result = FireResult::Skipped {
                                reason: "serial: prior fire still in-flight".to_string(),
                            };
                            (intent.on_result)(result.clone());
                            return result;
                        }
                        Ok(false) => { /* no in-flight; proceed */ }
                        Err(e) => {
                            let result = FireResult::Errored {
                                error: format!("in-flight query: {e}"),
                            };
                            (intent.on_result)(result.clone());
                            return result;
                        }
                    }
                }
            }
            ConcurrencyMode::LatestOnly => {
                // Manual + LatestOnly is unusual (no persisted trigger id to
                // key a lock on); fall through to materialize without
                // serialization rather than error.
                if let Some(trigger_id) = intent.trigger_id.as_deref() {
                    // Acquire (or create) the per-trigger async mutex. The
                    // outer `per_trigger_locks` mutex is held only long enough
                    // to clone the `Arc<Mutex<()>>` for this `(trigger_id,
                    // trigger_kind)` tuple.
                    let lock_key = (trigger_id.to_owned(), intent.trigger_kind);
                    let lock = {
                        let mut map = self.per_trigger_locks.lock().await;
                        map.entry(lock_key)
                            .or_insert_with(|| Arc::new(Mutex::new(())))
                            .clone()
                    };
                    // `_guard` is bound at function scope so it outlives the
                    // materialize call below — supersede + materialize run
                    // inside the same critical section.
                    let _guard = lock.lock().await;
                    match self
                        .materializer
                        .supersede_nonterminal_requests_for_trigger(
                            trigger_id,
                            intent.trigger_kind,
                        )
                        .await
                    {
                        Ok(_count) => { /* proceed to materialize under lock */ }
                        Err(e) => {
                            let result = FireResult::Errored {
                                error: format!("supersede: {e}"),
                            };
                            (intent.on_result)(result.clone());
                            return result;
                        }
                    }
                    // Materialize under the held lock. `_guard` stays alive
                    // for the duration of this await because it's a local in
                    // this block and Rust drops locals only when the enclosing
                    // scope exits; `return` evaluates its expression first,
                    // so the guard covers the materialize call.
                    return self.materialize_after_lock(intent, rendered).await;
                }
            }
        }

        // 4. Materialize.
        self.materialize_after_lock(intent, rendered).await
    }

    /// Run the materialize step and fire the `on_result` callback. Factored
    /// out so the `LatestOnly` path can call it while still holding the
    /// per-trigger lock (the caller's `_guard` stays alive across this await
    /// because it was declared in the caller's outer scope).
    async fn materialize_after_lock(&self, intent: FireIntent, rendered: String) -> FireResult {
        let request_id = match self
            .materializer
            .materialize(
                &intent.task,
                intent.trigger_id.as_deref(),
                intent.trigger_kind,
                &rendered,
            )
            .await
        {
            Ok(id) => id,
            Err(e) => {
                let result = FireResult::Errored {
                    error: format!("materialize: {e}"),
                };
                (intent.on_result)(result.clone());
                return result;
            }
        };

        let result = FireResult::Fired { request_id };
        (intent.on_result)(result.clone());
        result
    }
}
