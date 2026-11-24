//! Scenario tests ported from common LSM-tree test suites.
//!
//! Each test exercises a specific behavior that production
//! storage engines are expected to satisfy. The scenarios are
//! the valuable part - they encode 15+ years of bugs-found-the-
//! hard-way from LSM implementations. Cross-engine validation
//! (running the same scenario against both regolith and another
//! engine) can be added as a follow-up.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads, the filesystem or proptest, none of which exist there. The browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use regolith::{CompactionStyle, CompressionType, Db, FifoCompactionOptions, Options, WriteBatch};
use tempfile::TempDir;

mod common;

use common::{open, small_opts as opts};

// ── basic CRUD ────────────────────────────────────────────────

#[test]
fn put_get_delete_cycle() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    regolith::through_stalls(|| db.put(b"k1", b"v1")).unwrap();
    assert_eq!(db.get(b"k1").unwrap(), Some(b"v1".to_vec()));
    regolith::through_stalls(|| db.delete(b"k1")).unwrap();
    assert_eq!(db.get(b"k1").unwrap(), None);
}

#[test]
fn overwrite_returns_latest_value() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    regolith::through_stalls(|| db.put(b"k", b"v1")).unwrap();
    regolith::through_stalls(|| db.put(b"k", b"v2")).unwrap();
    regolith::through_stalls(|| db.put(b"k", b"v3")).unwrap();
    assert_eq!(db.get(b"k").unwrap(), Some(b"v3".to_vec()));
}

#[test]
fn empty_value_is_distinct_from_missing() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    regolith::through_stalls(|| db.put(b"k", b"")).unwrap();
    assert_eq!(db.get(b"k").unwrap(), Some(vec![]));
    assert_eq!(db.get(b"missing").unwrap(), None);
}

#[test]
fn write_batch_atomic_visibility() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let mut batch = WriteBatch::new();
    batch.put(b"a", b"1");
    batch.put(b"b", b"2");
    batch.put(b"c", b"3");
    batch.delete(b"b");
    regolith::through_stalls(|| db.write(batch.clone())).unwrap();
    assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()));
    assert_eq!(db.get(b"b").unwrap(), None);
    assert_eq!(db.get(b"c").unwrap(), Some(b"3".to_vec()));
}

// ── reopen / recovery ─────────────────────────────────────────

#[test]
fn reopen_preserves_all_data() {
    let dir = TempDir::new().unwrap();
    {
        let db = open(&dir);
        for i in 0..200 {
            regolith::through_stalls(|| {
                db.put(format!("k{i:04}").as_bytes(), format!("v{i}").as_bytes())
            })
            .unwrap();
        }
    }
    let db = open(&dir);
    for i in 0..200 {
        assert_eq!(
            db.get(format!("k{i:04}").as_bytes()).unwrap(),
            Some(format!("v{i}").into_bytes()),
        );
    }
}

#[test]
fn reopen_after_delete_still_hides_key() {
    let dir = TempDir::new().unwrap();
    {
        let db = open(&dir);
        regolith::through_stalls(|| db.put(b"keep", b"yes")).unwrap();
        regolith::through_stalls(|| db.put(b"drop", b"no")).unwrap();
        regolith::through_stalls(|| db.delete(b"drop")).unwrap();
    }
    let db = open(&dir);
    assert_eq!(db.get(b"keep").unwrap(), Some(b"yes".to_vec()));
    assert_eq!(db.get(b"drop").unwrap(), None);
}

#[test]
fn reopen_after_compact_range() {
    let dir = TempDir::new().unwrap();
    {
        let db = open(&dir);
        for i in 0..500 {
            regolith::through_stalls(|| db.put(format!("k{i:04}").as_bytes(), b"v")).unwrap();
        }
        db.compact_range(None, None).wait().unwrap();
    }
    let db = open(&dir);
    for i in 0..500 {
        assert_eq!(
            db.get(format!("k{i:04}").as_bytes()).unwrap(),
            Some(b"v".to_vec())
        );
    }
}

// ── snapshot isolation ────────────────────────────────────────

#[test]
fn snapshot_sees_pre_snapshot_writes_only() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    regolith::through_stalls(|| db.put(b"a", b"before")).unwrap();
    let snap = db.snapshot();
    regolith::through_stalls(|| db.put(b"a", b"after")).unwrap();
    regolith::through_stalls(|| db.put(b"b", b"new")).unwrap();
    assert_eq!(snap.get(b"a").unwrap(), Some(b"before".to_vec()));
    assert_eq!(snap.get(b"b").unwrap(), None);
    assert_eq!(db.get(b"a").unwrap(), Some(b"after".to_vec()));
}

#[test]
fn snapshot_survives_compaction() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    for i in 0..200 {
        regolith::through_stalls(|| db.put(format!("k{i:04}").as_bytes(), b"old")).unwrap();
    }
    let snap = db.snapshot();
    for i in 0..200 {
        regolith::through_stalls(|| db.put(format!("k{i:04}").as_bytes(), b"new")).unwrap();
    }
    db.compact_range(None, None).wait().unwrap();
    // Snapshot still sees old values.
    for i in 0..200 {
        assert_eq!(
            snap.get(format!("k{i:04}").as_bytes()).unwrap(),
            Some(b"old".to_vec()),
        );
    }
    // Current view sees new values.
    for i in 0..200 {
        assert_eq!(
            db.get(format!("k{i:04}").as_bytes()).unwrap(),
            Some(b"new".to_vec()),
        );
    }
}

// ── iterator edge cases ───────────────────────────────────────

#[test]
fn iter_seek_to_first_on_empty_db() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let mut it = db.iter();
    it.seek_to_first();
    assert!(!it.valid());
}

#[test]
fn iter_seek_past_all_keys() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    regolith::through_stalls(|| db.put(b"a", b"1")).unwrap();
    regolith::through_stalls(|| db.put(b"b", b"2")).unwrap();
    let mut it = db.iter();
    it.seek(b"z");
    assert!(!it.valid());
}

#[test]
fn iter_full_forward_scan_matches_scan() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    for i in 0..100 {
        regolith::through_stalls(|| {
            db.put(format!("k{i:04}").as_bytes(), format!("v{i}").as_bytes())
        })
        .unwrap();
    }
    db.compact_range(None, None).wait().unwrap();

    let scan_result = db.scan(None, None).unwrap();
    let mut iter_result = Vec::new();
    let mut it = db.iter();
    it.seek_to_first();
    while it.valid() {
        iter_result.push((it.key().unwrap().to_vec(), it.value().unwrap().to_vec()));
        it.next();
    }
    assert_eq!(iter_result, scan_result);
}

#[test]
fn iter_reverse_scan_after_compact() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    for i in 0..50 {
        regolith::through_stalls(|| db.put(format!("k{i:02}").as_bytes(), b"v")).unwrap();
    }
    db.compact_range(None, None).wait().unwrap();

    let mut it = db.iter();
    it.seek_to_last();
    let mut keys: Vec<Vec<u8>> = Vec::new();
    while it.valid() {
        keys.push(it.key().unwrap().to_vec());
        it.prev();
    }
    assert_eq!(keys.len(), 50);
    assert_eq!(keys.first().unwrap(), b"k49");
    assert_eq!(keys.last().unwrap(), b"k00");
}

// ── flush / compaction ────────────────────────────────────────

#[test]
fn compact_range_merges_l0_to_l1() {
    let dir = TempDir::new().unwrap();
    // No compaction worker: one could flush late or drain L0 early, and
    // then what L0 holds when it is read below would be a matter of timing.
    let db = Db::open(dir.path(), opts().max_background_compactions(0)).unwrap();
    // Write enough to produce multiple L0 files.
    for i in 0..500 {
        regolith::through_stalls(|| db.put(format!("k{i:04}").as_bytes(), b"v")).unwrap();
    }
    db.flush().unwrap();
    let l0_before = db.get_int_property("regolith.num-files-at-level0").unwrap();
    assert!(l0_before > 0);
    db.compact_range(None, None).wait().unwrap();
    let l0_after = db.get_int_property("regolith.num-files-at-level0").unwrap();
    assert_eq!(l0_after, 0, "compact_range should drain L0");
    // Data still readable.
    for i in 0..500 {
        assert_eq!(
            db.get(format!("k{i:04}").as_bytes()).unwrap(),
            Some(b"v".to_vec())
        );
    }
}

#[test]
fn compaction_drops_shadowed_deletions() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    for i in 0..100 {
        regolith::through_stalls(|| db.put(format!("k{i:02}").as_bytes(), b"v")).unwrap();
    }
    for i in 0..100 {
        regolith::through_stalls(|| db.delete(format!("k{i:02}").as_bytes())).unwrap();
    }
    db.compact_range(None, None).wait().unwrap();
    let total = db
        .get_int_property("regolith.total-sst-files-size")
        .unwrap();
    // All keys deleted + compacted → the remaining SSTs should
    // be tiny (just tombstones or empty).
    assert!(
        total < 4096,
        "compacted db with all deletes should be tiny, got {total}"
    );
}

// ── compression ───────────────────────────────────────────────

#[test]
fn compression_none_produces_larger_files_than_lz4() {
    let dir_none = TempDir::new().unwrap();
    let dir_lz4 = TempDir::new().unwrap();

    let write = |dir: &TempDir, compression: CompressionType| {
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .compression(compression);
        let db = Db::open(dir.path(), opts).unwrap();
        let payload = vec![0xABu8; 256];
        for i in 0..500 {
            regolith::through_stalls(|| db.put(format!("k{i:04}").as_bytes(), &payload)).unwrap();
        }
        db.compact_range(None, None).wait().unwrap();
        db.get_int_property("regolith.total-sst-files-size")
            .unwrap()
    };

    let size_none = write(&dir_none, CompressionType::None);
    let size_lz4 = write(&dir_lz4, CompressionType::Lz4);
    assert!(
        size_lz4 < size_none,
        "LZ4 ({size_lz4}) should produce smaller files than None ({size_none})"
    );
}

// ── FIFO compaction ───────────────────────────────────────────

#[test]
fn fifo_drops_oldest_file_when_over_cap() {
    let dir = TempDir::new().unwrap();
    let opts = Options::default()
        .write_buffer_size(4 * 1024)
        .compaction_style(CompactionStyle::Fifo)
        .fifo_compaction_options(FifoCompactionOptions {
            max_table_files_size: 32 * 1024,
        });
    let db = Db::open(dir.path(), opts).unwrap();
    let payload = vec![0xEEu8; 256];
    for i in 0..256 {
        regolith::through_stalls(|| db.put(format!("k{i:04}").as_bytes(), &payload)).unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    db.compact_range(None, None).wait().unwrap();
    let total = db
        .get_int_property("regolith.total-sst-files-size")
        .unwrap();
    assert!(
        total <= 64 * 1024,
        "FIFO cap 32KB should keep total bounded, got {total}"
    );
}

// ── range delete ──────────────────────────────────────────────

#[test]
fn range_delete_hides_keys_in_range() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    for i in 0..10 {
        regolith::through_stalls(|| db.put(format!("k{i}").as_bytes(), b"v")).unwrap();
    }
    regolith::through_stalls(|| db.delete_range(b"k3", b"k7")).unwrap();
    // k0..k2 and k7..k9 survive; k3..k6 are gone.
    for i in 0..10 {
        let k = format!("k{i}");
        let expected = if (3..7).contains(&i) {
            None
        } else {
            Some(b"v".to_vec())
        };
        assert_eq!(db.get(k.as_bytes()).unwrap(), expected, "key {k}");
    }
}

#[test]
fn range_delete_survives_compaction() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    for i in 0..100 {
        regolith::through_stalls(|| db.put(format!("k{i:02}").as_bytes(), b"v")).unwrap();
    }
    regolith::through_stalls(|| db.delete_range(b"k20", b"k80")).unwrap();
    db.compact_range(None, None).wait().unwrap();
    assert_eq!(db.get(b"k19").unwrap(), Some(b"v".to_vec()));
    assert_eq!(db.get(b"k20").unwrap(), None);
    assert_eq!(db.get(b"k79").unwrap(), None);
    assert_eq!(db.get(b"k80").unwrap(), Some(b"v".to_vec()));
}

// ── column families ───────────────────────────────────────────

#[test]
fn column_families_are_isolated() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let cf1 = db.create_column_family("cf1").unwrap();
    let cf2 = db.create_column_family("cf2").unwrap();
    regolith::through_stalls(|| db.put_cf(&cf1, b"shared_key", b"from_cf1")).unwrap();
    regolith::through_stalls(|| db.put_cf(&cf2, b"shared_key", b"from_cf2")).unwrap();
    assert_eq!(
        db.get_cf(&cf1, b"shared_key").unwrap(),
        Some(b"from_cf1".to_vec())
    );
    assert_eq!(
        db.get_cf(&cf2, b"shared_key").unwrap(),
        Some(b"from_cf2".to_vec())
    );
    // Default CF doesn't see either.
    assert_eq!(db.get(b"shared_key").unwrap(), None);
}
