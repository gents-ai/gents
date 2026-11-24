//! What a `CompactionFilter` is handed: the key and the range bounds as the
//! caller wrote them, only the caller's own entries, once for each pass that
//! rewrites an entry.
//!
//! The filter here decides by the key, as a caller's filter does: it removes
//! the values under one prefix and rewrites those under another. Handed
//! anything but the caller's key it would keep everything. It also records
//! every key and every range it is shown, so each test checks both what the
//! filter did and what it saw.

// Native-only. wasm-pack builds every test target for wasm32, and the browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::{Arc, Mutex};

use regolith::{
    ColumnFamilyHandle, CompactionDecision, CompactionFilter, CompactionOutcome, Db, Env, Options,
    Statistics, Ticker,
};

/// Decides by the first byte of the key: a value under `x` is removed, a
/// value under `u` is upper-cased, and every other value is kept. With
/// `everything` set it removes every value and every range delete it is
/// shown instead.
#[derive(Default)]
struct ByKey {
    everything: bool,
    keys: Mutex<Vec<Vec<u8>>>,
    ranges: Mutex<Vec<(Vec<u8>, Vec<u8>)>>,
}

impl ByKey {
    /// The distinct keys shown since the last time this was asked, sorted.
    fn keys(&self) -> Vec<Vec<u8>> {
        let mut keys = std::mem::take(&mut *self.keys.lock().unwrap());
        keys.sort();
        keys.dedup();
        keys
    }

    /// The distinct ranges shown since the last time this was asked, sorted.
    fn ranges(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut ranges = std::mem::take(&mut *self.ranges.lock().unwrap());
        ranges.sort();
        ranges.dedup();
        ranges
    }
}

impl CompactionFilter for ByKey {
    fn name(&self) -> &'static str {
        "by-key"
    }

    fn filter(&self, _level: usize, key: &[u8], value: &[u8]) -> CompactionDecision {
        self.keys.lock().unwrap().push(key.to_vec());
        if self.everything {
            return CompactionDecision::Remove;
        }
        match key.first() {
            Some(b'x') => CompactionDecision::Remove,
            Some(b'u') => CompactionDecision::Change(value.to_ascii_uppercase()),
            _ => CompactionDecision::Keep,
        }
    }

    fn filter_range_delete(&self, _level: usize, start: &[u8], end: &[u8]) -> CompactionDecision {
        self.ranges
            .lock()
            .unwrap()
            .push((start.to_vec(), end.to_vec()));
        if self.everything {
            CompactionDecision::Remove
        } else {
            CompactionDecision::Keep
        }
    }
}

/// An in-memory database on `env` with `filter`: no compaction worker, and a
/// compaction due once level 0 holds two tables.
fn open_on(env: &Arc<dyn Env>, filter: &Arc<ByKey>) -> Db {
    let filter: Arc<dyn CompactionFilter> = filter.clone();
    let options = Options::memory()
        .env(Arc::clone(env))
        .l0_compaction_trigger(2)
        .compaction_filter(Some(filter));
    Db::open("compaction-filter-key", options).unwrap()
}

fn memory() -> Arc<dyn Env> {
    Arc::clone(Options::memory().get_env())
}

fn open(filter: &Arc<ByKey>) -> Db {
    open_on(&memory(), filter)
}

/// Run the one compaction pass that two tables in level 0 are due. Nothing
/// else compacts them first: the database has no worker, and the step a flush
/// leaves owing runs only at the next write.
fn compact(db: &Db) {
    assert_eq!(db.compact_step().unwrap(), CompactionOutcome::DidWork);
}

/// The default column family through the plain calls, or a named one through
/// the `_cf` calls.
enum Family {
    Default,
    Named(ColumnFamilyHandle),
}

impl Family {
    fn both(db: &Db) -> [Self; 2] {
        let named = db.create_column_family("named").unwrap();
        [Self::Default, Self::Named(named)]
    }

    fn put(&self, db: &Db, key: &[u8], value: &[u8]) {
        match self {
            Self::Default => db.put(key, value).unwrap(),
            Self::Named(cf) => db.put_cf(cf, key, value).unwrap(),
        }
    }

    fn get(&self, db: &Db, key: &[u8]) -> Option<Vec<u8>> {
        match self {
            Self::Default => db.get(key).unwrap(),
            Self::Named(cf) => db.get_cf(cf, key).unwrap(),
        }
    }

    fn delete_range(&self, db: &Db, start: &[u8], end: &[u8]) {
        match self {
            Self::Default => db.delete_range(start, end).unwrap(),
            Self::Named(cf) => db.delete_range_cf(cf, start, end).unwrap(),
        }
    }
}

/// The filter's decision follows the caller's key in both families, and the
/// keys it is shown are the keys the caller wrote.
#[test]
fn the_filter_is_handed_the_callers_key() {
    let filter = Arc::new(ByKey::default());
    let db = open(&filter);
    for family in Family::both(&db) {
        family.put(&db, b"x/gone", b"v");
        family.put(&db, b"u/shout", b"quiet");
        db.flush().unwrap();
        family.put(&db, b"k/plain", b"as written");
        db.flush().unwrap();
        filter.keys();
        compact(&db);

        assert_eq!(
            filter.keys(),
            [b"k/plain".to_vec(), b"u/shout".to_vec(), b"x/gone".to_vec()],
            "the keys as the caller wrote them, and no other entry"
        );
        assert_eq!(family.get(&db, b"x/gone"), None);
        assert_eq!(family.get(&db, b"u/shout"), Some(b"QUIET".to_vec()));
        assert_eq!(family.get(&db, b"k/plain"), Some(b"as written".to_vec()));
    }
}

/// The database keeps entries of its own beside the caller's: the list of
/// column families. A filter that removes everything it is shown is never
/// shown those, so the families are still there when the database is opened
/// again, and a new family still gets a name of its own.
#[test]
fn the_filter_is_never_shown_the_databases_own_entries() {
    let env = memory();
    let filter = Arc::new(ByKey {
        everything: true,
        ..ByKey::default()
    });
    {
        let db = open_on(&env, &filter);
        let named = db.create_column_family("named").unwrap();
        db.put(b"default-key", b"v").unwrap();
        db.put_cf(&named, b"named-key", b"v").unwrap();
        db.flush().unwrap();
        db.put(b"default-other", b"v").unwrap();
        db.put_cf(&named, b"named-other", b"v").unwrap();
        db.flush().unwrap();
        filter.keys();
        compact(&db);
        assert_eq!(
            filter.keys(),
            [
                b"default-key".to_vec(),
                b"default-other".to_vec(),
                b"named-key".to_vec(),
                b"named-other".to_vec(),
            ]
        );
        assert_eq!(db.get(b"default-key").unwrap(), None);
        assert_eq!(db.get_cf(&named, b"named-key").unwrap(), None);
        db.close().unwrap();
    }

    let db = open_on(&env, &filter);
    let named = db
        .column_family("named")
        .expect("the family outlives a filter that removes every value");
    db.put_cf(&named, b"again", b"v").unwrap();
    let other = db.create_column_family("other").unwrap();
    db.put_cf(&other, b"again", b"w").unwrap();
    assert_eq!(db.get_cf(&named, b"again").unwrap(), Some(b"v".to_vec()));
    assert_eq!(db.get_cf(&other, b"again").unwrap(), Some(b"w".to_vec()));
}

/// A range delete the caller made is shown by the bounds the caller passed.
#[test]
fn a_range_delete_is_shown_by_the_callers_bounds() {
    let filter = Arc::new(ByKey::default());
    let db = open(&filter);
    for family in Family::both(&db) {
        for key in [b"a", b"c", b"f"] {
            family.put(&db, key, b"v");
        }
        db.flush().unwrap();
        family.delete_range(&db, b"b", b"e");
        db.flush().unwrap();
        filter.ranges();
        compact(&db);

        assert_eq!(filter.ranges(), [(b"b".to_vec(), b"e".to_vec())]);
        assert_eq!(family.get(&db, b"a"), Some(b"v".to_vec()));
        assert_eq!(family.get(&db, b"c"), None);
        assert_eq!(family.get(&db, b"f"), Some(b"v".to_vec()));
    }
}

/// Dropping a column family deletes its whole range. That range delete is the
/// database's own, not one the caller made, so a filter that removes every
/// range delete it is shown is not shown it.
#[test]
fn the_range_delete_of_a_dropped_family_is_not_shown() {
    let filter = Arc::new(ByKey {
        everything: true,
        ..ByKey::default()
    });
    let db = open(&filter);
    let doomed = db.create_column_family("doomed").unwrap();
    db.put_cf(&doomed, b"k", b"v").unwrap();
    db.flush().unwrap();
    db.drop_column_family(doomed).unwrap();
    db.flush().unwrap();
    filter.ranges();
    compact(&db);
    assert_eq!(filter.ranges(), []);
    assert!(db.column_family("doomed").is_none());
}

/// The filter is asked about an entry once for each compaction pass that
/// rewrites it, however many that is: a manual compaction that carries a
/// table down several levels asks once a level. It gets the caller's key
/// every time, and since it answers the same every time the value ends as
/// one application of its decision left it.
#[test]
fn the_filter_is_asked_once_for_each_pass_that_rewrites_an_entry() {
    let stats = Arc::new(Statistics::new());
    let filter = Arc::new(ByKey::default());
    let shared: Arc<dyn CompactionFilter> = filter.clone();
    let options = Options::memory()
        .statistics(Some(Arc::clone(&stats)))
        .compaction_filter(Some(shared));
    let db = Db::open("compaction-filter-passes", options).unwrap();
    db.put(b"u/only", b"quiet").unwrap();

    db.compact_range(None, None).wait().unwrap();
    let passes = stats.get_ticker(Ticker::CompactionCount);
    assert!(passes >= 1);
    let shown = std::mem::take(&mut *filter.keys.lock().unwrap());
    assert_eq!(
        shown,
        vec![b"u/only".to_vec(); usize::try_from(passes).unwrap()],
        "once for each pass, by the caller's key"
    );
    assert_eq!(db.get(b"u/only").unwrap(), Some(b"QUIET".to_vec()));

    // An entry no pass rewrites is not asked about: nothing is compacted
    // here, so nothing is shown.
    db.put(b"x/pending", b"v").unwrap();
    db.flush().unwrap();
    assert_eq!(filter.keys(), [] as [Vec<u8>; 0]);
    assert_eq!(db.get(b"x/pending").unwrap(), Some(b"v".to_vec()));
}
