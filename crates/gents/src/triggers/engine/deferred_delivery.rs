use rapidhash::{HashMapExt, RapidHashMap};
use std::time::{Duration, Instant};

pub(crate) const DEFERRAL_WARN_AFTER: Duration = Duration::from_secs(300);

struct Deferral {
    since: Instant,
    warned: bool,
}

pub(crate) struct DeferralWatch {
    warn_after: Duration,
    deferred: RapidHashMap<(String, String), RapidHashMap<String, Deferral>>,
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
            deferred: RapidHashMap::new(),
        }
    }

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

    pub(crate) fn settled(&mut self, collection: &str, doc_id: &str, trigger_id: &str) {
        let key = (collection.to_owned(), doc_id.to_owned());
        if let Some(triggers) = self.deferred.get_mut(&key) {
            triggers.remove(trigger_id);
            if triggers.is_empty() {
                self.deferred.remove(&key);
            }
        }
    }

    pub(crate) fn settled_document(&mut self, collection: &str, doc_id: &str) {
        self.deferred
            .remove(&(collection.to_owned(), doc_id.to_owned()));
    }

    #[cfg(test)]
    pub(crate) fn stuck(&self) -> usize {
        self.deferred
            .values()
            .flat_map(RapidHashMap::values)
            .filter(|deferral| deferral.warned)
            .count()
    }

    #[cfg(test)]
    pub(crate) fn pending(&self) -> usize {
        self.deferred.values().map(RapidHashMap::len).sum()
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
