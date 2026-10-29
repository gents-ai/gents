use std::collections::BTreeMap;
use std::future::Future;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

use super::projection::{ProjectionEngine, UpdateTimestamps};
use super::turn::sender::{PromptSender, PromptSenderLine};

/// Run views loaded per observation tick. Each load is several queries on
/// the session observer's shared loop, so a session that resumes with many
/// runs is covered round-robin instead of delaying its token stream.
pub(super) const LOADS_PER_TICK: usize = 4;

/// The revision of the terminal frame sent when observation stops. The pager
/// keeps the highest revision it holds across leader reconnects (see
/// [`ObservedView::durable_revision`]), so this frame must outrank anything an earlier
/// observer delivered. Stopping is final for this actor, so no frame follows
/// it. 2^53 - 1 is the largest integer a JSON number keeps exactly as an
/// IEEE double.
pub(super) const ABANDONED_REVISION: u64 = (1 << 53) - 1;

/// Upper bound on the delay before re-reading a subject whose last read
/// failed.
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

/// One subject the pager is shown: its last delivered revision and update,
/// whether observation ended, and the failure streak of its reads.
pub(super) struct Observed {
    pub(super) name: String,
    pub(super) revision: u64,
    pub(super) delivered: Option<Delivered>,
    pub(super) finished: bool,
    pub(super) backoff: Backoff,
}

impl Observed {
    pub(super) fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            revision: 0,
            delivered: None,
            finished: false,
            backoff: Backoff::default(),
        }
    }
}

#[derive(Default)]
pub(super) struct ObservationSet {
    pub(super) subjects: BTreeMap<String, Observed>,
    /// The last subject loaded; the next tick's loads continue after it.
    pub(super) loaded_through: Option<String>,
}

impl ObservationSet {
    /// Keeps the entry of a subject that is already observed.
    pub(super) fn insert(&mut self, id: String, observed: Observed) {
        self.subjects.entry(id).or_insert(observed);
    }

    /// Up to [`LOADS_PER_TICK`] subjects that are neither finished nor
    /// waiting out a failure, continuing after the last subject loaded.
    pub(super) fn due(&mut self, now: DateTime<Utc>) -> Vec<String> {
        let after = self.loaded_through.as_deref();
        let (later, earlier): (Vec<&String>, Vec<&String>) = self
            .subjects
            .iter()
            .filter(|(_, subject)| !subject.finished && subject.backoff.ready(now))
            .map(|(id, _)| id)
            .partition(|id| after.is_some_and(|after| id.as_str() > after));
        let due = later
            .into_iter()
            .chain(earlier)
            .take(LOADS_PER_TICK)
            .cloned()
            .collect::<Vec<_>>();
        if let Some(last) = due.last() {
            self.loaded_through = Some(last.clone());
        }
        due
    }
}

/// What the observer reads of one loaded subject. `project` is called only
/// after `is_terminal` returned `Ok`.
pub(super) trait ObservedView {
    /// `Err` is permanent: reading the same view again fails the same way.
    fn is_terminal(&self) -> Result<bool>;
    /// The stock pager keeps the highest revision it holds for a subject,
    /// across leader reconnects, and drops a lower one (probed against
    /// v1.0.46). So this is a count of durable facts that only grow, and a
    /// fresh observer never delivers a revision below what an earlier one
    /// delivered.
    fn durable_revision(&self) -> u64;
    /// The `workflow_updated` body at `revision`.
    fn project(&self, revision: u64, now: DateTime<Utc>) -> Value;
}

/// Where one tick's updates go.
pub(super) struct Sink<'a> {
    pub(super) session: &'a str,
    pub(super) sender: &'a PromptSender,
    pub(super) projections: &'a ProjectionEngine,
}

/// Loads and delivers every due subject. `classify` returns true for a load
/// error that is permanent. `unobserved` builds the final update of a
/// subject that never delivered one.
pub(super) async fn observe_due<V, F, Fut>(
    set: &mut ObservationSet,
    sink: &Sink<'_>,
    now: DateTime<Utc>,
    mut load: F,
    classify: fn(&anyhow::Error) -> bool,
    unobserved: fn(&str, &Observed) -> Value,
    subject_label: &str,
) -> Vec<anyhow::Error>
where
    V: ObservedView,
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<V>>,
{
    let Sink {
        session,
        sender,
        projections,
    } = *sink;
    let mut reports = Vec::new();
    for id in set.due(now) {
        let Some(observed) = set.subjects.get_mut(&id) else {
            continue;
        };
        let outcome = async {
            let view = load(id.clone()).await.map_err(|error| {
                if classify(&error) {
                    ObservationError::Permanent(error)
                } else {
                    ObservationError::Transient(error)
                }
            })?;
            let terminal = view.is_terminal().map_err(ObservationError::Permanent)?;
            let revision = view.durable_revision().max(observed.revision + 1);
            let update = view.project(revision, now);
            let shape = without_clock(&update);
            if observed
                .delivered
                .as_ref()
                .is_none_or(|delivered| without_clock(&delivered.update) != shape)
            {
                send(session, update.clone(), sender, projections)
                    .await
                    .map_err(ObservationError::Transient)?;
                observed.revision = revision;
                observed.delivered = Some(Delivered { update, at: now });
            }
            observed.finished = terminal;
            Ok(())
        }
        .await;
        let subject = format!("{subject_label} {id}");
        match outcome {
            Ok(()) => observed.backoff = Backoff::default(),
            Err(ObservationError::Transient(error)) => {
                reports.extend(observed.backoff.fail(now, error, &subject));
            }
            Err(ObservationError::Permanent(error)) => {
                observed.finished = true;
                observed.revision = ABANDONED_REVISION;
                let update = abandoned(observed, &error, now, subject_label, || {
                    unobserved(&id, observed)
                });
                reports.push(error.context(format!("{subject}: observation stopped")));
                if let Err(error) = send(session, update, sender, projections).await {
                    reports.push(error.context(format!("{subject}: final update")));
                }
            }
        }
    }
    reports
}

/// The one terminal update for a subject this connection stops observing, so
/// the pager does not keep ticking something nothing reports on. It
/// continues the last delivered update when there is one.
pub(super) fn abandoned(
    observed: &Observed,
    error: &anyhow::Error,
    now: DateTime<Utc>,
    subject_label: &str,
    fallback: impl FnOnce() -> Value,
) -> Value {
    let mut update = match &observed.delivered {
        Some(delivered) => {
            let mut update = delivered.update.clone();
            let shown = update["elapsed_ms"].as_u64().unwrap_or_default();
            let since = (now - delivered.at).num_milliseconds().max(0) as u64;
            update["elapsed_ms"] = json!(shown + since);
            update
        }
        None => fallback(),
    };
    update["revision"] = json!(observed.revision);
    update["status"] = json!("failed");
    update["active_agents"] = json!(0);
    update["pause_message"] = json!(format!(
        "Gents stopped observing this {subject_label}: {error}"
    ));
    update
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(second: i64) -> DateTime<Utc> {
        "2026-10-02T05:30:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::seconds(second)
    }

    #[test]
    fn backoff_doubles_to_the_cap() {
        let mut backoff = Backoff::default();
        let now = at(0);
        let mut delays = Vec::new();
        let mut reported = Vec::new();
        for _ in 0..8 {
            reported.push(backoff.fail(now, anyhow::anyhow!("down"), "run").is_some());
            delays.push((backoff.retry_at.unwrap() - now).num_seconds());
        }
        assert_eq!(delays, [1, 2, 4, 8, 16, MAX_RETRY_DELAY_SECS, 30, 30]);
        assert_eq!(
            reported,
            [true, false, false, false, false, false, false, false]
        );
        assert!(!backoff.ready(now));
        assert!(backoff.ready(now + Duration::seconds(MAX_RETRY_DELAY_SECS)));
    }

    fn set_of(ids: &[&str]) -> ObservationSet {
        let mut set = ObservationSet::default();
        for id in ids {
            set.insert((*id).to_owned(), Observed::new("job"));
        }
        set
    }

    #[test]
    fn due_round_robins_under_the_cap() {
        let ids = ["a", "b", "c", "d", "e", "f"];
        let mut set = set_of(&ids);
        assert_eq!(LOADS_PER_TICK, 4);
        assert_eq!(set.due(at(0)), ["a", "b", "c", "d"]);
        assert_eq!(set.due(at(0)), ["e", "f", "a", "b"]);
        assert_eq!(set.due(at(0)), ["c", "d", "e", "f"]);
    }

    #[test]
    fn due_skips_finished_and_backing_off_subjects() {
        let mut set = set_of(&["a", "b", "c"]);
        set.subjects.get_mut("a").unwrap().finished = true;
        set.subjects
            .get_mut("b")
            .unwrap()
            .backoff
            .fail(at(0), anyhow::anyhow!("down"), "b");
        assert_eq!(set.due(at(0)), ["c"]);
        assert_eq!(set.due(at(1)), ["b", "c"]);
    }
}
