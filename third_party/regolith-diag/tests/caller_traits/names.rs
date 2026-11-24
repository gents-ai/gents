//! The `name` of a merge operator, a compaction filter and a prefix
//! extractor: each is asked for what the options print about themselves.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use regolith::{CompactionDecision, CompactionFilter, Db, MergeOperator, Options, PrefixExtractor};

/// A hook of every kind that has a name, counting how often it is asked.
struct Named(&'static str, AtomicUsize);

impl Named {
    fn new(name: &'static str) -> Arc<Self> {
        Arc::new(Self(name, AtomicUsize::new(0)))
    }

    fn asked(&self) -> usize {
        self.1.load(Ordering::Relaxed)
    }

    fn say(&self) -> &'static str {
        self.1.fetch_add(1, Ordering::Relaxed);
        self.0
    }
}

impl MergeOperator for Named {
    fn full_merge(&self, _: &[u8], _: Option<&[u8]>, _: &[&[u8]]) -> Option<Vec<u8>> {
        None
    }

    fn name(&self) -> &'static str {
        self.say()
    }
}

impl CompactionFilter for Named {
    fn filter(&self, _: usize, _: &[u8], _: &[u8]) -> CompactionDecision {
        CompactionDecision::Keep
    }

    fn name(&self) -> &'static str {
        self.say()
    }
}

impl PrefixExtractor for Named {
    fn extract<'a>(&self, key: &'a [u8]) -> Option<&'a [u8]> {
        Some(key)
    }

    fn name(&self) -> &'static str {
        self.say()
    }
}

#[test]
fn the_name_of_each_hook_is_what_the_options_print_for_it() {
    let (merge, filter, extractor) = (
        Named::new("a-merge-operator"),
        Named::new("a-compaction-filter"),
        Named::new("a-prefix-extractor"),
    );
    let options = Options::memory()
        .merge_operator(Some(merge.clone()))
        .compaction_filter(Some(filter.clone()))
        .prefix_extractor(Some(extractor.clone()));

    let printed = format!("{options:?}");
    for hook in [&merge, &filter, &extractor] {
        assert!(printed.contains(hook.0), "{} is not in {printed}", hook.0);
        assert_eq!(hook.asked(), 1, "{} asked for its name", hook.0);
    }

    // A merge the operator declines is refused, and the operator is asked
    // its name for the refusal.
    let db = Db::open("names", options).unwrap();
    db.merge(b"k", b"operand").unwrap();
    assert!(db.get(b"k").is_err());
    assert_eq!(merge.asked(), 2);
}
