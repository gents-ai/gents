//! Model of invariant A5: an arena outlives every [`crate::DbSlice`]
//! taken from it.
//!
//! The memtable hands out values by reference-counting the arena rather
//! than by copying, so the question "may this chunk go back to the pool"
//! is answered by an `Arc<Arena>` refcount and nothing else. The model
//! makes the last memtable handle die on one thread while another still
//! holds a slice, which is the ordering the refcount exists for.

use loom::sync::Arc;

use super::{explore, memtable, memtable_with_budget, probe};

/// Arena budget for the chunk-growth model. One 256-byte chunk holds the
/// seeded entry, and a whole-budget value cannot fit beside it, so the
/// writer's single insert is guaranteed to reserve a second chunk.
const SMALL_BUDGET: usize = 256;

/// A slice keeps reading correctly after every memtable handle is gone,
/// including when a concurrent writer grows the arena underneath it.
///
/// Loom decides which thread runs the last `Arc<MemTable>` drop and
/// where the writer's insert lands relative to it, and checks at the end
/// of every execution that no reference count was leaked. The witness is
/// the schedule that matters: the reader itself holding the last
/// memtable handle, so its own `drop` is what would retire the arena if
/// the slice were not counted.
pub fn a_slice_outlives_every_memtable_handle() {
    explore(
        "a_slice_outlives_every_memtable_handle",
        256,
        8,
        |witness| {
            let mt = Arc::new(memtable());
            mt.put(probe(b"k").prefixed_user_key(), b"pinned", 1);

            let reader = {
                let mt = Arc::clone(&mt);
                let witness = witness.clone();
                loom::thread::spawn(move || {
                    let (_, value) = mt.get(&probe(b"k")).expect("`k` was seeded");
                    let value = value.expect("`k` is a live value");
                    // A5: dropping the last handle this thread owns must not
                    // be able to retire the chunk the slice points into.
                    let last = Arc::strong_count(&mt) == 1;
                    drop(mt);
                    assert_eq!(value.as_slice(), b"pinned");
                    if last {
                        witness.record();
                    }
                })
            };
            let writer = {
                let mt = Arc::clone(&mt);
                loom::thread::spawn(move || {
                    mt.put(probe(b"z").prefixed_user_key(), b"later", 2);
                    drop(mt);
                })
            };

            drop(mt);
            reader.join().expect("reader");
            writer.join().expect("writer");
        },
    );
}

/// Invariants A2, A3 and A5 across a chunk boundary: a slice taken from
/// the arena's first chunk keeps reading while a writer forces a second
/// one.
///
/// This is the case the single-chunk model above cannot reach. Growing
/// the arena pushes onto `ArenaState::chunks`, which reallocates the
/// handle vector and moves every `ChunkHandle`; the bytes a published
/// node occupies do not move with them, and the reader's raw pointer is
/// into those bytes rather than into the vector. The writer's entry is a
/// whole budget wide, so it cannot fit beside the seeded one and the
/// second chunk is forced rather than hoped for - the witness is the
/// schedule where the reader observes the growth before it reads.
pub fn a_slice_survives_the_arena_growing_a_new_chunk() {
    explore(
        "a_slice_survives_the_arena_growing_a_new_chunk",
        64,
        8,
        |witness| {
            let mt = Arc::new(memtable_with_budget(SMALL_BUDGET));
            mt.put(probe(b"k").prefixed_user_key(), b"pinned", 1);
            let reserved_before = mt.reserved_size();
            assert!(reserved_before > 0, "the seed took the arena's first chunk");

            let reader = {
                let mt = Arc::clone(&mt);
                let witness = witness.clone();
                loom::thread::spawn(move || {
                    let (_, value) = mt.get(&probe(b"k")).expect("`k` was seeded");
                    let value = value.expect("`k` is a live value");
                    let grew = mt.reserved_size() > reserved_before;
                    drop(mt);
                    assert_eq!(
                        value.as_slice(),
                        b"pinned",
                        "A2, A3, A5: a pinned slice reads its own chunk's bytes"
                    );
                    if grew {
                        witness.record();
                    }
                })
            };
            let writer = {
                let mt = Arc::clone(&mt);
                loom::thread::spawn(move || {
                    mt.put(probe(b"big").prefixed_user_key(), &[0xab; SMALL_BUDGET], 2);
                    drop(mt);
                })
            };

            drop(mt);
            reader.join().expect("reader");
            writer.join().expect("writer");
        },
    );
}
