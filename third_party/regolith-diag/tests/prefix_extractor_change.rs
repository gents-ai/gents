//! A database reopened with another prefix extractor, or with none, still
//! finds every row a prefix seek asks for: a table's prefix filter holds
//! the prefixes the extractor that built it carved out, so it is asked only
//! under that extractor's name, and a table another one built is read
//! whole. A table the same extractor built still rules out what it lacks.

// Native-only. wasm-pack builds every test target for wasm32, and the browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use regolith::{Db, FixedLengthPrefix, MemEnv, Options, PrefixExtractor, Statistics, Ticker};

/// A database on `env` with a `FixedLengthPrefix` of `width`, or with no
/// extractor, nothing compacted until a test asks, and the statistics it
/// reports to.
fn open(env: &Arc<MemEnv>, width: Option<usize>) -> (Db, Arc<Statistics>) {
    let stats = Arc::new(Statistics::new());
    let extractor =
        width.map(|width| Arc::new(FixedLengthPrefix(width)) as Arc<dyn PrefixExtractor>);
    let options = Options::memory()
        .env(env.clone())
        .prefix_extractor(extractor)
        .statistics(Some(stats.clone()))
        .l0_compaction_trigger(1_000)
        .inline_compaction(false);
    (Db::open("db", options).unwrap(), stats)
}

/// Three tables: `aabb:` keys, `aacc:` keys, and `bbxx:` keys.
fn write_three_tables(db: &Db) {
    for keys in [&[&b"aabb:1"[..], b"aabb:2"][..], &[b"aacc:1"], &[b"bbxx:1"]] {
        for key in keys {
            db.put(key, b"v").unwrap();
        }
        db.flush().unwrap();
    }
}

/// The keys a prefix seek yields, and how many tables its prefix filter
/// ruled out. The seek must yield what a full scan has under the prefix.
fn seek(db: &Db, stats: &Statistics, prefix: &[u8]) -> (Vec<Vec<u8>>, u64) {
    let before = stats.get_ticker(Ticker::BloomFilterUseful);
    let mut iter = db.iter();
    iter.seek_prefix(prefix);
    let mut found = Vec::new();
    while iter.valid() {
        found.push(iter.key().unwrap().to_vec());
        iter.next();
    }
    iter.status().unwrap();
    let ruled_out = stats.get_ticker(Ticker::BloomFilterUseful) - before;

    let mut scan = db.iter();
    scan.seek_to_first();
    let mut expected = Vec::new();
    while scan.valid() {
        let key = scan.key().unwrap();
        if key.starts_with(prefix) {
            expected.push(key.to_vec());
        }
        scan.next();
    }
    scan.status().unwrap();
    assert_eq!(
        found, expected,
        "a seek for {prefix:?} and a full scan disagree"
    );
    (found, ruled_out)
}

const SEEKS: [&[u8]; 9] = [
    b"", b"a", b"aa", b"aabb", b"aabb:1", b"aacc", b"aadd", b"bb", b"bbxx",
];

/// Written with one extractor, or none, and reopened with another, or none:
/// every seek finds what a scan finds, and no table another extractor built
/// is ruled out.
#[test]
fn a_table_another_extractor_built_is_read_whole() {
    let changes = [
        (Some(2), Some(4)),
        (Some(4), Some(2)),
        (Some(2), None),
        (None, Some(2)),
        (Some(4), Some(3)),
    ];
    for (before, after) in changes {
        let env = Arc::new(MemEnv::new());
        let (db, _) = open(&env, before);
        write_three_tables(&db);
        db.close().unwrap();
        drop(db);

        let (db, stats) = open(&env, after);
        for prefix in SEEKS {
            let (_, ruled_out) = seek(&db, &stats, prefix);
            assert_eq!(ruled_out, 0, "{before:?} then {after:?}: {prefix:?}");
        }
        // The rows are all there: a seek for each tenant finds its own.
        assert_eq!(seek(&db, &stats, b"aabb").0.len(), 2);
        assert_eq!(seek(&db, &stats, b"aa").0.len(), 3);

        // A compaction rewrites the tables under the extractor the database
        // runs with now, and every seek still finds what a scan finds.
        db.compact_range(None, None).wait().unwrap();
        for prefix in SEEKS {
            seek(&db, &stats, prefix);
        }
    }
}

/// Reopened with an extractor of the same name, a seek still rules out the
/// tables that lack its prefix.
#[test]
fn a_table_the_same_extractor_built_still_filters() {
    let env = Arc::new(MemEnv::new());
    let (db, _) = open(&env, Some(4));
    write_three_tables(&db);
    db.close().unwrap();
    drop(db);

    let (db, stats) = open(&env, Some(4));
    let (found, ruled_out) = seek(&db, &stats, b"aabb");
    assert_eq!(found.len(), 2);
    assert_eq!(ruled_out, 2, "the `aacc` and `bbxx` tables");
    assert_eq!(seek(&db, &stats, b"aadd"), (Vec::new(), 3));
    // A prefix shorter than the extractor's width rules nothing out.
    assert_eq!(seek(&db, &stats, b"aa").1, 0);
}
