//! Flushes off the commit path (E9) and their order in L0 (`LsmOrder.tla`,
//! RED FlushAnyOrder).

use std::sync::mpsc;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use tempfile::TempDir;

use super::{DurabilityMode, EngineOptions, RegolithEngine};
use crate::WriteBatchOp;
use crate::column_family::{DEFAULT_CF_ID, prefix_key};
use crate::event_listener::{EventListener, FlushJobInfo};

fn key_of(name: &[u8]) -> Vec<u8> {
    prefix_key(DEFAULT_CF_ID, name)
}

fn put(engine: &RegolithEngine, name: &[u8], value: &[u8]) -> std::io::Result<u64> {
    engine.apply_batch(
        vec![WriteBatchOp::Put {
            key: key_of(name),
            value: value.to_vec(),
        }],
        DurabilityMode::Eventual,
        false,
    )
}

/// A buffer that two 1 KiB puts fill, so the third rotates it.
fn tiny(workers: usize) -> EngineOptions {
    EngineOptions {
        write_buffer_size: 2 * 1024,
        max_background_compactions: workers,
        ..EngineOptions::default()
    }
}

/// Write until the active memtable is sealed, returning how many puts it took.
fn write_until_sealed(engine: &RegolithEngine, prefix: &str) -> usize {
    let frozen_before = engine.view.load().frozen.len();
    let active_before = Arc::clone(&engine.view.load().active);
    for i in 0..64 {
        put(engine, format!("{prefix}{i:03}").as_bytes(), &[7u8; 1024]).unwrap();
        let view = engine.view.load();
        if !Arc::ptr_eq(&view.active, &active_before) {
            assert!(view.frozen.len() <= frozen_before + 1);
            return i + 1;
        }
    }
    panic!("64 puts of 1 KiB never rotated a 2 KiB buffer");
}

/// A rotating write seals the memtable and returns without flushing it: with
/// the flush exclusion held by this test, a write that flushed under the
/// commit mutex would wait here instead of returning.
#[test]
fn a_rotating_write_returns_without_flushing() {
    let dir = TempDir::new().unwrap();
    let engine = RegolithEngine::open(dir.path(), tiny(1)).unwrap();
    let flushing = engine.flusher.flushing().unwrap();
    let (done, returned) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            done.send(write_until_sealed(&engine, "k")).unwrap();
        });
        returned
            .recv_timeout(Duration::from_secs(30))
            .expect("the rotating write waited for a flush");
        assert_eq!(
            engine.view.load().frozen.len(),
            1,
            "the sealed memtable waits for the background"
        );
        drop(flushing);
    });
}

/// The worker writes out what writers seal: the frozen list drains with no
/// call that flushes.
#[test]
fn the_worker_flushes_what_a_rotation_sealed() {
    let dir = TempDir::new().unwrap();
    let engine = RegolithEngine::open(dir.path(), tiny(1)).unwrap();
    write_until_sealed(&engine, "k");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut pause = Duration::from_millis(1);
    while !engine.view.load().frozen.is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the worker never flushed the sealed memtable"
        );
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(50));
    }
    assert!(!engine.view.load().version.levels[0].is_empty());
    assert_eq!(
        engine.get(&key_of(b"k000"), u64::MAX).unwrap(),
        Some(vec![7u8; 1024])
    );
}

/// Checks, when it is told of a flush, whether the commit pipeline is free.
#[derive(Default)]
struct PipelineFreeDuringFlush {
    engine: OnceLock<Weak<RegolithEngine>>,
    free: std::sync::Mutex<Vec<bool>>,
}

impl EventListener for PipelineFreeDuringFlush {
    fn on_flush_completed(&self, _: &FlushJobInfo) {
        if let Some(engine) = self.engine.get().and_then(Weak::upgrade) {
            let free = engine.pipeline_is_idle();
            self.free.lock().unwrap().push(free);
        }
    }
}

/// Without a worker the write that sealed the memtable flushes it, once its
/// own commit has left the pipeline: the flush is done when the write
/// returns, and the pipeline was free while it ran.
#[test]
fn without_a_worker_the_sealing_write_flushes_after_it_commits() {
    let dir = TempDir::new().unwrap();
    let listener = Arc::new(PipelineFreeDuringFlush::default());
    let engine = RegolithEngine::open(
        dir.path(),
        EngineOptions {
            listeners: vec![listener.clone()],
            ..tiny(0)
        },
    )
    .unwrap();
    let _ = listener.engine.set(Arc::downgrade(&engine));
    for round in 0..3 {
        write_until_sealed(&engine, &format!("r{round}-"));
        assert!(
            engine.view.load().frozen.is_empty(),
            "the sealing write returned with its memtable still frozen"
        );
    }
    let free = listener.free.lock().unwrap().clone();
    assert!(free.len() >= 3, "{free:?}");
    assert!(
        free.iter().all(|&free| free),
        "a flush ran while the commit pipeline was held: {free:?}"
    );
}

/// RED FlushAnyOrder: frozen memtables install in L0 in the order they were
/// sealed, whoever flushes them and however many flush at once, so a read
/// never goes back from a newer version to an older one.
#[test]
fn frozen_memtables_install_in_l0_oldest_first_whoever_flushes() {
    let dir = TempDir::new().unwrap();
    // No cap, so the memtables stay frozen while this test holds the flush
    // exclusion, and the worker waits on it.
    let engine = RegolithEngine::open(
        dir.path(),
        EngineOptions {
            max_write_buffer_number: 0,
            ..tiny(1)
        },
    )
    .unwrap();
    const ROUNDS: u8 = 6;
    {
        let _held = engine.flusher.flushing().unwrap();
        // Each round seals a memtable holding `k = round`.
        for round in 0..ROUNDS {
            put(&engine, b"k", &[round]).unwrap();
            write_until_sealed(&engine, &format!("fill{round}-"));
        }
        assert_eq!(engine.view.load().frozen.len(), ROUNDS as usize);
    }

    // The worker and three more threads flush at once, and a reader checks
    // that every answer is at least the last one it saw.
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let mut seen = 0u8;
            loop {
                let draining = !engine.view.load().frozen.is_empty();
                let value = engine.get(&key_of(b"k"), u64::MAX).unwrap().unwrap();
                assert!(
                    value[0] >= seen,
                    "a read went back from {seen} to {}",
                    value[0]
                );
                seen = value[0];
                if !draining {
                    break;
                }
            }
        });
        for _ in 0..3 {
            scope.spawn(|| engine.flusher.flush_all_frozen());
        }
        reader.join().unwrap();
    });
    assert!(engine.view.load().frozen.is_empty());
    assert_eq!(
        engine.get(&key_of(b"k"), u64::MAX).unwrap(),
        Some(vec![ROUNDS - 1]),
        "the newest version answers once every memtable is in L0"
    );
}

/// A rotation leaves at most `max_write_buffer_number` memtables: when the
/// flush before it has not finished, the write that fills the next memtable
/// writes the oldest out itself, and waits for the flush exclusion to do so.
#[test]
fn a_rotation_never_leaves_more_memtables_than_the_cap() {
    let dir = TempDir::new().unwrap();
    // The default cap of two: the active memtable and one frozen.
    let engine = RegolithEngine::open(dir.path(), tiny(1)).unwrap();
    let held = engine.flusher.flushing().unwrap();
    write_until_sealed(&engine, "first");
    assert_eq!(engine.view.load().frozen.len(), 1);
    let (done, returned) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            done.send(write_until_sealed(&engine, "second")).unwrap();
        });
        // The worker cannot flush while the exclusion is held, so the
        // second rotation has to wait to write the first memtable out.
        assert!(
            returned.recv_timeout(Duration::from_millis(200)).is_err(),
            "a second rotation sealed past the cap while the first flush was pending"
        );
        assert!(engine.view.load().frozen.len() <= 1);
        drop(held);
        returned
            .recv_timeout(Duration::from_secs(30))
            .expect("the rotation finished once the exclusion was free");
    });
    assert!(engine.view.load().frozen.len() <= 1);
    assert_eq!(
        engine.get(&key_of(b"first000"), u64::MAX).unwrap(),
        Some(vec![7u8; 1024])
    );
}

/// E16: with no worker, the write after the flush that brings L0 to
/// `l0_compaction_trigger` compacts it, so L0 stays at the trigger instead of
/// growing to the slowdown trigger before anything compacts.
#[test]
fn without_a_worker_writes_compact_once_l0_reaches_its_trigger() {
    let dir = TempDir::new().unwrap();
    let engine = RegolithEngine::open(
        dir.path(),
        EngineOptions {
            l0_compaction_trigger: 2,
            level0_slowdown_writes_trigger: 20,
            level0_stop_writes_trigger: 36,
            ..tiny(0)
        },
    )
    .unwrap();
    let mut peak_l0 = 0;
    for round in 0..12 {
        write_until_sealed(&engine, &format!("r{round:02}-"));
        peak_l0 = peak_l0.max(engine.view.load().version.levels[0].len());
    }
    assert!(
        peak_l0 <= 2,
        "L0 reached {peak_l0} tables with a trigger of 2: nothing compacted"
    );
    assert!(
        !engine.view.load().version.levels[1].is_empty(),
        "nothing was compacted into L1"
    );
    for round in 0..12 {
        assert_eq!(
            engine
                .get(&key_of(format!("r{round:02}-000").as_bytes()), u64::MAX)
                .unwrap(),
            Some(vec![7u8; 1024])
        );
    }
}

/// The worker waits on the flush exclusion while a writer or a checkpoint
/// flushes, a whole table write. The wait releases the views its last read
/// holds back before it parks, as every blocking wait of the engine does,
/// so a view retired meanwhile is freed during the wait rather than after
/// it.
#[test]
fn a_worker_waiting_on_the_flush_exclusion_holds_back_no_retired_view() {
    let dir = TempDir::new().unwrap();
    let engine = RegolithEngine::open(dir.path(), tiny(0)).unwrap();
    // A memtable only the published view names, so it is dropped exactly
    // when the view that names it is freed.
    let probe = {
        let doomed = Arc::new(super::MemTable::new(&engine.memtable_config).unwrap());
        let probe = Arc::downgrade(&doomed);
        engine
            .view
            .update_memtables(|_, frozen| (Arc::clone(&doomed), frozen.to_vec(), ()));
        probe
    };
    let held = engine.flusher.flushing().unwrap();
    let (waiting_tx, waiting) = mpsc::channel();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            // The worker's last read: a load after the probe's view was
            // published, released at once.
            assert!(engine.view.load().active.is_empty());
            waiting_tx.send(()).unwrap();
            engine.flusher.flush_all_frozen();
        });
        waiting.recv().unwrap();
        // Retire the probe's view while the worker waits for the exclusion.
        let fresh = Arc::new(super::MemTable::new(&engine.memtable_config).unwrap());
        engine
            .view
            .update_memtables(|_, frozen| (Arc::clone(&fresh), frozen.to_vec(), ()));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut backoff = Duration::from_millis(1);
        while probe.upgrade().is_some() && std::time::Instant::now() < deadline {
            assert!(!worker.is_finished(), "the worker passed a held exclusion");
            super::read_view::idle();
            drop(engine.view.load());
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_millis(50));
        }
        let freed = probe.upgrade().is_none();
        drop(held);
        worker.join().unwrap();
        assert!(
            freed,
            "a view retired while the worker waited on the flush exclusion stayed alive"
        );
    });
}

/// Where a worker's round waits for the compaction gate.
#[derive(Clone, Copy, Debug)]
enum GateWait {
    /// Its pass waits for a job that holds the gate alone.
    Pass,
    /// A job it runs waits for a pass that holds the gate shared.
    Job,
}

/// A worker's round waits for the gate in two places: its pass, behind a
/// job that holds the gate alone, and a job it runs, behind passes. Each
/// wait releases the views the thread's last read holds back before it
/// parks, so a view retired while it waits is freed during the wait rather
/// than after it. The job reads engine state of its own after the round
/// began, so a release at the top of the round would not be enough.
fn a_worker_waiting_for_the_gate_frees_a_retired_view(wait: GateWait) {
    let dir = TempDir::new().unwrap();
    let engine = RegolithEngine::open(dir.path(), tiny(1)).unwrap();
    // The worker sits in a job of the test's until the gate is taken, so
    // what it does next is known: the rest of this round.
    let (in_job, when_in_job) = mpsc::channel();
    let (go, gone) = mpsc::channel::<()>();
    engine.compaction_scheduler().run_on_worker(move || {
        in_job.send(()).unwrap();
        let _ = gone.recv();
    });
    when_in_job
        .recv_timeout(Duration::from_secs(60))
        .expect("the worker never ran the job");

    let gate = engine.compaction_gate();
    let (alone, shared, ticket) = match wait {
        GateWait::Pass => (Some(gate.exclusive().unwrap()), None, None),
        GateWait::Job => {
            let shared = gate.try_shared().expect("no pass runs and no job waits");
            // Queued behind the test's job: the next round runs it, and it
            // waits for the gate alone.
            let ticket = engine.start_foreground(super::foreground::Work::CompactRange {
                lower: key_of(b"y0"),
                upper: key_of(b"y1"),
            });
            (None, Some(shared), Some(ticket))
        }
    };
    // A memtable only the published view names, so it is dropped exactly
    // when the view that names it is freed.
    let probe = {
        let doomed = Arc::new(super::MemTable::new(&engine.memtable_config).unwrap());
        let probe = Arc::downgrade(&doomed);
        engine
            .view
            .update_memtables(|_, frozen| (Arc::clone(&doomed), frozen.to_vec(), ()));
        probe
    };
    // Every load the worker makes before it waits is followed by a
    // re-classification of the stall: one after its flush step, and, when
    // its pass is let through, one more at the end of the round.
    let loads = match wait {
        GateWait::Pass => 1,
        GateWait::Job => 2,
    };
    let before = engine.stall_signal().progress();
    go.send(()).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut backoff = Duration::from_millis(1);
    while engine.stall_signal().progress().wrapping_sub(before) < loads {
        assert!(
            std::time::Instant::now() < deadline,
            "{wait:?}: the worker's round never reached the gate"
        );
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(50));
    }

    // Retire the probe's view while the worker waits for the gate.
    let fresh = Arc::new(super::MemTable::new(&engine.memtable_config).unwrap());
    engine
        .view
        .update_memtables(|_, frozen| (Arc::clone(&fresh), frozen.to_vec(), ()));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut backoff = Duration::from_millis(1);
    while probe.upgrade().is_some() && std::time::Instant::now() < deadline {
        super::read_view::idle();
        drop(engine.view.load());
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(50));
    }
    let freed = probe.upgrade().is_none();
    if let Some(ticket) = &ticket {
        assert!(
            !ticket.is_ready(),
            "setup: the job ran while a pass held the gate"
        );
    }
    drop((alone, shared));
    if let Some(ticket) = ticket {
        ticket.wait().unwrap();
    }
    assert!(
        freed,
        "{wait:?}: a view retired while the worker waited for the gate stayed alive"
    );
}

#[test]
fn a_worker_whose_pass_waits_for_the_gate_holds_back_no_retired_view() {
    a_worker_waiting_for_the_gate_frees_a_retired_view(GateWait::Pass);
}

#[test]
fn a_worker_whose_job_waits_for_the_gate_holds_back_no_retired_view() {
    a_worker_waiting_for_the_gate_frees_a_retired_view(GateWait::Job);
}

struct Wakes(std::sync::atomic::AtomicUsize);

impl std::task::Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The exclusion a queue's poll took for a unit's body is found held by
/// that thread's own calls, which go on, and by no other thread.
#[test]
fn the_exclusion_held_for_a_units_body_is_held_for_its_thread_alone() {
    let dir = TempDir::new().unwrap();
    let engine = RegolithEngine::open(dir.path(), tiny(0)).unwrap();
    let hold = engine
        .flusher
        .hold_or_park(std::task::Waker::noop())
        .expect("no flush is in flight");
    // The body's own calls: neither waits, and neither releases it.
    drop(engine.flusher.flushing().expect("held by this thread"));
    drop(engine.flusher.try_flushing().expect("held by this thread"));
    std::thread::scope(|scope| {
        let other = scope.spawn(|| engine.flusher.try_flushing().is_some());
        assert!(!other.join().unwrap(), "another thread flushed under it");
    });
    drop(hold);
    assert!(engine.flusher.try_flushing().is_some(), "released");
}

/// A job that finds a flush in flight parks, and the release of the
/// exclusion wakes the jobs parked for it.
#[test]
fn a_job_that_finds_a_flush_in_flight_is_woken_when_it_ends() {
    let dir = TempDir::new().unwrap();
    let engine = RegolithEngine::open(dir.path(), tiny(0)).unwrap();
    let wakes = Arc::new(Wakes(std::sync::atomic::AtomicUsize::new(0)));
    let waker = std::task::Waker::from(Arc::clone(&wakes));
    let count = || wakes.0.load(std::sync::atomic::Ordering::SeqCst);
    let (taken, is_taken) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let flusher = &engine.flusher;
        let flusher = scope.spawn(move || {
            let flushing = flusher.flushing().unwrap();
            taken.send(()).unwrap();
            let _ = released.recv();
            drop(flushing);
        });
        is_taken.recv_timeout(Duration::from_secs(30)).unwrap();
        assert!(engine.flusher.hold_or_park(&waker).is_none());
        assert_eq!(count(), 0);
        drop(release);
        flusher.join().unwrap();
    });
    assert_eq!(count(), 1);
    assert!(engine.flusher.hold_or_park(&waker).is_some());
}

/// A write that owes a step on a database no queue was ever opened on runs
/// the step inline without making the queue table, and so do a rotation's
/// unit and the close's: the table would be built for nothing and freed
/// again with the database, on every open and close of a database used
/// without queues.
#[test]
fn a_database_used_without_queues_makes_no_queue_table() {
    let dir = TempDir::new().expect("tempdir");
    let engine = RegolithEngine::open(dir.path(), tiny(0)).expect("open");
    assert_ne!(engine.owed.load(std::sync::atomic::Ordering::Acquire), 0);
    put(&engine, b"key", b"value").expect("put");
    assert_eq!(
        engine.owed.load(std::sync::atomic::Ordering::Acquire),
        0,
        "the step ran"
    );
    assert!(engine.cache.opened_io().is_none());
    write_until_sealed(&engine, "fill");
    assert!(engine.cache.opened_io().is_none());
    engine.close().expect("close");
    assert!(engine.cache.opened_io().is_none());
}
