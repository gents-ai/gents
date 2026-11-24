//! Bounded helping on the code (E21; `helping_is_bounded` in
//! `PipelineHelping.lean`, `WorksUpToItsOwn` in `CommitPipeline.tla`): a
//! commit decides, writes and publishes the slots up to its own, each once,
//! and no slot past it, whatever stands behind it.
//!
//! Threads are stopped through the pipeline's test hooks, released through
//! channels, and watched with a deadline; nothing is timed.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::hook::{self, Stage};
use super::pipeline::Pipeline;
use super::pipeline_tests::{eventual_put, key};
use super::ring::Loaded;
use super::*;
use crate::engine::EngineOptions;

const DEADLINE: Duration = Duration::from_secs(30);

/// Commits stopped before they decide anything, slots taken past the
/// commit, and the commit itself: what it did is counted.
#[test]
fn a_commit_decides_each_slot_up_to_its_own_once_and_none_past_it() {
    const AHEAD: usize = 4;
    const PAST: usize = 3;
    let dir = TempDir::new().unwrap();
    // A worker: with none, a write owes a background step once it has
    // committed, and that step's unit takes a slot of its own after the
    // commit's, which would decide the slots taken past it.
    let engine = RegolithEngine::open(dir.path(), EngineOptions::default()).unwrap();
    let pipe = engine.pipe().unwrap();
    let start = pipe.ring.published();

    // Plain threads, not scoped ones: a commit that never returns fails the
    // test at its deadline rather than holding a scope's join for ever.
    let (stopped, all_stopped) = mpsc::channel();
    let mut releases = Vec::new();
    let mut ahead = Vec::new();
    for i in 0..AHEAD {
        let (release, released) = mpsc::channel::<()>();
        releases.push(release);
        let (engine, stopped) = (Arc::clone(&engine), stopped.clone());
        ahead.push(thread::spawn(move || {
            // Each stops at the first slot it is about to decide: the
            // frontier's, which none of them gets past.
            let mut fired = false;
            hook::set(move |stage| {
                if stage == Stage::Decide && !fired {
                    fired = true;
                    stopped.send(()).unwrap();
                    let _ = released.recv_timeout(DEADLINE);
                }
            });
            let out = engine.submit(eventual_put(format!("ahead{i}").as_bytes(), b"v"));
            hook::clear();
            out
        }));
    }
    for _ in 0..AHEAD {
        all_stopped
            .recv_timeout(DEADLINE)
            .expect("a commit ahead never reached its decision");
    }

    // The commit: once it has its slot, slots are taken past it and never
    // installed, as by writers that stopped right after their reservation.
    // It counts the decisions it makes.
    let decisions = Arc::new(AtomicUsize::new(0));
    let past = Arc::new(Mutex::new(Vec::new()));
    let (done, finished) = mpsc::channel();
    let committer = {
        let (engine, counted, taken_past) = (
            Arc::clone(&engine),
            Arc::clone(&decisions),
            Arc::clone(&past),
        );
        thread::spawn(move || {
            let reserving = Arc::clone(&engine);
            let mut reserved = false;
            hook::set(move |stage| match stage {
                Stage::Reserve if !reserved => {
                    reserved = true;
                    let ring = &reserving.pipe().unwrap().ring;
                    taken_past
                        .lock()
                        .unwrap()
                        .extend((0..PAST).map(|_| ring.reserve()));
                }
                Stage::Decide => {
                    counted.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            });
            let out = engine.submit(eventual_put(b"mine", b"v"));
            hook::clear();
            done.send(out).unwrap();
        })
    };
    wait_for_commit(&finished, pipe, &past)
        .expect("the commit goes through with the threads ahead of it stopped");
    committer.join().unwrap();

    // Its own slot is the one before the first taken past it. (Another
    // thread's unit, a background job's, may have taken a slot among the
    // ones ahead: the count of decisions below does not include it.)
    let own = past.lock().unwrap()[0] - 1;
    assert!(own > start + AHEAD as u64);
    assert_eq!(
        decisions.load(Ordering::SeqCst),
        AHEAD + 1,
        "each slot up to its own decided once, its own included"
    );
    assert_eq!(pipe.ring.decided(), own, "nothing past its own decided");
    assert_eq!(pipe.ring.published(), own, "nothing past its own published");
    for &slot in past.lock().unwrap().iter() {
        assert!(slot > own);
        assert!(
            matches!(pipe.ring.load(slot), Loaded::Empty),
            "slot {slot}, past its own, was touched"
        );
    }

    for release in releases {
        release.send(()).unwrap();
    }
    for done in ahead {
        done.join()
            .unwrap()
            .expect("a commit ahead was brought through by the one behind it");
    }
    // The slots taken past it and never installed hold nobody up: the next
    // commit voids them.
    engine.submit(eventual_put(b"after", b"v")).unwrap();
    assert_eq!(
        engine.get(&key(b"after"), engine.snapshot_seq()).unwrap(),
        Some(b"v".to_vec())
    );
    assert!(engine.pipeline_is_idle());
}

/// What the commit's thread sent, waited for with a deadline and a bounded
/// backoff. A commit still out at the deadline fails the test, naming its
/// slot and the first slot taken past it.
fn wait_for_commit<T>(finished: &mpsc::Receiver<T>, pipe: &Pipeline, past: &Mutex<Vec<u64>>) -> T {
    let deadline = Instant::now() + DEADLINE;
    let mut pause = Duration::from_micros(50);
    loop {
        match finished.try_recv() {
            Ok(out) => return out,
            Err(TryRecvError::Disconnected) => {
                panic!("the commit's thread ended without its result")
            }
            Err(TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            let Some(&first_past) = past.lock().unwrap().first() else {
                panic!("the commit took no slot within {DEADLINE:?}");
            };
            panic!(
                "the commit of slot {} did not return within {DEADLINE:?}: decided through {}, \
                 published through {}, and slot {first_past}, past its own, is never installed",
                first_past - 1,
                pipe.ring.decided(),
                pipe.ring.published(),
            );
        }
        thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(20));
    }
}
