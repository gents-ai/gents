//! Trigger engine scaffold.
//!
//! Unifies how schedules, event triggers, and manual requests are turned into
//! `AgentRequest` materializations. The full engine is built up across
//! Tasks 27-33; this module currently defines only the public types that
//! downstream tasks will consume.

#[cfg(test)]
mod tests;

/// Kind of trigger that produced a fire intent.
///
/// Schedule and Event triggers both drive the engine from stored trigger
/// documents. Manual is reserved for direct fire requests (e.g. CLI / API
/// invocations) that do not have a persisted trigger document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
