//! Public-API coverage for the reads that answer without handing back
//! a copy: `has`, `get_size`, and the iterator's `value_slice`.
#![cfg(not(target_arch = "wasm32"))]

use proptest::prelude::*;
use regolith::{Db, MergeOperator, Options, WriteBatch};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

mod common;

use common::{fill_sequential, force_compaction, open};

/// Concatenates operands onto the base, so a merge chain has an
/// observable length that differs from any single operand.
#[derive(Debug)]
struct Concat;

impl MergeOperator for Concat {
    fn name(&self) -> &'static str {
        "concat"
    }

    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(|b| b.to_vec()).unwrap_or_default();
        for op in operands {
            out.extend_from_slice(op);
        }
        Some(out)
    }
}

#[test]
fn has_and_get_size_agree_with_get_across_every_source() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);

    // Enough keys to spill past the 4 KiB write buffer, so the reads
    // below hit the memtable, frozen memtables and SSTables alike.
    fill_sequential(&db, 500);
    regolith::through_stalls(|| db.put(b"in_memtable", b"fresh")).unwrap();
    regolith::through_stalls(|| db.put(b"empty_value", b"")).unwrap();
    regolith::through_stalls(|| db.delete(b"key_000010")).unwrap();

    for i in 0..500 {
        let key = format!("key_{i:06}");
        let got = db.get(key.as_bytes()).unwrap();
        assert_eq!(db.has(key.as_bytes()).unwrap(), got.is_some(), "{key}");
        assert_eq!(
            db.get_size(key.as_bytes()).unwrap(),
            got.as_ref().map(|v| v.len()),
            "{key}"
        );
    }

    assert!(db.has(b"in_memtable").unwrap());
    assert_eq!(db.get_size(b"in_memtable").unwrap(), Some(5));
    assert!(!db.has(b"key_000010").unwrap(), "deleted key");
    assert_eq!(db.get_size(b"key_000010").unwrap(), None);
    assert!(!db.has(b"absent").unwrap());
    assert_eq!(db.get_size(b"absent").unwrap(), None);
}

#[test]
fn an_empty_value_is_present_with_length_zero() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    regolith::through_stalls(|| db.put(b"empty", b"")).unwrap();

    assert!(db.has(b"empty").unwrap(), "an empty value still exists");
    assert_eq!(db.get_size(b"empty").unwrap(), Some(0));
    assert!(!db.has(b"never_written").unwrap());
    assert_eq!(db.get_size(b"never_written").unwrap(), None);

    force_compaction(&db);
    assert!(db.has(b"empty").unwrap(), "still present from an SSTable");
    assert_eq!(db.get_size(b"empty").unwrap(), Some(0));
}

#[test]
fn has_respects_range_tombstones() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    regolith::through_stalls(|| db.put(b"aa", b"1")).unwrap();
    regolith::through_stalls(|| db.put(b"ab", b"2")).unwrap();
    regolith::through_stalls(|| db.put(b"zz", b"3")).unwrap();
    regolith::through_stalls(|| db.delete_range(b"aa", b"b")).unwrap();

    assert!(!db.has(b"aa").unwrap());
    assert!(!db.has(b"ab").unwrap());
    assert_eq!(db.get_size(b"aa").unwrap(), None);
    assert!(db.has(b"zz").unwrap(), "outside the deleted range");

    force_compaction(&db);
    assert!(!db.has(b"aa").unwrap());
    assert!(db.has(b"zz").unwrap());
}

#[test]
fn has_respects_snapshot_visibility() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    regolith::through_stalls(|| db.put(b"before", b"v")).unwrap();
    let snap = db.snapshot();
    regolith::through_stalls(|| db.put(b"after", b"v")).unwrap();
    regolith::through_stalls(|| db.delete(b"before")).unwrap();

    assert!(snap.has(b"before").unwrap(), "still live at the snapshot");
    assert_eq!(snap.get_size(b"before").unwrap(), Some(1));
    assert!(!snap.has(b"after").unwrap(), "written after the snapshot");
    assert_eq!(snap.get_size(b"after").unwrap(), None);

    assert!(!db.has(b"before").unwrap(), "deleted at the live view");
    assert!(db.has(b"after").unwrap());
}

#[test]
fn has_and_get_size_are_scoped_to_a_column_family() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();
    let cf = db.create_column_family("other").unwrap();

    regolith::through_stalls(|| db.put(b"shared", b"default")).unwrap();
    regolith::through_stalls(|| db.put_cf(&cf, b"cf_only", b"cf-value")).unwrap();

    assert!(db.has(b"shared").unwrap());
    assert!(!db.has(b"cf_only").unwrap(), "CF key is not in the default");
    assert!(db.has_cf(&cf, b"cf_only").unwrap());
    assert!(!db.has_cf(&cf, b"shared").unwrap());
    assert_eq!(db.get_size_cf(&cf, b"cf_only").unwrap(), Some(8));
    assert_eq!(db.get_size_cf(&cf, b"shared").unwrap(), None);

    let snap = db.snapshot();
    assert!(snap.has_cf(&cf, b"cf_only").unwrap());
    assert_eq!(snap.get_size_cf(&cf, b"cf_only").unwrap(), Some(8));
}

#[test]
fn has_with_a_merge_operator_reports_the_collapsed_chain() {
    // Documented behavior, asserted rather than hidden: with a merge
    // operator configured, `has` and `get_size` answer about the value
    // `full_merge` produces, which means they materialize the chain.
    let dir = TempDir::new().unwrap();
    let opts = Options::default().merge_operator(Some(std::sync::Arc::new(Concat)));
    let db = Db::open(dir.path(), opts).unwrap();

    regolith::through_stalls(|| db.put(b"k", b"base")).unwrap();
    regolith::through_stalls(|| db.merge(b"k", b"+one")).unwrap();
    regolith::through_stalls(|| db.merge(b"k", b"+two")).unwrap();

    let merged = db.get(b"k").unwrap().expect("merged value");
    assert_eq!(merged, b"base+one+two");
    assert!(db.has(b"k").unwrap());
    assert_eq!(db.get_size(b"k").unwrap(), Some(merged.len()));
}

#[test]
fn iterator_value_slice_matches_value_and_outlives_the_step() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    fill_sequential(&db, 300);
    force_compaction(&db);

    // Hold every slice while the cursor keeps moving, so a slice that
    // borrowed a block the iterator has since left would read wrong.
    let mut held = Vec::new();
    let mut expected = Vec::new();
    let mut it = db.iter();
    it.seek_to_first();
    while it.valid() {
        let value = it.value().expect("value").to_vec();
        let slice = it.value_slice().expect("value_slice");
        assert_eq!(slice.as_slice(), value.as_slice());
        expected.push(value);
        held.push(slice);
        it.next();
    }
    assert_eq!(held.len(), 300);
    drop(it);

    for (slice, want) in held.iter().zip(&expected) {
        assert_eq!(slice.as_slice(), want.as_slice());
    }
}

#[test]
fn iterator_value_slice_covers_memtable_reverse_and_merge_sources() {
    let dir = TempDir::new().unwrap();
    let opts = Options::default()
        .merge_operator(Some(std::sync::Arc::new(Concat)))
        .write_buffer_size(4 * 1024);
    let db = Db::open(dir.path(), opts).unwrap();

    let mut batch = WriteBatch::new();
    batch.put(b"a", b"alpha");
    batch.put(b"b", b"beta");
    regolith::through_stalls(|| db.write(batch.clone())).unwrap();
    regolith::through_stalls(|| db.merge(b"c", b"gamma")).unwrap();

    // Forward over the memtable.
    let mut it = db.iter();
    it.seek_to_first();
    let mut forward = Vec::new();
    while it.valid() {
        forward.push((
            it.key().expect("key").to_vec(),
            it.value_slice().expect("slice").to_vec(),
        ));
        it.next();
    }
    assert_eq!(
        forward,
        vec![
            (b"a".to_vec(), b"alpha".to_vec()),
            (b"b".to_vec(), b"beta".to_vec()),
            (b"c".to_vec(), b"gamma".to_vec()),
        ]
    );

    // Reverse.
    let mut it = db.iter();
    it.seek_to_last();
    let mut reverse = Vec::new();
    while it.valid() {
        reverse.push((
            it.key().expect("key").to_vec(),
            it.value_slice().expect("slice").to_vec(),
        ));
        it.prev();
    }
    reverse.reverse();
    assert_eq!(reverse, forward);

    // An unpositioned iterator has no value to hand out.
    let fresh = db.iter();
    assert!(fresh.value_slice().is_none());
}

#[test]
fn cf_and_owned_iterators_expose_value_slice() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();
    let cf = db.create_column_family("cf").unwrap();
    regolith::through_stalls(|| db.put_cf(&cf, b"k", b"cf-value")).unwrap();
    regolith::through_stalls(|| db.put(b"k", b"default-value")).unwrap();

    let mut it = db.iter_cf(&cf);
    it.seek_to_first();
    assert!(it.valid());
    assert_eq!(it.value_slice().expect("slice").as_slice(), b"cf-value");

    let mut owned = db.snapshot().into_owned_iter();
    owned.seek_to_first();
    assert!(owned.valid());
    assert_eq!(
        owned.value_slice().expect("slice").as_slice(),
        b"default-value"
    );
}

proptest! {
    /// `has` and `get_size` must never disagree with `get` about
    /// whether a key exists or how long its value is, for any mix of
    /// writes, overwrites and deletes across the memtable and SSTables.
    #[test]
    fn has_and_get_size_never_disagree_with_get(
        ops in proptest::collection::vec(
            (0u8..20, proptest::option::of(
                proptest::collection::vec(any::<u8>(), 0..24))),
            1..60),
        compact in any::<bool>(),
    ) {
        let dir = TempDir::new().unwrap();
        let db = open(&dir);
        for (key_byte, value) in &ops {
            let key = [*key_byte];
            match value {
                Some(v) => regolith::through_stalls(|| db.put(&key, v)).unwrap(),
                None => regolith::through_stalls(|| db.delete(&key)).unwrap(),
            }
        }
        if compact {
            force_compaction(&db);
        }
        for key_byte in 0u8..24 {
            let key = [key_byte];
            let got = db.get(&key).unwrap();
            prop_assert_eq!(db.has(&key).unwrap(), got.is_some());
            prop_assert_eq!(db.get_size(&key).unwrap(), got.as_ref().map(|v| v.len()));
        }
    }
}

/// Waiting for readers to finish has to see what the embedder cannot:
/// an iterator taken from a snapshot pins the snapshot again, so a
/// database is still busy after the `Snapshot` handle itself is gone.
#[test]
fn waiting_for_snapshots_counts_iterator_pins_not_just_snapshot_handles() {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), Options::default()).expect("open");
    for i in 0..32u32 {
        regolith::through_stalls(|| db.put(format!("k{i:04}").as_bytes(), b"v")).expect("put");
    }

    assert_eq!(
        db.wait_for_snapshots().wait(Duration::from_secs(5)),
        0,
        "a database with no readers must not wait at all",
    );

    let snapshot = db.snapshot();
    assert_eq!(
        db.wait_for_snapshots().wait(Duration::from_millis(50)),
        1,
        "a live snapshot must be counted",
    );

    // The iterator pins the snapshot independently, so dropping the
    // handle is not enough.
    let iter = snapshot.into_owned_iter();
    assert_eq!(
        db.wait_for_snapshots().wait(Duration::from_millis(50)),
        1,
        "an iterator outliving its snapshot handle must keep the pin",
    );

    drop(iter);
    assert_eq!(
        db.wait_for_snapshots().wait(Duration::from_secs(5)),
        0,
        "every pin was released, so the wait must return immediately",
    );
    db.close().expect("close");
}

/// Asking about the snapshots never waits: the call returns with the pins
/// live now while this very thread still holds one, which a call that waited
/// for the release could never do. The handle it returns reports the count
/// again each time it is asked.
#[test]
fn asking_about_snapshots_returns_at_once_with_what_is_pinned() {
    let db = Db::open("snapshots-status", Options::memory()).expect("open");
    let status = db.wait_for_snapshots();
    assert_eq!(status.pinned(), 0);
    assert!(status.is_ready());

    let snapshot = db.snapshot();
    let iter = snapshot.owned_iter();
    let held = db.wait_for_snapshots();
    assert_eq!(held.pinned(), 2, "the handle and its iterator each pin");
    assert!(!held.is_ready());
    assert_eq!(status.pinned(), 2, "a handle reports what is pinned now");

    drop(snapshot);
    assert_eq!(held.pinned(), 1);
    assert!(!held.is_ready());
    drop(iter);
    assert_eq!(held.pinned(), 0);
    assert!(held.is_ready());
}

/// Counts the times it is woken.
#[derive(Default)]
struct Wakes(std::sync::atomic::AtomicUsize);

impl std::task::Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Polled as a future, the wait is pending while a snapshot is pinned and
/// wakes its task when the last pin is released, with nothing blocking and
/// no timer: the release itself wakes it.
#[test]
fn the_wait_is_a_future_that_the_last_release_wakes() {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::Ordering;
    use std::task::{Context, Poll, Waker};

    let db = Db::open("snapshots-future", Options::memory()).expect("open");
    let wakes = Arc::new(Wakes::default());
    let waker = Waker::from(Arc::clone(&wakes));
    let mut cx = Context::from_waker(&waker);

    // Nothing pinned: ready at the first poll.
    let mut idle = db.wait_for_snapshots();
    assert_eq!(Pin::new(&mut idle).poll(&mut cx), Poll::Ready(()));

    let first = db.snapshot();
    let second = db.snapshot();
    let mut wait = db.wait_for_snapshots();
    assert_eq!(Pin::new(&mut wait).poll(&mut cx), Poll::Pending);
    assert_eq!(wakes.0.load(Ordering::SeqCst), 0);

    // One pin left: whether or not the release woke the task, the wait is
    // still pending when polled.
    drop(first);
    assert_eq!(Pin::new(&mut wait).poll(&mut cx), Poll::Pending);
    let before = wakes.0.load(Ordering::SeqCst);

    drop(second);
    assert!(
        wakes.0.load(Ordering::SeqCst) > before,
        "the release of the last pin must wake the waiting task"
    );
    assert_eq!(Pin::new(&mut wait).poll(&mut cx), Poll::Ready(()));

    // Ready said nothing was pinned then. A wait polled again starts over.
    let third = db.snapshot();
    assert_eq!(Pin::new(&mut wait).poll(&mut cx), Poll::Pending);
    drop(third);
    assert_eq!(Pin::new(&mut wait).poll(&mut cx), Poll::Ready(()));
}

/// A task that awaits the wait on one thread is resumed by a release on
/// another, in whichever order the two happen.
#[test]
fn a_release_on_another_thread_resumes_the_awaiting_task() {
    let db = Db::open("snapshots-await", Options::memory()).expect("open");
    for _ in 0..200 {
        let snapshot = db.snapshot();
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                let mut queue = db.io_queue();
                queue.block_on(db.wait_for_snapshots());
            });
            drop(snapshot);
            waiter.join().expect("the awaiting task finished");
        });
        assert_eq!(db.wait_for_snapshots().pinned(), 0);
    }
}

/// A wait dropped while pending leaves nothing behind: later releases and
/// later waits behave as if it had never been polled.
#[test]
fn a_wait_dropped_while_pending_is_withdrawn() {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::Ordering;
    use std::task::{Context, Poll, Waker};

    let db = Db::open("snapshots-dropped-wait", Options::memory()).expect("open");
    let wakes = Arc::new(Wakes::default());
    let waker = Waker::from(Arc::clone(&wakes));
    let mut cx = Context::from_waker(&waker);

    let snapshot = db.snapshot();
    let mut abandoned = db.wait_for_snapshots();
    assert_eq!(Pin::new(&mut abandoned).poll(&mut cx), Poll::Pending);
    drop(abandoned);
    drop(snapshot);
    assert_eq!(
        wakes.0.load(Ordering::SeqCst),
        0,
        "a withdrawn wait is not woken"
    );
    let mut fresh = db.wait_for_snapshots();
    assert_eq!(Pin::new(&mut fresh).poll(&mut cx), Poll::Ready(()));
}

/// The wait must return as soon as the last reader finishes rather than
/// after a fixed poll interval, so a generous timeout costs nothing.
#[test]
fn waiting_for_snapshots_returns_as_soon_as_the_last_reader_finishes() {
    let dir = TempDir::new().expect("tempdir");
    let db = Arc::new(Db::open(dir.path(), Options::default()).expect("open"));
    regolith::through_stalls(|| db.put(b"k", b"v")).expect("put");

    let snapshot = db.snapshot();
    let holder = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            assert_eq!(snapshot.get(b"k").expect("get").as_deref(), Some(&b"v"[..]));
            std::thread::sleep(Duration::from_millis(120));
            drop(snapshot);
            drop(db);
        })
    };

    let started = std::time::Instant::now();
    let outstanding = db.wait_for_snapshots().wait(Duration::from_secs(30));
    let waited = started.elapsed();
    holder.join().expect("holder");

    assert_eq!(outstanding, 0, "the wait timed out with the reader gone");
    assert!(
        waited < Duration::from_secs(5),
        "the wait took {waited:?} against a 30s timeout, so it is sleeping out an interval rather \
         than waking on the release",
    );
}
