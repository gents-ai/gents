//! Visibility for event deliveries deferred on a missing correlation value.
//!
//! An event trigger with a `correlation_field` defers a source document until
//! that field is populated, and the periodic rescan re-evaluates it. A
//! producer that never fills the field leaves the delivery deferred forever,
//! which at `info` is indistinguishable from an idle pipeline (#1592). This
//! watch remembers when each `(document, trigger)` delivery first deferred and
//! warns once when it has waited longer than [`DEFERRAL_WARN_AFTER`]. An entry
//! is dropped as soon as its delivery settles, so the watch holds only
//! deliveries that are deferred right now.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a delivery may wait on its correlation value before it is
/// reported as stuck.
pub(crate) const DEFERRAL_WARN_AFTER: Duration = Duration::from_secs(300);

struct Deferral {
    since: Instant,
    warned: bool,
}

/// Deferred deliveries, keyed by source collection and document, then trigger.
pub(crate) struct DeferralWatch {
    warn_after: Duration,
    deferred: HashMap<(String, String), HashMap<String, Deferral>>,
}

impl Default for DeferralWatch {
    fn default() -> Self {
        Self::new(DEFERRAL_WARN_AFTER)
    }
}

impl DeferralWatch {
    pub(crate) fn new(warn_after: Duration) -> Self {
        Self {
            warn_after,
            deferred: HashMap::new(),
        }
    }

    /// Records that `trigger_id` deferred `collection`/`doc_id` because
    /// `correlation_field` is empty, warning once when that delivery has
    /// waited past the threshold.
    pub(crate) fn deferred(
        &mut self,
        collection: &str,
        doc_id: &str,
        trigger_id: &str,
        correlation_field: &str,
    ) {
        let deferral = self
            .deferred
            .entry((collection.to_owned(), doc_id.to_owned()))
            .or_default()
            .entry(trigger_id.to_owned())
            .or_insert_with(|| Deferral {
                since: Instant::now(),
                warned: false,
            });
        let waited = deferral.since.elapsed();
        if !deferral.warned && waited >= self.warn_after {
            deferral.warned = true;
            tracing::warn!(
                trigger_id,
                source_collection = collection,
                source_doc_id = doc_id,
                correlation_field,
                waited_secs = waited.as_secs(),
                "event delivery is still waiting for its correlation field; the trigger will not fire until the source document sets it",
            );
        }
    }

    /// The delivery of `collection`/`doc_id` to `trigger_id` settled.
    pub(crate) fn settled(&mut self, collection: &str, doc_id: &str, trigger_id: &str) {
        let key = (collection.to_owned(), doc_id.to_owned());
        if let Some(triggers) = self.deferred.get_mut(&key) {
            triggers.remove(trigger_id);
            if triggers.is_empty() {
                self.deferred.remove(&key);
            }
        }
    }

    /// Every delivery of `collection`/`doc_id` settled.
    pub(crate) fn settled_document(&mut self, collection: &str, doc_id: &str) {
        self.deferred
            .remove(&(collection.to_owned(), doc_id.to_owned()));
    }

    /// Deferred deliveries that have already been reported as stuck.
    #[cfg(test)]
    pub(crate) fn stuck(&self) -> usize {
        self.deferred
            .values()
            .flat_map(HashMap::values)
            .filter(|deferral| deferral.warned)
            .count()
    }

    /// Deliveries deferred right now.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> usize {
        self.deferred.values().map(HashMap::len).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deferral_is_reported_once_after_the_threshold_and_cleared_when_it_settles() {
        let mut watch = DeferralWatch::new(Duration::ZERO);
        watch.deferred("Job", "doc-1", "trigger-a", "run_id");
        watch.deferred("Job", "doc-1", "trigger-a", "run_id");
        watch.deferred("Job", "doc-1", "trigger-b", "run_id");
        assert_eq!((watch.pending(), watch.stuck()), (2, 2));

        watch.settled("Job", "doc-1", "trigger-a");
        assert_eq!((watch.pending(), watch.stuck()), (1, 1));
        watch.settled_document("Job", "doc-1");
        assert_eq!((watch.pending(), watch.stuck()), (0, 0));
        assert!(watch.deferred.is_empty());
    }

    #[test]
    fn a_deferral_younger_than_the_threshold_is_not_stuck() {
        let mut watch = DeferralWatch::new(Duration::from_secs(3600));
        watch.deferred("Job", "doc-1", "trigger-a", "run_id");
        assert_eq!((watch.pending(), watch.stuck()), (1, 0));
    }
}
