//! A writeback error kept for the next sync of a log (E46, Apple).
//!
//! On Apple the log writeback lane starts the writeback of a log with a
//! plain `fsync` ([`crate::env::LogFile::start_writeback`]), whose result
//! nothing waits for, while the log's own sync is `F_FULLFSYNC`. The host
//! may report a failed write of a page to one sync of the file and not to
//! the next, so an error the lane's `fsync` got would be lost: the next
//! `F_FULLFSYNC` could succeed, and the pipeline would take bytes for
//! durable that never reached the drive. So the error is kept here, and the
//! next sync that succeeds reports it instead of success.
//!
//! # Invariants
//!
//! - **No sync passes a writeback in flight.** The writeback runs, and its
//!   error is kept, under the same lock a sync takes before it begins, so a
//!   sync either sees the error kept or begins after the writeback ended.
//! - **Kept until reported.** A sync that fails leaves the error kept for
//!   the next one; the first sync that succeeds reports it and forgets it.
//!   The first error kept is the one reported.

use std::io;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// See the module docs.
#[derive(Debug, Default)]
pub(crate) struct DeferredError(Mutex<Option<io::Error>>);

impl DeferredError {
    fn lock(&self) -> MutexGuard<'_, Option<io::Error>> {
        // The slot is an `Option` written whole: a panic elsewhere leaves it
        // as valid as before.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Run `writeback`, and keep its error, if any, for the next sync. An
    /// error already kept is the one that stays.
    pub(crate) fn writeback(&self, writeback: impl FnOnce() -> io::Result<()>) {
        let mut kept = self.lock();
        if let Err(err) = writeback() {
            kept.get_or_insert(err);
        }
    }

    /// Run `sync`. One that fails returns its own error and leaves a kept
    /// one for the next sync; one that succeeds returns the error a
    /// writeback kept, if any, and forgets it.
    pub(crate) fn sync(&self, sync: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        let mut kept = self.lock();
        sync()?;
        kept.take().map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    fn failed(what: &str) -> io::Result<()> {
        Err(io::Error::other(what.to_string()))
    }

    /// A writeback's error is reported by the next sync that succeeds, once.
    #[test]
    fn a_failed_writeback_is_reported_by_the_next_sync() {
        let deferred = DeferredError::default();
        deferred.writeback(|| failed("the writeback"));
        let err = deferred.sync(|| Ok(())).unwrap_err();
        assert_eq!(err.to_string(), "the writeback");
        deferred.sync(|| Ok(())).unwrap();
    }

    /// A sync that fails reports its own error and keeps the writeback's for
    /// the next sync; the first writeback error is the one kept.
    #[test]
    fn a_failed_sync_keeps_the_writeback_error_for_the_next() {
        let deferred = DeferredError::default();
        deferred.writeback(|| failed("first"));
        deferred.writeback(|| failed("second"));
        deferred.writeback(|| Ok(()));
        let err = deferred.sync(|| failed("the sync")).unwrap_err();
        assert_eq!(err.to_string(), "the sync");
        let err = deferred.sync(|| Ok(())).unwrap_err();
        assert_eq!(err.to_string(), "first");
        deferred.sync(|| Ok(())).unwrap();
    }

    /// A writeback that succeeds keeps nothing.
    #[test]
    fn a_writeback_that_succeeds_keeps_nothing() {
        let deferred = DeferredError::default();
        deferred.writeback(|| Ok(()));
        deferred.sync(|| Ok(())).unwrap();
    }

    /// A sync that begins while a writeback is in flight runs after it, and
    /// reports its error: no sync passes a writeback in flight.
    #[test]
    fn a_sync_waits_for_a_writeback_in_flight_and_reports_it() {
        let deferred = Arc::new(DeferredError::default());
        let inside = Arc::new(AtomicBool::new(false));
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let writer = {
            let deferred = Arc::clone(&deferred);
            let inside = Arc::clone(&inside);
            std::thread::spawn(move || {
                deferred.writeback(|| {
                    inside.store(true, Ordering::SeqCst);
                    wait.recv().unwrap();
                    failed("in flight")
                });
            })
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !inside.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "the writeback did not begin"
            );
            std::thread::yield_now();
        }
        let syncing = {
            let deferred = Arc::clone(&deferred);
            std::thread::spawn(move || deferred.sync(|| Ok(())))
        };
        go.send(()).unwrap();
        writer.join().unwrap();
        let err = syncing.join().unwrap().unwrap_err();
        assert_eq!(err.to_string(), "in flight");
    }
}
