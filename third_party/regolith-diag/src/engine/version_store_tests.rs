//! The version store's step: sections run alone and in order, by whichever
//! thread holds the step, and a caller's borrows and panics stay its own.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;
use crate::engine::memtable::{MemTable, MemTableConfig};
use crate::engine::read_view::ReadView;
use crate::engine::sstable::MetadataPolicy;
use crate::env::{Env, MemEnv};

/// A version store whose manifest is in memory: these tests are about the
/// step, not about the manifest's files, and miri, which they run under,
/// does not sync a directory.
fn store() -> VersionStore {
    let env: Arc<dyn Env> = Arc::new(MemEnv::new());
    let db_dir = Path::new("version-store-tests");
    let sst_dir = db_dir.join("sst");
    env.create_dir_all(&sst_dir).unwrap();
    let versions =
        VersionSet::open_with_policy(&env, db_dir, &sst_dir, MetadataPolicy::Pinned, None).unwrap();
    VersionStore::new(versions)
}

fn with_view(store: &VersionStore) -> Arc<ReadViewCell> {
    let cell = Arc::new(ReadViewCell::new(ReadView {
        active: Arc::new(MemTable::new(&MemTableConfig::default()).unwrap()),
        frozen: Vec::new(),
        version: store.current(),
    }));
    store.attach_view(Arc::clone(&cell));
    cell
}

#[test]
fn a_section_returns_what_it_computed_from_its_callers_stack() {
    let store = store();
    let mut seen = Vec::new();
    let first = store.run(|versions| {
        // V4: the section borrows from this frame.
        seen.push(versions.current().next_file_id);
        seen.len()
    });
    assert_eq!(first, 1);
    assert_eq!(seen.len(), 1);
}

#[test]
fn file_ids_are_taken_once_each_and_recorded() {
    let store = store();
    let first = store.take_file_id().unwrap();
    let second = store.take_file_id().unwrap();
    assert_eq!(second, first + 1);
    assert_eq!(store.current().next_file_id, second + 1);
}

/// V1 and V3 under threads: every section runs alone, none is lost, and
/// each caller gets its own section's answer. Each section reads the id,
/// checks no other section is inside, and takes the next id; the ids handed
/// out are then all distinct and leave no gap.
#[test]
fn concurrent_sections_run_alone_and_none_is_lost() {
    const THREADS: u64 = 8;
    // Each section appends to the manifest; interpreted, a handful a
    // thread still overlaps every push with a sweep.
    #[cfg(not(miri))]
    const EACH: u64 = 50;
    #[cfg(miri)]
    const EACH: u64 = 4;
    let store = store();
    let cell = with_view(&store);
    let first = store.current().next_file_id;
    let inside = AtomicBool::new(false);
    let mut taken: Vec<u64> = thread::scope(|scope| {
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let (store, inside) = (&store, &inside);
                scope.spawn(move || {
                    (0..EACH)
                        .map(|_| {
                            store
                                .run(|versions| {
                                    assert!(
                                        !inside.swap(true, Ordering::SeqCst),
                                        "two sections ran at once"
                                    );
                                    let id = versions.current().next_file_id;
                                    versions.apply(&[VersionEdit::SetNextFileId(id + 1)]).map(
                                        |()| {
                                            inside.store(false, Ordering::SeqCst);
                                            id
                                        },
                                    )
                                })
                                .unwrap()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect()
    });
    taken.sort_unstable();
    let expected: Vec<u64> = (first..first + THREADS * EACH).collect();
    assert_eq!(taken, expected, "an id was handed out twice or skipped");
    assert_eq!(store.current().next_file_id, first + THREADS * EACH);
    assert_eq!(
        cell.load().version.next_file_id,
        first + THREADS * EACH,
        "the last section's version is the published one"
    );
}

/// The step is taken, not waited for: a section pushed while another thread
/// holds the step is run by that thread, not by its caller.
#[test]
fn a_section_pushed_while_the_step_is_held_is_run_by_its_holder() {
    let store = store();
    let (entered, is_inside) = mpsc::channel();
    let (leave, may_leave) = mpsc::channel::<()>();
    let ran_on = AtomicU64::new(0);
    let id_of = |thread: thread::ThreadId| {
        // A thread's id as a number, for the atomic.
        format!("{thread:?}")
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u64>()
            .unwrap()
    };
    thread::scope(|scope| {
        let held = &store;
        let holder = scope.spawn(move || {
            held.run(move |_| {
                entered.send(()).unwrap();
                may_leave.recv().unwrap();
            });
            id_of(thread::current().id())
        });
        is_inside
            .recv_timeout(Duration::from_secs(30))
            .expect("the holder never ran its section");
        let waiter = scope.spawn(|| {
            store.run(|_| ran_on.store(id_of(thread::current().id()), Ordering::SeqCst));
            id_of(thread::current().id())
        });
        // The waiter's section is on the list once the list is not empty;
        // the holder is let go only then, so its sweep finds the section.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !store.has_waiting() {
            assert!(
                std::time::Instant::now() < deadline,
                "the waiter never pushed"
            );
            thread::yield_now();
        }
        leave.send(()).unwrap();
        let holder = holder.join().unwrap();
        let waiter = waiter.join().unwrap();
        assert_ne!(holder, waiter);
        assert_eq!(
            ran_on.load(Ordering::SeqCst),
            holder,
            "the waiter's section waited for its own thread"
        );
    });
}

/// V5: a section's panic unwinds its caller, and the step is free
/// afterwards.
#[test]
fn a_panic_in_a_section_is_its_callers_and_frees_the_step() {
    let store = store();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.run(|_| panic!("the section panics"));
    }));
    assert!(caught.is_err(), "the panic did not reach the caller");
    assert_eq!(
        store.run(|versions| versions.current().next_file_id),
        store.current().next_file_id
    );
}

#[test]
fn a_section_that_changes_no_version_publishes_nothing() {
    let store = store();
    let cell = with_view(&store);
    let before = cell.load();
    store.run(|versions| {
        let _ = versions.current();
    });
    assert!(
        std::ptr::eq::<ReadView>(&*before, &*cell.load()),
        "a read-only section must not churn the published view",
    );
}

struct Wakes(std::sync::atomic::AtomicUsize);

impl std::task::Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn counting() -> (Arc<Wakes>, std::task::Waker) {
    let wakes = Arc::new(Wakes(std::sync::atomic::AtomicUsize::new(0)));
    (
        Arc::clone(&wakes),
        std::task::Waker::from(Arc::clone(&wakes)),
    )
}

/// ST6: a thread that keeps the step runs its own sections at once, without
/// waiting, and the step is free again when the hold ends.
#[test]
fn a_thread_that_holds_the_step_runs_its_own_sections_without_waiting() {
    let store = store();
    let hold = store
        .hold_or_park(std::task::Waker::noop())
        .expect("the step is free");
    let first = store.take_file_id().unwrap();
    let second = store.take_file_id().unwrap();
    assert_eq!(second, first + 1);
    assert!(!store.has_waiting());
    drop(hold);
    let hold = store.hold_or_park(std::task::Waker::noop());
    assert!(hold.is_some(), "the step was not given up");
}

/// ST6 and ST3: a section another thread hands over while the step is held
/// runs on the holder's thread, in the order it was handed over: before the
/// holder's own next section.
#[test]
fn a_section_handed_over_under_a_hold_runs_on_the_holder_in_order() {
    let store = store();
    let hold = store
        .hold_or_park(std::task::Waker::noop())
        .expect("the step is free");
    let holder = thread::current().id();
    thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            store.run(|versions| (versions.current().next_file_id, thread::current().id()))
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !store.has_waiting() {
            assert!(
                std::time::Instant::now() < deadline,
                "the waiter never pushed"
            );
            thread::yield_now();
        }
        // The holder's next section: the waiter's runs first, here.
        let mine = store.take_file_id().unwrap();
        let (seen, ran_on) = waiter.join().unwrap();
        assert_eq!(ran_on, holder, "the section waited for its own thread");
        assert_eq!(seen, mine, "the waiter's section ran after the holder's");
    });
    drop(hold);
}

/// ST6: a section handed over under a hold that hands nothing over itself
/// runs when the hold ends, on the holder's thread.
#[test]
fn a_section_handed_over_under_a_hold_runs_when_the_hold_ends() {
    let store = store();
    let hold = store
        .hold_or_park(std::task::Waker::noop())
        .expect("the step is free");
    let holder = thread::current().id();
    thread::scope(|scope| {
        let waiter = scope.spawn(|| store.run(|_| thread::current().id()));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !store.has_waiting() {
            assert!(
                std::time::Instant::now() < deadline,
                "the waiter never pushed"
            );
            thread::yield_now();
        }
        drop(hold);
        assert_eq!(waiter.join().unwrap(), holder);
    });
    assert!(!store.has_waiting());
}

/// ST6: a job that finds the step held by another thread parks, and is
/// woken when that thread gives the step up.
#[test]
fn a_job_that_finds_the_step_held_is_woken_when_it_is_given_up() {
    let store = store();
    let (wakes, waker) = counting();
    let (taken, is_taken) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    thread::scope(|scope| {
        let held = &store;
        let holder = scope.spawn(move || {
            let hold = held
                .hold_or_park(std::task::Waker::noop())
                .expect("the step is free");
            taken.send(()).unwrap();
            let _ = released.recv();
            drop(hold);
        });
        is_taken.recv_timeout(Duration::from_secs(30)).unwrap();
        assert!(store.hold_or_park(&waker).is_none(), "two threads held it");
        assert_eq!(wakes.0.load(Ordering::SeqCst), 0);
        drop(release);
        holder.join().unwrap();
    });
    assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
    assert!(store.hold_or_park(&waker).is_some());
}

/// A sweep that holds the step for its sections also wakes the jobs parked
/// for the step, once it gives the step up.
#[test]
fn a_sweep_that_ends_wakes_the_jobs_parked_for_the_step() {
    let store = store();
    let (wakes, waker) = counting();
    store.run(|_| {
        assert!(store.hold_or_park(&waker).is_none(), "a section is running");
        assert_eq!(wakes.0.load(Ordering::SeqCst), 0);
    });
    assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
}
