//! A small number for the calling thread, so state that every thread
//! updates can be split into one cache line per thread.
//!
//! [`index`] gives the calling thread a number below [`width`]. The first
//! call on a thread claims the lowest free number from a fixed pool with
//! one compare-and-swap and keeps it in a thread-local; every later call is
//! one thread-local read, with no system call and no shared write. A thread
//! that exits gives its number back, so a pool of short-lived threads keeps
//! reusing the low numbers instead of spreading over the whole range.
//!
//! The pool holds [`width`] numbers: the machine's parallelism rounded up
//! to a power of two, capped at [`MAX_WIDTH`]. A thread that finds every
//! number taken shares one, picked round robin, and never gives it back.
//!
//! # What a number promises
//!
//! Only "probably nobody else is using this right now". Everything indexed
//! by these numbers (the snapshot registry's slots, the statistics shards)
//! is updated with atomic read-modify-writes and stays exact when several
//! threads share a number; sharing costs contention, never correctness.
//! That is also why a number may be reused while a handle created under it
//! is still alive on another thread: the handle records where it is
//! announced and releases exactly there.
//!
//! On a target with one thread (wasm without the `atomics` feature) the
//! width is 1 and every call returns 0.
//!
//! Under `--cfg loom` [`index`] is always 0: loom's models place state in
//! chosen numbers themselves and check [`IndexPool`] directly.

#[cfg(not(loom))]
use std::cell::Cell;

use crate::sync::internal::{AtomicUsize, Ordering};

/// The most numbers [`index`] hands out. Past it, threads share.
pub(crate) const MAX_WIDTH: usize = 256;

const WORD_BITS: usize = usize::BITS as usize;

/// Words in the pool's bitmap.
const WORDS: usize = MAX_WIDTH / WORD_BITS;

/// [`width`] before the first call computed it.
const WIDTH_UNKNOWN: usize = 0;

/// How many per-thread copies a structure indexed by [`index`] keeps: a
/// power of two, at least 1 and at most [`MAX_WIDTH`]. Fixed for the life
/// of the process, so a structure sized by it once always covers every
/// number [`index`] returns.
///
/// The first call reads [`parallelism`]; every later call is one relaxed
/// load. Two threads racing on the first call compute the same value.
pub(crate) fn width() -> usize {
    static WIDTH: crate::portability::AtomicUsize =
        crate::portability::AtomicUsize::new(WIDTH_UNKNOWN);
    let known = WIDTH.load(crate::portability::Ordering::Relaxed);
    if known != WIDTH_UNKNOWN {
        return known;
    }
    let width = width_for(parallelism(), crate::env::PLATFORM_THREADS);
    WIDTH.store(width, crate::portability::Ordering::Relaxed);
    width
}

/// The machine's available parallelism, at least 1, asked of the OS once
/// per process. Every sizing by the core count reads it here.
///
/// On Linux the OS answer reads the cgroup's CPU quota, about a dozen
/// system calls; an open of a database sizes its commit ring by it, and
/// a process that opens many databases must not pay them each time.
pub(crate) fn parallelism() -> usize {
    static PARALLELISM: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *PARALLELISM.get_or_init(|| {
        #[cfg(test)]
        OS_ASKED.fetch_add(1, crate::portability::Ordering::Relaxed);
        std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
    })
}

/// Test-only: how many times [`parallelism`] asked the OS.
#[cfg(test)]
static OS_ASKED: crate::portability::AtomicUsize = crate::portability::AtomicUsize::new(0);

/// The width for a machine with `parallelism` cores, on a target that has
/// other threads (`threads`) or only this one.
fn width_for(parallelism: usize, threads: bool) -> usize {
    if !threads {
        return 1;
    }
    parallelism.max(1).next_power_of_two().min(MAX_WIDTH)
}

/// A thread-local value meaning "this thread holds no number".
#[cfg(not(loom))]
const UNSET: usize = usize::MAX;

#[cfg(not(loom))]
static POOL: IndexPool = IndexPool::new();

#[cfg(not(loom))]
std::thread_local! {
    /// The calling thread's number, or [`UNSET`]. No destructor, so it can
    /// be read at any point of the thread's life, its exit included.
    static INDEX: Cell<usize> = const { Cell::new(UNSET) };
    /// The number this thread owns and gives back when it exits, or
    /// [`UNSET`] for a shared number.
    static HELD: Held = const { Held(Cell::new(UNSET)) };
}

/// The calling thread's number, below [`width`].
///
/// One thread-local read once the thread has a number. See the module
/// documentation for what the number promises.
#[cfg(not(loom))]
#[inline]
pub(crate) fn index() -> usize {
    let index = INDEX.with(Cell::get);
    if index != UNSET { index } else { assign() }
}

/// Under loom every thread is number 0: models that need distinct numbers
/// pass them explicitly.
#[cfg(loom)]
#[inline]
pub(crate) fn index() -> usize {
    0
}

/// The calling thread's first call: claim a number and remember it.
#[cfg(not(loom))]
#[cold]
fn assign() -> usize {
    let (index, owned) = POOL.claim(width());
    // A thread that is already running its thread-local destructors cannot
    // register the one that gives the number back, so it uses the number as
    // a shared one and returns it at once.
    if owned && HELD.try_with(|held| held.0.set(index)).is_err() {
        POOL.give_back(index);
    }
    INDEX.with(|cell| cell.set(index));
    index
}

/// Gives the thread's number back to the pool when the thread exits.
#[cfg(not(loom))]
struct Held(Cell<usize>);

#[cfg(not(loom))]
impl Drop for Held {
    fn drop(&mut self) {
        let index = self.0.get();
        if index != UNSET {
            // A later call from another thread-local's destructor must not
            // keep using a number another thread may claim next; it takes a
            // fresh one, as a shared number (see `assign`).
            INDEX.with(|cell| cell.set(UNSET));
            POOL.give_back(index);
        }
    }
}

/// The pool [`index`] claims numbers from: one bit per number, set while a
/// live thread owns it.
///
/// A claim takes the lowest clear bit below the width with one
/// compare-and-swap, retried only when another claim or give-back changed
/// the same word in between. When every bit is set the claim returns a
/// shared number instead, so it never waits and never fails.
pub(crate) struct IndexPool {
    taken: [AtomicUsize; WORDS],
    /// Round-robin cursor for threads past the pool.
    shared: AtomicUsize,
}

impl IndexPool {
    /// A pool with every number free.
    #[cfg(not(loom))]
    pub(crate) const fn new() -> Self {
        Self {
            taken: [const { AtomicUsize::new(0) }; WORDS],
            shared: AtomicUsize::new(0),
        }
    }

    /// A pool with every number free.
    ///
    /// Not `const` under loom: the mock atomics allocate their own state.
    #[cfg(loom)]
    pub(crate) fn new() -> Self {
        Self {
            taken: std::array::from_fn(|_| AtomicUsize::new(0)),
            shared: AtomicUsize::new(0),
        }
    }

    /// Claims the lowest free number below `width` (at most [`MAX_WIDTH`]):
    /// `(number, true)`, to be given back with [`Self::give_back`]. When
    /// every number is taken, a shared number: `(number, false)`, which is
    /// never given back.
    pub(crate) fn claim(&self, width: usize) -> (usize, bool) {
        let width = width.clamp(1, MAX_WIDTH);
        for (word_no, word) in self.taken.iter().enumerate() {
            let base = word_no * WORD_BITS;
            if base >= width {
                break;
            }
            let usable = if width - base >= WORD_BITS {
                usize::MAX
            } else {
                (1usize << (width - base)) - 1
            };
            let mut bits = word.load(Ordering::Relaxed);
            loop {
                let free = !bits & usable;
                if free == 0 {
                    break;
                }
                let bit = free.trailing_zeros() as usize;
                match word.compare_exchange_weak(
                    bits,
                    bits | (1 << bit),
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return (base + bit, true),
                    Err(now) => bits = now,
                }
            }
        }
        (self.shared.fetch_add(1, Ordering::Relaxed) % width, false)
    }

    /// Gives back a number [`Self::claim`] returned as owned.
    pub(crate) fn give_back(&self, index: usize) {
        if let Some(word) = self.taken.get(index / WORD_BITS) {
            word.fetch_and(!(1 << (index % WORD_BITS)), Ordering::Release);
        }
    }

    /// Whether `index` is owned by a live thread. Test and model use only.
    #[cfg(any(test, loom))]
    pub(crate) fn is_taken(&self, index: usize) -> bool {
        self.taken
            .get(index / WORD_BITS)
            .is_some_and(|word| word.load(Ordering::Acquire) & (1 << (index % WORD_BITS)) != 0)
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn the_width_is_a_capped_power_of_two_and_one_without_threads() {
        assert_eq!(width_for(1, true), 1);
        assert_eq!(width_for(0, true), 1);
        assert_eq!(width_for(3, true), 4);
        assert_eq!(width_for(16, true), 16);
        assert_eq!(width_for(17, true), 32);
        assert_eq!(width_for(100_000, true), MAX_WIDTH);
        assert_eq!(width_for(64, false), 1);
        let width = width();
        assert!(width.is_power_of_two() && width <= MAX_WIDTH);
        assert_eq!(width, super::width(), "fixed for the process");
    }

    /// Sizing by the core count, the commit ring an open builds included,
    /// asks the OS once per process, not once per call.
    #[test]
    fn the_os_is_asked_for_the_parallelism_once() {
        let cores = parallelism();
        assert!(cores >= 1);
        for _ in 0..8 {
            assert_eq!(parallelism(), cores);
            assert_eq!(
                crate::engine::commit::commit_ring_capacity(),
                cores.saturating_mul(4).clamp(256, 1024)
            );
            let _ = width();
        }
        assert_eq!(OS_ASKED.load(crate::portability::Ordering::Relaxed), 1);
    }

    #[test]
    fn claims_are_distinct_until_the_pool_is_full_then_shared() {
        let pool = IndexPool::new();
        let owned: Vec<usize> = (0..4)
            .map(|_| pool.claim(4))
            .map(|(i, o)| {
                assert!(o, "a free number is owned");
                i
            })
            .collect();
        assert_eq!(owned, vec![0, 1, 2, 3], "the lowest free number first");
        let shared: Vec<(usize, bool)> = (0..6).map(|_| pool.claim(4)).collect();
        assert!(shared.iter().all(|&(i, o)| !o && i < 4), "{shared:?}");
        assert_eq!(
            shared.iter().map(|&(i, _)| i).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 0, 1],
            "threads past the pool spread round robin"
        );
    }

    #[test]
    fn a_number_given_back_is_claimed_again() {
        let pool = IndexPool::new();
        for _ in 0..3 {
            pool.claim(8);
        }
        pool.give_back(1);
        assert!(!pool.is_taken(1));
        assert_eq!(pool.claim(8), (1, true));
        assert!(pool.is_taken(1));
    }

    #[test]
    fn a_pool_wider_than_one_word_fills_every_word() {
        let pool = IndexPool::new();
        let claimed: Vec<usize> = (0..MAX_WIDTH).map(|_| pool.claim(MAX_WIDTH).0).collect();
        assert_eq!(claimed, (0..MAX_WIDTH).collect::<Vec<_>>());
        assert!(!pool.claim(MAX_WIDTH).1, "full");
        pool.give_back(MAX_WIDTH - 1);
        assert_eq!(pool.claim(MAX_WIDTH), (MAX_WIDTH - 1, true));
    }

    #[test]
    fn concurrent_claims_never_hand_one_number_to_two_owners() {
        let pool = std::sync::Arc::new(IndexPool::new());
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let pool = std::sync::Arc::clone(&pool);
                std::thread::spawn(move || {
                    (0..200)
                        .map(|_| {
                            let claim = pool.claim(64);
                            if claim.1 {
                                pool.give_back(claim.0);
                            }
                            claim
                        })
                        .count()
                })
            })
            .collect();
        for t in threads {
            assert_eq!(t.join().unwrap(), 200);
        }
        assert!(
            (0..64).all(|i| !pool.is_taken(i)),
            "every claim was given back"
        );

        // Held claims stay distinct.
        let held: Vec<usize> = (0..64)
            .map(|_| pool.claim(64))
            .map(|(i, o)| {
                assert!(o);
                i
            })
            .collect();
        let mut sorted = held.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 64);
    }

    #[test]
    fn a_thread_keeps_its_number_and_owns_it_while_it_lives() {
        let (index, again, held, taken) = std::thread::spawn(|| {
            let index = index();
            let held = HELD.with(|held| held.0.get());
            (index, super::index(), held, POOL.is_taken(index))
        })
        .join()
        .unwrap();
        assert_eq!(index, again, "one number for the thread's whole life");
        assert!(index < width());
        if held == index {
            assert!(
                taken,
                "an owned number is marked taken while its thread lives"
            );
        } else {
            assert_eq!(held, UNSET, "a shared number is never given back");
        }
    }

    #[test]
    fn the_exit_hook_gives_the_number_back_and_forgets_it() {
        std::thread::spawn(|| {
            let index = index();
            let owned = HELD.with(|held| held.0.get()) == index;
            // Run the exit hook by hand on a copy: it returns the number and
            // makes the next call claim afresh.
            if owned {
                HELD.with(|held| held.0.set(UNSET));
                drop(Held(Cell::new(index)));
                assert_eq!(INDEX.with(Cell::get), UNSET);
                let next = super::index();
                assert!(next < width());
            }
        })
        .join()
        .unwrap();
    }
}
