//! Models of `engine::skiplist`: writers publishing nodes against
//! concurrent readers and against each other, and the two-step seek that
//! must not be.

use loom::sync::Arc;
use loom::sync::atomic::{AtomicUsize, Ordering};

use super::super::internal_key::{VALUE_TYPE_VALUE, decode_internal_key, user_key_of};
use super::{explore, explore_bounded, memtable, probe};

/// Invariants S1, S3 and S7: an insert publishes a whole node and never
/// hides one that is already published.
///
/// The writer inserts `b`, which sorts strictly between the two keys the
/// reader was seeded with, so every level of the reader's descent can be
/// diverted by the new node. The reader asserts that `a` is always
/// found (S3: nothing is unlinked, and re-linking a predecessor cannot
/// lose it) and that `b`, if seen at all, carries its own sequence and
/// its own value (S1: the node was complete before it was reachable).
pub fn insert_publishes_a_whole_node_to_a_concurrent_reader() {
    explore(
        "insert_publishes_a_whole_node_to_a_concurrent_reader",
        16,
        4,
        |witness| {
            let mt = Arc::new(memtable());
            mt.put(probe(b"a").prefixed_user_key(), b"va", 1);
            mt.put(probe(b"c").prefixed_user_key(), b"vc", 1);

            let writer = {
                let mt = Arc::clone(&mt);
                loom::thread::spawn(move || {
                    mt.put(probe(b"b").prefixed_user_key(), b"vb", 2);
                })
            };
            let reader = {
                let mt = Arc::clone(&mt);
                let witness = witness.clone();
                loom::thread::spawn(move || {
                    let (seq, value) = mt.get(&probe(b"a")).expect("S3: `a` cannot be lost");
                    assert_eq!(seq, 1);
                    assert_eq!(value.expect("`a` is a live value").as_slice(), b"va");

                    if let Some((seq, value)) = mt.get(&probe(b"b")) {
                        witness.record();
                        assert_eq!(seq, 2, "S1: a visible node carries its own sequence");
                        assert_eq!(
                            value.expect("`b` is a live value").as_slice(),
                            b"vb",
                            "S1: a visible node carries its own value"
                        );
                    }
                })
            };

            writer.join().expect("writer");
            reader.join().expect("reader");

            for (key, want) in [(&b"a"[..], &b"va"[..]), (b"b", b"vb"), (b"c", b"vc")] {
                let (_, value) = mt
                    .get(&probe(key))
                    .expect("every key is present after the join");
                assert_eq!(value.expect("live value").as_slice(), want);
            }
        },
    );
}

/// Invariants S1 and S3 for the hinted insert path: a writer that takes
/// one hint and inserts two keys through it publishes each as a whole
/// node, and the ordering the hint exists to skip a descent for is the
/// same ordering that makes the second node's visibility imply the
/// first's.
///
/// `a` and `e` bracket the run; the writer inserts `b` then `d` through
/// one hint (`d` sorts after `b` and before `e`, so the second insert
/// takes the level-0 bracket path with no descent from the head). The
/// reader asserts `a` and `e` are always found with their own values
/// (S3), that `b` or `d`, if seen at all, carry their own sequence and
/// value (S1), and that whenever `d` is visible a fresh lookup of `b`
/// finds it too: `b`'s `Release` stores precede `d`'s in the writer's
/// program order, so the `Acquire` load that just revealed `d` orders
/// `b`'s publication before the next load this reader issues.
pub fn a_hinted_run_publishes_whole_nodes_to_a_concurrent_reader() {
    explore(
        "a_hinted_run_publishes_whole_nodes_to_a_concurrent_reader",
        16,
        4,
        |witness| {
            let mt = Arc::new(memtable());
            mt.put(probe(b"a").prefixed_user_key(), b"va", 1);
            mt.put(probe(b"e").prefixed_user_key(), b"ve", 1);

            let writer = {
                let mt = Arc::clone(&mt);
                loom::thread::spawn(move || {
                    let mut hint = mt.insert_hint();
                    mt.put_hinted(&mut hint, probe(b"b").prefixed_user_key(), b"vb", 2);
                    mt.put_hinted(&mut hint, probe(b"d").prefixed_user_key(), b"vd", 3);
                })
            };
            let reader = {
                let mt = Arc::clone(&mt);
                let witness = witness.clone();
                loom::thread::spawn(move || {
                    let (seq, value) = mt.get(&probe(b"a")).expect("S3: `a` cannot be lost");
                    assert_eq!(seq, 1);
                    assert_eq!(value.expect("`a` is a live value").as_slice(), b"va");
                    let (seq, value) = mt.get(&probe(b"e")).expect("S3: `e` cannot be lost");
                    assert_eq!(seq, 1);
                    assert_eq!(value.expect("`e` is a live value").as_slice(), b"ve");

                    if let Some((seq, value)) = mt.get(&probe(b"b")) {
                        assert_eq!(seq, 2, "S1: a visible node carries its own sequence");
                        assert_eq!(
                            value.expect("`b` is a live value").as_slice(),
                            b"vb",
                            "S1: a visible node carries its own value"
                        );
                    }

                    if let Some((seq, value)) = mt.get(&probe(b"d")) {
                        witness.record();
                        assert_eq!(seq, 3, "S1: a visible node carries its own sequence");
                        assert_eq!(
                            value.expect("`d` is a live value").as_slice(),
                            b"vd",
                            "S1: a visible node carries its own value"
                        );
                        // The hinted publication order: `b` was released
                        // before `d` (same writer thread), so the
                        // `Acquire` that just revealed `d` also orders
                        // `b`'s publication before this next load.
                        let (b_seq, b_value) = mt
                            .get(&probe(b"b"))
                            .expect("`d` visible must imply `b` is too");
                        assert_eq!(b_seq, 2, "S1: a visible node carries its own sequence");
                        assert_eq!(b_value.expect("`b` is a live value").as_slice(), b"vb");
                    }
                })
            };

            writer.join().expect("writer");
            reader.join().expect("reader");

            for (key, want) in [
                (&b"a"[..], &b"va"[..]),
                (b"b", b"vb"),
                (b"d", b"vd"),
                (b"e", b"ve"),
            ] {
                let (_, value) = mt
                    .get(&probe(key))
                    .expect("every key is present after the join");
                assert_eq!(value.expect("live value").as_slice(), want);
            }
        },
    );
}

/// The regression that a two-load seek reintroduces: a key already in
/// the memtable stays findable while a writer inserts immediately before
/// it.
///
/// `seek_ge` folds the level-0 step into the descent for exactly this
/// reason. Loom drives the interleaving that a stress test can only
/// stumble into.
pub fn a_seeded_key_stays_findable_while_a_writer_inserts_before_it() {
    explore(
        "a_seeded_key_stays_findable_while_a_writer_inserts_before_it",
        16,
        4,
        |witness| {
            let mt = Arc::new(memtable());
            mt.put(probe(b"a").prefixed_user_key(), b"va", 1);
            mt.put(probe(b"c").prefixed_user_key(), b"vc", 1);

            let writer = {
                let mt = Arc::clone(&mt);
                loom::thread::spawn(move || {
                    mt.put(probe(b"b").prefixed_user_key(), b"vb", 2);
                })
            };
            let reader = {
                let mt = Arc::clone(&mt);
                let witness = witness.clone();
                loom::thread::spawn(move || {
                    let (_, value) = mt
                        .get(&probe(b"c"))
                        .expect("`c` is memtable-resident and cannot go missing");
                    assert_eq!(value.expect("live value").as_slice(), b"vc");
                    // The seek raced the insert at least once: `b` was
                    // already linked when this reader looked.
                    if mt.get(&probe(b"b")).is_some() {
                        witness.record();
                    }
                })
            };

            writer.join().expect("writer");
            reader.join().expect("reader");
        },
    );
}

/// Calibration for [`a_seeded_key_stays_findable_while_a_writer_inserts_before_it`].
///
/// Descend to the predecessor, then load its level-0 link a second time:
/// the shape `seek_ge` had before the fix. If loom explores the schedule
/// where the writer links its node between the two loads, this loses a
/// key that is present, and the model must fail. A model that cannot
/// fail here is not exploring the interleaving the fix exists for, and
/// the model above would be passing vacuously.
pub fn the_two_step_seek_loses_a_key_that_is_present() {
    explore(
        "the_two_step_seek_loses_a_key_that_is_present",
        4,
        1,
        |witness| {
            let mt = Arc::new(memtable());
            mt.put(probe(b"a").prefixed_user_key(), b"va", 1);
            mt.put(probe(b"c").prefixed_user_key(), b"vc", 1);

            let writer = {
                let mt = Arc::clone(&mt);
                loom::thread::spawn(move || {
                    mt.put(probe(b"b").prefixed_user_key(), b"vb", 2);
                })
            };
            let reader = {
                let mt = Arc::clone(&mt);
                let witness = witness.clone();
                loom::thread::spawn(move || {
                    let target = probe(b"c");
                    let list = mt.list();
                    // Descend to the predecessor, then load its level-0 link
                    // a second time. `MemTable::get` treats "the first key
                    // at or after mine is not mine" as "absent", so the same
                    // comparison decides the answer here.
                    let successor = match list.seek_lt(target.internal()) {
                        Some(predecessor) => predecessor.next(),
                        None => list.first(),
                    };
                    witness.record();
                    let found = successor
                        .is_some_and(|node| user_key_of(node.key()) == target.prefixed_user_key());
                    assert!(found, "the two-step seek lost `c`");
                })
            };

            writer.join().expect("writer");
            reader.join().expect("reader");
        },
    );
}

/// Invariants S2 and S5 with nothing serializing the writers: two threads
/// put two versions of one key at once, as two helpers of the commit
/// pipeline apply two slots, while a reader walks it.
///
/// The reader may legally see nothing, the older version or the newer
/// one; what it must never see is a value belonging to the other
/// sequence, or a key that decodes to something else. After the join
/// both versions are present, newest sequence first: neither insert lost
/// its link to the other's compare-and-swap.
///
/// Bound: 2 preemptions (three threads). Unbounded, three threads each
/// walking a twelve-level tower do not finish.
pub fn two_writers_and_a_reader_share_one_key() {
    explore_bounded(
        "two_writers_and_a_reader_share_one_key",
        Some(2),
        100,
        1,
        |witness| {
            let mt = Arc::new(memtable());

            let writers: Vec<_> = [(1u64, &b"v1"[..]), (2, b"v2")]
                .into_iter()
                .map(|(seq, value)| {
                    let mt = Arc::clone(&mt);
                    loom::thread::spawn(move || {
                        mt.put(probe(b"k").prefixed_user_key(), value, seq);
                    })
                })
                .collect();

            let reader = {
                let mt = Arc::clone(&mt);
                let witness = witness.clone();
                loom::thread::spawn(move || {
                    if let Some((seq, value)) = mt.get(&probe(b"k")) {
                        let value = value.expect("both writers write live values");
                        let want: &[u8] = if seq == 1 { b"v1" } else { b"v2" };
                        assert!(seq == 1 || seq == 2, "S5: sequence {seq} was never written");
                        assert_eq!(value.as_slice(), want, "S5: value belongs to another entry");
                        // The intermediate state: only the older writer
                        // has landed, so the reader is inside the pair.
                        if seq == 1 {
                            witness.record();
                        }
                    }
                })
            };

            for writer in writers {
                writer.join().expect("writer");
            }
            reader.join().expect("reader");

            let entries = mt.iter_internal();
            assert_eq!(entries.len(), 2, "S2: an insert was lost, or held twice");
            let decoded: Vec<_> = entries
                .iter()
                .map(|(key, value)| {
                    let (user_key, seq, value_type) = decode_internal_key(key);
                    (user_key.to_vec(), seq, value_type, value.clone())
                })
                .collect();
            assert_eq!(decoded[0].1, 2, "the newer sequence sorts first");
            assert_eq!(decoded[0].3, b"v2");
            assert_eq!(decoded[1].1, 1);
            assert_eq!(decoded[1].3, b"v1");
            for entry in &decoded {
                assert_eq!(entry.0, probe(b"k").prefixed_user_key());
                assert_eq!(entry.2, VALUE_TYPE_VALUE);
            }
        },
    );
}

/// Invariant S2, insert if absent: two threads insert the same key at the
/// same sequence at once, as two helpers applying one slot of the commit
/// pipeline do. The list holds the entry once, whichever insert linked it,
/// and the other insert reports it held. One of the two goes through a
/// hint, the batch path, so both entry points keep the rule.
///
/// Bound: 3 preemptions.
pub fn two_writers_of_one_entry_hold_it_once() {
    explore_bounded(
        "two_writers_of_one_entry_hold_it_once",
        Some(3),
        16,
        1,
        |witness| {
            let mt = Arc::new(memtable());
            let first = {
                let mt = Arc::clone(&mt);
                loom::thread::spawn(move || {
                    mt.put(probe(b"k").prefixed_user_key(), b"v", 1);
                })
            };
            let second = {
                let mt = Arc::clone(&mt);
                loom::thread::spawn(move || {
                    let mut hint = mt.insert_hint();
                    mt.put_hinted(&mut hint, probe(b"k").prefixed_user_key(), b"v", 1);
                })
            };
            first.join().expect("first");
            second.join().expect("second");
            witness.record();

            let entries = mt.iter_internal();
            assert_eq!(entries.len(), 1, "S2: one entry was linked twice");
            let (seq, value) = mt.get(&probe(b"k")).expect("the entry is present");
            assert_eq!(seq, 1);
            assert_eq!(value.expect("a live value").as_slice(), b"v");
        },
    );
}

/// Calibration for [`two_writers_and_a_reader_share_one_key`]: a list whose
/// head is linked by a load and a store. Two inserts read the same head,
/// each points its node at it, and the second store unlinks the first
/// insert's node.
pub fn a_link_stored_without_a_compare_and_swap_loses_an_insert() {
    explore_bounded(
        "a_link_stored_without_a_compare_and_swap_loses_an_insert",
        Some(3),
        2,
        1,
        |witness| {
            // Node ids 1 and 2; 0 is the end of the list.
            let head = Arc::new(AtomicUsize::new(0));
            let next = Arc::new([
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
            ]);
            let writers: Vec<_> = (1..=2usize)
                .map(|node| {
                    let (head, next) = (Arc::clone(&head), Arc::clone(&next));
                    loom::thread::spawn(move || {
                        let first = head.load(Ordering::Acquire);
                        next[node].store(first, Ordering::Release);
                        head.store(node, Ordering::Release);
                    })
                })
                .collect();
            for writer in writers {
                writer.join().expect("writer");
            }
            witness.record();
            let mut linked = 0;
            let mut at = head.load(Ordering::Acquire);
            while at != 0 {
                linked += 1;
                at = next[at].load(Ordering::Acquire);
            }
            assert_eq!(linked, 2, "an insert was lost");
        },
    );
}
