use super::TxnBuffer;
use core::hash::Hash;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

/// The values of `key`'s chain, newest first, through the first one `done`
/// accepts, copied out.
fn chain_of<K, V>(buffer: &TxnBuffer<K, V>, key: &K, done: impl Fn(&V) -> bool) -> Vec<V>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    let mut out = Vec::new();
    buffer.walk_chain(key, |value| {
        out.push(value.clone());
        if done(value) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    out
}

struct CountedValue {
    value: Option<usize>,
    copies: Arc<AtomicUsize>,
}

impl Clone for CountedValue {
    fn clone(&self) -> Self {
        self.copies.fetch_add(1, Ordering::Relaxed);
        Self {
            value: self.value,
            copies: self.copies.clone(),
        }
    }
}

#[test]
fn narrow_snapshot_copies_only_matching_latest_values() {
    for spill_at in [0, 16] {
        let copies = Arc::new(AtomicUsize::new(0));
        let buffer = TxnBuffer::new(spill_at);
        for key in 0..12_000 {
            buffer.insert(
                key,
                CountedValue {
                    value: Some(key),
                    copies: copies.clone(),
                },
            );
        }
        buffer.insert(
            5,
            CountedValue {
                value: Some(99),
                copies: copies.clone(),
            },
        );
        buffer.insert(
            6,
            CountedValue {
                value: None,
                copies: copies.clone(),
            },
        );
        copies.store(0, Ordering::Relaxed);
        let mut range = buffer.chains_matching(|key| (5..7).contains(key), |_| true);
        range.sort_by_key(|(key, _)| *key);
        assert_eq!(range.len(), 2);
        assert_eq!(range[0].1.value, Some(99));
        assert_eq!(range[1].1.value, None);
        assert_eq!(copies.load(Ordering::Relaxed), 2);
        assert!(
            buffer
                .chains_matching(|key| *key >= 12_000, |_| true)
                .is_empty()
        );
        assert_eq!(copies.load(Ordering::Relaxed), 2);
        assert_eq!(buffer.chains_matching(|_| true, |_| true).len(), 12_000);
    }
}

#[test]
fn a_chain_runs_newest_first_through_the_last_replacement() {
    for spill_at in [0, 2] {
        let buffer = TxnBuffer::new(spill_at);
        // A negative value replaces its key outright; any other is an operand.
        for (key, value) in [
            (1, -1),
            (1, 10),
            (2, 20),
            (1, -2),
            (1, 30),
            (2, 40),
            (1, 50),
        ] {
            buffer.insert(key, value);
        }
        let replaces = |value: &i32| *value < 0;

        assert_eq!(chain_of(&buffer, &1, replaces), [50, 30, -2]);
        assert_eq!(chain_of(&buffer, &2, replaces), [40, 20]);
        assert!(chain_of(&buffer, &3, replaces).is_empty());

        let mut all = buffer.chains_matching(|_| true, replaces);
        // Stable, so a key keeps its newest-first order.
        all.sort_by_key(|(key, _)| *key);
        assert_eq!(all, [(1, 50), (1, 30), (1, -2), (2, 40), (2, 20)]);
        assert_eq!(
            buffer.chains_matching(|key| *key == 2, replaces),
            [(2, 40), (2, 20)]
        );
    }
}

#[test]
fn a_chain_among_ten_thousand_unrelated_entries_is_read_whole() {
    for spill_at in [0, 2, 16, 100_000] {
        let buffer = TxnBuffer::new(spill_at);
        // The key's first writes lie beneath the unrelated entries, the rest
        // above them, and the index (when there is one) was built in between.
        for value in [1, 2] {
            buffer.insert(1, value);
        }
        for key in 100..10_100 {
            buffer.insert(key, 0);
        }
        for value in [3, 4] {
            buffer.insert(1, value);
        }
        for key in 10_100..10_200 {
            buffer.insert(key, 0);
        }
        let never = |_: &i32| false;
        assert_eq!(chain_of(&buffer, &1, never), [4, 3, 2, 1], "{spill_at}");
        assert_eq!(chain_of(&buffer, &100, never), [0], "{spill_at}");
        assert!(chain_of(&buffer, &2, never).is_empty(), "{spill_at}");

        let mut seen = 0;
        buffer.walk_chain(&1, |_| {
            seen += 1;
            ControlFlow::Break(())
        });
        assert_eq!(seen, 1, "a walk that breaks stops there");
    }
}

#[test]
fn a_chain_spans_the_writes_before_the_index_and_after_it() {
    let buffer = TxnBuffer::new(4);
    for round in 0..3 {
        for key in 0..4 {
            buffer.insert(key, (key, round));
        }
    }
    assert!(buffer.is_indexed());
    for key in 0..4 {
        assert_eq!(
            chain_of(&buffer, &key, |_| false),
            [(key, 2), (key, 1), (key, 0)]
        );
        assert_eq!(buffer.get(&key), Some((key, 2)));
    }
}

/// Every ordering of `0..n`.
fn permutations(n: usize) -> Vec<Vec<usize>> {
    if n == 0 {
        return vec![Vec::new()];
    }
    permutations(n - 1)
        .into_iter()
        .flat_map(|rest| {
            (0..=rest.len()).map(move |at| {
                let mut order = rest.clone();
                order.insert(at, n - 1);
                order
            })
        })
        .collect()
}

#[test]
fn nodes_linked_in_any_order_form_one_chain_in_list_order() {
    for order in permutations(4) {
        let buffer = TxnBuffer::<usize, usize>::new(2);
        // Two writes of the key before the index exists, so its chain also
        // runs through nodes that carry no back-link, and a third entry to
        // build and seed the index.
        buffer.insert(1, 0);
        buffer.insert(1, 1);
        buffer.insert(2, 0);
        assert!(buffer.is_indexed());
        // Four more writes of the key, pushed and not yet linked.
        let pushed: Vec<_> = (2..6).map(|write| buffer.push(1, write)).collect();
        let spill = buffer.spill.get().expect("the index was built");
        for &at in &order {
            buffer.link(spill, pushed[at]);
        }
        assert_eq!(
            chain_of(&buffer, &1, |_| false),
            [5, 4, 3, 2, 1, 0],
            "{order:?}"
        );
        assert_eq!(buffer.get(&1), Some(5), "{order:?}");
    }
}

#[test]
fn a_node_linked_before_the_seed_does_not_hide_a_newer_one() {
    let buffer = TxnBuffer::<usize, usize>::new(0);
    // Two writes of one key. The inserter of the older stalls after its push,
    // and another thread publishes the index before it gets to link.
    buffer.insert(1, 10);
    buffer.insert(1, 20);
    let older = std::ptr::from_ref(buffer.nodes().nth(1).unwrap()).cast_mut();
    let spill = buffer.publish().expect("the first to publish");
    buffer.link(spill, older);

    // The map holds the older node and lags the list, so a lookup walks.
    assert_eq!(buffer.get(&1), Some(20));
    assert_eq!(chain_of(&buffer, &1, |_| false), [20, 10]);

    buffer.seed(spill);
    assert_eq!(buffer.get(&1), Some(20), "the seed names the newer node");
    buffer.insert(1, 30);
    assert_eq!(chain_of(&buffer, &1, |_| false), [30, 20, 10]);
}

#[test]
fn walking_a_chain_copies_no_value() {
    for spill_at in [0, 4] {
        let copies = Arc::new(AtomicUsize::new(0));
        let buffer = TxnBuffer::new(spill_at);
        for write in 0..50 {
            buffer.insert(
                write % 5,
                CountedValue {
                    value: Some(write),
                    copies: copies.clone(),
                },
            );
        }
        copies.store(0, Ordering::Relaxed);
        let mut seen = Vec::new();
        buffer.walk_chain(&3, |value| {
            seen.push(value.value);
            ControlFlow::Continue(())
        });
        assert_eq!(seen.len(), 10);
        assert_eq!(seen.first(), Some(&Some(48)));
        assert_eq!(seen.last(), Some(&Some(3)));
        assert_eq!(copies.load(Ordering::Relaxed), 0, "{spill_at}");
    }
}

/// Every key's chain, followed through its back-links, is the key's writes in
/// the order the list holds them, and the index names the newest.
fn assert_chains_match_the_list(
    buffer: &TxnBuffer<usize, (usize, usize)>,
    keys: impl IntoIterator<Item = usize>,
) {
    for key in keys {
        let listed: Vec<(usize, usize)> = buffer
            .chains_matching(|listed| *listed == key, |_| false)
            .into_iter()
            .map(|(_, value)| value)
            .collect();
        assert_eq!(chain_of(buffer, &key, |_| false), listed, "key {key}");
        assert_eq!(buffer.get(&key), listed.first().copied(), "key {key}");
    }
}

#[test]
fn threads_writing_one_key_leave_its_chain_and_the_index_in_list_order() {
    const WRITES: usize = 200;
    for spill_at in [2, 16] {
        for round in 0..50 {
            let buffer = TxnBuffer::new(spill_at);
            // Enough unrelated entries for the index to exist before the
            // threads start, and one more key each of them writes beside the
            // contested one.
            for key in 100..140 {
                buffer.insert(key, (key, 0));
            }
            let start = Barrier::new(2);
            std::thread::scope(|scope| {
                for thread in 0..2 {
                    let (buffer, start) = (&buffer, &start);
                    scope.spawn(move || {
                        start.wait();
                        for write in 0..WRITES {
                            buffer.insert(1, (thread, write));
                            buffer.insert(10 + thread, (thread, write));
                        }
                    });
                }
            });
            assert_eq!(buffer.len(), 40 + 4 * WRITES, "{spill_at} {round}");
            assert_chains_match_the_list(&buffer, [1, 10, 11, 100, 139]);
        }
    }
}

#[test]
fn threads_writing_one_key_while_the_index_is_built_lose_nothing() {
    const WRITES: usize = 60;
    for round in 0..100 {
        let buffer = TxnBuffer::new(8);
        let start = Barrier::new(3);
        std::thread::scope(|scope| {
            for thread in 0..3 {
                let (buffer, start) = (&buffer, &start);
                scope.spawn(move || {
                    start.wait();
                    for write in 0..WRITES {
                        buffer.insert(1, (thread, write));
                        buffer.insert(2 + write % 5, (thread, write));
                    }
                });
            }
        });
        assert!(buffer.is_indexed(), "{round}");
        assert_eq!(
            chain_of(&buffer, &1, |_| false).len(),
            3 * WRITES,
            "{round}"
        );
        assert_chains_match_the_list(&buffer, 1..7);
    }
}
