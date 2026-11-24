//! `TxnBuffer::truncate` against a model: what it keeps, what it frees, and
//! that the index, once built, agrees with the list afterwards.

use super::TxnBuffer;
use proptest::prelude::*;
use std::ops::ControlFlow;
use std::sync::Arc;

/// The values of `key`'s chain, newest first, copied out.
fn chain_of(buffer: &TxnBuffer<u8, u32>, key: u8) -> Vec<u32> {
    let mut out = Vec::new();
    buffer.walk_chain(&key, |value| {
        out.push(*value);
        ControlFlow::Continue(())
    });
    out
}

#[test]
fn truncate_drops_the_newest_entries() {
    for spill_at in [0, 2] {
        let mut buffer = TxnBuffer::new(spill_at);
        for (key, value) in [(1u8, 10u32), (2, 20), (1, 11), (3, 30), (2, 21)] {
            buffer.insert(key, value);
        }
        buffer.truncate(3);
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.get(&1), Some(11));
        assert_eq!(buffer.get(&2), Some(20));
        assert_eq!(buffer.get(&3), None);
        assert_eq!(chain_of(&buffer, 1), [11, 10]);
        assert_eq!(chain_of(&buffer, 2), [20]);
    }
}

#[test]
fn truncate_to_the_current_length_or_above_drops_nothing() {
    let mut buffer = TxnBuffer::new(0);
    buffer.insert(1u8, 1u32);
    buffer.insert(2, 2);
    buffer.truncate(2);
    buffer.truncate(99);
    assert_eq!(buffer.len(), 2);
    assert_eq!(buffer.get(&1), Some(1));
    assert_eq!(buffer.get(&2), Some(2));
}

#[test]
fn truncate_to_zero_empties_the_buffer_and_it_takes_writes_again() {
    for spill_at in [0, 1] {
        let mut buffer = TxnBuffer::new(spill_at);
        for i in 0..6u8 {
            buffer.insert(i % 3, u32::from(i));
        }
        buffer.truncate(0);
        assert_eq!(buffer.len(), 0);
        assert_eq!(buffer.get(&0), None);
        buffer.insert(0, 7);
        assert_eq!(buffer.get(&0), Some(7));
        assert_eq!(chain_of(&buffer, 0), [7]);
    }
}

#[test]
fn truncate_repoints_the_index_at_each_keys_older_node() {
    let mut buffer = TxnBuffer::new(1);
    for (key, value) in [(1u8, 1u32), (2, 2), (1, 3), (2, 4)] {
        buffer.insert(key, value);
    }
    assert!(buffer.is_indexed());
    buffer.truncate(3);
    assert_eq!((buffer.get(&1), buffer.get(&2)), (Some(3), Some(2)));
    buffer.truncate(2);
    assert_eq!((buffer.get(&1), buffer.get(&2)), (Some(1), Some(2)));
    buffer.truncate(1);
    assert_eq!((buffer.get(&1), buffer.get(&2)), (Some(1), None));
}

#[test]
fn a_node_pushed_before_the_index_was_built_is_found_again_after_truncation() {
    // Entries pushed before the index exists carry no back-links, so the
    // older node of a key is found by walking; the rollback must find it too.
    let mut buffer = TxnBuffer::new(0);
    for (key, value) in [(1u8, 1u32), (1, 2), (1, 3)] {
        buffer.insert(key, value);
    }
    buffer.ensure_indexed();
    buffer.insert(1, 4);
    buffer.truncate(3);
    assert_eq!(buffer.get(&1), Some(3));
    buffer.truncate(1);
    assert_eq!(buffer.get(&1), Some(1));
    assert_eq!(chain_of(&buffer, 1), [1]);
}

#[test]
fn truncate_frees_each_dropped_value_once_and_keeps_the_rest() {
    for spill_at in [0, 2] {
        let value = Arc::new(());
        let mut buffer = TxnBuffer::new(spill_at);
        for i in 0..8u8 {
            buffer.insert(i % 3, Arc::clone(&value));
        }
        assert_eq!(Arc::strong_count(&value), 9);
        buffer.truncate(5);
        assert_eq!(Arc::strong_count(&value), 6);
        buffer.truncate(5);
        assert_eq!(Arc::strong_count(&value), 6);
        drop(buffer);
        assert_eq!(Arc::strong_count(&value), 1);
    }
}

#[test]
fn truncate_into_hands_back_what_it_took_and_putting_it_back_restores_the_buffer() {
    for spill_at in [0, 2] {
        let mut buffer = TxnBuffer::new(spill_at);
        for (key, value) in [(1u8, 10u32), (2, 20), (1, 11), (3, 30), (2, 21)] {
            buffer.insert(key, value);
        }
        let mut taken = Vec::new();
        buffer.truncate_into(2, |entry| taken.push(entry));
        assert_eq!(taken, [(2, 21), (3, 30), (1, 11)], "newest first");
        assert_eq!(buffer.len(), 2);
        assert_eq!((buffer.get(&1), buffer.get(&3)), (Some(10), None));
        for (key, value) in taken.into_iter().rev() {
            buffer.insert(key, value);
        }
        assert_eq!(buffer.len(), 5);
        assert_eq!(chain_of(&buffer, 1), [11, 10]);
        assert_eq!(chain_of(&buffer, 2), [21, 20]);
        assert_eq!(chain_of(&buffer, 3), [30]);
    }
}

#[test]
fn truncate_then_drain_returns_only_what_is_left() {
    let mut buffer = TxnBuffer::new(1);
    for (key, value) in [(1u8, 1u32), (2, 2), (1, 3)] {
        buffer.insert(key, value);
    }
    buffer.truncate(2);
    assert_eq!(buffer.drain(), [(2, 2), (1, 1)]);
}

#[derive(Debug, Clone)]
enum Op {
    Insert(u8, u32),
    Truncate(usize),
    Index,
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(
        prop_oneof![
            6 => (0u8..6, any::<u32>()).prop_map(|(k, v)| Op::Insert(k, v)),
            3 => (0usize..40).prop_map(Op::Truncate),
            1 => Just(Op::Index),
        ],
        0..120,
    )
}

proptest! {
    /// Whatever the order of inserts, truncations and index builds, the buffer
    /// reads exactly what a plain list of the surviving writes says: its
    /// length, the newest value of each key and each key's chain.
    #[test]
    fn the_buffer_matches_a_list_of_the_surviving_writes(
        spill_at in prop_oneof![Just(0usize), Just(1), Just(3), Just(32)],
        ops in ops(),
    ) {
        let mut buffer = TxnBuffer::new(spill_at);
        let mut model: Vec<(u8, u32)> = Vec::new();
        for op in ops {
            match op {
                Op::Insert(key, value) => {
                    buffer.insert(key, value);
                    model.push((key, value));
                }
                Op::Truncate(len) => {
                    buffer.truncate(len);
                    model.truncate(len);
                }
                Op::Index => buffer.ensure_indexed(),
            }
            prop_assert_eq!(buffer.len(), model.len());
            for key in 0u8..6 {
                let chain: Vec<u32> = model
                    .iter()
                    .rev()
                    .filter(|(k, _)| *k == key)
                    .map(|(_, v)| *v)
                    .collect();
                prop_assert_eq!(buffer.get(&key), chain.first().copied());
                prop_assert_eq!(chain_of(&buffer, key), chain);
            }
        }
    }
}

#[test]
fn the_generation_grows_on_every_insert_and_every_truncate_that_drops_something() {
    for spill_at in [0, 2] {
        let mut buffer = TxnBuffer::new(spill_at);
        let empty = buffer.generation();
        buffer.insert(1u8, 1u32);
        buffer.insert(2, 2);
        let two = buffer.generation();
        assert!(two > empty);

        buffer.truncate(1);
        let one = buffer.generation();
        assert!(one > two, "a truncate is a change");

        // The length is the one it had before the second insert, and the
        // buffer holds a different second entry: the generation tells.
        buffer.insert(3, 3);
        assert_eq!(buffer.len(), 2);
        assert!(buffer.generation() > one);
        assert_ne!(buffer.generation(), two);

        let before = buffer.generation();
        buffer.truncate(2);
        buffer.truncate(99);
        assert_eq!(buffer.generation(), before, "nothing was dropped");

        buffer.drain();
        assert!(buffer.generation() > before, "a drain is a change");
        let drained = buffer.generation();
        buffer.insert(4, 4);
        assert!(buffer.generation() > drained);
    }
}

proptest! {
    /// The generation changes exactly when the buffer does, and only upward,
    /// whatever the order of inserts, truncations and drains: so two reads of
    /// it that agree saw the same buffer.
    #[test]
    fn the_generation_never_repeats_for_two_different_buffers(
        spill_at in prop_oneof![Just(0usize), Just(2)],
        ops in prop::collection::vec(
            prop_oneof![
                6 => (0u8..6, any::<u32>()).prop_map(|(k, v)| Op::Insert(k, v)),
                3 => (0usize..40).prop_map(Op::Truncate),
                1 => Just(Op::Index),
            ],
            0..120,
        ),
        drain_at in prop::option::of(0usize..120),
    ) {
        let mut buffer = TxnBuffer::new(spill_at);
        let mut model: Vec<(u8, u32)> = Vec::new();
        let mut seen: Vec<(u64, Vec<(u8, u32)>)> = vec![(buffer.generation(), Vec::new())];
        for (step, op) in ops.into_iter().enumerate() {
            let before = buffer.generation();
            match op {
                Op::Insert(key, value) => {
                    buffer.insert(key, value);
                    model.push((key, value));
                }
                Op::Truncate(len) => {
                    buffer.truncate(len);
                    model.truncate(len);
                }
                Op::Index => buffer.ensure_indexed(),
            }
            if drain_at == Some(step) {
                buffer.drain();
                model.clear();
            }
            let after = buffer.generation();
            prop_assert!(after >= before, "the generation fell");
            if let Some((_, was)) = seen.iter().find(|(generation, _)| *generation == after) {
                prop_assert_eq!(was, &model, "one generation, two buffers");
            } else {
                seen.push((after, model.clone()));
            }
        }
    }
}
