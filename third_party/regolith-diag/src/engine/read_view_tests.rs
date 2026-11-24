//! The read view under racing publishers and readers, against a sequential
//! model, and across the reclamation of views a reader still holds.

use std::sync::Weak;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use proptest::prelude::*;

use super::*;
use crate::engine::manifest::{VersionEdit, VersionSet};
use crate::engine::version_store::VersionStore;

fn memtable() -> Arc<MemTable> {
    let config = crate::engine::memtable::MemTableConfig::new(
        crate::engine::arena::ArenaProfile::EMBEDDED,
        64 * 1024,
        2,
    );
    Arc::new(MemTable::new(&config).expect("memtable"))
}

fn store_with_view() -> (tempfile::TempDir, Arc<VersionStore>, Arc<ReadViewCell>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let sst_dir = dir.path().join("sst");
    std::fs::create_dir_all(&sst_dir).expect("sst dir");
    let versions = VersionSet::open(dir.path(), &sst_dir).expect("version set");
    let store = Arc::new(VersionStore::new(versions));
    let cell = Arc::new(ReadViewCell::new(ReadView {
        active: memtable(),
        frozen: Vec::new(),
        version: store.current(),
    }));
    store.attach_view(Arc::clone(&cell));
    (dir, store, cell)
}

/// Every publisher's change lands, whichever wins each race, and a reader
/// never sees the view go back: its frozen list and its version only grow
/// from one load to the next on the same thread.
#[test]
fn racing_publishers_lose_nothing_and_readers_never_go_back() {
    const PUBLISHERS: usize = 4;
    const EACH: usize = 200;
    const VERSIONS: u64 = 300;
    let (_dir, store, cell) = store_with_view();
    let pushed: Vec<Vec<Arc<MemTable>>> = (0..PUBLISHERS)
        .map(|_| (0..EACH).map(|_| memtable()).collect())
        .collect();
    const READERS: usize = 3;
    let done = AtomicBool::new(false);
    let loads = AtomicUsize::new(0);
    let reading = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..READERS {
            scope.spawn(|| {
                let (mut frozen, mut file_id) = (0usize, 0u64);
                let mut counted = false;
                while !done.load(Ordering::Acquire) {
                    let view = cell.load();
                    assert!(
                        view.frozen.len() >= frozen,
                        "a reader saw the frozen list shrink"
                    );
                    assert!(
                        view.version.next_file_id >= file_id,
                        "a reader saw the version go back"
                    );
                    frozen = view.frozen.len();
                    file_id = view.version.next_file_id;
                    loads.fetch_add(1, Ordering::Relaxed);
                    if !counted {
                        counted = true;
                        reading.fetch_add(1, Ordering::Release);
                    }
                }
            });
        }
        // The publishers start once every reader has loaded, so the readers
        // run beside them on any number of cores: left to the scheduler,
        // publishers on one core can finish before a reader runs once. A
        // reader's first load compares against zero, so it cannot fail
        // before it counts itself and this wait always ends.
        while reading.load(Ordering::Acquire) < READERS {
            std::thread::yield_now();
        }
        let publishers: Vec<_> = pushed
            .iter()
            .map(|mine| {
                let cell = &cell;
                scope.spawn(move || {
                    for mt in mine {
                        cell.update_memtables(|active, frozen| {
                            let mut next = frozen.to_vec();
                            next.push(Arc::clone(mt));
                            (Arc::clone(active), next, ())
                        });
                    }
                })
            })
            .collect();
        let versions = {
            let store = &store;
            scope.spawn(move || {
                for id in 1..=VERSIONS {
                    store
                        .run(|versions| versions.apply(&[VersionEdit::SetNextFileId(id)]))
                        .expect("apply");
                }
            })
        };
        for publisher in publishers {
            publisher.join().expect("publisher");
        }
        versions.join().expect("version publisher");
        done.store(true, Ordering::Release);
    });
    let view = cell.load();
    assert_eq!(
        view.frozen.len(),
        PUBLISHERS * EACH,
        "a publication was lost"
    );
    for mine in &pushed {
        for mt in mine {
            assert_eq!(
                view.frozen.iter().filter(|f| Arc::ptr_eq(f, mt)).count(),
                1,
                "a memtable was lost or published twice"
            );
        }
        // Each publisher's own pushes stay in the order it made them.
        let positions: Vec<usize> = mine
            .iter()
            .map(|mt| {
                view.frozen
                    .iter()
                    .position(|f| Arc::ptr_eq(f, mt))
                    .unwrap_or(usize::MAX)
            })
            .collect();
        assert!(
            positions.windows(2).all(|w| w[0] < w[1]),
            "a publisher's pushes reordered"
        );
    }
    assert_eq!(
        view.version.next_file_id, VERSIONS,
        "a version publication was lost"
    );
    assert!(loads.load(Ordering::Relaxed) > 0);
}

/// A view a reader holds stays whole while publications replace it many
/// times over, and once the reader lets go the memtable only that view
/// named is dropped: a replaced view is freed when, and only when, no
/// reader holds it.
#[test]
fn a_held_view_stays_readable_and_is_freed_once_released() {
    let (_dir, _store, cell) = store_with_view();
    let doomed = memtable();
    doomed.put(b"k", b"v", 1);
    cell.update_memtables(|active, _| (Arc::clone(active), vec![Arc::clone(&doomed)], ()));
    let probe: Weak<MemTable> = Arc::downgrade(&doomed);
    drop(doomed);

    let held = cell.load();
    assert_eq!(held.frozen.len(), 1);
    // Retire the memtable from the published view, then replace the view
    // many more times: none of that may free what `held` reaches.
    cell.update_memtables(|active, _| (Arc::clone(active), Vec::new(), ()));
    for _ in 0..500 {
        let fresh = memtable();
        cell.update_memtables(|_, frozen| (Arc::clone(&fresh), frozen.to_vec(), ()));
    }
    let lk = crate::engine::lookup_key::LookupKey::from_prefixed(b"k", u64::MAX);
    let read = held.frozen[0]
        .get(&lk)
        .expect("the held view's memtable lost its data");
    assert_eq!(read.0, 1);
    assert!(probe.upgrade().is_some(), "a view a reader holds was freed");
    drop(read);
    drop(held);

    // Released: the reclaimer frees it. Deferred, so poll with a deadline.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut backoff = Duration::from_millis(1);
    while probe.upgrade().is_some() && Instant::now() < deadline {
        idle();
        let _ = cell.load();
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(50));
    }
    assert!(
        probe.upgrade().is_none(),
        "a replaced view nobody holds was never freed"
    );
}

#[derive(Debug, Clone)]
enum Op {
    /// Seal the active memtable behind a fresh one.
    Rotate,
    /// Push a fresh memtable onto the frozen list.
    Push,
    /// Retire the frozen memtable at this index modulo the list's length.
    Retire(usize),
    /// Publish a version edit.
    Version(u64),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        Just(Op::Rotate),
        Just(Op::Push),
        (0usize..8).prop_map(Op::Retire),
        (1u64..1000).prop_map(Op::Version),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    /// The view a load returns is exactly the sequential model's after
    /// every publication.
    #[test]
    fn every_load_matches_the_sequential_model(ops in proptest::collection::vec(op(), 1..60)) {
        let (_dir, store, cell) = store_with_view();
        let mut active = Arc::clone(&cell.load().active);
        let mut frozen: Vec<Arc<MemTable>> = Vec::new();
        let mut next_file_id = cell.load().version.next_file_id;
        for op in ops {
            match op {
                Op::Rotate => {
                    let fresh = memtable();
                    let sealed = cell.update_memtables(|current, list| {
                        let mut next = list.to_vec();
                        next.push(Arc::clone(current));
                        (Arc::clone(&fresh), next, Arc::clone(current))
                    });
                    prop_assert!(Arc::ptr_eq(&sealed, &active));
                    frozen.push(std::mem::replace(&mut active, fresh));
                }
                Op::Push => {
                    let mt = memtable();
                    cell.update_memtables(|current, list| {
                        let mut next = list.to_vec();
                        next.push(Arc::clone(&mt));
                        (Arc::clone(current), next, ())
                    });
                    frozen.push(mt);
                }
                Op::Retire(i) => {
                    if !frozen.is_empty() {
                        let victim = frozen.remove(i % frozen.len());
                        cell.retire_memtable(&victim);
                    }
                }
                Op::Version(id) => {
                    store
                        .run(|versions| versions.apply(&[VersionEdit::SetNextFileId(id)]))
                        .expect("apply");
                    next_file_id = id;
                }
            }
            let view = cell.load();
            prop_assert!(Arc::ptr_eq(&view.active, &active));
            prop_assert_eq!(view.frozen.len(), frozen.len());
            for (got, want) in view.frozen.iter().zip(&frozen) {
                prop_assert!(Arc::ptr_eq(got, want));
            }
            prop_assert_eq!(view.version.next_file_id, next_file_id);
        }
    }
}

/// An open records its new log in the manifest before its view exists, so
/// the view starts at that version and the open publishes none of its own:
/// each publication retires the view it replaces and pays a padded
/// reclamation batch for it (`reclaim::submit`). A reopen, which replays
/// the log, publishes none either.
#[test]
fn an_open_publishes_no_view_of_its_own() {
    let dir = tempfile::tempdir().expect("tempdir");
    for _ in 0..2 {
        let db = crate::Db::open(dir.path(), crate::Options::default()).expect("open");
        assert_eq!(db.engine.view.publications(), 0);
        let wal_id = db.engine.wal_id.load(Ordering::SeqCst);
        let view = db.engine.view.load();
        assert_eq!(view.version.min_wal_id, wal_id);
        assert_eq!(view.version.next_file_id, wal_id + 1);
        drop(view);
        db.put(b"key", b"value").expect("put");
        db.close().expect("close");
    }
}
