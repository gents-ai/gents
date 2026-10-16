//! Transport liveness of one armed provider attempt, stamped by the capturing
//! transport. Bytes rather than decoded items are the signal because healthy
//! providers stream long runs that decode to no loop item. Admission waits
//! happen before the send stamp and durable finalization after the settle, so
//! neither counts as idleness.

use std::sync::Mutex;

use tokio::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ActivityPhase {
    #[default]
    AwaitingSend,
    Active(Instant),
    Settled,
}

#[derive(Debug, Default)]
struct ActivityState {
    phase: ActivityPhase,
    stall_reason: Option<String>,
    token_limit_reached: bool,
}

#[derive(Debug, Default)]
pub struct ProviderActivity {
    state: Mutex<ActivityState>,
}

impl ProviderActivity {
    fn lock(&self) -> std::sync::MutexGuard<'_, ActivityState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn touch(&self) {
        self.lock().phase = ActivityPhase::Active(Instant::now());
    }

    pub fn settle(&self) {
        self.lock().phase = ActivityPhase::Settled;
    }

    pub fn phase(&self) -> ActivityPhase {
        self.lock().phase
    }

    pub(crate) fn observe_token_limit(&self) {
        self.lock().token_limit_reached = true;
    }

    pub(crate) fn token_limit_reached(&self) -> bool {
        self.lock().token_limit_reached
    }

    /// Recorded before the stalled attempt's stream is dropped, so the
    /// admission permit's drop path can report why the call ended.
    pub fn mark_stalled(&self, reason: String) {
        self.lock().stall_reason = Some(reason);
    }

    pub fn stall_reason(&self) -> Option<String> {
        self.lock().stall_reason.clone()
    }
}
