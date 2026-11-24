//! Models of invariant A8: any number of threads allocate from one arena at
//! once, and each allocation is a range of its own.
//!
//! `Arena::alloc` takes no lock. A chunk's cursor moves by compare-and-swap,
//! and the chunk list grows by one compare-and-swap of its head; both are
//! atomics from `crate::sync::internal`, so loom sees them. The positive
//! model races two allocations on the creation of the arena's first chunk,
//! the one step where both the head and a cursor are contended. The
//! calibration rebuilds the cursor as a load and a store and must fail.

use loom::sync::Arc;
use loom::sync::atomic::{AtomicUsize, Ordering};

use super::super::arena::{Arena, ArenaProfile, ChunkPool};
use super::explore_bounded;

/// One chunk large enough to hold both threads' allocations, so the race
/// under test is the concurrent creation of the arena's first chunk, not
/// a second growth competing with it.
const BUDGET: usize = 4096;

/// Bytes each thread allocates.
const SIZE: usize = 64;

/// A8: two threads allocate at once from an empty arena. Both get a range,
/// the two ranges do not overlap, and the arena counts both.
///
/// Bound: 3 preemptions. Two allocations, on an arena with no chunk yet.
pub fn two_allocations_at_once_get_ranges_of_their_own() {
    explore_bounded(
        "two_allocations_at_once_get_ranges_of_their_own",
        Some(3),
        8,
        1,
        |witness| {
            let profile = ArenaProfile::EMBEDDED;
            let pool = Arc::new(ChunkPool::new(profile, BUDGET, 2));
            let arena = Arc::new(Arena::new(pool, BUDGET, profile));

            let writers: Vec<_> = (0..2)
                .map(|_| {
                    let arena = Arc::clone(&arena);
                    loom::thread::spawn(move || {
                        arena
                            .alloc(SIZE, 8)
                            .expect("the budget holds two allocations")
                            .as_ptr() as usize
                    })
                })
                .collect();
            let ranges: Vec<usize> = writers
                .into_iter()
                .map(|writer| writer.join().expect("writer"))
                .collect();
            witness.record();
            assert!(
                ranges[0].abs_diff(ranges[1]) >= SIZE,
                "A8: two allocations share bytes"
            );
            assert_eq!(
                arena.used_bytes(),
                2 * SIZE,
                "A8: an allocation was not counted"
            );
        },
    );
}

/// Calibration for [`two_allocations_at_once_get_ranges_of_their_own`]: a
/// cursor moved by a load and a store. Both threads read the same offset
/// and are handed the same range.
pub fn a_cursor_moved_by_a_load_and_a_store_hands_one_range_out_twice() {
    explore_bounded(
        "a_cursor_moved_by_a_load_and_a_store_hands_one_range_out_twice",
        Some(3),
        2,
        1,
        |witness| {
            let cursor = Arc::new(AtomicUsize::new(0));
            let writers: Vec<_> = (0..2)
                .map(|_| {
                    let cursor = Arc::clone(&cursor);
                    loom::thread::spawn(move || {
                        let at = cursor.load(Ordering::Acquire);
                        cursor.store(at + SIZE, Ordering::Release);
                        at
                    })
                })
                .collect();
            let ranges: Vec<usize> = writers
                .into_iter()
                .map(|writer| writer.join().expect("writer"))
                .collect();
            witness.record();
            assert!(
                ranges[0].abs_diff(ranges[1]) >= SIZE,
                "two allocations share bytes"
            );
        },
    );
}
