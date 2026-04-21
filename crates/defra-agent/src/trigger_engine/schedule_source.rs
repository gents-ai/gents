//! Schedule-backed `TriggerSource` skeleton.
//!
//! Polls the active runtime snapshot on a fixed tick for `Schedule` triggers
//! whose `next_fire_at` has elapsed and emits `FireIntent`s for them. The tick
//! loop, due-detection, and `next_fire_at`/`last_*` bookkeeping all land in
//! Task 35; this file is the plumbing skeleton so downstream tasks (39, etc.)
//! can wire it into the engine without a forward reference.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use defra_node::EmbeddedNode;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::runtime_snapshot::ActiveRuntimeSnapshot;
use crate::trigger_engine::{FireIntent, TriggerSource};

/// `TriggerSource` that drives the schedule clock.
///
/// Reads enabled schedules from the active runtime snapshot, ticks at
/// `tick_every` (default: 1s), and yields a `FireIntent` whenever a schedule's
/// `next_fire_at` has passed. Honors `cancel` for graceful shutdown.
pub(crate) struct ScheduleSource {
    #[allow(dead_code)]
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    #[allow(dead_code)]
    node: Arc<EmbeddedNode>,
    #[allow(dead_code)]
    tick_every: Duration,
    #[allow(dead_code)]
    cancel: CancellationToken,
}

impl ScheduleSource {
    /// Build a schedule source with the default 1s tick cadence.
    pub(crate) fn new(
        snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
        node: Arc<EmbeddedNode>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            snapshot_rx,
            node,
            tick_every: Duration::from_secs(1),
            cancel,
        }
    }
}

impl TriggerSource for ScheduleSource {
    fn next_fire(&mut self) -> Pin<Box<dyn Future<Output = Option<FireIntent>> + Send + '_>> {
        // TODO(Task 35): tick at `tick_every`, scan the active snapshot's
        // enabled schedules for those whose `next_fire_at` has elapsed, and
        // yield a `FireIntent` for the next due schedule. Honors `cancel` for
        // graceful shutdown by returning `None` when cancelled.
        Box::pin(async { None })
    }
}
