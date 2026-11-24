//! What a `PrefixExtractor` is handed: the key, and the prefix of a seek, as
//! the caller wrote them, and only the caller's own entries.
//!
//! The first test records every key the extractor is shown, at a flush, at a
//! compaction and at a prefix seek, in the default column family and a named
//! one. The others count the tables a prefix seek ruled out by its prefix
//! filter: `FixedLengthPrefix(n)` must rule tables out on `n` bytes of the
//! caller's key, and one prefix in two column families must not make the
//! filter answer for both.

// Native-only. wasm-pack builds every test target for wasm32, and the browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::{Arc, Mutex};

use regolith::{
    ColumnFamilyHandle, CompactionOutcome, Db, FixedLengthPrefix, Options, PrefixExtractor,
    Statistics, Ticker,
};

/// The first four bytes of a key, recording every key and every seek prefix
/// it is handed.
#[derive(Default)]
struct Recording {
    keys: Mutex<Vec<Vec<u8>>>,
    seeks: Mutex<Vec<Vec<u8>>>,
}

impl Recording {
    /// The distinct keys handed to `extract` since the last time this was
    /// asked, sorted.
    fn keys(&self) -> Vec<Vec<u8>> {
        let mut keys = std::mem::take(&mut *self.keys.lock().unwrap());
        keys.sort();
        keys.dedup();
        keys
    }

    /// The prefixes handed to `extract_query` since the last time this was
    /// asked, in order.
    fn seeks(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut *self.seeks.lock().unwrap())
    }
}

impl PrefixExtractor for Recording {
    fn extract<'a>(&self, key: &'a [u8]) -> Option<&'a [u8]> {
        self.keys.lock().unwrap().push(key.to_vec());
        FixedLengthPrefix(4).extract(key)
    }

    fn extract_query<'a>(&self, prefix: &'a [u8]) -> Option<&'a [u8]> {
        self.seeks.lock().unwrap().push(prefix.to_vec());
        FixedLengthPrefix(4).extract_query(prefix)
    }

    fn name(&self) -> &'static str {
        "recording"
    }
}

/// Options for an in-memory database with `extractor`, and the statistics it
/// reports to: no compaction worker, and a compaction due once level 0 holds
/// `trigger` tables.
fn options(extractor: Arc<dyn PrefixExtractor>, trigger: usize) -> (Options, Arc<Statistics>) {
    let stats = Arc::new(Statistics::new());
    let options = Options::memory()
        .l0_compaction_trigger(trigger)
        .prefix_extractor(Some(extractor))
        .statistics(Some(Arc::clone(&stats)));
    (options, stats)
}

/// A database on [`options`].
fn open(extractor: Arc<dyn PrefixExtractor>, trigger: usize) -> (Db, Arc<Statistics>) {
    let (options, stats) = options(extractor, trigger);
    (Db::open("prefix-extractor-key", options).unwrap(), stats)
}

/// The keys a prefix seek in `family` yields, in order.
fn seek(db: &Db, family: Option<&ColumnFamilyHandle>, prefix: &[u8]) -> Vec<Vec<u8>> {
    let mut iter = match family {
        None => db.iter(),
        Some(cf) => db.iter_cf(cf),
    };
    iter.seek_prefix(prefix);
    let mut keys = Vec::new();
    while iter.valid() {
        keys.push(iter.key().unwrap().to_vec());
        iter.next();
    }
    iter.status().unwrap();
    keys
}

/// What a prefix seek in `family` yields, and how many tables its prefix
/// filter ruled out.
fn seek_counting(
    db: &Db,
    stats: &Statistics,
    family: Option<&ColumnFamilyHandle>,
    prefix: &[u8],
) -> (Vec<Vec<u8>>, u64) {
    let before = stats.get_ticker(Ticker::BloomFilterUseful);
    let keys = seek(db, family, prefix);
    (keys, stats.get_ticker(Ticker::BloomFilterUseful) - before)
}

fn keys(names: &[&[u8]]) -> Vec<Vec<u8>> {
    names.iter().map(|name| name.to_vec()).collect()
}

/// At a flush, at a compaction and at a seek the extractor is shown the keys
/// and the prefix the caller wrote, in both families, and never an entry the
/// database keeps for itself.
#[test]
fn the_extractor_is_handed_the_callers_keys() {
    let extractor = Arc::new(Recording::default());
    let (db, _stats) = open(extractor.clone(), 2);
    let named = db.create_column_family("named").unwrap();

    db.put(b"aaaa:1", b"v").unwrap();
    db.put(b"bbbb:1", b"v").unwrap();
    db.put_cf(&named, b"cccc:1", b"v").unwrap();
    extractor.keys();
    db.flush().unwrap();
    assert_eq!(
        extractor.keys(),
        keys(&[b"aaaa:1", b"bbbb:1", b"cccc:1"]),
        "a flush shows the keys as the caller wrote them, and no other entry"
    );

    // A seek while the table is in level 0, where its prefix filter is asked.
    assert_eq!(seek(&db, None, b"aaaa"), keys(&[b"aaaa:1"]));
    assert_eq!(seek(&db, Some(&named), b"cccc:"), keys(&[b"cccc:1"]));
    assert_eq!(extractor.seeks(), keys(&[b"aaaa", b"cccc:"]));

    db.put(b"aaaa:2", b"v").unwrap();
    db.put_cf(&named, b"dddd:1", b"v").unwrap();
    db.flush().unwrap();
    extractor.keys();
    assert_eq!(db.compact_step().unwrap(), CompactionOutcome::DidWork);
    assert_eq!(
        extractor.keys(),
        keys(&[b"aaaa:1", b"aaaa:2", b"bbbb:1", b"cccc:1", b"dddd:1"]),
        "a compaction shows the keys of the table it writes the same way"
    );

    assert_eq!(seek(&db, None, b"aaaa"), keys(&[b"aaaa:1", b"aaaa:2"]));
    assert_eq!(seek(&db, Some(&named), b"dddd"), keys(&[b"dddd:1"]));
    assert_eq!(extractor.seeks(), keys(&[b"aaaa", b"dddd"]));
}

/// `FixedLengthPrefix(n)` rules a table out on `n` bytes of the caller's key:
/// a seek for a prefix no key of a table starts with does not read the table.
#[test]
fn a_fixed_length_prefix_counts_bytes_of_the_callers_key() {
    let (db, stats) = open(Arc::new(FixedLengthPrefix(4)), 1_000);
    db.put(b"aaaa:1", b"v").unwrap();
    db.put(b"aaaa:2", b"v").unwrap();
    db.flush().unwrap();
    db.put(b"bbbb:1", b"v").unwrap();
    db.flush().unwrap();
    let seek = |prefix: &[u8]| seek_counting(&db, &stats, None, prefix);

    assert_eq!(seek(b"cccc"), (keys(&[]), 2), "neither table holds it");
    assert_eq!(seek(b"aaaa"), (keys(&[b"aaaa:1", b"aaaa:2"]), 1));
    assert_eq!(seek(b"bbbb"), (keys(&[b"bbbb:1"]), 1));
    // The fourth byte decides too.
    assert_eq!(seek(b"aaab"), (keys(&[]), 2));
    // A longer prefix is filtered by its first four bytes.
    assert_eq!(seek(b"aaaa:2"), (keys(&[b"aaaa:2"]), 1));
    // A prefix shorter than the extractor's width gives no filter key, so
    // no table is ruled out, and the seek still finds what is there.
    assert_eq!(seek(b"aa"), (keys(&[b"aaaa:1", b"aaaa:2"]), 0));
}

/// The filter answers for one column family at a time: a prefix one family
/// holds does not make a table look as if the other family held it.
#[test]
fn two_families_do_not_share_a_prefix() {
    let (db, stats) = open(Arc::new(FixedLengthPrefix(4)), 1_000);
    let named = db.create_column_family("named").unwrap();
    db.put(b"aaaa:1", b"v").unwrap();
    db.put_cf(&named, b"bbbb:1", b"v").unwrap();
    db.flush().unwrap();

    assert_eq!(
        seek_counting(&db, &stats, Some(&named), b"aaaa"),
        (keys(&[]), 1),
        "the table holds the prefix in the default family only"
    );
    assert_eq!(
        seek_counting(&db, &stats, None, b"bbbb"),
        (keys(&[]), 1),
        "and this one in the named family only"
    );
    assert_eq!(
        seek_counting(&db, &stats, None, b"aaaa"),
        (keys(&[b"aaaa:1"]), 0)
    );
    assert_eq!(
        seek_counting(&db, &stats, Some(&named), b"bbbb"),
        (keys(&[b"bbbb:1"]), 0)
    );
}

/// A table that compaction moved below level 0 is ruled out by its prefix
/// filter as one in level 0 is, and a seek past the last table of a level
/// reads none of them.
#[test]
fn a_table_below_level_zero_is_ruled_out_by_its_prefix_filter() {
    let (db, stats) = open(Arc::new(FixedLengthPrefix(4)), 2);
    db.put(b"aaaa:1", b"v").unwrap();
    db.put(b"cccc:1", b"v").unwrap();
    db.flush().unwrap();
    db.put(b"bbbb:1", b"v").unwrap();
    db.flush().unwrap();
    assert_eq!(db.compact_step().unwrap(), CompactionOutcome::DidWork);
    assert_eq!(db.get_int_property("regolith.num-files-at-level0"), Some(0));
    let seek = |prefix: &[u8]| seek_counting(&db, &stats, None, prefix);

    assert_eq!(
        seek(b"abab"),
        (keys(&[]), 1),
        "between the table's keys, and not one of its prefixes"
    );
    assert_eq!(seek(b"bbbb"), (keys(&[b"bbbb:1"]), 0));
    assert_eq!(seek(b"cccc"), (keys(&[b"cccc:1"]), 0));
    // Past the table's last key there is no table to ask.
    assert_eq!(seek(b"dddd"), (keys(&[]), 0));
}

/// In a level of many tables a prefix seek asks only the tables its prefix
/// can be in: one that starts past the prefix ends the search, so the tables
/// after it are neither asked nor read.
#[test]
fn a_prefix_seek_asks_only_the_tables_its_prefix_can_be_in() {
    let (options, stats) = options(Arc::new(FixedLengthPrefix(4)), 2);
    // A table for each key, once compaction has written them.
    let db = Db::open("prefix-extractor-key", options.target_file_size(1)).unwrap();
    for key in [&b"aaaa:1"[..], b"cccc:1", b"eeee:1"] {
        db.put(key, b"v").unwrap();
    }
    db.flush().unwrap();
    db.put(b"cccc:2", b"v").unwrap();
    db.flush().unwrap();
    assert_eq!(db.compact_step().unwrap(), CompactionOutcome::DidWork);
    assert_eq!(db.get_int_property("regolith.num-files-at-level0"), Some(0));
    assert_eq!(db.get_int_property("regolith.num-files-at-level1"), Some(4));
    let seek = |prefix: &[u8]| seek_counting(&db, &stats, None, prefix);

    // The first table at or after the prefix starts past it: none is asked.
    assert_eq!(seek(b"bbbb"), (keys(&[]), 0));
    assert_eq!(seek(b"dddd"), (keys(&[]), 0));
    // Two tables hold the prefix, and the one after them is not ruled out
    // by a filter: the seek's bound ends the scan before it.
    assert_eq!(seek(b"cccc"), (keys(&[b"cccc:1", b"cccc:2"]), 0));
    assert_eq!(seek(b"eeee"), (keys(&[b"eeee:1"]), 0));
}
