//! A caller's `CompactionFilter` on a TTL database: the database's own
//! expiry filter runs first, and the caller's is asked about every value
//! that has not expired, by the value as the caller wrote it.

use std::sync::{Arc, Mutex};

use regolith::{CompactionDecision, CompactionFilter, DbWithTtl, MemEnv, Options};

type Pair = (Vec<u8>, Vec<u8>);

/// Drops a value of `drop`, rewrites a value of `grow`, keeps the rest, and
/// records everything it is shown.
#[derive(Default)]
struct ByValue {
    values: Mutex<Vec<Pair>>,
    ranges: Mutex<Vec<Pair>>,
}

impl CompactionFilter for ByValue {
    fn name(&self) -> &'static str {
        "by-value"
    }

    fn filter(&self, _level: usize, key: &[u8], value: &[u8]) -> CompactionDecision {
        self.values
            .lock()
            .unwrap()
            .push((key.to_vec(), value.to_vec()));
        match value {
            b"drop" => CompactionDecision::Remove,
            b"grow" => CompactionDecision::Change(b"grown".to_vec()),
            _ => CompactionDecision::Keep,
        }
    }

    fn filter_range_delete(&self, _level: usize, start: &[u8], end: &[u8]) -> CompactionDecision {
        self.ranges
            .lock()
            .unwrap()
            .push((start.to_vec(), end.to_vec()));
        CompactionDecision::Keep
    }
}

/// Sorted and without repeats: a compaction that passes through several
/// levels asks about an entry once for each.
fn seen(pairs: &Mutex<Vec<Pair>>) -> Vec<Pair> {
    let mut pairs = std::mem::take(&mut *pairs.lock().unwrap());
    pairs.sort();
    pairs.dedup();
    pairs
}

fn pair(key: &[u8], value: &[u8]) -> Pair {
    (key.to_vec(), value.to_vec())
}

#[test]
fn a_ttl_database_asks_the_callers_filter_about_the_callers_value() {
    let env = Arc::new(MemEnv::new());
    let filter = Arc::new(ByValue::default());
    let options = Options::memory()
        .env(env.clone())
        .compaction_filter(Some(filter.clone()));
    let db = DbWithTtl::open("ttl", options, 3_600).unwrap();

    db.put(b"old", b"expires").unwrap();
    // Two hours on, `old` is past the hour it was given.
    env.advance_micros(2 * 3_600 * 1_000_000);
    db.put(b"a-keep", b"kept").unwrap();
    db.put(b"b-drop", b"drop").unwrap();
    db.put(b"c-grow", b"grow").unwrap();
    db.delete_range(b"x", b"z").unwrap();
    db.compact_range(None, None).wait().unwrap();

    // The caller's values, with nothing behind them, and never the one the
    // database expired itself.
    assert_eq!(
        seen(&filter.values),
        [
            pair(b"a-keep", b"kept"),
            pair(b"b-drop", b"drop"),
            pair(b"c-grow", b"grow"),
            // What the filter made of `grow`, asked about again by the
            // passes below the one that changed it.
            pair(b"c-grow", b"grown"),
        ]
    );
    assert_eq!(seen(&filter.ranges), [pair(b"x", b"z")]);

    assert_eq!(db.get(b"old").unwrap(), None);
    assert_eq!(db.get(b"a-keep").unwrap(), Some(b"kept".to_vec()));
    assert_eq!(db.get(b"b-drop").unwrap(), None);
    // The changed value is still one of this database's: it reads back,
    // and it kept the time it was written at.
    assert_eq!(db.get(b"c-grow").unwrap(), Some(b"grown".to_vec()));
    env.advance_micros(3_000 * 1_000_000);
    assert_eq!(db.get(b"c-grow").unwrap(), Some(b"grown".to_vec()));
    env.advance_micros(700 * 1_000_000);
    assert_eq!(
        db.get(b"c-grow").unwrap(),
        None,
        "an hour after it was written"
    );
}
