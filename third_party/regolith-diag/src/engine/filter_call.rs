//! Every call the engine makes of the caller's [`CompactionFilter`].
//!
//! The filter is the caller's code, and it judges the caller's entries. So
//! each call here hands it the key, or the bounds of a range delete, as the
//! caller wrote them, and it is never shown what is the database's own: an
//! entry of the column-family registry, or the range delete that drops a
//! column family. A filter that removes what it is shown can therefore
//! remove the caller's data and nothing else.
//!
//! Compaction reaches the filter through this module and nowhere else.

use crate::column_family::{META_CF_ID, callers_key, family_of};
use crate::options::{CompactionDecision, CompactionFilter};

/// Whether an engine key belongs to the database itself and not to a caller:
/// an entry of the registry the reserved family holds.
fn is_the_databases_own(prefixed: &[u8]) -> bool {
    family_of(prefixed) == Some(META_CF_ID)
}

/// [`CompactionFilter::filter`] for the value of the entry the engine keys
/// `prefixed`. An entry of the database's own is kept without asking.
pub(crate) fn filter_value(
    filter: &dyn CompactionFilter,
    level: usize,
    prefixed: &[u8],
    value: &[u8],
) -> CompactionDecision {
    if is_the_databases_own(prefixed) {
        return CompactionDecision::Keep;
    }
    filter.filter(level, callers_key(prefixed), value)
}

/// [`CompactionFilter::filter_range_delete`] for the range delete the engine
/// keeps as `[start, end)`.
///
/// A range delete the caller made lies inside one column family, so both
/// bounds carry that family's id. One whose bounds do not is the database's
/// own (dropping a family deletes from its first possible key to the next
/// family's), and so is one inside the reserved family: both are kept
/// without asking.
pub(crate) fn filter_range_delete(
    filter: &dyn CompactionFilter,
    level: usize,
    start: &[u8],
    end: &[u8],
) -> CompactionDecision {
    let family = family_of(start);
    if family.is_none() || family != family_of(end) || family == Some(META_CF_ID) {
        return CompactionDecision::Keep;
    }
    filter.filter_range_delete(level, callers_key(start), callers_key(end))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::column_family::{cf_lower_bound, cf_upper_bound, prefix_key};

    /// Removes everything it is shown, and records it.
    #[derive(Default)]
    struct Shown {
        keys: Mutex<Vec<Vec<u8>>>,
        ranges: Mutex<Vec<(Vec<u8>, Vec<u8>)>>,
    }

    impl CompactionFilter for Shown {
        fn name(&self) -> &'static str {
            "shown"
        }

        fn filter(&self, _: usize, key: &[u8], _: &[u8]) -> CompactionDecision {
            self.keys.lock().unwrap().push(key.to_vec());
            CompactionDecision::Remove
        }

        fn filter_range_delete(&self, _: usize, start: &[u8], end: &[u8]) -> CompactionDecision {
            self.ranges
                .lock()
                .unwrap()
                .push((start.to_vec(), end.to_vec()));
            CompactionDecision::Remove
        }
    }

    #[test]
    fn a_callers_entry_is_shown_by_the_key_the_caller_wrote() {
        let filter = Shown::default();
        for family in [1, 2, u32::MAX - 1] {
            let decision = filter_value(&filter, 0, &prefix_key(family, b"doc/7"), b"v");
            assert_eq!(decision, CompactionDecision::Remove);
        }
        assert_eq!(*filter.keys.lock().unwrap(), vec![b"doc/7".to_vec(); 3]);
    }

    #[test]
    fn an_entry_of_the_registry_is_kept_without_asking() {
        let filter = Shown::default();
        let decision = filter_value(&filter, 0, &prefix_key(META_CF_ID, b"next_id"), b"v");
        assert_eq!(decision, CompactionDecision::Keep);
        assert!(filter.keys.lock().unwrap().is_empty());
    }

    #[test]
    fn a_callers_range_delete_is_shown_by_the_bounds_the_caller_wrote() {
        let filter = Shown::default();
        let decision = filter_range_delete(&filter, 0, &prefix_key(7, b"b"), &prefix_key(7, b"e"));
        assert_eq!(decision, CompactionDecision::Remove);
        assert_eq!(
            *filter.ranges.lock().unwrap(),
            [(b"b".to_vec(), b"e".to_vec())]
        );
    }

    #[test]
    fn a_range_delete_of_the_databases_own_is_kept_without_asking() {
        let filter = Shown::default();
        let dropped_family = (cf_lower_bound(7), cf_upper_bound(7));
        let in_the_registry = (prefix_key(META_CF_ID, b"a"), prefix_key(META_CF_ID, b"z"));
        let no_family = (b"a".to_vec(), b"z".to_vec());
        for (start, end) in [dropped_family, in_the_registry, no_family] {
            assert_eq!(
                filter_range_delete(&filter, 0, &start, &end),
                CompactionDecision::Keep
            );
        }
        assert!(filter.ranges.lock().unwrap().is_empty());
    }
}
