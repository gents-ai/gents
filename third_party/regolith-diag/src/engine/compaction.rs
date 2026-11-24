use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
// The periodic worker poll is the only user of these, and the worker
// itself does not exist on a target without threads.
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

#[cfg(not(target_arch = "wasm32"))]
use kovan_channel::RecvDeadline;
#[cfg(not(target_arch = "wasm32"))]
use kovan_channel::unbounded::Receiver;
use kovan_channel::unbounded::Sender;

// Through the portability shim so a target without 64-bit atomics still
// builds; see `src/portability.rs`.
use crate::portability::{AtomicBool, Ordering};

use super::block_cache::BlockCache;
use super::internal_key::{compare_internal_keys, decode_internal_key, user_key_of};
use super::manifest::{MAX_LEVELS, VersionEdit, overlapping};
use super::range_tombstone::{
    RangeTombstone, RangeTombstoneSet, exclusive_successor, sort_dedup_tombstones,
};
use super::snapshot_registry::SnapshotRegistry;
use super::sstable::{
    LiveSst, MetadataPolicy, SsTableInternalIter, SsTableMeta, SsTableReader, SsTableWriter,
    remove_sst_in, sst_filename,
};
use super::version_store::VersionStore;
use crate::engine::background_health::BackgroundHealth;
#[cfg(not(target_arch = "wasm32"))]
use crate::engine::background_health::Job;
#[cfg(not(target_arch = "wasm32"))]
use crate::engine::compaction_backoff::FailureBackoff;
use crate::engine::compaction_gate::CompactionGate;
use crate::engine::io::job::JobBody;
use crate::engine::pending_outputs::PendingOutputs;
use crate::env::{Env, FileLock, JoinHandle};
use kovan_queue::seg_queue::SegQueue;

mod census;
pub(crate) mod claims;
mod deletion;
mod retire;
mod stripes;

pub(crate) use claims::Claims;

use retire::Retirement;

use stripes::Stripes;

/// Default compaction trigger: flush L0 → L1 when L0 has this many SSTables.
pub(crate) const L0_COMPACTION_TRIGGER: usize = 4;

/// Default level size multiplier between levels.
pub(crate) const LEVEL_SIZE_MULTIPLIER: u64 = 10;

/// Default max bytes for level 1 (256 MB).
pub(crate) const DEFAULT_LEVEL_BASE_BYTES: u64 = 256 * 1024 * 1024;

/// Default target SSTable file size (64 MB).
pub(crate) const DEFAULT_TARGET_FILE_SIZE: u64 = 64 * 1024 * 1024;

/// How long a worker sleeps between periodic checks when no wake-up
/// arrives. The backstop that keeps compaction live if a coalesced
/// notification is ever dropped.
#[cfg(not(target_arch = "wasm32"))]
const WORKER_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// What a worker runs on every wake-up before it compacts: the engine's
/// flush of its frozen memtables, oldest first (E9). Set once the engine
/// exists, which is after its workers start.
type FlushHook = Arc<std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>>;

/// Jobs handed to the workers instead of run on a caller's thread: a
/// foreground job (`compact_range`, an ingest, a checkpoint) and the disk
/// check (D15, plan 4.10). Whichever worker wakes first runs each, in
/// arrival order, before it flushes or compacts. What bounds it: each job is
/// a call a caller made and holds a ticket for, plus one disk check per open.
pub(crate) struct WorkerJobs {
    jobs: SegQueue<Box<dyn JobBody>>,
    /// Set by shutdown: no job is accepted after it.
    closed: AtomicBool,
}

impl WorkerJobs {
    fn new() -> Self {
        Self {
            jobs: SegQueue::new(),
            closed: AtomicBool::new(false),
        }
    }

    /// Run the oldest queued job on this worker, if there is one. One at a
    /// time, so a worker's passes and flushes take turns with the jobs
    /// however many are queued (E28).
    #[cfg(not(target_arch = "wasm32"))]
    fn run_one(&self) -> bool {
        self.jobs.pop().map(|job| job.run()).is_some()
    }

    /// Settle every job still queued without running it: the workers have
    /// stopped. Called after they joined, so no job is left untold.
    pub(crate) fn release_queued(&self) {
        self.closed.store(true, Ordering::Release);
        while let Some(job) = self.jobs.pop() {
            job.release();
        }
    }
}

/// Manages background compaction on one or more dedicated OS threads.
///
/// Every call takes `&self` and none waits for another: the engine holds it
/// with no lock around it. Its state is the workers' shutdown flag, the
/// coalescing flag of the wake-up channel and two lock-free queues (the jobs,
/// and the workers' join handles, which shutdown takes).
pub(crate) struct CompactionScheduler {
    shutdown: Arc<AtomicBool>,
    /// Wake-up channel. One token wakes one idle worker.
    trigger: Sender<()>,
    /// Coalescing gate: at most one unconsumed token is ever in flight,
    /// so a flush storm cannot queue thousands of wake-ups for work a
    /// single pass already covers.
    pending: Arc<AtomicBool>,
    /// How many workers were started. Fixed at start, so it still says
    /// whether workers ran once their handles were taken.
    workers: usize,
    /// Joined through the env, so a target that cannot spawn threads
    /// runs compaction in the foreground instead of failing to build.
    /// Popped by whichever shutdown comes first; each handle is joined once.
    handles: SegQueue<Box<dyn JoinHandle>>,
    /// The flush the workers run before compacting.
    flush: FlushHook,
    /// Jobs handed to the workers.
    jobs: Arc<WorkerJobs>,
}

impl CompactionScheduler {
    /// Construct a scheduler with no background workers.
    pub(crate) fn disabled() -> Self {
        let (trigger, _rx) = kovan_channel::unbounded();
        Self {
            shutdown: Arc::new(AtomicBool::new(true)),
            trigger,
            pending: Arc::new(AtomicBool::new(false)),
            workers: 0,
            handles: SegQueue::new(),
            flush: FlushHook::default(),
            jobs: Arc::new(WorkerJobs::new()),
        }
    }

    /// Hand `job` to the workers, waking one. `Err` hands it back when no
    /// worker runs or they were told to stop.
    pub(crate) fn submit(&self, job: Box<dyn JobBody>) -> Result<(), Box<dyn JobBody>> {
        if self.workers == 0 || self.jobs.closed.load(Ordering::Acquire) {
            return Err(job);
        }
        self.jobs.jobs.push(job);
        // Re-checked after the push: a shutdown that began meanwhile may
        // have released the queue before this job reached it.
        if self.jobs.closed.load(Ordering::Acquire) {
            self.jobs.release_queued();
            return Ok(());
        }
        self.notify();
        Ok(())
    }

    /// Test-only: run `work` on a worker, as a job in its next round.
    #[cfg(test)]
    pub(crate) fn run_on_worker(&self, work: impl FnOnce() + Send + 'static) {
        struct OnWorker<F>(F);
        impl<F: FnOnce() + Send> JobBody for OnWorker<F> {
            fn run(self: Box<Self>) {
                (self.0)();
            }
            fn release(self: Box<Self>) {}
        }
        assert!(
            self.submit(Box::new(OnWorker(work))).is_ok(),
            "no worker runs here"
        );
    }

    /// Whether background workers run here. Without one, a writer that
    /// seals a memtable or fills L0 owes the work itself (E9, E16).
    pub(crate) fn has_workers(&self) -> bool {
        self.workers != 0
    }

    /// Install what the workers run before compacting: the engine's flush
    /// of its frozen memtables. The first call wins; the engine makes one.
    pub(crate) fn set_flush(&self, flush: Box<dyn Fn() + Send + Sync>) {
        let _ = self.flush.set(flush);
    }

    /// Start one or more background compaction threads.
    ///
    /// `directory` is the engine's lock on its directory. Each worker holds
    /// a share of it until it has returned, so the directory stays locked
    /// while a worker could still write in it, the engine gone or not.
    ///
    /// `gate` is the engine-wide compaction gate: a worker's pass holds it
    /// shared, so workers run side by side, and a foreground job holds it
    /// alone. `claims` keeps passes that share it off each other's tables.
    ///
    /// `snapshot_registry` lets each compaction pass read the live
    /// snapshots, so it can cut each key's versions into stripes and
    /// drop or fold what no reader of a stripe can see.
    ///
    /// `claims` is the engine-wide record of the tables passes are working
    /// on. The engine owns it and shares it with the passes its callers'
    /// threads run, so a worker and such a pass never pick overlapping
    /// inputs.
    ///
    /// `max_background_compactions == 0` starts no worker at all and
    /// returns a scheduler equivalent to [`CompactionScheduler::disabled`];
    /// compaction then runs on whichever thread asks for it. That is the
    /// only mode a single-threaded host such as `wasm32-wasip1` can open
    /// in, because `std::thread::spawn` reports
    /// [`std::io::ErrorKind::Unsupported`] there.
    ///
    /// Returns the spawn error if a worker thread cannot be created,
    /// after shutting down and joining any worker already started, so
    /// a failed open leaves no detached thread behind.
    // Every parameter is a distinct shared handle the workers need;
    // `compaction_loop` below carries the same fan-out for the same
    // reason.
    #[allow(clippy::too_many_arguments)]
    // On wasm32 the spawn half below is compiled out, which leaves the
    // shared handles the workers would have taken unread.
    #[cfg_attr(target_arch = "wasm32", allow(unused_variables))]
    pub(crate) fn start(
        directory: Arc<dyn FileLock>,
        gate: CompactionGate,
        snapshot_registry: Arc<SnapshotRegistry>,
        versions: Arc<VersionStore>,
        sst_dir: Arc<Path>,
        cache: Arc<BlockCache>,
        opts: CompactionOptions,
        stall_signal: Arc<crate::engine::StallSignal>,
        claims: Arc<Claims>,
        health: Arc<BackgroundHealth>,
    ) -> std::io::Result<Self> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let (trigger, receiver) = kovan_channel::unbounded::<()>();
        let pending = Arc::new(AtomicBool::new(false));

        // Zero means no background worker: compaction runs on the
        // calling thread, which is what a target without threads needs.
        let worker_count = opts.max_background_compactions;
        if worker_count == 0 {
            return Ok(Self::disabled());
        }

        // wasm32 has no threads at all, so `Env::spawn` reports
        // `Unsupported` for every worker. Report it up front and leave
        // the whole worker loop out of the build rather than carrying
        // machinery the target can never reach.
        #[cfg(target_arch = "wasm32")]
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "failed to spawn compaction thread 0: this target has no threads; set \
             Options::max_background_compactions = 0 to run compaction on the calling thread",
        ));

        #[cfg(not(target_arch = "wasm32"))]
        {
            let scheduler = Self {
                shutdown: Arc::clone(&shutdown),
                trigger,
                pending: Arc::clone(&pending),
                workers: worker_count,
                handles: SegQueue::new(),
                flush: FlushHook::default(),
                jobs: Arc::new(WorkerJobs::new()),
            };

            for i in 0..worker_count {
                let shutdown_clone = Arc::clone(&shutdown);
                let receiver_clone = receiver.clone();
                let pending_clone = Arc::clone(&pending);
                let gate_clone = gate.clone();
                let registry_clone = Arc::clone(&snapshot_registry);
                let versions_clone = Arc::clone(&versions);
                let sst_dir_clone = Arc::clone(&sst_dir);
                let cache_clone = Arc::clone(&cache);
                let opts_clone = opts.clone();
                let stall_clone = Arc::clone(&stall_signal);
                let claims_clone = Arc::clone(&claims);
                let health_clone = Arc::clone(&health);
                let flush_clone = Arc::clone(&scheduler.flush);
                let jobs_clone = Arc::clone(&scheduler.jobs);
                let directory_clone = Arc::clone(&directory);

                let spawned = spawn_worker(&*opts.env, i, move || {
                    compaction_loop(
                        flush_clone,
                        jobs_clone,
                        shutdown_clone,
                        receiver_clone,
                        pending_clone,
                        gate_clone,
                        registry_clone,
                        versions_clone,
                        sst_dir_clone,
                        cache_clone,
                        opts_clone,
                        stall_clone,
                        claims_clone,
                        health_clone,
                    );
                    // Let go of the directory only now, with every handle
                    // this worker wrote through dropped above.
                    drop(directory_clone);
                });

                match spawned {
                    Ok(handle) => scheduler.handles.push(handle),
                    Err(e) => {
                        // Dropping `scheduler` on the way out signals
                        // shutdown and joins the workers already started,
                        // which is the same path `shutdown` takes.
                        return Err(std::io::Error::new(
                            e.kind(),
                            format!(
                                "failed to spawn compaction thread {i}: {e}; set \
                             Options::max_background_compactions = 0 to run compaction \
                             on the calling thread"
                            ),
                        ));
                    }
                }
            }

            Ok(scheduler)
        }
    }

    /// Notify the compaction workers that work may be available.
    ///
    /// Coalesced: a notification arriving while one is still unconsumed
    /// is dropped, because the worker that picks the outstanding token up
    /// re-checks the whole version before it decides there is nothing to
    /// do. Send is non-blocking, so this never stalls a flush.
    pub(crate) fn notify(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            self.trigger.send(());
        }
    }

    /// Shut down all compaction threads.
    ///
    /// Wakes every worker, not just one: `join` below waits for all of
    /// them, so a `notify_one` here would leave the rest sleeping until
    /// their condvar timeout and make close latency scale with
    /// `max_background_compactions`.
    pub(crate) fn shutdown(&self) {
        self.signal_shutdown();
        self.join_workers();
        self.release_jobs();
    }

    /// Settle every job handed to the workers that they did not run, once
    /// they stopped, and refuse every later one.
    pub(crate) fn release_jobs(&self) {
        self.jobs.release_queued();
    }

    /// Tell every worker to stop, without waiting for any of them.
    pub(crate) fn signal_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        // One token per worker, bypassing the coalescing gate, so every
        // thread wakes now instead of waiting out its periodic poll.
        for _ in 0..self.workers {
            self.trigger.send(());
        }
    }

    /// Join every worker not joined yet. A worker waiting for the compaction
    /// gate exits once whoever holds the gate lets go, so this returns only
    /// then. Each handle is popped by one caller and joined once, so two
    /// shutdowns at once join disjoint workers.
    pub(crate) fn join_workers(&self) {
        while let Some(handle) = self.handles.pop() {
            handle.join();
        }
    }
}

impl Drop for CompactionScheduler {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Spawn one named compaction worker.
///
/// Split out of [`CompactionScheduler::start`] so tests can force the
/// spawn failure that a single-threaded target produces natively.
#[cfg(not(target_arch = "wasm32"))]
fn spawn_worker<F>(env: &dyn Env, index: usize, body: F) -> std::io::Result<Box<dyn JoinHandle>>
where
    F: FnOnce() + Send + 'static,
{
    #[cfg(test)]
    {
        if SPAWN_FAILURE_AFTER.with(|limit| limit.get().is_some_and(|after| index >= after)) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "test seam: compaction worker spawn disabled",
            ));
        }
    }
    env.spawn(&format!("regolith-compaction-{index}"), Box::new(body))
}

#[cfg(test)]
thread_local! {
    /// Test seam: index of the first compaction worker whose spawn
    /// reports `Unsupported`, mirroring what a single-threaded target
    /// does natively. `None` disables the seam. Thread-local because
    /// `start` runs entirely on its caller's thread, so a parallel
    /// test never observes another test's setting.
    static SPAWN_FAILURE_AFTER: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

/// Test-only guard that makes compaction-worker spawning fail on the
/// current thread for the guard's lifetime.
#[cfg(test)]
pub(crate) struct SpawnFailureGuard;

#[cfg(test)]
impl SpawnFailureGuard {
    /// Let the first `after` workers spawn and fail every one past
    /// them until the guard is dropped.
    pub(crate) fn allowing(after: usize) -> Self {
        SPAWN_FAILURE_AFTER.with(|limit| limit.set(Some(after)));
        Self
    }
}

#[cfg(test)]
impl Drop for SpawnFailureGuard {
    fn drop(&mut self) {
        SPAWN_FAILURE_AFTER.with(|limit| limit.set(None));
    }
}

#[derive(Clone)]
pub(crate) struct CompactionOptions {
    pub(crate) l0_compaction_trigger: usize,
    /// The L0 thresholds that slow and stop writes, as the write-stall
    /// classification reads them (`stall_state.rs`); 0 turns one off. A pass
    /// reads them to tell whether one is near (`deletion::stall_near`).
    pub(crate) level0_slowdown_writes_trigger: usize,
    pub(crate) level0_stop_writes_trigger: usize,
    pub(crate) soft_pending_compaction_bytes_limit: u64,
    pub(crate) hard_pending_compaction_bytes_limit: u64,
    pub(crate) level_base_bytes: u64,
    pub(crate) level_size_multiplier: u64,
    pub(crate) target_file_size: u64,
    pub(crate) block_size: usize,
    pub(crate) bloom_bits_per_key: usize,
    pub(crate) compression: crate::options::CompressionType,
    pub(crate) compression_per_level: Option<Vec<crate::options::CompressionType>>,
    pub(crate) compaction_filter: Option<Arc<dyn crate::options::CompactionFilter>>,
    pub(crate) prefix_extractor: Option<Arc<dyn crate::options::PrefixExtractor>>,
    pub(crate) merge_operator: Option<Arc<dyn crate::options::MergeOperator>>,
    pub(crate) listeners: Vec<Arc<dyn crate::event_listener::EventListener>>,
    pub(crate) statistics: Option<Arc<crate::statistics::Statistics>>,
    pub(crate) rate_limiter: Option<Arc<dyn crate::rate_limiter::RateLimiter>>,
    pub(crate) compaction_style: crate::options::CompactionStyle,
    pub(crate) fifo_compaction_options: crate::options::FifoCompactionOptions,
    pub(crate) universal_compaction_options: crate::options::UniversalCompactionOptions,
    pub(crate) evict_compaction_data_from_page_cache: bool,
    pub(crate) max_background_compactions: usize,
    pub(crate) partitioned_index: bool,
    pub(crate) metadata_block_size: usize,
    pub(crate) cache_index_and_filter_blocks: bool,
    /// Set when the database is encrypted at rest; every output table is
    /// sealed under its current key.
    pub(crate) keyring: Option<Arc<super::seal::Keyring>>,
    /// The host platform. Compaction reads, writes, and unlinks
    /// SSTables through it, and starts its workers on it.
    pub(crate) env: Arc<dyn Env>,
}

impl CompactionOptions {
    /// How readers opened by compaction should hold their index and
    /// filter blocks. Mirrors `EngineOptions::metadata_policy`.
    pub(crate) fn metadata_policy(&self) -> MetadataPolicy {
        if self.cache_index_and_filter_blocks {
            MetadataPolicy::Cached
        } else {
            MetadataPolicy::Pinned
        }
    }

    /// Resolve the codec used when writing an output SSTable destined
    /// for `level`. Mirrors `EngineOptions::compression_for_level`.
    pub(crate) fn compression_for_level(&self, level: usize) -> crate::options::CompressionType {
        match &self.compression_per_level {
            Some(per_level) if level < per_level.len() => per_level[level],
            _ => self.compression,
        }
    }
}

impl Default for CompactionOptions {
    fn default() -> Self {
        Self {
            l0_compaction_trigger: L0_COMPACTION_TRIGGER,
            level0_slowdown_writes_trigger: 0,
            level0_stop_writes_trigger: 0,
            soft_pending_compaction_bytes_limit: 0,
            hard_pending_compaction_bytes_limit: 0,
            level_base_bytes: DEFAULT_LEVEL_BASE_BYTES,
            level_size_multiplier: LEVEL_SIZE_MULTIPLIER,
            target_file_size: DEFAULT_TARGET_FILE_SIZE,
            block_size: 16 * 1024,
            bloom_bits_per_key: 10,
            compression: crate::options::CompressionType::Lz4,
            compression_per_level: None,
            compaction_filter: None,
            prefix_extractor: None,
            merge_operator: None,
            listeners: Vec::new(),
            statistics: None,
            rate_limiter: None,
            compaction_style: crate::options::CompactionStyle::Level,
            fifo_compaction_options: crate::options::FifoCompactionOptions::default(),
            universal_compaction_options: crate::options::UniversalCompactionOptions::default(),
            evict_compaction_data_from_page_cache: false,
            max_background_compactions: 1,
            partitioned_index: false,
            metadata_block_size: 4096,
            cache_index_and_filter_blocks: false,
            keyring: None,
            env: crate::env::std_env(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn compaction_loop(
    flush: FlushHook,
    jobs: Arc<WorkerJobs>,
    shutdown: Arc<AtomicBool>,
    trigger: Receiver<()>,
    pending: Arc<AtomicBool>,
    gate: CompactionGate,
    snapshot_registry: Arc<SnapshotRegistry>,
    versions: Arc<VersionStore>,
    sst_dir: Arc<Path>,
    cache: Arc<BlockCache>,
    opts: CompactionOptions,
    stall_signal: Arc<crate::engine::StallSignal>,
    claims: Arc<Claims>,
    health: Arc<BackgroundHealth>,
) {
    // The engine throttles this thread's I/O, and no caller's.
    crate::rate_limiter::mark_worker_thread();
    let mut backoff = FailureBackoff::default();
    loop {
        // About to idle: release the read views this thread's last pass
        // was holding back, so an idle worker never pins a replaced
        // version's table descriptors. The waits inside a round (for the
        // gate, for the flush exclusion) release them where they park
        // (`sync::internal::block_on`).
        crate::engine::read_view::idle();
        // Wait for a trigger, or fall through on the periodic poll.
        // The gate is reopened before the pass runs, so a notification
        // that lands mid-pass queues a token instead of being swallowed.
        match trigger.recv_deadline(Instant::now() + WORKER_POLL_INTERVAL) {
            RecvDeadline::Msg(()) => pending.store(false, Ordering::Release),
            RecvDeadline::Timeout => {}
            RecvDeadline::Disconnected => break,
        }

        // Rounds, until one finds nothing to do. Each round gives one turn
        // to each kind of work, so none keeps another waiting however much
        // of it there is (E28): a stream of jobs used to hold this thread in
        // its job queue, and a busy L0 in its passes.
        while !shutdown.load(Ordering::Acquire) {
            // One job a caller handed over: a call a caller holds a ticket
            // for. It takes the compaction gate alone, waiting here for the
            // passes other workers have in flight.
            let ran_job = jobs.run_one();
            // A job can hold the engine's last reference: its caller dropped
            // every handle without waiting, and the engine was torn down here
            // when the job ended, signalling shutdown. Nothing else in this
            // round is wanted then; the worker returns at once, and the
            // directory it keeps a share of is released when it has.
            if ran_job && shutdown.load(Ordering::Acquire) {
                break;
            }
            // Frozen memtables, oldest first: a flush feeds L0, and the
            // commit path left it to this thread (E9). Not held back by the
            // compaction backoff, which is about compaction failing.
            if let Some(flush) = flush.get() {
                flush();
            }
            // One compaction pass, holding the gate shared so that workers
            // run side by side.
            let did_work = backoff.ready(Instant::now()) && {
                let _pass = gate.shared();
                match pick_and_run_compaction(
                    &versions,
                    &sst_dir,
                    &cache,
                    &opts,
                    &snapshot_registry,
                    &claims,
                    // What a census step stops for: this worker was told of
                    // other work, or to shut down.
                    &|| shutdown.load(Ordering::Acquire) || pending.load(Ordering::Acquire),
                ) {
                    Ok(outcome) => {
                        backoff.record_success();
                        health.record_success(Job::Compaction);
                        outcome == CompactionOutcome::DidWork
                    }
                    Err(e) => {
                        let hazard = health.record_failure(Job::Compaction, &e);
                        let retry_in = backoff.record_failure(Instant::now());
                        tracing::error!(
                            error = %e,
                            hazard = hazard.label(),
                            consecutive_failures = backoff.consecutive_failures(),
                            retry_in_secs = retry_in.as_secs(),
                            "Compaction failed"
                        );
                        // Surface the failure to any registered
                        // listeners so metrics pipelines and
                        // debuggers notice it - the scheduler
                        // itself keeps running. A listener's panic is
                        // logged where it was caught.
                        let _told = tell_failure(&opts.listeners, &e);
                        false
                    }
                }
            };

            // After each round, re-classify the stall against the freshly
            // updated version: writers a stop stopped are told once it no
            // longer holds.
            stall_signal.refresh();

            if !ran_job && !did_work {
                break;
            }
        }
        if shutdown.load(Ordering::Acquire) {
            break;
        }
    }
}

/// What one call to [`crate::Db::compact_step`] achieved.
///
/// The distinction between [`CompactionOutcome::Idle`] and
/// [`CompactionOutcome::Contended`] is load-bearing, not cosmetic. A
/// writer stalling with no background worker performs the compaction
/// itself; if it treated "another thread already holds these inputs"
/// as "there is nothing to compact", it would fail the write with
/// [`crate::Error::Busy`] the moment a second thread was mid-pass.
///
/// The same trap catches a caller draining compaction from outside the
/// engine. A loop that stops on anything other than
/// [`CompactionOutcome::DidWork`] exits while a background worker still
/// holds the level, and concludes the tree is settled when it is not.
/// Wait out a [`CompactionOutcome::Contended`] and stop only on
/// [`CompactionOutcome::Idle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompactionOutcome {
    /// A compaction job ran to completion.
    DidWork,
    /// Nothing was due: no level was over its trigger and no deleted range
    /// was worth a pass. Such a pass may have counted the deletions of one
    /// table an older release wrote, which compacts nothing.
    Idle,
    /// Work may be pending, but another thread is in the way: another
    /// pass holds the input files, or an exclusive user of the compaction
    /// gate holds it or is owed it. The caller should try again later
    /// rather than conclude the engine is idle.
    Contended,
}

/// Pick the highest-priority pending compaction job for the configured
/// style and run it to completion on the calling thread. Returns
/// whether any work was done.
///
/// Shared by the background worker loop and by the engine's foreground
/// pass ([`crate::engine::RegolithEngine::run_one_compaction_pass`]) so the
/// two can never diverge on what "one job" means. Callers hold either
/// side of the engine-wide compaction gate and pass the engine-wide
/// claims.
///
/// Under level and universal compaction a job is size-triggered or
/// deletion-triggered, whichever the turn rule gives (`deletion.rs`). A
/// pass that found neither, with no write stall near, runs one step of the
/// census (`census.rs`) and still reports what the jobs did: the census
/// compacts nothing. `interrupted` is what such a step stops for; a caller
/// with nothing to stop it for passes `&|| false`.
pub(crate) fn pick_and_run_compaction(
    versions: &Arc<VersionStore>,
    sst_dir: &Path,
    cache: &BlockCache,
    opts: &CompactionOptions,
    snapshots: &SnapshotRegistry,
    claims: &Claims,
    interrupted: &dyn Fn() -> bool,
) -> std::io::Result<CompactionOutcome> {
    let pool = match opts.compaction_style {
        crate::options::CompactionStyle::Level => None,
        // One L0 pool, so there is nothing safe to pick in parallel: FIFO
        // drops across it and universal merges across it, so two jobs would
        // pick overlapping inputs. The claim is held until this returns, by
        // any path.
        crate::options::CompactionStyle::Fifo | crate::options::CompactionStyle::Universal => {
            match claims.pool() {
                Some(pool) => Some(pool),
                None => return Ok(CompactionOutcome::Contended),
            }
        }
    };
    if opts.compaction_style == crate::options::CompactionStyle::Fifo {
        // FIFO never merges tables, so no deletion is ever compacted away
        // and nothing is counted for it.
        return Ok(outcome(run_fifo_pass(versions, sst_dir, cache, opts)?));
    }
    let pressed = deletion::stall_near(&versions.current(), opts);
    let ran = if pool.is_some() {
        claims.turn.pass(
            pressed,
            || pick_and_run_universal(versions, sst_dir, cache, opts, snapshots).map(outcome),
            || run_universal_deletion_merge(versions, sst_dir, cache, opts, snapshots),
        )?
    } else {
        claims.turn.pass(
            pressed,
            || pick_and_run_level_compaction(versions, sst_dir, cache, opts, snapshots, claims),
            || run_level_deletion_pass(versions, sst_dir, cache, opts, snapshots, claims),
        )?
    };
    if ran != CompactionOutcome::DidWork && !pressed {
        census::step(versions, cache, opts, claims, pool.is_some(), interrupted);
    }
    Ok(ran)
}

/// Run the level-style pass picked for its deletions, if there is one
/// (`deletion::pick_level`). Claimed and checked as a size-triggered pass
/// is in [`compact_level`].
fn run_level_deletion_pass(
    versions: &Arc<VersionStore>,
    sst_dir: &Path,
    cache: &BlockCache,
    opts: &CompactionOptions,
    snapshots: &SnapshotRegistry,
    claims: &Claims,
) -> std::io::Result<CompactionOutcome> {
    let Some(job) = deletion::pick_level(&versions.current(), snapshots.oldest_live_seq(), cache)
    else {
        return Ok(CompactionOutcome::Idle);
    };
    let Some(_claim) = claims.tables(
        job.inputs
            .iter()
            .chain(&job.below)
            .map(Arc::clone)
            .collect(),
    ) else {
        return Ok(CompactionOutcome::Contended);
    };
    let current = versions.current();
    if !claims::all_live(&current.levels[job.level], &job.inputs)
        || !claims::all_live(&current.levels[job.target], &job.below)
    {
        return Ok(CompactionOutcome::Contended);
    }
    perform_compaction_to(
        versions, sst_dir, cache, opts, job.level, job.target, job.inputs, job.below, snapshots,
    )?;
    count_deletion_triggered(opts);
    Ok(CompactionOutcome::DidWork)
}

/// Run the universal-style merge picked for its deletions, if there is one
/// (`deletion::pick_universal`). The caller holds the pool's claim.
fn run_universal_deletion_merge(
    versions: &Arc<VersionStore>,
    sst_dir: &Path,
    cache: &BlockCache,
    opts: &CompactionOptions,
    snapshots: &SnapshotRegistry,
) -> std::io::Result<CompactionOutcome> {
    let Some(runs) =
        deletion::pick_universal(&versions.current(), snapshots.oldest_live_seq(), cache)
    else {
        return Ok(CompactionOutcome::Idle);
    };
    perform_universal_merge(versions, sst_dir, cache, opts, runs, snapshots)?;
    count_deletion_triggered(opts);
    Ok(CompactionOutcome::DidWork)
}

fn count_deletion_triggered(opts: &CompactionOptions) {
    if let Some(statistics) = &opts.statistics {
        statistics.add(crate::statistics::Ticker::CompactionDeletionTriggered, 1);
    }
}

/// Map a picker that has no notion of contention onto an outcome.
fn outcome(did_work: bool) -> CompactionOutcome {
    if did_work {
        CompactionOutcome::DidWork
    } else {
        CompactionOutcome::Idle
    }
}

/// Universal (size-tiered) picker. Every L0 file is a "run"; the
/// picker either merges the newest handful of files (size-ratio
/// rule) or folds every file into one run (size-amplification
/// rule). See the [`crate::UniversalCompactionOptions`] doc for
/// the meaning of each trigger.
///
/// The picker returns after at most one merge per call; the
/// background compaction loop re-invokes it until there's nothing
/// left to do.
fn pick_and_run_universal(
    versions: &Arc<VersionStore>,
    sst_dir: &Path,
    cache: &BlockCache,
    opts: &CompactionOptions,
    snapshots: &SnapshotRegistry,
) -> std::io::Result<bool> {
    let universal = opts.universal_compaction_options;

    // Snapshot L0 under the version lock, then make decisions
    // without holding it.
    let l0_files: Vec<Arc<LiveSst>> = {
        let version = versions.current();
        version.levels[0].clone()
    };
    if l0_files.len() < 2 {
        return Ok(false);
    }

    // Newest first. L0 lists its tables by age, oldest first, and that is
    // the order a read follows, so it is the order here. A file id is not
    // an age: a merge's output draws its id when it starts writing and takes
    // the place of its oldest input, so a run flushed while the merge ran
    // has the smaller id and the newer place. Ordered by id, the size rule
    // could merge the runs on either side of such a run and leave it out,
    // and the output, placed below it, would hide a newer write under an
    // older one.
    let by_age: Vec<Arc<LiveSst>> = l0_files.into_iter().rev().collect();

    // Rule 1 - size ratio merge. Accumulate newest files into a
    // candidate group; keep adding until the group's total size
    // grows past `size_ratio` percent of the next file's size, at
    // which point the next file is too much larger to be worth
    // folding in. If we end up with `min_merge_width` or more
    // files in the group, merge them.
    let max_width = universal.max_merge_width.max(1) as usize;
    let min_width = universal.min_merge_width.max(2) as usize;
    let ratio = universal.size_ratio as u128;
    let mut group: Vec<Arc<LiveSst>> = Vec::new();
    let mut group_size: u128 = 0;
    for (i, file) in by_age.iter().enumerate() {
        if group.len() >= max_width {
            break;
        }
        if group.is_empty() {
            group.push(Arc::clone(file));
            group_size = file.meta.file_size as u128;
            continue;
        }
        // Does adding this file keep the "size-tier" invariant?
        // The rule: the running total must be within
        // `size_ratio` percent of the new candidate - i.e. the
        // candidate should not dwarf the accumulator.
        let candidate_size = file.meta.file_size as u128;
        if group_size * (100 + ratio) / 100 >= candidate_size {
            group.push(Arc::clone(file));
            group_size += candidate_size;
        } else {
            break;
        }
        // Safety: never include the final file unless we also
        // trigger the size-amp rule below.
        let _ = i;
    }
    if group.len() >= min_width {
        return perform_universal_merge(versions, sst_dir, cache, opts, group, snapshots)
            .map(|_| true);
    }

    // Rule 2 - size amplification. Compute the ratio of "all
    // files except the oldest" to "the oldest file". When it
    // exceeds the configured percent, fold everything into one
    // run so the database stops accumulating redundancy.
    let mut oldest_first = by_age.clone();
    oldest_first.reverse();
    let oldest_size = oldest_first[0].meta.file_size as u128;
    if oldest_size == 0 {
        return Ok(false);
    }
    let younger_total: u128 = oldest_first
        .iter()
        .skip(1)
        .map(|f| f.meta.file_size as u128)
        .sum();
    let amp_percent = younger_total * 100 / oldest_size;
    if amp_percent >= universal.max_size_amplification_percent as u128 {
        return perform_universal_merge(versions, sst_dir, cache, opts, by_age, snapshots)
            .map(|_| true);
    }

    Ok(false)
}

/// Merge every file in `inputs` into a single new L0 file. Input
/// files are removed from the version; the new file gets a fresh id
/// and takes the place of its oldest input, so its age in L0 is that
/// input's, whatever its id.
///
/// The output is deliberately **one** file regardless of
/// [`CompactionOptions::target_file_size`]. Under this style each L0
/// file is one sorted run and the picker's size-ratio rule reasons
/// about runs, so splitting a merge across several files re-creates
/// the very group that was just merged: the picker re-selects it, the
/// writer re-splits it, and the pass never terminates while burning a
/// full merge each time. Emitting one run makes every merge reduce the
/// L0 file count by at least one, which is what bounds the loop in
/// [`crate::Db::compact_step`] and in the inline stall path.
fn perform_universal_merge(
    versions: &Arc<VersionStore>,
    sst_dir: &Path,
    cache: &BlockCache,
    opts: &CompactionOptions,
    inputs: Vec<Arc<LiveSst>>,
    snapshots: &SnapshotRegistry,
) -> std::io::Result<()> {
    let mut run_opts = opts.clone();
    run_opts.target_file_size = u64::MAX;
    // Universal merges are L0 -> L0. The shared
    // `perform_compaction_to` helper handles the merge-and-write
    // path; we just tell it to target L0 and pass no overlap
    // files (there is no "next level" to drag in).
    perform_compaction_to(
        versions,
        sst_dir,
        cache,
        &run_opts,
        0,
        0,
        inputs,
        Vec::new(),
        snapshots,
    )
}

/// Public wrapper for the FIFO picker, used by the synchronous
/// `compact_range` path so callers can trigger a deterministic
/// drop of over-cap files.
pub(crate) fn run_fifo_pass(
    versions: &Arc<VersionStore>,
    sst_dir: &Path,
    cache: &BlockCache,
    opts: &CompactionOptions,
) -> std::io::Result<bool> {
    pick_and_run_fifo(versions, sst_dir, cache, opts)
}

/// Tell `listeners` a compaction pass failed with `err`: the one place a
/// failing pass is told, whether it ran on a worker or on a caller's
/// thread. A listener's panic comes back as an error where a background
/// step contains it (`engine::callback`), already logged there.
pub(crate) fn tell_failure(
    listeners: &[Arc<dyn crate::event_listener::EventListener>],
    err: &std::io::Error,
) -> Result<(), crate::Error> {
    if listeners.is_empty() {
        return Ok(());
    }
    let err = crate::Error::from(crate::Error::clone_io(err));
    crate::event_listener::dispatch_contained(listeners, |l| {
        l.on_background_error(
            crate::event_listener::BackgroundErrorReason::Compaction,
            &err,
        )
    })
}

/// Force-merge every L0 file into a single run. Called by the
/// synchronous `Db::compact_range` path when the configured
/// compaction style is [`crate::CompactionStyle::Universal`], so a
/// caller asking for a manual compaction gets full deduplication
/// across every live file regardless of the picker's ratio rules.
pub(crate) fn run_universal_full_compaction(
    versions: &Arc<VersionStore>,
    sst_dir: &Path,
    cache: &BlockCache,
    opts: &CompactionOptions,
    snapshots: &SnapshotRegistry,
) -> std::io::Result<()> {
    let l0_files: Vec<Arc<LiveSst>> = {
        let version = versions.current();
        version.levels[0].clone()
    };
    if l0_files.len() < 2 {
        return Ok(());
    }
    perform_universal_merge(versions, sst_dir, cache, opts, l0_files, snapshots)
}

fn pick_and_run_level_compaction(
    versions: &Arc<VersionStore>,
    sst_dir: &Path,
    cache: &BlockCache,
    opts: &CompactionOptions,
    snapshots: &SnapshotRegistry,
    claims: &Claims,
) -> std::io::Result<CompactionOutcome> {
    let version = versions.current();

    // Check L0 first
    if version.l0_count() >= opts.l0_compaction_trigger {
        return compact_l0(versions, sst_dir, cache, opts, snapshots, claims);
    }

    // Check other levels
    for level in 1..MAX_LEVELS - 1 {
        let target = level_target_size(level, opts);
        if version.level_size(level) > target {
            return compact_level(versions, sst_dir, cache, opts, level, snapshots, claims);
        }
    }

    Ok(CompactionOutcome::Idle)
}

/// FIFO picker: every flush lands a new L0 file, and regolith never
/// merges in this style. After each pass, if the total bytes held
/// across L0 exceed `max_table_files_size`, we unlink the oldest
/// file (the first L0 lists) and emit a `RemoveFile` edit. We stop
/// when the cap is satisfied or only one file remains - a single
/// oversized file is not worth deleting because that would lose
/// data with no successor on disk.
fn pick_and_run_fifo(
    versions: &Arc<VersionStore>,
    sst_dir: &Path,
    cache: &BlockCache,
    opts: &CompactionOptions,
) -> std::io::Result<bool> {
    let limit = opts.fifo_compaction_options.max_table_files_size;
    if limit == 0 {
        return Ok(false);
    }

    // Snapshot the current L0 contents under the version lock, then
    // make decisions outside it so we don't hold the lock across
    // the unlink syscall.
    let l0_files: Vec<Arc<crate::engine::sstable::LiveSst>> = {
        let version = versions.current();
        version.levels[0].clone()
    };

    let total: u64 = l0_files.iter().map(|f| f.meta.file_size).sum();
    if total <= limit {
        return Ok(false);
    }

    // Oldest first: the order L0 lists its tables in, which is the order a
    // read follows. Not the order of their file ids (E38): an ingest draws
    // its id when it stages its file and installs it later, at the newest
    // place, so a table flushed in between has the larger id and the older
    // place. Dropped by id, the ingested table would go first and the
    // older value under it would read again.
    let by_age: Vec<Arc<crate::engine::sstable::LiveSst>> = l0_files;

    let mut running_total = total;
    let mut edits: Vec<VersionEdit> = Vec::new();
    let mut removed: Vec<Arc<crate::engine::sstable::LiveSst>> = Vec::new();
    for file in &by_age {
        // Always keep at least one file alive - dropping the only
        // remaining file would delete every byte of user data the
        // database currently holds.
        if by_age.len() - edits.len() <= 1 {
            break;
        }
        if running_total <= limit {
            break;
        }
        edits.push(VersionEdit::RemoveFile {
            level: 0,
            file_id: file.meta.file_id,
        });
        removed.push(Arc::clone(file));
        running_total = running_total.saturating_sub(file.meta.file_size);
    }

    if edits.is_empty() {
        return Ok(false);
    }

    versions.run(|versions| versions.apply(&edits))?;

    // Best-effort unlink, as after a merge. The `LiveSst` reader Arcs in
    // older versions (held by long-running snapshots / iterators) keep
    // file descriptors open, so the kernel preserves the inode until those
    // readers drop. A failure here doesn't corrupt the database - the
    // manifest already reflects the removal.
    delete_old_files(&*opts.env, sst_dir, &removed, cache);
    tell_deleted(&opts.listeners, sst_dir, &removed);

    if let Some(s) = &opts.statistics {
        s.add(
            crate::statistics::Ticker::CompactionCount,
            edits.len() as u64,
        );
    }

    Ok(true)
}

fn level_target_size(level: usize, opts: &CompactionOptions) -> u64 {
    let mut size = opts.level_base_bytes;
    for _ in 1..level {
        size = size.saturating_mul(opts.level_size_multiplier);
    }
    size
}

/// Compact all L0 SSTables into L1.
///
/// L0 files may overlap each other, so the whole level is one job.
/// `compact_level` claims it, which is where the contention check for L0
/// lives: a check here would be a separate critical section from the claim
/// and so would not exclude anything.
fn compact_l0(
    versions: &Arc<VersionStore>,
    sst_dir: &Path,
    cache: &BlockCache,
    opts: &CompactionOptions,
    snapshots: &SnapshotRegistry,
    claims: &Claims,
) -> std::io::Result<CompactionOutcome> {
    compact_level(versions, sst_dir, cache, opts, 0, snapshots, claims)
}

/// Compact a level into the next level using the standard size-based
/// heuristic (all L0 files, or the first file of L1+). Used by the
/// background scheduler.
fn compact_level(
    versions: &Arc<VersionStore>,
    sst_dir: &Path,
    cache: &BlockCache,
    opts: &CompactionOptions,
    level: usize,
    snapshots: &SnapshotRegistry,
    claims: &Claims,
) -> std::io::Result<CompactionOutcome> {
    let target_level = level + 1;
    if target_level >= MAX_LEVELS {
        return Ok(CompactionOutcome::Idle);
    }

    // Choose the inputs from the current version, skipping what another
    // pass holds, then claim them (`claims.rs`).
    let version = versions.current();
    let level_files = &version.levels[level];
    if level_files.is_empty() {
        return Ok(CompactionOutcome::Idle);
    }
    let held = |file: &Arc<LiveSst>| file.claimed.load(Ordering::Acquire);

    // L0 files may overlap each other, so the level goes in one job and
    // any claimed file blocks it. L1+ files do not overlap, so one
    // unclaimed file at a time is a valid job on its own.
    let input_files: Vec<Arc<LiveSst>> = if level == 0 {
        if level_files.iter().any(held) {
            return Ok(CompactionOutcome::Contended);
        }
        level_files.clone()
    } else {
        match level_files.iter().find(|file| !held(file)) {
            Some(file) => vec![Arc::clone(file)],
            None => return Ok(CompactionOutcome::Contended),
        }
    };

    let (min_key, max_key) = key_range(&input_files);
    let overlap_files = find_overlapping(&version.levels[target_level], &min_key, &max_key);
    if overlap_files.iter().any(held) {
        return Ok(CompactionOutcome::Contended);
    }

    #[cfg(test)]
    after_pick();

    // Seeing a table unclaimed above excludes nothing: two passes can both
    // see it free. The claim does, one compare-and-swap per table, and gives
    // everything back when it meets a table another pass took meanwhile.
    // Held until this returns, by any path, so a pass that errors does not
    // strand its tables.
    let Some(_claim) = claims.tables(
        input_files
            .iter()
            .chain(&overlap_files)
            .map(Arc::clone)
            .collect(),
    ) else {
        return Ok(CompactionOutcome::Contended);
    };

    // The version these were picked from may be behind: a pass that held one
    // of them can have installed its outputs and let go between the pick and
    // the claim. Read again now that the claims are held. A table still in
    // the version stays there while its claim is, and one that is gone was
    // compacted already, so compacting it again would put its rows in the
    // level twice.
    let current = versions.current();
    if !claims::all_live(&current.levels[level], &input_files)
        || !claims::all_live(&current.levels[target_level], &overlap_files)
    {
        return Ok(CompactionOutcome::Contended);
    }

    perform_compaction(
        versions,
        sst_dir,
        cache,
        opts,
        level,
        input_files,
        overlap_files,
        snapshots,
    )?;
    Ok(CompactionOutcome::DidWork)
}

#[cfg(test)]
thread_local! {
    /// Test seam: run once on this thread between a pass's pick of its
    /// inputs and its claim of them, where another pass can compact the
    /// same tables away. Thread-local, so a parallel test never runs
    /// another test's hook.
    static AFTER_PICK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run and clear the [`AFTER_PICK`] hook. Taken before it runs, so a pass
/// the hook itself makes does not run it again.
#[cfg(test)]
fn after_pick() {
    let hook = AFTER_PICK.with(|hook| hook.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

/// Run a manual `compact_range` pass synchronously: for every level from
/// 0 down to MAX_LEVELS-2, pick the files overlapping `[start, end)` and
/// push them down to the next level. Returns once no level has any more
/// files in the range that can be pushed down.
///
/// Callers must hold the engine-wide compaction gate so the background
/// scheduler can't pick an overlapping input set concurrently.
pub(crate) fn run_compact_range(
    versions: &Arc<VersionStore>,
    sst_dir: &Path,
    cache: &BlockCache,
    opts: &CompactionOptions,
    start: Option<&[u8]>,
    end: Option<&[u8]>,
    snapshots: &SnapshotRegistry,
) -> std::io::Result<()> {
    for level in 0..MAX_LEVELS - 1 {
        loop {
            let version = versions.current();
            let files = &version.levels[level];
            if files.is_empty() {
                break;
            }

            // At L0 files overlap each other, so every L0 file the range
            // drags in is taken in one shot, see `l0_inputs`.
            //
            // At L1+ files are non-overlapping, so we can pick them
            // one at a time.
            let inputs: Vec<Arc<LiveSst>> = if level == 0 {
                l0_inputs(files, start, end)
            } else {
                files
                    .iter()
                    .find(|f| file_overlaps_range(f, start, end))
                    .map(Arc::clone)
                    .into_iter()
                    .collect()
            };

            if inputs.is_empty() {
                break;
            }

            let (min_key, max_key) = key_range(&inputs);
            let target_level = level + 1;
            let overlap_files = find_overlapping(&version.levels[target_level], &min_key, &max_key);

            perform_compaction(
                versions,
                sst_dir,
                cache,
                opts,
                level,
                inputs,
                overlap_files,
                snapshots,
            )?;

            // At L0 we handled every range-overlapping file in one
            // shot, so we're done with this level.
            if level == 0 {
                break;
            }
            // At L1+, loop again to pick the next file in the range,
            // if any.
        }
    }
    Ok(())
}

/// Rewrite, each in place at its own level, every table that is not sealed
/// under the keyring's current key: one written before encryption was
/// turned on, or sealed under a key since rotated away. Does nothing on a
/// database without a keyring, and costs one pass over the table list when
/// every table is current.
///
/// The tables are listed once, so a key rotated while this runs cannot keep
/// it going. Each rewrite is an ordinary one-table compaction into the same
/// level (L0 keeps its order), so it reduces versions exactly as any
/// compaction would. The caller holds the compaction gate.
pub(crate) fn reseal_tables(
    versions: &Arc<VersionStore>,
    sst_dir: &Path,
    cache: &BlockCache,
    opts: &CompactionOptions,
    snapshots: &SnapshotRegistry,
) -> std::io::Result<()> {
    let Some(keyring) = &opts.keyring else {
        return Ok(());
    };
    let current = Some(keyring.current_id());
    let version = versions.current();
    let stale: Vec<(usize, Arc<LiveSst>)> = version
        .levels
        .iter()
        .enumerate()
        .flat_map(|(level, files)| {
            files
                .iter()
                .filter(|f| f.reader.seal_key() != current)
                .map(move |f| (level, Arc::clone(f)))
        })
        .collect();
    drop(version);
    for (level, file) in stale {
        perform_compaction_to(
            versions,
            sst_dir,
            cache,
            opts,
            level,
            level,
            vec![file],
            Vec::new(),
            snapshots,
        )?;
    }
    Ok(())
}

/// The L0 tables a `[start, end)` compaction pushes down: those that
/// intersect it, and every older table whose keys overlap theirs.
///
/// L0 is in age order, oldest first, and a read walks it from the newest table
/// back taking the first match (see `Version::add_file`). A table moved to L1
/// with an older one left behind in L0 would read as the older value, so each
/// table that joins widens the range the next older one is tested against.
fn l0_inputs(
    files: &[Arc<LiveSst>],
    start: Option<&[u8]>,
    end: Option<&[u8]>,
) -> Vec<Arc<LiveSst>> {
    let mut picked: Vec<bool> = files
        .iter()
        .map(|f| file_overlaps_range(f, start, end))
        .collect();
    let Some(newest) = picked.iter().rposition(|&p| p) else {
        return Vec::new();
    };
    loop {
        let inputs: Vec<Arc<LiveSst>> = files
            .iter()
            .zip(&picked)
            .filter(|(_, p)| **p)
            .map(|(f, _)| Arc::clone(f))
            .collect();
        let (min_key, mut max_key) = key_range(&inputs);
        // The range end is exclusive and a table starting at `max_key`
        // shares it, so test against the smallest key above it.
        max_key.push(0);
        let mut grew = false;
        for (f, p) in files[..newest].iter().zip(&mut picked) {
            if !*p && file_overlaps_range(f, Some(&min_key), Some(&max_key)) {
                *p = true;
                grew = true;
            }
        }
        if !grew {
            return inputs;
        }
    }
}

/// Inner body of a compaction: read the merged entries from
/// `input_files` + `overlap_files`, write new output SSTables at
/// `target_level`, and atomically apply the version edit (Remove old
/// files, Add new ones). File descriptors of old files stay alive via
/// any `Arc<LiveSst>` still referenced by older versions / iterators.
///
/// The live snapshots, read from `snapshots` once the inputs are fixed, cut
/// the key's versions into stripes (see the `stripes` module). With no
/// snapshot live every key is one stripe, and only its newest state is
/// retained.
#[allow(clippy::too_many_arguments)]
fn perform_compaction(
    versions: &Arc<VersionStore>,
    sst_dir: &Path,
    cache: &BlockCache,
    opts: &CompactionOptions,
    level: usize,
    input_files: Vec<Arc<LiveSst>>,
    overlap_files: Vec<Arc<LiveSst>>,
    snapshots: &SnapshotRegistry,
) -> std::io::Result<()> {
    // Leveled callers always push down one level.
    perform_compaction_to(
        versions,
        sst_dir,
        cache,
        opts,
        level,
        level + 1,
        input_files,
        overlap_files,
        snapshots,
    )
}

/// Core compaction body. Accepts an explicit `output_level` so the
/// caller can target either the next level (leveled push-down) or
/// the same level (universal in-place merge at L0). `overlap_files`
/// are read as additional inputs and their version entries at
/// `output_level` are removed alongside the `input_files` at
/// `input_level`.
#[allow(clippy::too_many_arguments)]
fn perform_compaction_to(
    versions: &Arc<VersionStore>,
    sst_dir: &Path,
    cache: &BlockCache,
    opts: &CompactionOptions,
    level: usize,
    target_level: usize,
    input_files: Vec<Arc<LiveSst>>,
    overlap_files: Vec<Arc<LiveSst>>,
    snapshots: &SnapshotRegistry,
) -> std::io::Result<()> {
    let compaction_start = opts.env.now_micros();

    // Snapshot input file ids up-front so the on_compaction_begin
    // callback has them even if the job errors out later.
    let (begin_inputs_l, begin_inputs_l1): (Vec<u64>, Vec<u64>) = (
        input_files.iter().map(|f| f.meta.file_id).collect(),
        overlap_files.iter().map(|f| f.meta.file_id).collect(),
    );
    if !opts.listeners.is_empty() {
        let info = crate::event_listener::CompactionJobInfo {
            input_level: level,
            output_level: target_level,
            input_files_input_level: begin_inputs_l.clone(),
            input_files_output_level: begin_inputs_l1.clone(),
            output_files: Vec::new(),
            duration: std::time::Duration::ZERO,
        };
        crate::event_listener::dispatch(&opts.listeners, |l| l.on_compaction_begin(&info));
    }

    // Range tombstones are small metadata relative to the point-entry
    // stream, and readers need the complete covering set to decide
    // whether a point version is shadowed. Point entries themselves are
    // merged below by a k-way stream so compaction memory no longer
    // scales with total input data bytes.
    let mut merged_range_tombstones: Vec<RangeTombstone> = Vec::new();
    for file in input_files.iter().chain(overlap_files.iter()) {
        for rt in file.reader.range_tombstones() {
            merged_range_tombstones.push(rt.clone());
        }
    }

    // User compaction filter for range tombstones. Run this **before**
    // the point-entry RT shadow pass so a filter that drops a range
    // tombstone doesn't leave orphaned point entries already wiped
    // out by that same RT.
    if let Some(filter) = opts.compaction_filter.as_ref() {
        merged_range_tombstones.retain(|rt| {
            !matches!(
                super::filter_call::filter_range_delete(
                    filter.as_ref(),
                    target_level,
                    &rt.start,
                    &rt.end
                ),
                crate::options::CompactionDecision::Remove
            )
        });
    }

    // Dedup merged range tombstones by (start, end, seq) - a single
    // logical RT may appear in multiple input files after previous
    // compactions carried it forward.
    let merged_range_tombstones = RangeTombstoneSet::from_vec(merged_range_tombstones);

    let mut edits = Vec::new();

    for file in &input_files {
        edits.push(VersionEdit::RemoveFile {
            level,
            file_id: file.meta.file_id,
        });
    }
    for file in &overlap_files {
        edits.push(VersionEdit::RemoveFile {
            level: target_level,
            file_id: file.meta.file_id,
        });
    }

    // Read here, with the inputs fixed, never before they are picked. A
    // snapshot registered before this read is in the list. One registered
    // after it reads at a sequence at or above every version in the inputs,
    // and the newest version of every key survives in its stripe, so it
    // loses nothing. Read before the pick, a snapshot could register and a
    // newer version be flushed into a table the pass then picks, with the
    // list none the wiser: both would share a stripe and the older one, the
    // version that snapshot reads, would be dropped.
    let live = snapshots.live_seqs();
    // Read with the inputs fixed too: the levels below the output can only
    // gain a key of these inputs' ranges through this pass.
    let version = versions.current();
    let retirement = Retirement::new(target_level, &version, &input_files, live.first().copied());
    // Shared with each output's writer, which counts what they hide of it.
    let carried = Arc::new(retirement.carried_tombstones(&merged_range_tombstones));

    // Compaction inputs are obsolete after publication; caching their full
    // scan evicts foreground reads and retains buffers until the merge ends.
    let scan_cache = BlockCache::with_config(0, 0, true);
    let CompactionOutputs {
        edits: new_file_edits,
        infos: output_file_infos,
        pending,
    } = stream_compaction_outputs(
        versions,
        sst_dir,
        &scan_cache,
        opts,
        target_level,
        &input_files,
        &overlap_files,
        &live,
        &merged_range_tombstones,
        &carried,
        &retirement,
    )?;
    edits.extend(new_file_edits);

    // Tell the OS it can drop pages we just read from the page cache.
    // Opt-in via `evict_compaction_data_from_page_cache`; on non-Linux targets
    // the hint is a no-op.
    if opts.evict_compaction_data_from_page_cache {
        for file in input_files.iter().chain(overlap_files.iter()) {
            let path = sst_dir.join(sst_filename(file.meta.file_id));
            opts.env.drop_page_cache(&path);
        }
    }

    // Atomically apply the remove / add edits. `SetNextFileId` is not
    // needed here - each output file already advanced it when it was
    // allocated above.
    versions.run(|versions| -> std::io::Result<()> {
        if level == 0 && target_level == 0 {
            // An L0 to L0 merge has to be placed, not appended.
            //
            // Recency inside L0 is position, not sequence number: `get`
            // walks the level in reverse and takes the first match without
            // comparing sequence numbers across files. `apply` appends, so
            // an appended merge output claims the newest slot. Nothing
            // excludes a flush while the merge runs, and a memtable flushed
            // mid-merge installs a genuinely newer file first. Appending
            // then puts the older merged data in front of it and the newer
            // write reads as lost until some later merge folds it in.
            //
            // So rebuild the level explicitly: everything that was in front
            // of the oldest input stays in front, the merge output takes
            // that oldest input's slot, and every survivor after it keeps
            // its order. Files installed while the merge ran are survivors
            // and stay newer, which is what they are.
            //
            // Expressed with the edits that already exist rather than a new
            // one, because every edit is also a manifest record and replay
            // has to reproduce this order from the log. `apply` walks edits
            // in order, so remove-all-then-add-in-order says it exactly.
            let current = versions.current();
            let input_ids: std::collections::HashSet<u64> =
                input_files.iter().map(|f| f.meta.file_id).collect();
            let oldest = current.levels[0]
                .iter()
                .position(|f| input_ids.contains(&f.meta.file_id))
                .unwrap_or(0);
            // The inputs lie next to one another in L0 (E38). A table left
            // out between them would keep a newer place than the output and
            // hide the writes of every input newer than it.
            debug_assert!(
                current.levels[0][oldest..]
                    .iter()
                    .take(input_ids.len())
                    .all(|f| input_ids.contains(&f.meta.file_id)),
                "the inputs of a merge inside L0 lie next to one another"
            );

            let mut rebuilt: Vec<VersionEdit> = Vec::new();
            for f in current.levels[0].iter() {
                rebuilt.push(VersionEdit::RemoveFile {
                    level: 0,
                    file_id: f.meta.file_id,
                });
            }
            let keep = |f: &Arc<LiveSst>| !input_ids.contains(&f.meta.file_id);
            for f in current.levels[0][..oldest].iter().filter(|f| keep(f)) {
                rebuilt.push(VersionEdit::AddFile {
                    level: 0,
                    file: Arc::clone(f),
                });
            }
            for edit in edits.iter() {
                if let VersionEdit::AddFile { level: 0, file } = edit {
                    rebuilt.push(VersionEdit::AddFile {
                        level: 0,
                        file: Arc::clone(file),
                    });
                }
            }
            for f in current.levels[0][oldest..].iter().filter(|f| keep(f)) {
                rebuilt.push(VersionEdit::AddFile {
                    level: 0,
                    file: Arc::clone(f),
                });
            }
            pending.offered_to_manifest();
            versions.apply(&rebuilt)
        } else {
            pending.offered_to_manifest();
            versions.apply(&edits)
        }
    })?;

    // Unlink the old SSTable paths. Their file descriptors stay alive
    // through any `Arc<LiveSst>` still held by older versions or by
    // iterators, so the data remains readable until those Arcs drop.
    delete_old_files(
        &*opts.env,
        sst_dir,
        input_files.iter().chain(&overlap_files),
        cache,
    );

    // Publish compaction statistics. Bytes-in is the sum of
    // every input file's on-disk size; bytes-out is the sum of
    // the freshly-emitted output files. Both the foreground
    // `compact_range` path and the background scheduler route
    // through this function, so the tickers cover both.
    if let Some(s) = opts.statistics.as_deref() {
        let bytes_in: u64 = input_files
            .iter()
            .chain(overlap_files.iter())
            .map(|f| f.meta.file_size)
            .sum();
        let bytes_out: u64 = output_file_infos.iter().map(|f| f.file_size).sum();
        s.add(crate::statistics::Ticker::CompactionCount, 1);
        s.add(crate::statistics::Ticker::CompactionBytesRead, bytes_in);
        s.add(crate::statistics::Ticker::CompactionBytesWritten, bytes_out);
        if s.timed()
            && let Some(micros) = crate::env::elapsed_micros(&*opts.env, compaction_start)
        {
            s.record(crate::statistics::Histogram::CompactionTime, micros);
        }
    }

    // Dispatch compaction completion + per-file creation +
    // per-file deletion events to registered listeners. The
    // caller may fire many file-level callbacks plus one
    // compaction-level aggregate.
    if !opts.listeners.is_empty() {
        for out in &output_file_infos {
            let info = crate::event_listener::TableFileCreationInfo {
                file_id: out.file_id,
                file_path: out.path.clone(),
                level: target_level,
                reason: crate::event_listener::TableFileCreationReason::Compaction,
                file_size: out.file_size,
                num_entries: out.num_entries,
            };
            crate::event_listener::dispatch(&opts.listeners, |l| l.on_table_file_created(&info));
        }
        tell_deleted(
            &opts.listeners,
            sst_dir,
            input_files.iter().chain(&overlap_files),
        );
        let job_info = crate::event_listener::CompactionJobInfo {
            input_level: level,
            output_level: target_level,
            input_files_input_level: begin_inputs_l,
            input_files_output_level: begin_inputs_l1,
            output_files: output_file_infos.iter().map(|f| f.file_id).collect(),
            duration: std::time::Duration::from_micros(
                crate::env::elapsed_micros(&*opts.env, compaction_start).unwrap_or(0),
            ),
        };
        crate::event_listener::dispatch(&opts.listeners, |l| l.on_compaction_completed(&job_info));
    }

    tracing::info!(
        level,
        target_level,
        input_files = input_files.len() + overlap_files.len(),
        "Compaction completed"
    );

    Ok(())
}

/// Whether a file's user-key range intersects `[start, end)`. `None`
/// bounds are treated as unbounded.
fn file_overlaps_range(file: &Arc<LiveSst>, start: Option<&[u8]>, end: Option<&[u8]>) -> bool {
    if let Some(s) = start
        && file.meta.largest_key.as_slice() < s
    {
        return false;
    }
    if let Some(e) = end
        && file.meta.smallest_key.as_slice() >= e
    {
        return false;
    }
    true
}

/// Info a compaction run accumulates for each output file it
/// produces, so the listener-dispatch block at the end has
/// enough data to fire `on_table_file_created` /
/// `on_compaction_completed` without going back to the version.
struct OutputFileInfo {
    file_id: u64,
    path: PathBuf,
    file_size: u64,
    num_entries: u64,
}

struct MergeHeapEntry {
    key: Vec<u8>,
    value: Vec<u8>,
    stream_idx: usize,
}

impl PartialEq for MergeHeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.stream_idx == other.stream_idx && self.key == other.key
    }
}

impl Eq for MergeHeapEntry {}

impl PartialOrd for MergeHeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for MergeHeapEntry {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        match compare_internal_keys(&other.key, &self.key) {
            CmpOrdering::Equal => other.stream_idx.cmp(&self.stream_idx),
            ordering => ordering,
        }
    }
}

struct CompactionInputStream<'a> {
    iter: SsTableInternalIter<'a>,
}

#[allow(clippy::too_many_arguments)]
fn stream_compaction_outputs(
    versions: &Arc<VersionStore>,
    sst_dir: &Path,
    cache: &BlockCache,
    opts: &CompactionOptions,
    target_level: usize,
    input_files: &[Arc<LiveSst>],
    overlap_files: &[Arc<LiveSst>],
    live: &[u64],
    merged_range_tombstones: &RangeTombstoneSet,
    carried_range_tombstones: &Arc<RangeTombstoneSet>,
    retirement: &Retirement<'_>,
) -> std::io::Result<CompactionOutputs> {
    let all_files: Vec<Arc<LiveSst>> = input_files
        .iter()
        .chain(overlap_files.iter())
        .map(Arc::clone)
        .collect();
    let mut streams = Vec::with_capacity(all_files.len());
    let mut heap = BinaryHeap::new();

    for file in &all_files {
        let mut iter = file.reader.iter_internal_stream(cache)?;
        let stream_idx = streams.len();
        if let Some((key, value)) = iter.next_entry()? {
            heap.push(MergeHeapEntry {
                key,
                value,
                stream_idx,
            });
        }
        streams.push(CompactionInputStream { iter });
    }

    // Every tombstone of the inputs shadows the entries it covers; only
    // those not retired are written out.
    let mut writer = StreamingCompactionWriter::new(
        versions,
        sst_dir,
        opts,
        target_level,
        carried_range_tombstones,
    );
    let stripes = Stripes::new(
        live,
        opts.compaction_filter.as_deref(),
        opts.merge_operator.as_deref(),
        target_level,
    );
    let mut last_internal_key: Option<Vec<u8>> = None;
    let mut current_user_key: Option<Vec<u8>> = None;
    let mut group: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

    while let Some(entry) = heap.pop() {
        if let Some((key, value)) = streams[entry.stream_idx].iter.next_entry()? {
            heap.push(MergeHeapEntry {
                key,
                value,
                stream_idx: entry.stream_idx,
            });
        }

        if last_internal_key.as_deref() == Some(entry.key.as_slice()) {
            continue;
        }
        last_internal_key = Some(entry.key.clone());

        let user_key = user_key_of(&entry.key);
        if current_user_key
            .as_deref()
            .is_some_and(|current| current != user_key)
        {
            let mut reduced = stripes.reduce_group(std::mem::take(&mut group));
            retirement.retire_deletion(&mut reduced);
            writer.add_group(&reduced)?;
        }
        if current_user_key.as_deref() != Some(user_key) {
            current_user_key = Some(user_key.to_vec());
        }

        let (_, seq, _) = decode_internal_key(&entry.key);
        if !stripes.shadowed(merged_range_tombstones, user_key, seq) {
            group.push((entry.key, entry.value));
        }
    }

    if current_user_key.is_some() {
        let mut reduced = stripes.reduce_group(group);
        retirement.retire_deletion(&mut reduced);
        writer.add_group(&reduced)?;
    }

    writer.finish()
}

/// What a successful output stream hands back: the edits that install
/// the new files, their stats, and the claim on their paths that unlinks
/// them unless the caller offers them to the manifest.
struct CompactionOutputs {
    edits: Vec<VersionEdit>,
    infos: Vec<OutputFileInfo>,
    pending: PendingOutputs,
}

struct StreamingOutputBuilder {
    file_id: u64,
    path: PathBuf,
    writer: SsTableWriter,
    estimated_size: u64,
    smallest_user_key: Option<Vec<u8>>,
    largest_user_key: Option<Vec<u8>>,
}

struct StreamingCompactionWriter<'a> {
    versions: &'a Arc<VersionStore>,
    sst_dir: &'a Path,
    opts: &'a CompactionOptions,
    target_level: usize,
    /// The range tombstones the pass writes out.
    range_tombstones: &'a Arc<RangeTombstoneSet>,
    point_ranges: Vec<(Vec<u8>, Vec<u8>)>,
    current: Option<StreamingOutputBuilder>,
    edits: Vec<VersionEdit>,
    infos: Vec<OutputFileInfo>,
    pending: PendingOutputs,
}

impl<'a> StreamingCompactionWriter<'a> {
    fn new(
        versions: &'a Arc<VersionStore>,
        sst_dir: &'a Path,
        opts: &'a CompactionOptions,
        target_level: usize,
        range_tombstones: &'a Arc<RangeTombstoneSet>,
    ) -> Self {
        Self {
            versions,
            sst_dir,
            opts,
            target_level,
            range_tombstones,
            point_ranges: Vec::new(),
            current: None,
            edits: Vec::new(),
            infos: Vec::new(),
            pending: PendingOutputs::new(Arc::clone(&opts.env)),
        }
    }

    fn add_group(&mut self, entries: &[(Vec<u8>, Vec<u8>)]) -> std::io::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        if self
            .current
            .as_ref()
            .is_some_and(|current| current.estimated_size >= self.opts.target_file_size)
        {
            self.finish_current()?;
        }

        // `ensure_current` either installs an output or returns an
        // error, so `current` is `Some` on this line.
        self.ensure_current()?;
        let current = self.current.as_mut().expect("current output is open");
        let user_key = user_key_of(&entries[0].0);
        if current.smallest_user_key.is_none() {
            current.smallest_user_key = Some(user_key.to_vec());
        }
        current.largest_user_key = Some(user_key.to_vec());
        for (ik, value) in entries {
            current.writer.add(ik, value)?;
            current.estimated_size += (ik.len() + value.len()) as u64;
        }
        Ok(())
    }

    fn finish(mut self) -> std::io::Result<CompactionOutputs> {
        self.finish_current()?;
        self.write_uncovered_range_tombstones()?;
        Ok(CompactionOutputs {
            pending: self.pending.take(),
            edits: std::mem::take(&mut self.edits),
            infos: std::mem::take(&mut self.infos),
        })
    }

    fn ensure_current(&mut self) -> std::io::Result<()> {
        if self.current.is_some() {
            return Ok(());
        }

        let file_id = self.versions.take_file_id()?;

        let path = self.sst_dir.join(sst_filename(file_id));
        self.pending.track(path.clone());
        let mut writer = SsTableWriter::new_in(
            &self.opts.env,
            &path,
            self.opts.block_size,
            self.opts.bloom_bits_per_key,
            self.opts.compression_for_level(self.target_level),
            self.opts.prefix_extractor.clone(),
            self.opts.partitioned_index,
            self.opts.metadata_block_size,
            self.opts.keyring.as_deref(),
        )?;
        // The output carries the tombstones of these that reach its keys
        // (`finish_current`), so what they hide of it is counted with its
        // deletions as it is written.
        if !self.range_tombstones.is_empty() {
            writer.hidden_by(Arc::clone(self.range_tombstones));
        }

        self.current = Some(StreamingOutputBuilder {
            file_id,
            path,
            writer,
            estimated_size: 0,
            smallest_user_key: None,
            largest_user_key: None,
        });
        Ok(())
    }

    fn finish_current(&mut self) -> std::io::Result<()> {
        let Some(mut current) = self.current.take() else {
            return Ok(());
        };

        let point_range = current
            .smallest_user_key
            .as_ref()
            .zip(current.largest_user_key.as_ref())
            .map(|(smallest, largest)| (smallest.clone(), exclusive_successor(largest)));

        if let Some((start, end)) = &point_range {
            for rt in self.range_tombstones.clipped_overlaps(start, end) {
                current
                    .writer
                    .add_range_tombstone(&rt.start, &rt.end, rt.seq);
            }
        }

        let summary = match current.writer.finish()? {
            Some(summary) => summary,
            None => {
                let _ = self.opts.env.remove_file(&current.path);
                return Ok(());
            }
        };

        let num_entries = summary.num_entries;
        let file_size = self.opts.env.metadata(&current.path)?.len;

        // On a worker only: a caller's own compaction is never throttled.
        crate::rate_limiter::throttle_background(self.opts.rate_limiter.as_ref(), file_size)?;

        if self.opts.evict_compaction_data_from_page_cache {
            self.opts.env.drop_page_cache(&current.path);
        }

        let reader = Arc::new(SsTableReader::open_with(
            &self.opts.env,
            &current.path,
            current.file_id,
            self.opts.metadata_policy(),
            self.opts.keyring.as_deref(),
        )?);
        let (smallest_key, largest_key) = match (
            current.smallest_user_key.take(),
            current.largest_user_key.take(),
        ) {
            (Some(smallest), Some(largest)) => {
                if let Some((start, end)) = point_range {
                    self.point_ranges.push((start, end));
                }
                (smallest, largest)
            }
            _ => (summary.smallest_user_key, summary.largest_user_key),
        };
        let new_file = LiveSst::new(
            SsTableMeta {
                file_id: current.file_id,
                smallest_key,
                largest_key,
                file_size,
                num_entries,
                global_seq: None,
                deletions: Some(summary.deletions),
            },
            reader,
            summary.newest_seq,
        );

        self.infos.push(OutputFileInfo {
            file_id: current.file_id,
            path: current.path.clone(),
            file_size,
            num_entries,
        });

        self.edits.push(VersionEdit::AddFile {
            level: self.target_level,
            file: new_file,
        });

        Ok(())
    }

    fn write_uncovered_range_tombstones(&mut self) -> std::io::Result<()> {
        if self.range_tombstones.is_empty() {
            return Ok(());
        }

        let fragments = uncovered_range_tombstone_fragments(
            self.range_tombstones.as_slice(),
            &self.point_ranges,
        );
        if fragments.is_empty() {
            return Ok(());
        }

        for chunk in group_range_tombstone_fragments(fragments) {
            self.write_range_tombstone_only_file(&chunk)?;
        }

        Ok(())
    }

    fn write_range_tombstone_only_file(
        &mut self,
        tombstones: &[RangeTombstone],
    ) -> std::io::Result<()> {
        if tombstones.is_empty() {
            return Ok(());
        }

        let file_id = self.versions.take_file_id()?;

        let path = self.sst_dir.join(sst_filename(file_id));
        self.pending.track(path.clone());
        let mut writer = SsTableWriter::new_in(
            &self.opts.env,
            &path,
            self.opts.block_size,
            self.opts.bloom_bits_per_key,
            self.opts.compression_for_level(self.target_level),
            self.opts.prefix_extractor.clone(),
            self.opts.partitioned_index,
            self.opts.metadata_block_size,
            self.opts.keyring.as_deref(),
        )?;
        for rt in tombstones {
            writer.add_range_tombstone(&rt.start, &rt.end, rt.seq);
        }

        let Some(summary) = writer.finish()? else {
            let _ = self.opts.env.remove_file(&path);
            return Ok(());
        };

        let file_size = self.opts.env.metadata(&path)?.len;

        // On a worker only: a caller's own compaction is never throttled.
        crate::rate_limiter::throttle_background(self.opts.rate_limiter.as_ref(), file_size)?;

        if self.opts.evict_compaction_data_from_page_cache {
            self.opts.env.drop_page_cache(&path);
        }

        let reader = Arc::new(SsTableReader::open_with(
            &self.opts.env,
            &path,
            file_id,
            self.opts.metadata_policy(),
            self.opts.keyring.as_deref(),
        )?);
        let new_file = LiveSst::new(
            SsTableMeta {
                file_id,
                smallest_key: summary.smallest_user_key,
                largest_key: summary.largest_user_key,
                file_size,
                num_entries: summary.num_entries,
                global_seq: None,
                deletions: Some(summary.deletions),
            },
            reader,
            summary.newest_seq,
        );

        self.infos.push(OutputFileInfo {
            file_id,
            path: path.clone(),
            file_size,
            num_entries: summary.num_entries,
        });

        self.edits.push(VersionEdit::AddFile {
            level: self.target_level,
            file: new_file,
        });

        Ok(())
    }
}

fn uncovered_range_tombstone_fragments(
    tombstones: &[RangeTombstone],
    covered_ranges: &[(Vec<u8>, Vec<u8>)],
) -> Vec<RangeTombstone> {
    let covered_ranges = merge_key_ranges(covered_ranges);
    let mut fragments = Vec::new();

    for rt in tombstones {
        let mut cursor = rt.start.clone();
        for (covered_start, covered_end) in &covered_ranges {
            if covered_end.as_slice() <= cursor.as_slice() {
                continue;
            }
            if covered_start.as_slice() >= rt.end.as_slice() {
                break;
            }

            if cursor.as_slice() < covered_start.as_slice() {
                let fragment_end = min_key(covered_start, &rt.end);
                if cursor.as_slice() < fragment_end.as_slice() {
                    fragments.push(RangeTombstone::new(cursor.clone(), fragment_end, rt.seq));
                }
            }

            if cursor.as_slice() < covered_end.as_slice() {
                cursor = max_key(covered_end, &cursor);
            }
            if cursor.as_slice() >= rt.end.as_slice() {
                break;
            }
        }

        if cursor.as_slice() < rt.end.as_slice() {
            fragments.push(RangeTombstone::new(cursor, rt.end.clone(), rt.seq));
        }
    }

    sort_dedup_tombstones(&mut fragments);
    fragments
}

fn group_range_tombstone_fragments(mut fragments: Vec<RangeTombstone>) -> Vec<Vec<RangeTombstone>> {
    sort_dedup_tombstones(&mut fragments);
    let mut groups: Vec<Vec<RangeTombstone>> = Vec::new();
    let mut current: Vec<RangeTombstone> = Vec::new();
    let mut current_end: Option<Vec<u8>> = None;

    for rt in fragments {
        if let Some(end) = &current_end
            && rt.start.as_slice() > end.as_slice()
        {
            groups.push(std::mem::take(&mut current));
            current_end = None;
        }

        if current_end
            .as_ref()
            .is_none_or(|end| end.as_slice() < rt.end.as_slice())
        {
            current_end = Some(rt.end.clone());
        }
        current.push(rt);
    }

    if !current.is_empty() {
        groups.push(current);
    }

    groups
}

fn merge_key_ranges(ranges: &[(Vec<u8>, Vec<u8>)]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut ranges: Vec<(Vec<u8>, Vec<u8>)> = ranges
        .iter()
        .filter(|(start, end)| start < end)
        .cloned()
        .collect();
    ranges.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let mut merged: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for (start, end) in ranges {
        match merged.last_mut() {
            Some((_, merged_end)) if start.as_slice() <= merged_end.as_slice() => {
                if merged_end.as_slice() < end.as_slice() {
                    *merged_end = end;
                }
            }
            _ => merged.push((start, end)),
        }
    }
    merged
}

fn min_key(a: &[u8], b: &[u8]) -> Vec<u8> {
    if a <= b { a.to_vec() } else { b.to_vec() }
}

fn max_key(a: &[u8], b: &[u8]) -> Vec<u8> {
    if a >= b { a.to_vec() } else { b.to_vec() }
}

fn key_range(files: &[Arc<LiveSst>]) -> (Vec<u8>, Vec<u8>) {
    let mut min_key = files[0].meta.smallest_key.clone();
    let mut max_key = files[0].meta.largest_key.clone();

    for file in &files[1..] {
        if file.meta.smallest_key < min_key {
            min_key = file.meta.smallest_key.clone();
        }
        if file.meta.largest_key > max_key {
            max_key = file.meta.largest_key.clone();
        }
    }

    (min_key, max_key)
}

fn find_overlapping(files: &[Arc<LiveSst>], min_key: &[u8], max_key: &[u8]) -> Vec<Arc<LiveSst>> {
    overlapping(files, min_key, max_key).to_vec()
}

/// Tell `listeners` each of `files` was removed, once its version edit is
/// applied and its name unlinked: the one place a compaction says so, after
/// a merge and after a FIFO pass alike.
fn tell_deleted<'a>(
    listeners: &[Arc<dyn crate::event_listener::EventListener>],
    sst_dir: &Path,
    files: impl IntoIterator<Item = &'a Arc<LiveSst>>,
) {
    if listeners.is_empty() {
        return;
    }
    for file in files {
        let info = crate::event_listener::TableFileDeletionInfo {
            file_id: file.meta.file_id,
            file_path: sst_dir.join(sst_filename(file.meta.file_id)),
        };
        crate::event_listener::dispatch(listeners, |l| l.on_table_file_deleted(&info));
    }
}

/// Evict each of `files` from the block cache and unlink it, best effort:
/// a failure is logged and left for the next open's sweep.
fn delete_old_files<'a>(
    env: &dyn Env,
    sst_dir: &Path,
    files: impl IntoIterator<Item = &'a Arc<LiveSst>>,
    cache: &BlockCache,
) {
    for file in files {
        let path = sst_dir.join(sst_filename(file.meta.file_id));
        cache.evict_file(file.meta.file_id);
        if let Err(e) = remove_sst_in(env, &path) {
            tracing::warn!(
                file_id = file.meta.file_id,
                error = %e,
                "Failed to delete old SSTable"
            );
        }
    }
}

#[cfg(test)]
mod cache_tests;
#[cfg(test)]
mod fifo_order_tests;
#[cfg(test)]
mod retire_tests;
#[cfg(test)]
mod universal_order_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory lock that excludes nothing: what a scheduler built
    /// without an engine is handed, so a test can count its shares.
    struct NoLock;

    impl FileLock for NoLock {}

    fn no_lock() -> Arc<dyn FileLock> {
        Arc::new(NoLock)
    }

    fn rt(start: &[u8], end: &[u8], seq: u64) -> RangeTombstone {
        RangeTombstone::new(start.to_vec(), end.to_vec(), seq)
    }

    #[test]
    fn db_open_returns_an_error_when_a_compaction_worker_cannot_spawn() {
        let dir = tempfile::tempdir().unwrap();

        let err = {
            let _guard = SpawnFailureGuard::allowing(0);
            crate::Db::open(dir.path(), crate::Options::default()).unwrap_err()
        };
        match err {
            crate::Error::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::Unsupported),
            other => panic!("expected an I/O error, got {other:?}"),
        }

        // The failed open released the directory lock and left no
        // half-built database behind.
        let db = crate::Db::open(dir.path(), crate::Options::default()).unwrap();
        db.put(b"k", b"v").unwrap();
        assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"v"[..]));
    }

    /// A pass picks its inputs, and before it claims them another pass
    /// compacts the same tables, installs its outputs and lets go. The first
    /// pass's claims succeed, on tables that are no longer in the version:
    /// it must find that out and report `Contended`, never compact them a
    /// second time.
    #[test]
    fn a_pass_whose_pick_went_stale_before_its_claim_backs_off() {
        let dir = tempfile::tempdir().unwrap();
        let db = std::rc::Rc::new(
            crate::Db::open(
                dir.path(),
                crate::Options::default()
                    .max_background_compactions(0)
                    .l0_compaction_trigger(2),
            )
            .unwrap(),
        );
        // Two L0 tables, which is the trigger, with nothing compacting them
        // yet: a flush only owes a step to the next write, and no write
        // follows the second flush.
        for table in 0..2u32 {
            for i in 0..40u32 {
                db.put(
                    format!("k{i:03}").as_bytes(),
                    format!("v{table}").as_bytes(),
                )
                .unwrap();
            }
            db.flush().unwrap();
        }
        assert_eq!(
            db.engine().level_file_count(0),
            2,
            "the pass needs its work"
        );

        let raced = std::rc::Rc::new(std::cell::Cell::new(None));
        AFTER_PICK.with(|hook| {
            let (db, raced) = (std::rc::Rc::clone(&db), std::rc::Rc::clone(&raced));
            *hook.borrow_mut() = Some(Box::new(move || {
                raced.set(Some(db.compact_step().unwrap()));
            }));
        });
        let outer = db.compact_step().unwrap();

        assert_eq!(
            raced.get(),
            Some(CompactionOutcome::DidWork),
            "the racing pass compacted the picked tables"
        );
        assert_eq!(
            outer,
            CompactionOutcome::Contended,
            "the first pass compacted tables another pass had already replaced"
        );
        assert_eq!(db.engine().level_file_count(0), 0);
        assert!(
            db.engine().compaction_claims_idle(),
            "a claim was left held"
        );
        // Every key once, at its newest value.
        let mut iter = db.iter();
        iter.seek_to_first();
        let mut seen = 0;
        while iter.valid() {
            assert_eq!(iter.value(), Some(&b"v1"[..]));
            seen += 1;
            iter.next();
        }
        assert_eq!(seen, 40, "a key was lost or doubled");
    }

    /// Callers hand jobs over and wake the workers from many threads while a
    /// shutdown stops them: every job a hand-over accepted is run or settled
    /// exactly once, every refused one comes back, and the shutdown joins.
    #[test]
    fn jobs_handed_over_while_the_workers_stop_are_each_run_or_settled_once() {
        use std::sync::atomic::AtomicUsize;

        struct Counted {
            ran: Arc<AtomicUsize>,
            settled: Arc<AtomicUsize>,
        }
        impl JobBody for Counted {
            fn run(self: Box<Self>) {
                self.ran.fetch_add(1, Ordering::SeqCst);
            }
            fn release(self: Box<Self>) {
                self.settled.fetch_add(1, Ordering::SeqCst);
            }
        }

        const SUBMITTERS: usize = 4;
        const JOBS: usize = 300;
        for round in 0..20 {
            let dir = tempfile::tempdir().unwrap();
            let sst_dir = dir.path().join("sst");
            std::fs::create_dir_all(&sst_dir).unwrap();
            let versions = Arc::new(VersionStore::new(
                super::super::manifest::VersionSet::open(dir.path(), &sst_dir).unwrap(),
            ));
            let scheduler = CompactionScheduler::start(
                no_lock(),
                CompactionGate::new(),
                Arc::new(SnapshotRegistry::new()),
                Arc::clone(&versions),
                Arc::from(sst_dir.as_path()),
                Arc::new(BlockCache::new(4096)),
                CompactionOptions {
                    max_background_compactions: 2,
                    ..CompactionOptions::default()
                },
                Arc::new(crate::engine::StallSignal::for_tests(&versions)),
                Arc::new(Claims::new()),
                Arc::new(BackgroundHealth::default()),
            )
            .unwrap();
            assert!(scheduler.has_workers());
            let (ran, settled) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
            let (accepted, refused) = (AtomicUsize::new(0), AtomicUsize::new(0));
            std::thread::scope(|scope| {
                for _ in 0..SUBMITTERS {
                    scope.spawn(|| {
                        for _ in 0..JOBS {
                            let job = Box::new(Counted {
                                ran: Arc::clone(&ran),
                                settled: Arc::clone(&settled),
                            });
                            match scheduler.submit(job) {
                                Ok(()) => accepted.fetch_add(1, Ordering::SeqCst),
                                Err(_) => refused.fetch_add(1, Ordering::SeqCst),
                            };
                            scheduler.notify();
                        }
                    });
                }
                // Stops the workers part way through the hand-overs, later
                // each round.
                scope.spawn(|| {
                    while accepted.load(Ordering::SeqCst) + refused.load(Ordering::SeqCst)
                        < round * SUBMITTERS * JOBS / 20
                    {
                        std::hint::spin_loop();
                    }
                    scheduler.shutdown();
                });
            });
            // A second shutdown finds nothing left to join or settle.
            scheduler.shutdown();
            let (accepted, refused) = (
                accepted.load(Ordering::SeqCst),
                refused.load(Ordering::SeqCst),
            );
            assert_eq!(accepted + refused, SUBMITTERS * JOBS);
            assert_eq!(
                ran.load(Ordering::SeqCst) + settled.load(Ordering::SeqCst),
                accepted,
                "round {round}: an accepted job was dropped or handled twice"
            );
            assert!(
                scheduler
                    .submit(Box::new(Counted { ran, settled }))
                    .is_err(),
                "a stopped scheduler accepted a job"
            );
        }
    }

    /// Each worker holds a share of the directory until it has returned, so
    /// an engine torn down on a worker leaves the directory locked while
    /// that worker runs.
    #[test]
    fn every_worker_holds_the_directory_until_it_returns() {
        let dir = tempfile::tempdir().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();
        let versions = Arc::new(VersionStore::new(
            super::super::manifest::VersionSet::open(dir.path(), &sst_dir).unwrap(),
        ));
        let directory = no_lock();
        let scheduler = CompactionScheduler::start(
            Arc::clone(&directory),
            CompactionGate::new(),
            Arc::new(SnapshotRegistry::new()),
            Arc::clone(&versions),
            Arc::from(sst_dir.as_path()),
            Arc::new(BlockCache::new(4096)),
            CompactionOptions {
                max_background_compactions: 2,
                ..CompactionOptions::default()
            },
            Arc::new(crate::engine::StallSignal::for_tests(&versions)),
            Arc::new(Claims::new()),
            Arc::new(BackgroundHealth::default()),
        )
        .unwrap();
        assert_eq!(Arc::strong_count(&directory), 3, "one share per worker");
        scheduler.shutdown();
        assert_eq!(Arc::strong_count(&directory), 1, "a joined worker let go");
    }

    #[test]
    fn failed_start_joins_the_workers_it_already_spawned() {
        let dir = tempfile::tempdir().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();

        let versions = Arc::new(VersionStore::new(
            super::super::manifest::VersionSet::open(dir.path(), &sst_dir).unwrap(),
        ));
        let opts = CompactionOptions {
            max_background_compactions: 3,
            ..CompactionOptions::default()
        };

        let directory = no_lock();
        let started = {
            let _guard = SpawnFailureGuard::allowing(2);
            CompactionScheduler::start(
                Arc::clone(&directory),
                CompactionGate::new(),
                Arc::new(SnapshotRegistry::new()),
                Arc::clone(&versions),
                Arc::from(sst_dir.as_path()),
                Arc::new(BlockCache::new(4096)),
                opts,
                Arc::new(crate::engine::StallSignal::for_tests(&versions)),
                Arc::new(Claims::new()),
                Arc::new(BackgroundHealth::default()),
            )
        };

        match started {
            Ok(_) => panic!("expected the third worker spawn to fail"),
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::Unsupported),
        }
        // Every worker that did start has exited and been joined, so
        // its clone of the version set is gone, and so is its share of the
        // directory.
        assert_eq!(Arc::strong_count(&versions), 1);
        assert_eq!(Arc::strong_count(&directory), 1);
    }

    #[test]
    fn uncovered_fragments_split_around_point_ranges() {
        let fragments = uncovered_range_tombstone_fragments(
            &[rt(b"a", b"z", 7)],
            &[(b"m".to_vec(), b"m\0".to_vec())],
        );

        assert_eq!(fragments.len(), 2);
        assert_eq!(fragments[0].start, b"a");
        assert_eq!(fragments[0].end, b"m");
        assert_eq!(fragments[0].seq, 7);
        assert_eq!(fragments[1].start, b"m\0");
        assert_eq!(fragments[1].end, b"z");
        assert_eq!(fragments[1].seq, 7);
    }

    #[test]
    fn uncovered_fragments_drop_fully_covered_ranges() {
        let fragments = uncovered_range_tombstone_fragments(
            &[rt(b"b", b"d", 3)],
            &[(b"a".to_vec(), b"z".to_vec())],
        );
        assert!(fragments.is_empty());
    }

    #[test]
    fn tombstone_fragments_group_overlapping_gaps() {
        let groups = group_range_tombstone_fragments(vec![
            rt(b"a", b"c", 1),
            rt(b"b", b"d", 2),
            rt(b"f", b"g", 3),
        ]);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 2);
        assert_eq!(groups[1].len(), 1);
    }

    #[test]
    fn failed_open_preserves_data_already_written() {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = crate::Db::open(dir.path(), crate::Options::default()).unwrap();
            for i in 0..300 {
                db.put(format!("k{i:04}").as_bytes(), format!("v{i:04}").as_bytes())
                    .unwrap();
            }
        }

        for _ in 0..3 {
            let _guard = SpawnFailureGuard::allowing(0);
            assert!(crate::Db::open(dir.path(), crate::Options::default()).is_err());
        }

        let db = crate::Db::open(dir.path(), crate::Options::default()).unwrap();
        for i in 0..300 {
            assert_eq!(
                db.get(format!("k{i:04}").as_bytes()).unwrap().as_deref(),
                Some(format!("v{i:04}").as_bytes()),
                "key {i} lost across failed opens"
            );
        }
    }

    #[test]
    fn failed_start_with_many_workers_returns_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();
        let versions = Arc::new(VersionStore::new(
            super::super::manifest::VersionSet::open(dir.path(), &sst_dir).unwrap(),
        ));
        let opts = CompactionOptions {
            max_background_compactions: 8,
            ..CompactionOptions::default()
        };
        let start = std::time::Instant::now();
        let started = {
            let _guard = SpawnFailureGuard::allowing(7);
            CompactionScheduler::start(
                no_lock(),
                CompactionGate::new(),
                Arc::new(SnapshotRegistry::new()),
                Arc::clone(&versions),
                Arc::from(sst_dir.as_path()),
                Arc::new(BlockCache::new(4096)),
                opts,
                Arc::new(crate::engine::StallSignal::for_tests(&versions)),
                Arc::new(Claims::new()),
                Arc::new(BackgroundHealth::default()),
            )
        };
        let elapsed = start.elapsed();
        assert!(started.is_err());
        println!("failed start with 8 workers took {elapsed:?}");
        assert_eq!(Arc::strong_count(&versions), 1);
    }
}
