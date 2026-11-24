//! The write stall as a unit writers wait on, completed on each writer's own
//! queue when the stall clears (D23, plan 4.10).
//!
//! A write stopped by a stall threshold applies nothing and returns
//! [`WouldBlock::Stall`](crate::WouldBlock::Stall) at once, with a
//! [`StallWait`](crate::StallWait). Every writer stopped during one stall
//! waits on the same *passive* job (`engine::io::job`): no poll runs it, and
//! it lands only when the stall clears.
//!
//! - **Who clears it.** Whoever makes the progress that clears the stall
//!   re-classifies it here ([`StallSignal::refresh`]): a flush on a worker or
//!   on a writer's queue, a compaction pass, a rotation, an ingest. When
//!   writes are no longer stopped it lands the job, which pushes one
//!   completion to each stopped writer's queue and completes each wait with
//!   no queue directly.
//! - **No lost wakeup.** A writer gets or installs the job, registers its
//!   wait on it, and only then classifies the stall again: if the stall
//!   cleared in between, the writer lands the job itself. A clearer stores
//!   the level before it takes the job. So either the clearer finds the
//!   writer's job, or the writer sees the cleared stall.
//! - **Relief handed back.** On a database with no worker and inline
//!   compaction on, stopped writers relieve the stop themselves, and one
//!   whose step finds another thread's flush or job in the way waits for
//!   that thread instead. That thread may stop relieving with the stop
//!   still on: a write that ran its one owed step and returned, a job that
//!   ended, a relief that gave up. So there every piece of progress lands
//!   the job, stop or no stop, and each waiter looks again and relieves the
//!   stop itself or is contended anew. A waiter that saw the contention
//!   before such a landing and registered after it would miss it: progress
//!   is counted before the level is stored, the writer reads the count
//!   before its step and again once registered, and lands its own job when
//!   the count moved (`StallRelief.tla`, RED NoRecheck).
//! - **No thread waits.** Nothing here parks or sleeps; the slowdown delay
//!   and the timed wait a stopped writer used to park on are gone.

use std::sync::Arc;

use kovan::Atom;

use super::super::io::IoRuntime;
use super::super::io::job::{Delivery, Job};
use super::super::io::shared::{QueueShared, WaitSlot};
use super::super::read_view::ReadViewCell;
use super::super::{EngineOptions, stall_state};
use crate::StallWait;
use crate::portability::{AtomicU8, AtomicUsize, Ordering};

/// The stall writers are stopped on now, if any, and the level writers cache.
pub(crate) struct StallSignal {
    view: Arc<ReadViewCell>,
    options: EngineOptions,
    /// The level writers check on every write: 0 none, 1 slowdown, 2 stop.
    level: Arc<AtomicU8>,
    /// The unit writers stopped since the stall began wait on.
    current: Atom<Option<Arc<Job>>>,
    /// Stopped writers relieve the stop themselves: no worker, inline
    /// compaction on.
    writers_relieve: bool,
    /// The caller relieves a stop: no worker, inline compaction turned off.
    callers_relieve: bool,
    /// Pieces of progress so far, counted before each re-classification.
    progress: AtomicUsize,
}

impl StallSignal {
    pub(crate) fn new(
        view: Arc<ReadViewCell>,
        options: EngineOptions,
        level: Arc<AtomicU8>,
    ) -> Self {
        Self {
            writers_relieve: options.max_background_compactions == 0 && options.inline_compaction,
            callers_relieve: options.max_background_compactions == 0 && !options.inline_compaction,
            view,
            options,
            level,
            current: Atom::new(None),
            progress: AtomicUsize::new(0),
        }
    }

    /// A signal over an empty view of `versions`, for a test that starts
    /// workers on their own.
    #[cfg(test)]
    pub(crate) fn for_tests(versions: &super::super::version_store::VersionStore) -> Self {
        use super::super::memtable::{MemTable, MemTableConfig};
        use super::super::read_view::ReadView;
        let view = Arc::new(ReadViewCell::new(ReadView {
            active: Arc::new(MemTable::new(&MemTableConfig::default()).expect("memtable")),
            frozen: Vec::new(),
            version: versions.current(),
        }));
        Self::new(view, EngineOptions::default(), Arc::new(AtomicU8::new(0)))
    }

    /// The thresholds against the current view: `None`, or the reason and
    /// whether writes are stopped (`true`) or slowed (`false`).
    pub(crate) fn classify(&self) -> Option<(&'static str, bool)> {
        stall_state::classify(&self.view.load(), &self.options)
    }

    /// Re-classify after work that can change the thresholds' inputs (a
    /// rotation, a flush, a compaction pass, an ingest), store the level
    /// writers cache, and land the unit stopped writers wait on once writes
    /// are no longer stopped, or, where stopped writers relieve the stop
    /// themselves, whatever the stop (module docs). Called once the work
    /// released the flush or compaction exclusion it held.
    pub(crate) fn refresh(&self) {
        // Counted before the level is stored and the unit taken: a writer
        // contended before this and registered after the take sees the count
        // moved (module docs).
        self.progress.fetch_add(1, Ordering::SeqCst);
        let level = match self.classify() {
            None => 0,
            Some((_, false)) => 1,
            Some((_, true)) => 2,
        };
        // Stored before the unit is taken: a writer that registered after the
        // take sees this level when it classifies again (module docs).
        self.level.store(level, Ordering::Release);
        if level < 2 || self.writers_relieve {
            self.land();
        }
    }

    /// The pieces of progress so far, read by a writer before a relief step
    /// that may find another thread's work in the way.
    pub(crate) fn progress(&self) -> usize {
        self.progress.load(Ordering::SeqCst)
    }

    /// Land the unit stopped writers wait on now, whatever the stall: close,
    /// and a writer that found the stall cleared after it registered.
    pub(crate) fn land(&self) {
        if let Some(job) = self.current.swap(None).as_ref() {
            job.release();
        }
    }

    /// A wait for the stall `reason` to clear, for a write that stopped on
    /// it. Recorded on `queue` when the writer's thread has one, so it is
    /// completed at that queue's poll; else completed directly by whoever
    /// clears the stall. `seen` is the [`Self::progress`] a writer whose
    /// relief step was contended read before the step: progress since then
    /// completes the wait at once, so the writer looks again.
    pub(crate) fn wait(
        &self,
        io: &IoRuntime,
        queue: Option<Arc<QueueShared>>,
        reason: &'static str,
        seen: Option<usize>,
    ) -> StallWait {
        let job = self.unit();
        let slot = Arc::new(WaitSlot::new());
        let waiter = Arc::clone(&slot) as Arc<dyn Delivery>;
        let id = queue.as_ref().map(|queue| queue.id());
        let recorded = match &queue {
            // The queue's owner registers on the unit at its next poll; a
            // unit that landed by then is told at once (`IoQueue::accept_job`).
            Some(queue) => io.wait_on(queue, Arc::clone(&job), Some(Arc::clone(&waiter))),
            None => false,
        };
        // No queue, or one dropped meanwhile: the lander tells the wait
        // itself; refused when it landed already.
        if !recorded && !job.listen(waiter) {
            slot.complete();
        }
        // Classified again only now that the wait is recorded, so a clearer
        // that came before the registration is not missed; and the progress
        // read again, so a landing between the contended step and the
        // registration is not missed either.
        if !self.look_again() && seen.is_some_and(|seen| seen != self.progress()) {
            self.land();
        }
        StallWait::new(slot, job, id, reason).left_to_the_caller(self.callers_relieve)
    }

    /// Classify the stall again, for a writer that has just got or
    /// installed the unit: when writes are no longer stopped the unit is
    /// landed here (`true`), since the clearer may have looked before the
    /// unit was there (module docs, "No lost wakeup").
    pub(crate) fn look_again(&self) -> bool {
        let cleared = self.classify().is_none_or(|(_, stopped)| !stopped);
        if cleared {
            self.refresh();
        }
        cleared
    }

    /// The unit for the stall now, installed by the first writer to stop.
    /// Whoever takes it looks at the stall again afterwards
    /// ([`Self::look_again`]).
    pub(crate) fn unit(&self) -> Arc<Job> {
        loop {
            let current = self.current.load();
            if let Some(job) = current.as_ref()
                && !job.is_done()
            {
                return Arc::clone(job);
            }
            let fresh = Job::passive();
            if self
                .current
                .compare_and_swap(&current, Some(Arc::clone(&fresh)))
                .is_ok()
            {
                return fresh;
            }
        }
    }

    /// Test-only: whether a thread sleeps on the stall's unit, as a commit
    /// that waits the stall out does.
    #[cfg(test)]
    pub(crate) fn has_sleeper(&self) -> bool {
        self.current
            .load()
            .as_ref()
            .is_some_and(|job| job.has_sleeper())
    }

    /// Test-only: whether a unit is installed and has not landed, as it is
    /// from the first write a stall stops until the landing.
    #[cfg(test)]
    pub(crate) fn has_unit(&self) -> bool {
        self.current
            .load()
            .as_ref()
            .is_some_and(|job| !job.is_done())
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;

    use super::*;
    use crate::column_family::{DEFAULT_CF_ID, prefix_key};
    use crate::engine::compaction::CompactionOutcome;
    use crate::engine::{DurabilityMode, RegolithEngine};
    use crate::portability::AtomicBool;
    use crate::{Error, IoBudget, PrefixExtractor, WouldBlock, WriteBatchOp};

    fn put(key: &[u8]) -> Vec<WriteBatchOp> {
        vec![WriteBatchOp::Put {
            key: prefix_key(DEFAULT_CF_ID, key),
            value: b"v".to_vec(),
        }]
    }

    /// No worker, one L0 table at the stop.
    pub(super) fn stopped_engine_with(
        options: EngineOptions,
    ) -> (tempfile::TempDir, Arc<RegolithEngine>) {
        let dir = tempfile::TempDir::new().unwrap();
        let engine = RegolithEngine::open(
            dir.path(),
            EngineOptions {
                max_background_compactions: 0,
                level0_stop_writes_trigger: 1,
                ..options
            },
        )
        .unwrap();
        engine
            .apply_batch(put(b"k"), DurabilityMode::Eventual, false)
            .unwrap();
        engine.flush_active_memtable().unwrap();
        (dir, engine)
    }

    /// Stopped writers wait for a compaction someone else runs.
    fn stopped_engine() -> (tempfile::TempDir, Arc<RegolithEngine>) {
        stopped_engine_with(EngineOptions {
            l0_compaction_trigger: 1,
            ..EngineOptions::default()
        })
    }

    /// Stopped writers relieve the stop themselves, and no step can: level 0
    /// is below its compaction trigger.
    fn stopped_engine_where_writers_relieve() -> (tempfile::TempDir, Arc<RegolithEngine>) {
        stopped_engine_with(EngineOptions {
            inline_compaction: true,
            l0_compaction_trigger: 100,
            ..EngineOptions::default()
        })
    }

    #[test]
    fn a_wait_with_no_queue_is_completed_by_whoever_clears_the_stall() {
        let (_dir, engine) = stopped_engine();
        let signal = engine.stall_signal();
        assert!(matches!(signal.classify(), Some((_, true))));
        let wait = signal.wait(engine.io(), None, "stop", None);
        assert!(!wait.is_ready());
        let again = signal.wait(engine.io(), None, "stop", None);
        assert!(!again.is_ready());
        engine.run_one_compaction_pass().unwrap();
        assert!(
            wait.is_ready(),
            "the pass cleared the stall and landed the unit"
        );
        assert!(again.is_ready(), "one unit for every writer of the stall");
    }

    #[test]
    fn a_wait_on_a_queue_is_completed_only_at_that_queues_poll() {
        let (_dir, engine) = stopped_engine();
        let signal = engine.stall_signal();
        let mut queue = engine.io_queue();
        let shared = engine.io().queue(queue.id());
        let wait = signal.wait(engine.io(), shared, "stop", None);
        queue.poll(IoBudget::ALL);
        assert!(!wait.is_ready());
        engine.run_one_compaction_pass().unwrap();
        assert!(!wait.is_ready(), "cleared, but not delivered yet");
        assert_eq!(queue.poll(IoBudget::ALL).completed, 1);
        assert!(wait.is_ready());
    }

    #[test]
    fn a_wait_for_a_stall_that_already_cleared_is_ready_at_once() {
        let (_dir, engine) = stopped_engine();
        engine.run_one_compaction_pass().unwrap();
        let wait = engine.stall_signal().wait(engine.io(), None, "stop", None);
        assert!(wait.is_ready());
    }

    #[test]
    fn a_step_that_leaves_the_stop_hands_it_back_to_the_writer_it_held_off() {
        let (_dir, engine) = stopped_engine_where_writers_relieve();
        // A job holds the compaction gate: the stopped write's own step is
        // contended, and the write waits for the job's thread.
        let gate = engine.compaction_gate().exclusive().unwrap();
        let Err(Error::WouldBlock(WouldBlock::Stall(wait))) = engine.admit_write(false) else {
            panic!("a contended relief returns a wait");
        };
        assert!(!wait.is_ready());
        drop(gate);
        // That thread runs one step, which leaves the stop on, and relieves
        // no further: an owed step after its commit, or a job that ended.
        assert!(matches!(
            engine
                .run_one_background_step(super::super::Help::Wait)
                .unwrap(),
            Some(CompactionOutcome::Idle)
        ));
        assert!(matches!(engine.stall_signal().classify(), Some((_, true))));
        assert!(wait.is_ready(), "handed back: the writer looks again");
        // Looking again, the writer finds no step that relieves the stop and
        // says so, instead of waiting for ever.
        assert!(matches!(engine.admit_write(false), Err(Error::Busy(_))));
    }

    #[test]
    fn progress_between_a_contended_step_and_its_wait_completes_the_wait() {
        let (_dir, engine) = stopped_engine();
        let signal = engine.stall_signal();
        let early = signal.wait(engine.io(), None, "stop", None);
        // The writer read the progress, and its step was contended.
        let seen = signal.progress();
        // The holder's work ends with the stop on; with a worker relieving,
        // nothing lands.
        signal.refresh();
        assert!(!early.is_ready());
        assert!(
            !signal
                .wait(engine.io(), None, "stop", Some(signal.progress()))
                .is_ready()
        );
        // The writer registers only now: it sees the progress it missed.
        assert!(
            signal
                .wait(engine.io(), None, "stop", Some(seen))
                .is_ready()
        );
    }

    #[test]
    fn a_stall_step_dropped_unrun_hands_the_stop_back() {
        let (_dir, engine) = stopped_engine_where_writers_relieve();
        // Writer A's stop left the relieving step on A's queue.
        let queue_a = engine.io_queue();
        let shared_a = engine.io().queue(queue_a.id()).unwrap();
        engine.leave_stall_step(&shared_a);
        // Writer B found it there and waits.
        let mut queue_b = engine.io_queue();
        let shared_b = engine.io().queue(queue_b.id());
        let wait = engine
            .stall_signal()
            .wait(engine.io(), shared_b, "stop", None);
        queue_b.poll(IoBudget::ALL);
        assert!(!wait.is_ready());
        // A's queue goes away before A polled it.
        drop(queue_a);
        queue_b.poll(IoBudget::ALL);
        assert!(wait.is_ready(), "B looks again and leaves its own step");
    }

    /// Holds the flush that calls it, once armed, until it is let go.
    pub(super) struct HeldFlush {
        pub(super) armed: AtomicBool,
        pub(super) entered: Mutex<mpsc::Sender<()>>,
        pub(super) release: Mutex<mpsc::Receiver<()>>,
    }

    impl PrefixExtractor for HeldFlush {
        fn extract<'a>(&self, key: &'a [u8]) -> Option<&'a [u8]> {
            if self.armed.swap(false, Ordering::AcqRel) {
                self.entered.lock().unwrap().send(()).unwrap();
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(60))
                    .expect("the flush was never let go");
            }
            Some(key)
        }

        fn name(&self) -> &'static str {
            "held-flush"
        }
    }

    /// A stopped write whose own step finds another writer's owed flush in
    /// flight waits for that writer. The flush adds an L0 table, so the stop
    /// is still on when it ends, and that writer writes nothing more: the
    /// stop is handed back, and the stopped write looks again instead of
    /// waiting for a relief nobody runs.
    #[test]
    fn a_flush_that_leaves_the_stop_hands_it_back_to_the_writer_it_held_off() {
        let (entered, when_entered) = mpsc::channel();
        let (release, when_released) = mpsc::channel();
        let held = Arc::new(HeldFlush {
            armed: AtomicBool::new(false),
            entered: Mutex::new(entered),
            release: Mutex::new(when_released),
        });
        let (_dir, engine) = stopped_engine_with(EngineOptions {
            inline_compaction: true,
            l0_compaction_trigger: 100,
            write_buffer_size: 2 * 1024,
            prefix_extractor: Some(held.clone()),
            ..EngineOptions::default()
        });
        let fill = |key: &[u8]| {
            let ops = vec![WriteBatchOp::Put {
                key: prefix_key(DEFAULT_CF_ID, key),
                value: vec![7u8; 1024],
            }];
            engine
                .apply_batch(ops, DurabilityMode::Eventual, false)
                .unwrap();
        };
        // Two 1 KiB puts fill the buffer, so the third seals it, and the
        // step that write owes flushes it.
        fill(b"a");
        fill(b"b");
        held.armed.store(true, Ordering::Release);
        let wait = std::thread::scope(|scope| {
            scope.spawn(|| fill(b"c"));
            when_entered
                .recv_timeout(Duration::from_secs(60))
                .expect("the sealing write never flushed");
            let Err(Error::WouldBlock(WouldBlock::Stall(wait))) = engine.admit_write(false) else {
                panic!("a relief contended by a flush in flight returns a wait");
            };
            assert!(!wait.is_ready());
            release.send(()).unwrap();
            wait
        });
        // The sealing write returned: its flush is done, and it writes no more.
        assert!(matches!(engine.stall_signal().classify(), Some((_, true))));
        assert!(wait.is_ready(), "handed back: the writer looks again");
    }
}

#[cfg(all(test, not(loom)))]
#[path = "stall_failure_tests.rs"]
mod failure_tests;

#[cfg(all(test, not(loom)))]
#[path = "stall_latch_tests.rs"]
mod latch_tests;

#[cfg(all(test, not(loom)))]
#[path = "stall_rollback_tests.rs"]
mod rollback_tests;

#[cfg(all(test, not(loom)))]
#[path = "stall_commit_tests.rs"]
mod commit_tests;
