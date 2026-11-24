//! Token-bucket rate limiter for background I/O.
//!
//! A [`RateLimiter`] throttles byte-denominated background work (flush and
//! compaction writes) so bursts of it don't saturate the disk and push
//! foreground latency off a cliff. The engine calls [`RateLimiter::request`]
//! with a byte count before it installs what it wrote, and only on its
//! compaction worker threads: a caller's call is never throttled, whether it
//! flushes, compacts a step on its own thread, or runs a job at its queue's
//! poll (plan 4.10). Work regolith runs on a caller's thread is the caller's
//! I/O, paced by the caller.
//!
//! [`TokenBucketRateLimiter`] is the stock implementation: one token bucket
//! kept in a few atomics, with no lock and no condition variable. It is the
//! only implementation regolith ships; the trait is public so callers can
//! drop in their own (e.g. for test harnesses or a shared limiter across
//! multiple databases).

use std::cell::Cell;
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use kovan::Atom;

use crate::portability::{AtomicBool, AtomicU64, Ordering};

// The module's own tests measure real elapsed time to prove the
// limiter actually blocks.
#[cfg(test)]
use std::time::Instant;

/// Priority of a rate-limited I/O request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Priority {
    /// Served at once: it takes its bytes from the bucket without waiting,
    /// so the requests behind it wait for them instead.
    High,
    /// Background work (flush, compaction): waits its turn.
    Low,
}

/// A byte-denominated rate limiter.
///
/// Implementations throttle callers of [`RateLimiter::request`] to the
/// rate they were built for. Requests block until quota is available;
/// shutdown (on drop or via an implementation-specific `stop()` call) must
/// release every blocked request.
///
/// The trait is the one call regolith makes. How a limiter is tuned and
/// what it reports are its own: [`TokenBucketRateLimiter`] has methods for
/// both, on the type.
///
/// regolith calls it only on its compaction worker threads, never on a
/// caller's thread, so a blocking `request` never blocks a caller.
///
/// A panic in this trait's code while a flush runs it fails that flush
/// with [`crate::Error::CallbackPanicked`]; the flush is retried as any
/// failing flush is.
pub trait RateLimiter: Send + Sync + 'static {
    /// Request `bytes` worth of I/O quota. Blocks until the request is
    /// served or the limiter is shut down.
    ///
    /// regolith asks once for each table a flush or a compaction wrote on
    /// a worker, with the table's size on disk, before the table is
    /// installed, and always at [`Priority::Low`].
    fn request(&self, bytes: u64, pri: Priority);
}

thread_local! {
    /// The thread is a compaction worker: the only threads the engine
    /// throttles.
    static ON_WORKER: Cell<bool> = const { Cell::new(false) };
}

/// Mark the calling thread as a compaction worker for the rest of its life.
#[cfg(any(test, not(target_arch = "wasm32")))]
pub(crate) fn mark_worker_thread() {
    ON_WORKER.with(|on| on.set(true));
}

/// Throttle `bytes` of background I/O through `limiter`, on a compaction
/// worker only: the one function every engine call site uses, so no caller's
/// call is ever throttled.
///
/// The limiter is the caller's code: a panic in it fails the flush or the
/// compaction that asked, as [`crate::Error::CallbackPanicked`], instead of
/// unwinding through the worker.
pub(crate) fn throttle_background(
    limiter: Option<&Arc<dyn RateLimiter>>,
    bytes: u64,
) -> std::io::Result<()> {
    let Some(limiter) = limiter else {
        return Ok(());
    };
    if !ON_WORKER.with(Cell::get) {
        return Ok(());
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        limiter.request(bytes, Priority::Low)
    }))
    .map_err(|_| {
        tracing::error!("the rate limiter panicked in background I/O");
        crate::Error::CallbackPanicked {
            callback: "RateLimiter",
            latched: false,
        }
        .into_io_error()
    })
}

/// The longest a blocked request sleeps before it checks for shutdown
/// again.
#[cfg(not(target_arch = "wasm32"))]
const SLEEP_SLICE: Duration = Duration::from_millis(50);

/// Nanoseconds `bytes` take at `rate` bytes per second, saturating.
#[cfg(not(target_arch = "wasm32"))]
fn nanos_for(bytes: u64, rate: u64) -> u64 {
    let nanos = u128::from(bytes).saturating_mul(1_000_000_000) / u128::from(rate.max(1));
    u64::try_from(nanos).unwrap_or(u64::MAX)
}

/// Bytes `nanos` buy at `rate` bytes per second, saturating.
fn bytes_for(nanos: u64, rate: u64) -> u64 {
    let bytes = u128::from(nanos).saturating_mul(u128::from(rate)) / 1_000_000_000;
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

/// Default rate-limiter implementation: one token bucket refilled
/// continuously at `bytes_per_second`, holding at most `burst_bytes`.
///
/// The bucket runs on a *byte clock*: how many bytes it has refilled since
/// it was made, each stretch of time counted at the rate set for it. Its
/// state is one reading of that clock, kept in an atomic: the reading at
/// which every byte reserved so far is paid for (the "theoretical arrival
/// time" of the generic cell rate algorithm, counted in bytes). A request
/// for `n` bytes moves it forward by `n` with one compare-and-swap, from
/// the clock's reading now if the bucket had refilled meanwhile, and may
/// proceed once the clock is within `burst_bytes` of the new reading; until
/// then it sleeps. Reservations are taken in the order the compare-and-swaps
/// land, so requests are served first come, first served, and a request
/// larger than the burst simply waits for the bytes past it. Nobody takes a
/// lock.
///
/// What is owed is counted in bytes, so it holds no rate. A rate change
/// ([`TokenBucketRateLimiter::set_bytes_per_second`]) replaces the clock's
/// pace in one step, from the instant it lands: every byte not yet served,
/// a waiting request's included, is then served at the new rate, and a
/// request racing the change reads the clock at one pace or the other,
/// never a mixture of the two.
///
/// A fresh bucket is full: the first `burst_bytes` are served at once.
pub struct TokenBucketRateLimiter {
    /// The rate and where the byte clock stood when it was set; replaced
    /// whole by a rate change.
    pace: Atom<Pace>,
    burst_bytes: u64,
    /// The byte-clock reading at which every reserved byte is paid for.
    paid: AtomicU64,
    shutdown: AtomicBool,
    total_high: AtomicU64,
    total_low: AtomicU64,
}

/// The pace the byte clock runs at, from one instant on.
struct Pace {
    /// Bytes per second; `0` turns the limiter off.
    rate: u64,
    /// The byte clock's reading at `since`.
    at: u64,
    /// Platform-clock nanoseconds this pace began at.
    since: u64,
}

impl Pace {
    /// The byte clock's reading at platform time `now`.
    fn clock(&self, now: u64) -> u64 {
        self.at
            .saturating_add(bytes_for(now.saturating_sub(self.since), self.rate))
    }
}

impl TokenBucketRateLimiter {
    /// Construct a new limiter.
    ///
    /// * `bytes_per_second` - sustained refill rate. `0` disables the
    ///   limiter (every request is served instantly).
    /// * `burst_bytes` - maximum number of tokens the bucket can hold.
    ///   A fresh bucket starts full so the first `burst_bytes` worth of
    ///   work is served without blocking. Clamped to at least 1.
    ///
    /// Never panics: out-of-range arguments are clamped rather than
    /// asserted, because this is a public constructor.
    pub fn new(bytes_per_second: u64, burst_bytes: u64) -> Self {
        Self {
            pace: Atom::new(Pace {
                rate: bytes_per_second,
                at: 0,
                since: crate::env::platform_nanos().unwrap_or(0),
            }),
            burst_bytes: burst_bytes.max(1),
            paid: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
            total_high: AtomicU64::new(0),
            total_low: AtomicU64::new(0),
        }
    }

    /// Release every blocked request and return. Subsequent calls to
    /// [`RateLimiter::request`] also return immediately without
    /// consuming tokens. Called automatically when the limiter is
    /// dropped.
    pub fn stop(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    /// Reserve `bytes` with the byte clock reading `clock`, and return the
    /// reading at which they are paid for.
    fn reserve(&self, bytes: u64, clock: u64) -> u64 {
        let mut current = self.paid.load(Ordering::Acquire);
        loop {
            // A bucket idle long enough refilled: the debt starts now.
            let next = current.max(clock).saturating_add(bytes);
            match self.paid.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return next,
                Err(actual) => current = actual,
            }
        }
    }

    /// Sleep until the byte clock reads `ready`, or until stopped. `false`
    /// when stopped first. Each slice reads the pace again, so a rate
    /// changed meanwhile paces the bytes still owed.
    #[cfg(not(target_arch = "wasm32"))]
    fn wait_until(&self, ready: u64) -> bool {
        loop {
            if self.shutdown.load(Ordering::Acquire) {
                return false;
            }
            let Some(now) = crate::env::platform_nanos() else {
                return true;
            };
            let (rate, clock) = {
                let pace = self.pace.load();
                (pace.rate, pace.clock(now))
            };
            // A limiter turned off serves what waits on it.
            if rate == 0 || clock >= ready {
                return true;
            }
            let owed = nanos_for(ready - clock, rate).max(1);
            std::thread::sleep(SLEEP_SLICE.min(Duration::from_nanos(owed)));
        }
    }

    /// A target with no thread to sleep serves at once rather than spin:
    /// nothing on it calls a limiter that must block. `false` when stopped.
    #[cfg(target_arch = "wasm32")]
    fn wait_until(&self, _ready: u64) -> bool {
        !self.shutdown.load(Ordering::Acquire)
    }

    fn count(&self, bytes: u64, pri: Priority) {
        match pri {
            Priority::High => self.total_high.fetch_add(bytes, Ordering::Relaxed),
            Priority::Low => self.total_low.fetch_add(bytes, Ordering::Relaxed),
        };
    }

    /// Change the refill rate from this call on. Every byte not yet served
    /// is served at the new rate, a waiting request's included: the bytes
    /// owed are counted in bytes, and the change only sets the pace the
    /// byte clock runs at from now. A limiter turned back on from `0`
    /// starts with a full bucket, as a fresh one does.
    pub fn set_bytes_per_second(&self, bytes_per_second: u64) {
        loop {
            let now = crate::env::platform_nanos().unwrap_or(0);
            let current = self.pace.load();
            let mut at = current.clock(now);
            if current.rate == 0 {
                // Nothing was paced while off: nothing is owed from then.
                at = at.max(self.paid.load(Ordering::Acquire));
            }
            let next = Pace {
                rate: bytes_per_second,
                at,
                since: now,
            };
            #[cfg(test)]
            in_rate_change();
            if self.pace.compare_and_swap(&current, next).is_ok() {
                return;
            }
        }
    }

    /// Test-only: whether more than `bytes` are reserved and not yet paid
    /// for.
    #[cfg(test)]
    fn owes_more_than(&self, bytes: u64) -> bool {
        let Some(now) = crate::env::platform_nanos() else {
            return false;
        };
        let clock = self.pace.load().clock(now);
        self.paid.load(Ordering::Acquire).saturating_sub(clock) > bytes
    }

    /// The refill rate, in bytes per second.
    pub fn get_bytes_per_second(&self) -> u64 {
        self.pace.load().rate
    }

    /// Total bytes served at `pri` since construction. Excludes requests
    /// still waiting.
    pub fn get_total_bytes_through(&self, pri: Priority) -> u64 {
        match pri {
            Priority::High => self.total_high.load(Ordering::Relaxed),
            Priority::Low => self.total_low.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Test seam: runs once, on this thread, in the middle of the next rate
    /// change, where a request on another thread could land.
    static IN_RATE_CHANGE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run the step a test set for this instant, if any.
#[cfg(test)]
fn in_rate_change() {
    if let Some(step) = IN_RATE_CHANGE.with(|hook| hook.borrow_mut().take()) {
        step();
    }
}

impl Drop for TokenBucketRateLimiter {
    fn drop(&mut self) {
        self.stop();
    }
}

impl RateLimiter for TokenBucketRateLimiter {
    fn request(&self, bytes: u64, pri: Priority) {
        if self.shutdown.load(Ordering::Acquire) {
            return;
        }
        // Rate limiting is a function of elapsed time. A platform with no
        // monotonic clock cannot measure it, so the limiter serves every
        // request at once instead of blocking on a bucket that could never
        // refill; a rate of zero turns it off.
        let clock = crate::env::platform_nanos().and_then(|now| {
            let pace = self.pace.load();
            (pace.rate > 0 && bytes > 0).then(|| pace.clock(now))
        });
        let Some(clock) = clock else {
            self.count(bytes, pri);
            return;
        };
        let paid = self.reserve(bytes, clock);
        if pri == Priority::High || self.wait_until(paid.saturating_sub(self.burst_bytes)) {
            self.count(bytes, pri);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::thread;

    #[test]
    fn single_request_within_burst_is_instant() {
        let lim = TokenBucketRateLimiter::new(1_000_000, 1_000_000);
        let start = Instant::now();
        lim.request(500_000, Priority::Low);
        assert!(start.elapsed() < Duration::from_millis(50));
        assert_eq!(lim.get_total_bytes_through(Priority::Low), 500_000);
    }

    #[test]
    fn ten_mb_through_one_mbps_takes_at_least_nine_seconds() {
        // The bucket starts full (1 MB burst) so a 10 MB request
        // sees 1 MB of free credit up front - expected wait is
        // ~9 seconds, not 10. Assert >= 9 to match.
        let lim = TokenBucketRateLimiter::new(1_000_000, 1_000_000);
        let start = Instant::now();
        lim.request(10_000_000, Priority::Low);
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_secs(9),
            "10 MB through 1 MB/s took {:?}, expected >= 9s",
            elapsed
        );
        assert_eq!(lim.get_total_bytes_through(Priority::Low), 10_000_000);
    }

    #[test]
    fn requests_are_served_in_the_order_they_reserved() {
        // 100 KB/s with a 10 KB burst: after the burst is drained, each
        // 10 KB waits 100 ms behind the one before it.
        let lim = Arc::new(TokenBucketRateLimiter::new(100_000, 10_000));
        lim.request(10_000, Priority::Low);
        let start = Instant::now();
        lim.request(10_000, Priority::Low);
        lim.request(10_000, Priority::Low);
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(180),
            "two 10 KB requests at 100 KB/s took {elapsed:?}"
        );
    }

    #[test]
    fn high_priority_preempts_low() {
        let lim = Arc::new(TokenBucketRateLimiter::new(100_000, 10_000));
        // Drain the initial burst so every following waiter has to
        // wait for refills.
        lim.request(10_000, Priority::Low);

        let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

        let low_handles: Vec<_> = (0..2)
            .map(|i| {
                let lim = lim.clone();
                let order = order.clone();
                thread::spawn(move || {
                    lim.request(30_000, Priority::Low);
                    let label = if i == 0 { "lo1" } else { "lo2" };
                    order.lock().unwrap().push(label);
                })
            })
            .collect();

        // The low requests reserved first; the high one arrives after,
        // takes its bytes at once and is served first.
        thread::sleep(Duration::from_millis(30));
        let high = {
            let lim = lim.clone();
            let order = order.clone();
            thread::spawn(move || {
                lim.request(30_000, Priority::High);
                order.lock().unwrap().push("high");
            })
        };

        high.join().unwrap();
        for h in low_handles {
            h.join().unwrap();
        }

        let order = order.lock().unwrap();
        let high_idx = order.iter().position(|&s| s == "high").unwrap();
        assert!(
            high_idx < 2,
            "high did not preempt any low: order = {:?}",
            *order
        );
    }

    #[test]
    fn shutdown_releases_blocked_requests() {
        let lim = Arc::new(TokenBucketRateLimiter::new(1_000, 1_000));
        // Drain the burst so the next request has to wait ~1s per KB.
        lim.request(1_000, Priority::Low);

        let blocked = {
            let lim = lim.clone();
            thread::spawn(move || {
                let start = Instant::now();
                lim.request(60_000, Priority::Low);
                start.elapsed()
            })
        };

        thread::sleep(Duration::from_millis(100));
        lim.stop();
        let waited = blocked.join().unwrap();
        assert!(
            waited < Duration::from_secs(5),
            "blocked request was not released promptly after stop: {:?}",
            waited
        );
        assert_eq!(
            lim.get_total_bytes_through(Priority::Low),
            1_000,
            "a request released by shutdown was not served"
        );
    }

    #[test]
    fn set_bytes_per_second_live_update_is_respected() {
        let lim = Arc::new(TokenBucketRateLimiter::new(100_000, 100_000));
        lim.request(100_000, Priority::Low); // drain burst
        assert_eq!(lim.get_bytes_per_second(), 100_000);

        // Bump the rate and confirm a subsequent large request
        // completes faster than it would have at the old rate.
        lim.set_bytes_per_second(10_000_000);
        assert_eq!(lim.get_bytes_per_second(), 10_000_000);
        let start = Instant::now();
        lim.request(1_000_000, Priority::Low);
        // At the old rate 1 MB would need ~10 seconds; at 10 MB/s
        // it should need ~100 ms. Give it generous slack for CI.
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "request took {:?} after rate bump",
            start.elapsed()
        );
    }

    /// Run `step` once, on this thread, in the middle of its next rate
    /// change.
    fn in_next_rate_change(step: impl FnOnce() + 'static) {
        IN_RATE_CHANGE.with(|hook| *hook.borrow_mut() = Some(Box::new(step)));
    }

    /// Wait for a 1-byte request on another thread, for at most `limit`;
    /// `None` when it was still waiting then. The limiter is stopped
    /// either way, so the thread ends.
    fn one_byte_wait(lim: &Arc<TokenBucketRateLimiter>, limit: Duration) -> Option<Duration> {
        let (done, waited) = std::sync::mpsc::channel();
        let next = {
            let lim = Arc::clone(lim);
            thread::spawn(move || {
                let start = Instant::now();
                lim.request(1, Priority::Low);
                let _ = done.send(start.elapsed());
            })
        };
        let waited = waited.recv_timeout(limit).ok();
        lim.stop();
        next.join().unwrap();
        waited
    }

    /// A request that reserves while a rate change is under way owes its
    /// bytes at one rate: a 1-byte request after the change to 1 B/s waits
    /// about a second, not the thousand seconds its debt would come to if
    /// it were read as owed at the old rate of 1000 B/s.
    #[test]
    fn a_request_racing_a_rate_change_is_owed_at_one_rate() {
        let lim = Arc::new(TokenBucketRateLimiter::new(1_000, 1));
        let racing = Arc::clone(&lim);
        in_next_rate_change(move || racing.request(1, Priority::Low));
        lim.set_bytes_per_second(1);
        assert_eq!(
            lim.get_total_bytes_through(Priority::Low),
            1,
            "setup: the racing request was served"
        );
        let waited = one_byte_wait(&lim, Duration::from_secs(10));
        assert!(
            waited.is_some(),
            "the next byte at 1 B/s waited more than 10 s"
        );
    }

    /// A rate raised while a request waits serves the bytes it still owes
    /// at the new rate: a request that would wait 10 s at 1000 B/s ends
    /// soon after the rate goes to 100 MB/s.
    #[test]
    fn a_rate_raised_while_a_request_waits_serves_it_at_the_new_rate() {
        let lim = Arc::new(TokenBucketRateLimiter::new(1_000, 1));
        lim.request(1, Priority::Low);
        let reserved = lim.get_total_bytes_through(Priority::Low);
        let (done, waited) = std::sync::mpsc::channel();
        let waiter = {
            let lim = Arc::clone(&lim);
            thread::spawn(move || {
                lim.request(10_000, Priority::Low);
                let _ = done.send(());
            })
        };
        // Until the waiter has reserved: its bytes are then owed.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !lim.owes_more_than(1) {
            assert!(
                Instant::now() < deadline,
                "setup: the waiter never reserved"
            );
            thread::yield_now();
        }
        lim.set_bytes_per_second(100_000_000);
        let served = waited.recv_timeout(Duration::from_secs(5)).is_ok();
        lim.stop();
        waiter.join().unwrap();
        assert!(served, "a waiting request kept the old rate");
        assert_eq!(
            lim.get_total_bytes_through(Priority::Low),
            reserved + 10_000
        );
    }

    #[test]
    fn zero_rate_disables_limiter() {
        let lim = TokenBucketRateLimiter::new(0, 1);
        let start = Instant::now();
        lim.request(100_000_000, Priority::Low);
        assert!(start.elapsed() < Duration::from_millis(50));
        assert_eq!(lim.get_total_bytes_through(Priority::Low), 100_000_000);
    }

    #[test]
    fn get_total_bytes_through_tracks_both_classes() {
        let lim = TokenBucketRateLimiter::new(10_000_000, 10_000_000);
        lim.request(1_000, Priority::High);
        lim.request(2_000, Priority::Low);
        lim.request(3_000, Priority::High);
        assert_eq!(lim.get_total_bytes_through(Priority::High), 4_000);
        assert_eq!(lim.get_total_bytes_through(Priority::Low), 2_000);
    }

    /// A limiter that serves every request at once and counts the bytes.
    #[derive(Default)]
    struct Counting(AtomicU64);

    impl RateLimiter for Counting {
        fn request(&self, bytes: u64, _: Priority) {
            self.0.fetch_add(bytes, Ordering::SeqCst);
        }
    }

    /// The engine throttles a worker thread only: a caller's thread never
    /// asks the limiter at all.
    #[test]
    fn only_a_worker_thread_is_throttled() {
        let counting = Arc::new(Counting::default());
        let limiter: Arc<dyn RateLimiter> = Arc::clone(&counting) as Arc<dyn RateLimiter>;
        throttle_background(Some(&limiter), 1_000_000).unwrap();
        assert_eq!(counting.0.load(Ordering::SeqCst), 0, "a caller asked");
        let throttled = Arc::clone(&limiter);
        thread::spawn(move || {
            mark_worker_thread();
            throttle_background(Some(&throttled), 1).unwrap();
        })
        .join()
        .unwrap();
        assert_eq!(counting.0.load(Ordering::SeqCst), 1, "the worker asked");
    }

    /// A limiter that panics fails the I/O that asked, as an error, and
    /// leaves the worker running.
    #[test]
    fn a_panicking_limiter_fails_the_request_instead_of_the_worker() {
        struct Panics;
        impl RateLimiter for Panics {
            fn request(&self, _: u64, _: Priority) {
                panic!("limiter panics");
            }
        }
        let limiter: Arc<dyn RateLimiter> = Arc::new(Panics);
        let failed = thread::spawn(move || {
            mark_worker_thread();
            throttle_background(Some(&limiter), 1)
        })
        .join()
        .expect("the panic did not unwind through the worker");
        let err = crate::Error::from(failed.unwrap_err());
        assert!(matches!(
            err,
            crate::Error::CallbackPanicked {
                callback: "RateLimiter",
                latched: false
            }
        ));
    }
}
