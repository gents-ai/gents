//! Whether the engine's background jobs are currently failing, and why.
//!
//! A flush or compaction that fails does not stop the engine: the job is
//! retried later. That is the right response to a passing fault and the
//! wrong one to a lasting one. While the disk is full, the descriptor
//! table is exhausted, or a file the engine needs has gone missing, every
//! retry fails the same way, and a writer stalled behind that work would
//! wait for it forever. The engine records the most recent failure of
//! each job here so a stalled writer can fail with the real cause instead,
//! and clears it on the job's next success.
//!
//! Each job's slot is one atomically swapped pointer ([`kovan::Atom`]), so
//! recording and reading never wait on each other: a healthy job's success
//! is one load, a failure is one swap, and a stalled writer reads the
//! failure whole, never half of one and half of the next.
//!
//! A writer that is stopped while the failure is recorded is told it at
//! once (`RegolithEngine::stopped`). One that was already waiting on the
//! stall's unit is told the same, at the same moment: recording a failure
//! lands the unit (E36), and the writer, run again, finds the failure
//! here. A failure is stored before the unit is taken, and a writer reads
//! it again once its wait is registered (`RegolithEngine::stall_wait`), so
//! none registers too late to be told.

use std::io;
use std::sync::Arc;

use kovan::Atom;

use super::commit::StallSignal;
use crate::portability::{AtomicU64, Ordering};

/// The background jobs a stalled writer can be waiting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Job {
    Flush,
    Compaction,
}

impl Job {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Job::Flush => "flush",
            Job::Compaction => "compaction",
        }
    }
}

/// The class of a failure, for operators deciding what to fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hazard {
    DiskFull,
    OutOfFileDescriptors,
    MissingFile,
    Other,
}

impl Hazard {
    pub(crate) fn of(err: &io::Error) -> Self {
        match err.kind() {
            io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => return Hazard::DiskFull,
            io::ErrorKind::NotFound => return Hazard::MissingFile,
            _ => {}
        }
        if is_descriptor_exhaustion(err) {
            return Hazard::OutOfFileDescriptors;
        }
        Hazard::Other
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Hazard::DiskFull => "disk full",
            Hazard::OutOfFileDescriptors => "out of file descriptors",
            Hazard::MissingFile => "missing file",
            Hazard::Other => "i/o error",
        }
    }
}

/// EMFILE and ENFILE have no `io::ErrorKind` of their own.
#[cfg(unix)]
fn is_descriptor_exhaustion(err: &io::Error) -> bool {
    const ENFILE: i32 = 23;
    const EMFILE: i32 = 24;
    matches!(err.raw_os_error(), Some(ENFILE | EMFILE))
}

#[cfg(windows)]
fn is_descriptor_exhaustion(err: &io::Error) -> bool {
    const ERROR_TOO_MANY_OPEN_FILES: i32 = 4;
    err.raw_os_error() == Some(ERROR_TOO_MANY_OPEN_FILES)
}

#[cfg(not(any(unix, windows)))]
fn is_descriptor_exhaustion(_err: &io::Error) -> bool {
    false
}

/// A recorded failure. `io::Error` is not `Clone`, so the parts needed to
/// rebuild an equivalent one are kept instead.
#[derive(Debug, Clone)]
pub(crate) struct Failure {
    pub(crate) job: Job,
    pub(crate) hazard: Hazard,
    kind: io::ErrorKind,
    raw_os_error: Option<i32>,
    message: String,
}

impl Failure {
    /// An `io::Error` equivalent to the one recorded: the OS error code
    /// when there was one, so a caller can still match on ENOSPC.
    pub(crate) fn to_io_error(&self) -> io::Error {
        match self.raw_os_error {
            Some(code) => io::Error::from_raw_os_error(code),
            None => io::Error::new(self.kind, self.message.clone()),
        }
    }
    /// The error a writer stalled behind this job returns.
    pub(crate) fn to_error(&self) -> crate::Error {
        crate::Error::BackgroundFailed {
            job: self.job.name(),
            hazard: self.hazard.label(),
            source: self.to_io_error(),
        }
    }
}

pub(crate) struct BackgroundHealth {
    errors: AtomicU64,
    flush: Atom<Option<Failure>>,
    compaction: Atom<Option<Failure>>,
    /// The stall whose stops these jobs relieve: a failure recorded here
    /// lands its unit. `None` where no writer waits on the jobs, as in a
    /// test that runs workers on their own.
    stall: Option<Arc<StallSignal>>,
}

impl Default for BackgroundHealth {
    fn default() -> Self {
        Self {
            errors: AtomicU64::new(0),
            flush: Atom::new(None),
            compaction: Atom::new(None),
            stall: None,
        }
    }
}

impl BackgroundHealth {
    /// The health of the jobs that relieve `stall`'s stops.
    pub(crate) fn relieving(stall: Arc<StallSignal>) -> Self {
        Self {
            stall: Some(stall),
            ..Self::default()
        }
    }

    fn slot(&self, job: Job) -> &Atom<Option<Failure>> {
        match job {
            Job::Flush => &self.flush,
            Job::Compaction => &self.compaction,
        }
    }

    /// Record that an attempt of `job` failed with `err`, and tell the
    /// writers waiting on the stall (module docs): from now until the job
    /// next succeeds, a write stopped behind it is told this failure.
    pub(crate) fn record_failure(&self, job: Job, err: &io::Error) -> Hazard {
        self.count_failure();
        let hazard = Hazard::of(err);
        self.slot(job).store(Some(Failure {
            job,
            hazard,
            kind: err.kind(),
            raw_os_error: err.raw_os_error(),
            message: err.to_string(),
        }));
        // Stored above, landed here: a writer that registers after this
        // take reads the failure itself.
        if let Some(stall) = &self.stall {
            stall.land();
        }
        hazard
    }

    /// Count a failed attempt without recording it as the failure a stopped
    /// writer is told: a compaction pass a caller's thread ran
    /// (`RegolithEngine::compaction_failed`).
    pub(crate) fn count_failure(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Clear the failure this success follows. One load while the job is
    /// healthy. A failure another thread records between the load and the
    /// swap is newer than this success and stays.
    pub(crate) fn record_success(&self, job: Job) {
        let slot = self.slot(job);
        let seen = slot.load();
        if seen.is_some() {
            Self::clear(slot, &seen);
        }
    }

    /// Empty `slot` if it still holds the failure `seen`.
    fn clear(slot: &Atom<Option<Failure>>, seen: &kovan::AtomGuard<'_, Option<Failure>>) {
        let _ = slot.compare_and_swap(seen, None);
    }

    /// The most recent failure of `job`, if its last attempt failed.
    pub(crate) fn failing(&self, job: Job) -> Option<Failure> {
        self.slot(job).load_clone()
    }

    /// Failed background job attempts since open.
    pub(crate) fn error_count(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_hazards_operators_act_on() {
        let full = io::Error::from(io::ErrorKind::StorageFull);
        assert_eq!(Hazard::of(&full), Hazard::DiskFull);
        let gone = io::Error::from(io::ErrorKind::NotFound);
        assert_eq!(Hazard::of(&gone), Hazard::MissingFile);
        let other = io::Error::other("boom");
        assert_eq!(Hazard::of(&other), Hazard::Other);
    }

    #[cfg(unix)]
    #[test]
    fn classifies_raw_errno_values() {
        assert_eq!(
            Hazard::of(&io::Error::from_raw_os_error(24)),
            Hazard::OutOfFileDescriptors
        );
        assert_eq!(
            Hazard::of(&io::Error::from_raw_os_error(23)),
            Hazard::OutOfFileDescriptors
        );
        // ENOSPC is 28 on every unix regolith targets.
        assert_eq!(
            Hazard::of(&io::Error::from_raw_os_error(28)),
            Hazard::DiskFull
        );
    }

    #[test]
    fn a_failure_is_cleared_by_the_same_jobs_next_success() {
        let health = BackgroundHealth::default();
        health.record_failure(Job::Compaction, &io::Error::other("boom"));
        assert!(health.failing(Job::Compaction).is_some());
        assert!(health.failing(Job::Flush).is_none());

        health.record_success(Job::Flush);
        assert!(health.failing(Job::Compaction).is_some());
        health.record_success(Job::Compaction);
        assert!(health.failing(Job::Compaction).is_none());
        assert_eq!(health.error_count(), 1);
    }

    /// Workers fail and succeed while writers read: a reader sees no failure
    /// or one whole failure of the job it asked about, every failure is
    /// counted, and once the jobs succeed last nothing is left failing.
    #[test]
    fn concurrent_records_never_tear_and_every_failure_is_counted() {
        const THREADS: u64 = 4;
        // Fewer rounds under miri, which runs each a hundred times slower.
        const ROUNDS: u64 = if cfg!(miri) { 40 } else { 2_000 };
        let health = BackgroundHealth::default();
        std::thread::scope(|scope| {
            for t in 0..THREADS {
                let health = &health;
                scope.spawn(move || {
                    let job = if t % 2 == 0 {
                        Job::Flush
                    } else {
                        Job::Compaction
                    };
                    for round in 0..ROUNDS {
                        let err = io::Error::other(format!("{}-{t}-{round}", job.name()));
                        health.record_failure(job, &err);
                        health.record_success(job);
                    }
                });
            }
            for _ in 0..2 {
                let health = &health;
                scope.spawn(move || {
                    for _ in 0..ROUNDS {
                        for job in [Job::Flush, Job::Compaction] {
                            if let Some(failure) = health.failing(job) {
                                assert_eq!(failure.job, job, "a failure of the other job");
                                assert!(
                                    failure.message.starts_with(job.name()),
                                    "a torn failure: {}",
                                    failure.message
                                );
                                assert_eq!(failure.hazard, Hazard::Other);
                            }
                        }
                    }
                });
            }
        });
        assert_eq!(health.error_count(), THREADS * ROUNDS);
        // Each thread's last record was a success, but a success only clears
        // the failure it saw: one that raced a newer failure of the same job
        // left it. One more success per job, with nothing racing, clears it.
        health.record_success(Job::Flush);
        health.record_success(Job::Compaction);
        assert!(health.failing(Job::Flush).is_none());
        assert!(health.failing(Job::Compaction).is_none());
    }

    /// A success never erases a failure recorded after the one it saw.
    #[test]
    fn a_success_clears_only_the_failure_it_saw() {
        let health = BackgroundHealth::default();
        health.record_failure(Job::Flush, &io::Error::other("first"));
        let slot = health.slot(Job::Flush);
        let seen = slot.load();
        health.record_failure(Job::Flush, &io::Error::other("second"));
        // What `record_success` does once it loaded `seen`.
        BackgroundHealth::clear(slot, &seen);
        assert_eq!(
            health.failing(Job::Flush).unwrap().message,
            "second",
            "a success erased a failure recorded after the one it saw"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_rebuilt_error_keeps_the_os_error_code() {
        let health = BackgroundHealth::default();
        health.record_failure(Job::Flush, &io::Error::from_raw_os_error(28));
        let rebuilt = health.failing(Job::Flush).unwrap().to_io_error();
        assert_eq!(rebuilt.raw_os_error(), Some(28));
        assert_eq!(rebuilt.kind(), io::ErrorKind::StorageFull);
    }
}
