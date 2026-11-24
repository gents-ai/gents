//! A common start for benches that time several threads at once.
//!
//! The clock must start before any timed operation can run. A single
//! barrier that the coordinator leaves and then reads the clock after does
//! not hold that: the workers leave the same barrier and start working while
//! the coordinator may still be descheduled, so their first operations land
//! before the window opens and the time per operation reads low. Here the
//! workers wait at two barriers: the first says every worker is ready, the
//! clock is read, and only then does the second let them go, so every timed
//! operation is inside the window.

use std::sync::{Barrier, OnceLock};
use std::time::{Duration, Instant};

/// The line `workers` threads wait at until the coordinator starts the
/// clock. Used once: one line per timed run.
pub struct StartLine {
    ready: Barrier,
    release: Barrier,
    at: OnceLock<Instant>,
}

impl StartLine {
    pub fn new(workers: usize) -> Self {
        StartLine {
            ready: Barrier::new(workers + 1),
            release: Barrier::new(workers + 1),
            at: OnceLock::new(),
        }
    }

    /// A worker's side: wait until the clock runs, and return when it
    /// started, so a worker's own deadline is measured from the same instant
    /// as the coordinator's window.
    pub fn wait(&self) -> Instant {
        self.ready.wait();
        self.release.wait();
        *self
            .at
            .get()
            .expect("the coordinator starts the clock before it lets the workers go")
    }

    /// The coordinator's side, once every worker is spawned: wait until all
    /// are at the line, start the clock, then let them go.
    pub fn start(&self) -> Instant {
        self.all_ready();
        self.go()
    }

    /// The first half of [`Self::start`], for a coordinator that reads
    /// something else (a CPU counter) with every worker parked at the line.
    pub fn all_ready(&self) {
        self.ready.wait();
    }

    /// The second half of [`Self::start`]: start the clock, then let the
    /// workers go.
    pub fn go(&self) -> Instant {
        let now = Instant::now();
        let _ = self.at.set(now);
        self.release.wait();
        now
    }
}

/// How many of `total` calls thread `t` of `threads` runs: `total / threads`,
/// one more for the first `total % threads` threads, so the shares add up to
/// exactly `total`. A criterion `iter_custom` divides its time by the
/// iterations it asked for; every thread running `total / threads` calls, at
/// least one, would time fewer calls than that (or more, below one call per
/// thread) and misstate the time per call.
pub fn share(total: u64, threads: usize, t: usize) -> u64 {
    let threads = threads as u64;
    total / threads + u64::from((t as u64) < total % threads)
}

/// Run `total` calls of `op(thread, i)` spread over `threads` threads, `i`
/// counting each thread's own calls from 0 (thread `t` runs
/// [`share`]`(total, threads, t)` of them), and return the wall time from a
/// common start until the last thread is done.
pub fn on_threads(threads: usize, total: u64, op: impl Fn(usize, u64) + Sync) -> Duration {
    let line = StartLine::new(threads);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|t| {
                let (op, line) = (&op, &line);
                let calls = share(total, threads, t);
                scope.spawn(move || {
                    line.wait();
                    for i in 0..calls {
                        op(t, i);
                    }
                })
            })
            .collect();
        let began = line.start();
        for worker in workers {
            worker.join().expect("bench worker");
        }
        began.elapsed()
    })
}
