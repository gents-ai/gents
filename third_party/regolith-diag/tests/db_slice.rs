//! Public-API coverage for the zero-copy read surface: `get_slice`,
//! its column-family and snapshot variants, and the lifetime guarantee
//! that a returned slice keeps its owner alive.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashSet;

use proptest::prelude::*;
use regolith::{Db, DbSlice, MergeOperator, Options, WriteBatch};
use tempfile::TempDir;

mod common;

use common::{fill_sequential, force_compaction, open};

fn owned(slice: Option<DbSlice>) -> Option<Vec<u8>> {
    slice.map(|s| s.to_vec())
}

#[test]
fn get_slice_agrees_with_get_across_every_source() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);

    // Enough keys to spill past the 4 KiB write buffer, so the reads
    // below hit the memtable, frozen memtables and SSTables alike.
    fill_sequential(&db, 500);
    regolith::through_stalls(|| db.put(b"in_memtable", b"fresh")).unwrap();
    regolith::through_stalls(|| db.delete(b"key_000010")).unwrap();

    for i in 0..500 {
        let key = format!("key_{i:06}");
        assert_eq!(
            owned(db.get_slice(key.as_bytes()).unwrap()),
            db.get(key.as_bytes()).unwrap(),
            "mismatch for {key}"
        );
    }
    assert_eq!(
        owned(db.get_slice(b"in_memtable").unwrap()),
        Some(b"fresh".to_vec())
    );
    assert_eq!(db.get_slice(b"key_000010").unwrap(), None, "deleted key");
    assert_eq!(db.get_slice(b"absent").unwrap(), None);
}

#[test]
fn get_slice_reads_an_empty_value() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    regolith::through_stalls(|| db.put(b"empty", b"")).unwrap();
    let slice = db.get_slice(b"empty").unwrap().expect("present");
    assert!(slice.is_empty());
    assert_eq!(slice.len(), 0);
    assert_eq!(slice.as_slice(), b"");
}

#[test]
fn get_slice_cf_is_scoped_to_its_column_family() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let cf = db.create_column_family("other").unwrap();

    regolith::through_stalls(|| db.put(b"k", b"default_value")).unwrap();
    regolith::through_stalls(|| db.put_cf(&cf, b"k", b"cf_value")).unwrap();

    assert_eq!(db.get_slice(b"k").unwrap().unwrap(), *b"default_value");
    assert_eq!(db.get_slice_cf(&cf, b"k").unwrap().unwrap(), *b"cf_value");
    assert_eq!(db.get_slice_cf(&cf, b"absent").unwrap(), None);
}

#[test]
fn snapshot_get_slice_sees_the_snapshot_view() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let cf = db.create_column_family("cf").unwrap();
    regolith::through_stalls(|| db.put(b"k", b"v1")).unwrap();
    regolith::through_stalls(|| db.put_cf(&cf, b"k", b"cf1")).unwrap();

    let snap = db.snapshot();
    regolith::through_stalls(|| db.put(b"k", b"v2")).unwrap();
    regolith::through_stalls(|| db.put_cf(&cf, b"k", b"cf2")).unwrap();

    assert_eq!(snap.get_slice(b"k").unwrap().unwrap(), *b"v1");
    assert_eq!(snap.get_slice_cf(&cf, b"k").unwrap().unwrap(), *b"cf1");
    assert_eq!(db.get_slice(b"k").unwrap().unwrap(), *b"v2");
}

#[test]
fn a_slice_outlives_the_flush_of_the_memtable_it_came_from() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    regolith::through_stalls(|| db.put(b"pinned", b"survives the flush")).unwrap();

    let slice = db.get_slice(b"pinned").unwrap().expect("present");
    // Push the memtable it came from out of memory and onto disk.
    fill_sequential(&db, 400);
    force_compaction(&db);

    assert_eq!(slice.as_slice(), b"survives the flush");
}

#[test]
fn a_slice_outlives_the_compaction_of_the_file_it_came_from() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    fill_sequential(&db, 400);
    force_compaction(&db);

    let slice = db.get_slice(b"key_000042").unwrap().expect("present");
    force_compaction(&db);

    assert_eq!(slice.as_slice(), b"val_000042");
}

#[test]
fn a_slice_outlives_the_database_handle() {
    let dir = TempDir::new().unwrap();
    let slice = {
        let db = open(&dir);
        regolith::through_stalls(|| db.put(b"k", b"outlives close")).unwrap();
        let slice = db.get_slice(b"k").unwrap().expect("present");
        db.close().unwrap();
        slice
    };
    assert_eq!(slice.as_slice(), b"outlives close");
}

#[test]
// A memtable-owned slice keeps an `Arc<Arena>`, whose chunk list is
// behind a mutex, so clippy sees interior mutability. `DbSlice` hashes
// only its bytes, which are immutable, so it is a sound key.
#[allow(clippy::mutable_key_type)]
fn slice_traits_cover_the_documented_surface() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    regolith::through_stalls(|| db.put(b"k", b"abc")).unwrap();
    let slice = db.get_slice(b"k").unwrap().expect("present");

    // Deref / AsRef.
    assert_eq!(slice.len(), 3);
    assert_eq!(&slice[..2], b"ab");
    assert_eq!(slice.as_ref(), b"abc");
    assert_eq!(slice.iter().copied().collect::<Vec<u8>>(), b"abc".to_vec());

    // Equality in both directions, against every documented shape.
    assert_eq!(slice, *b"abc".as_slice());
    assert_eq!(slice, b"abc".as_slice());
    assert_eq!(slice, b"abc".to_vec());
    assert_eq!(slice, *b"abc");
    assert!(*b"abc".as_slice() == slice);
    assert!(b"abc".to_vec() == slice);
    assert!(*b"abc" == slice);

    // Ord, Hash, Clone, Debug, From.
    let other = db.get_slice(b"k").unwrap().expect("present");
    assert_eq!(slice, other);
    assert!(slice <= other);
    let mut set = HashSet::new();
    set.insert(slice.clone());
    assert!(set.contains(&other));
    assert!(format!("{slice:?}").contains("abc"));
    let owned: Vec<u8> = slice.into();
    assert_eq!(owned, b"abc");
}

/// The zero-copy surface has to cover every iterator the crate hands
/// out, not just the point read. A tailing iterator is the one that
/// refreshes its own cursor as writes land, so a value borrowed from it
/// is the easiest to invalidate: the slice must survive both the `next`
/// and the refresh.
#[test]
fn a_tailing_iterator_hands_out_values_that_outlive_its_cursor() {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), Options::default()).expect("open");
    for i in 0..64u32 {
        regolith::through_stalls(|| {
            db.put(
                format!("t{i:04}").as_bytes(),
                format!("value-{i}").as_bytes(),
            )
        })
        .expect("put");
    }

    let mut iter = db.iter_tailing();
    iter.seek_to_first();
    assert!(iter.valid(), "the iterator must land on the first entry");

    // Take the slice, then move the cursor and write past it, so the
    // entry the slice came from is no longer the one under the cursor
    // and the iterator has had a reason to rebuild its view.
    let first = iter.value_slice().expect("value_slice on a valid entry");
    assert_eq!(&*first, b"value-0");
    iter.next();
    for i in 64..128u32 {
        regolith::through_stalls(|| db.put(format!("t{i:04}").as_bytes(), b"later")).expect("put");
    }
    while iter.valid() {
        iter.next();
    }

    assert_eq!(
        &*first, b"value-0",
        "a slice taken from a tailing iterator must not follow the cursor",
    );
}

/// `value_slice` and `value` must never disagree, on any iterator.
#[test]
fn every_iterator_value_slice_agrees_with_its_borrowed_value() {
    let dir = TempDir::new().expect("tempdir");
    let db = open(&dir);
    fill_sequential(&db, 400);
    force_compaction(&db);
    // Half in SSTables, half still in the memtable, so both owner
    // variants are exercised in one walk.
    for i in 400..600usize {
        regolith::through_stalls(|| {
            db.put(
                format!("key_{i:06}").as_bytes(),
                format!("val_{i:06}").as_bytes(),
            )
        })
        .expect("put");
    }

    let mut checked = 0usize;
    let mut iter = db.iter();
    iter.seek_to_first();
    while iter.valid() {
        let borrowed = iter.value().expect("value").to_vec();
        let owned = iter.value_slice().expect("value_slice").to_vec();
        assert_eq!(borrowed, owned, "iter value and value_slice disagree");
        checked += 1;
        iter.next();
    }
    assert_eq!(checked, 600, "the walk did not cover every key");

    let mut tail = db.iter_tailing();
    tail.seek_to_first();
    let mut tail_checked = 0usize;
    while tail.valid() {
        let borrowed = tail.value().expect("value").to_vec();
        let owned = tail.value_slice().expect("value_slice").to_vec();
        assert_eq!(borrowed, owned, "tailing value and value_slice disagree");
        tail_checked += 1;
        tail.next();
    }
    assert_eq!(
        tail_checked, 600,
        "the tailing walk did not cover every key"
    );
}

#[test]
fn try_subslice_narrows_a_read_value() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    regolith::through_stalls(|| db.put(b"k", b"header:payload")).unwrap();
    let slice = db.get_slice(b"k").unwrap().expect("present");

    assert_eq!(slice.try_subslice(7..14).unwrap(), *b"payload");
    assert!(slice.try_subslice(0..15).is_none());
}

/// Concatenates the base value and every operand, so a merge read has
/// an answer that neither the base nor any single operand equals.
struct AppendMerge;

impl MergeOperator for AppendMerge {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.unwrap_or(b"").to_vec();
        for operand in operands {
            out.extend_from_slice(operand);
        }
        Some(out)
    }

    fn name(&self) -> &'static str {
        "append"
    }
}

#[test]
fn merge_operator_reads_route_through_get_slice() {
    let dir = TempDir::new().unwrap();
    let opts = Options::default().merge_operator(Some(std::sync::Arc::new(AppendMerge)));
    let db = Db::open(dir.path(), opts).unwrap();
    regolith::through_stalls(|| db.put(b"k", b"base")).unwrap();
    regolith::through_stalls(|| db.merge(b"k", b"+1")).unwrap();

    assert_eq!(
        owned(db.get_slice(b"k").unwrap()),
        db.get(b"k").unwrap(),
        "merge results must agree between get and get_slice"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn get_slice_matches_get_for_arbitrary_writes(
        writes in proptest::collection::vec(
            (proptest::collection::vec(any::<u8>(), 1..24),
             proptest::collection::vec(any::<u8>(), 0..64),
             any::<bool>()),
            1..64,
        ),
    ) {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), Options::default()).unwrap();

        let mut batch = WriteBatch::new();
        for (key, value, delete) in &writes {
            if *delete {
                batch.delete(key);
            } else {
                batch.put(key, value);
            }
        }
        regolith::through_stalls(|| db.write(batch.clone())).unwrap();

        for (key, _, _) in &writes {
            let via_get = db.get(key).unwrap();
            let via_slice = db.get_slice(key).unwrap().map(|s| s.to_vec());
            prop_assert_eq!(via_slice, via_get);
        }
    }
}
