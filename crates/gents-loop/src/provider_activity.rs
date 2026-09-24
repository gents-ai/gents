//! Transport liveness of one armed provider attempt.
//!
//! The capturing transport stamps this when it starts the send, when response
//! headers arrive, and on every received body chunk; it settles it once the
//! provider's protocol terminal event or end of body is seen. The owned loop
//! reads it to decide provider idleness. Bytes rather than decoded items are
//! the signal because healthy providers stream long runs that decode to no
//! loop item (Anthropic thinking/signature/ping/input-JSON deltas, silent
//! Responses reasoning). Admission waits happen before the send stamp, and
//! durable finalization after the settle, so neither counts as idleness.

use std::sync::Mutex;

use tokio::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ActivityPhase {
    /// Armed, but the transport has not started sending.
    #[default]
    AwaitingSend,
    /// The send is in flight; the instant of the last transport activity.
    Active(Instant),
    /// The provider response is complete at the transport.
    Settled,
}

#[derive(Debug, Default)]
struct ActivityState {
    phase: ActivityPhase,
    stall_reason: Option<String>,
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

    /// Recorded before the stalled attempt's stream is dropped, so the
    /// admission permit's drop path can report why the call ended.
    pub fn mark_stalled(&self, reason: String) {
        self.lock().stall_reason = Some(reason);
    }

    pub fn stall_reason(&self) -> Option<String> {
        self.lock().stall_reason.clone()
    }
}
