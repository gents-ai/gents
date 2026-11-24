//! Reverse-seek reproducers for a live defect found by the differential
//! probe in `tests/adversarial_read_paths.rs`.
//!
//! Shape: a range tombstone, then a point write inside the tombstone's
//! range at a higher sequence, then a flush. Compaction emits a
//! zero-entry, range-tombstone-only SSTable whose key range overlaps its
//! neighbours in the same level, and `LevelConcatIter` assumes a level's
//! files are sorted and non-overlapping.
//!
//! Two observable failures follow:
//!
//! * `Iter::seek_for_prev(k)` reports "no key <= k" while `k` is live and
//!   `get`, `scan`, `seek` and full reverse iteration all return it.
//! * In a build with overflow checks on, `Iter::seek_to_last` panics in
//!   `SsTableReader::last_block_cursor`: `(!index.is_empty())
//!   .then_some(SsTableBlockCursor::Flat(index.len() - 1))` evaluates
//!   `index.len() - 1` eagerly, so an empty index underflows.
//!
//! Both reproduce byte for byte at the base commit `d1ec2e7`, so this is
//! a pre-existing defect in code this PR rewrote, not one it introduced.

use regolith::{Db, Options};
use tempfile::TempDir;

fn probe(label: &str, opts: Options, compact: bool) {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts).unwrap();

    // A range tombstone that covers a key written *after* it.
    db.delete_range(b"k0000", b"k0386").unwrap();
    db.put(b"k0192", b"live").unwrap();
    if compact {
        db.compact_range(None, None).wait().unwrap();
    }

    assert_eq!(
        db.get(b"k0192").unwrap(),
        Some(b"live".to_vec()),
        "{label}: get"
    );
    assert_eq!(
        db.scan(None, None).unwrap(),
        vec![(b"k0192".to_vec(), b"live".to_vec())],
        "{label}: scan"
    );

    let mut it = db.iter();
    it.seek_to_first();
    assert!(it.valid(), "{label}: seek_to_first");
    assert_eq!(it.key().unwrap(), b"k0192");

    let mut it = db.iter();
    it.seek_to_last();
    assert!(it.valid(), "{label}: seek_to_last");
    assert_eq!(it.key().unwrap(), b"k0192");

    let mut it = db.iter();
    it.seek(b"k0192");
    assert!(it.valid(), "{label}: seek(k0192)");
    assert_eq!(it.key().unwrap(), b"k0192");

    let mut it = db.iter();
    it.seek_for_prev(b"k0192");
    it.status().unwrap();
    assert!(
        it.valid(),
        "{label}: seek_for_prev(k0192) landed nowhere even though k0192 is live"
    );
    assert_eq!(it.key().unwrap(), b"k0192", "{label}: seek_for_prev key");
    assert_eq!(it.value().unwrap(), b"live", "{label}: seek_for_prev value");
}

#[test]
fn seek_for_prev_onto_a_key_written_after_a_covering_range_tombstone_memtable() {
    probe("memtable", Options::default(), false);
}

#[test]
fn seek_for_prev_onto_a_key_written_after_a_covering_range_tombstone_sstable() {
    probe("sstable", Options::default(), true);
}

/// The same shape, but seeking past the key rather than onto it.
#[test]
fn seek_for_prev_past_a_key_written_after_a_covering_range_tombstone() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();
    db.delete_range(b"k0000", b"k0386").unwrap();
    db.put(b"k0192", b"live").unwrap();
    db.compact_range(None, None).wait().unwrap();

    let mut it = db.iter();
    it.seek_for_prev(b"k0999");
    it.status().unwrap();
    assert!(it.valid(), "seek_for_prev(k0999) landed nowhere");
    assert_eq!(it.key().unwrap(), b"k0192");
}

/// Reverse iteration from the key itself, which is what a caller does
/// after `seek_for_prev`.
#[test]
fn reverse_iteration_from_the_key_itself() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();
    db.delete_range(b"a", b"z").unwrap();
    db.put(b"m", b"live").unwrap();
    db.compact_range(None, None).wait().unwrap();

    let mut it = db.iter();
    it.seek_to_last();
    let mut seen = Vec::new();
    while it.valid() {
        seen.push(it.key().unwrap().to_vec());
        it.prev();
    }
    it.status().unwrap();
    assert_eq!(seen, vec![b"m".to_vec()], "reverse walk lost the live key");

    let mut it = db.iter();
    it.seek_for_prev(b"m");
    it.status().unwrap();
    assert!(it.valid(), "seek_for_prev(m) landed nowhere");
}
