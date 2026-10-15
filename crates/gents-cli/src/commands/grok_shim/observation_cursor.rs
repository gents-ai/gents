use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

use super::projection::{ProjectionEngine, UpdateTimestamps};
use super::turn::{PromptSender, PromptSenderLine};

/// Run views loaded per observation tick. Each load is several queries on
/// the session observer's shared loop, so a session that resumes with many
/// runs is covered round-robin instead of delaying its token stream.
pub(super) const LOADS_PER_TICK: usize = 4;

/// The revision of the terminal frame sent when observation stops. The pager
/// keeps the highest revision it holds across leader reconnects (see
/// [`durable_revision`]), so this frame must outrank anything an earlier
/// observer delivered. Stopping is final for this actor, so no frame follows
/// it. 2^53 - 1 is the largest integer a JSON number keeps exactly as an
/// IEEE double.
pub(super) const ABANDONED_REVISION: u64 = (1 << 53) - 1;

/// Upper bound on the delay before re-reading a run or `run_graph` reply
/// whose last read failed.
pub(super) const MAX_RETRY_DELAY_SECS: i64 = 30;

/// The display-only failure streak of one observed subject. It defers the
/// subject's next read; it never repeats a read.
#[derive(Default)]
pub(super) struct Backoff {
    pub(super) failures: u32,
    pub(super) retry_at: Option<DateTime<Utc>>,
}

impl Backoff {
    pub(super) fn ready(&self, now: DateTime<Utc>) -> bool {
        self.retry_at.is_none_or(|at| now >= at)
    }

    /// Defers the next read exponentially. Returns the error worth
    /// reporting: only a streak's first, so a lasting failure is not logged
    /// on every read.
    pub(super) fn fail(
        &mut self,
        now: DateTime<Utc>,
        error: anyhow::Error,
        subject: &str,
    ) -> Option<anyhow::Error> {
        self.failures += 1;
        let delay = (1_i64 << (self.failures - 1).min(5)).min(MAX_RETRY_DELAY_SECS);
        self.retry_at = Some(now + Duration::seconds(delay));
        (self.failures == 1).then(|| error.context(format!("{subject}: retrying")))
    }
}

pub(super) struct Delivered {
    pub(super) update: Value,
    pub(super) at: DateTime<Utc>,
}

pub(super) enum ObservationError {
    /// Repeating the same read by the same actor fails the same way.
    Permanent(anyhow::Error),
    Transient(anyhow::Error),
}

pub(super) async fn send(
    session: &str,
    update: Value,
    sender: &PromptSender,
    projections: &ProjectionEngine,
) -> Result<()> {
    projections
        .session_updates()
        .send(
            session,
            |event_id, total_tokens| {
                Ok(super::projection::session_notification_for_method(
                    "x.ai/session_notification",
                    session,
                    update,
                    super::projection::stamp_update_meta(
                        event_id,
                        total_tokens,
                        None,
                        None,
                        UpdateTimestamps::default(),
                    ),
                ))
            },
            PromptSenderLine(sender),
        )
        .await
        .map(drop)
}

/// The pager ticks a running run's clock itself; elapsed time and the
/// delivery counter alone never justify another update.
pub(super) fn without_clock(update: &Value) -> Value {
    let mut shape = update.clone();
    if let Some(object) = shape.as_object_mut() {
        object.remove("revision");
        object.remove("elapsed_ms");
    }
    shape
}
