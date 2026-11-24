use super::*;
use crate::engine::arena::{ArenaProfile, ChunkPool};
use crate::engine::internal_key::{VALUE_TYPE_DELETION, VALUE_TYPE_VALUE, encode_internal_key};
use proptest::prelude::*;
use std::collections::BTreeMap;

fn list(budget: usize) -> ArenaSkipList {
    let profile = ArenaProfile::EMBEDDED;
    let pool = Arc::new(ChunkPool::new(profile, budget, 2));
    let arena = Arc::new(Arena::new(pool, budget, profile));
    ArenaSkipList::new(arena).expect("head allocation")
}

fn collect(list: &ArenaSkipList) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    let mut node = list.first();
    while let Some(current) = node {
        out.push((current.key().to_vec(), current.value().to_vec()));
        node = current.next();
    }
    out
}

#[test]
fn empty_list_has_no_entries() {
    let list = list(64 * 1024);
    assert!(list.is_empty());
    assert_eq!(list.len(), 0);
    assert!(list.first().is_none());
    assert!(list.last().is_none());
    assert!(list.seek_ge(b"anything").is_none());
    assert!(list.seek_le(b"anything").is_none());
    assert_eq!(
        list.arena().reserved_bytes(),
        0,
        "head is not an arena chunk"
    );
}

#[test]
fn insert_then_read_back() {
    let list = list(64 * 1024);
    assert!(list.insert(b"k", 1, VALUE_TYPE_VALUE, b"v1"));
    assert!(list.insert(b"k", 2, VALUE_TYPE_VALUE, b"v2"));
    assert!(list.insert(b"a", 3, VALUE_TYPE_DELETION, b""));
    assert_eq!(list.len(), 3);

    let entries = collect(&list);
    assert_eq!(entries.len(), 3);
    // "a" sorts first; for "k", the newer seq comes first.
    assert_eq!(
        entries[0].0,
        encode_internal_key(b"a", 3, VALUE_TYPE_DELETION)
    );
    assert_eq!(entries[0].1, b"");
    assert_eq!(entries[1].0, encode_internal_key(b"k", 2, VALUE_TYPE_VALUE));
    assert_eq!(entries[1].1, b"v2");
    assert_eq!(entries[2].1, b"v1");
}

#[test]
fn seeks_bracket_the_list() {
    let list = list(64 * 1024);
    for key in [b"b".as_slice(), b"m", b"y"] {
        assert!(list.insert(key, 1, VALUE_TYPE_VALUE, key));
    }
    let probe = |k: &[u8]| encode_internal_key(k, u64::MAX, VALUE_TYPE_DELETION);

    assert_eq!(list.seek_ge(&probe(b"a")).expect("first").value(), b"b");
    assert_eq!(list.seek_ge(&probe(b"m")).expect("exact").value(), b"m");
    assert_eq!(list.seek_ge(&probe(b"n")).expect("next").value(), b"y");
    assert!(list.seek_ge(&probe(b"z")).is_none());

    assert!(list.seek_lt(&probe(b"b")).is_none());
    assert_eq!(list.seek_lt(&probe(b"n")).expect("prev").value(), b"m");
    assert_eq!(list.last().expect("last").value(), b"y");
    assert_eq!(list.first().expect("first").value(), b"b");
}

#[test]
fn an_internal_key_inserted_twice_is_held_once() {
    // S2: two threads applying one commit, or WAL replay re-presenting a
    // (key, seq) an interrupted rewrite left twice, insert the same entry.
    // The list holds one node for it, the one that got there first.
    let list = list(64 * 1024);
    assert!(list.insert(b"k", 7, VALUE_TYPE_VALUE, b"first"));
    let used = list.arena().used_bytes();
    assert!(list.insert(b"k", 7, VALUE_TYPE_VALUE, b"second"));
    assert_eq!(list.len(), 1);
    assert_eq!(
        list.arena().used_bytes(),
        used,
        "a duplicate found before the node is built costs no arena bytes"
    );
    let probe = encode_internal_key(b"k", 7, VALUE_TYPE_DELETION);
    assert_eq!(list.seek_ge(&probe).expect("present").value(), b"first");
    assert_eq!(collect(&list).len(), 1);
    // Another sequence, or another type at the same sequence, is another key.
    assert!(list.insert(b"k", 8, VALUE_TYPE_VALUE, b"newer"));
    assert!(list.insert(b"k", 7, VALUE_TYPE_DELETION, b""));
    assert_eq!(list.len(), 3);
    list.assert_towers_consistent();
}

/// S2: threads insert disjoint keys and, two by two, the very same run of
/// keys, all at once. Afterwards the list holds every key exactly once, in
/// order, with consistent towers.
#[test]
fn concurrent_inserters_hold_every_key_once() {
    const PAIRS: u64 = 4;
    // Interpreted, an insert costs thousands of times what it does native;
    // a dozen keys a thread still race every pair on every level.
    const KEYS: u64 = if cfg!(miri) { 12 } else { 400 };
    const ROUNDS: u64 = if cfg!(miri) { 1 } else { 8 };
    for round in 0..ROUNDS {
        let list = list(8 * 1024 * 1024);
        std::thread::scope(|scope| {
            for thread in 0..2 * PAIRS {
                let list = &list;
                // Threads `2p` and `2p + 1` insert the same entries.
                let pair = thread / 2;
                scope.spawn(move || {
                    let mut hint = list.insert_hint();
                    for i in 0..KEYS {
                        let key = format!("k{:05}", i * PAIRS + pair);
                        let seq = i * PAIRS + pair + 1;
                        if (thread + i) % 3 == 0 {
                            assert!(list.insert(key.as_bytes(), seq, VALUE_TYPE_VALUE, b"v"));
                        } else {
                            assert!(list.insert_with_hint(
                                &mut hint,
                                key.as_bytes(),
                                seq,
                                VALUE_TYPE_VALUE,
                                b"v"
                            ));
                        }
                    }
                });
            }
        });
        let got = collect(&list);
        assert_eq!(got.len() as u64, PAIRS * KEYS, "round {round}");
        assert_eq!(list.len() as u64, PAIRS * KEYS, "round {round}");
        for (n, (key, value)) in got.iter().enumerate() {
            let want = encode_internal_key(
                format!("k{n:05}").as_bytes(),
                n as u64 + 1,
                VALUE_TYPE_VALUE,
            );
            assert_eq!(key, &want, "round {round}, entry {n}");
            assert_eq!(value, b"v");
        }
        list.assert_towers_consistent();
    }
}

#[test]
fn prefix_keys_sort_by_the_internal_comparator() {
    // Raw byte order would interleave these wrongly: "ab"'s !seq
    // trailer collides with "abc"'s literal 'c'.
    let list = list(64 * 1024);
    assert!(list.insert(b"abc", 1, VALUE_TYPE_VALUE, b"abc"));
    assert!(list.insert(b"ab", u64::MAX, VALUE_TYPE_VALUE, b"ab-high"));
    assert!(list.insert(b"ab", 0, VALUE_TYPE_VALUE, b"ab-low"));
    let values: Vec<Vec<u8>> = collect(&list).into_iter().map(|(_, v)| v).collect();
    assert_eq!(
        values,
        vec![b"ab-high".to_vec(), b"ab-low".to_vec(), b"abc".to_vec()]
    );
}

#[test]
fn empty_value_round_trips() {
    let list = list(64 * 1024);
    assert!(list.insert(b"k", 1, VALUE_TYPE_DELETION, b""));
    let node = list.first().expect("present");
    assert_eq!(node.value(), b"");
    assert_eq!(node.value_span().1, 0);
    assert!(node.value_span().0.is_none());
}

#[test]
fn a_value_larger_than_a_chunk_still_lands() {
    let list = list(64 * 1024);
    let big = vec![7u8; 300 * 1024];
    assert!(list.insert(b"big", 1, VALUE_TYPE_VALUE, &big));
    assert_eq!(list.first().expect("present").value(), big.as_slice());
}

#[test]
#[cfg_attr(
    miri,
    ignore = "20k inserts against 8 spinning readers; \
              `a_reader_walks_the_list_while_a_writer_publishes` is the miri-sized form"
)]
fn readers_see_whole_entries_while_a_writer_inserts() {
    // S1: a reader must never observe a node with a torn key or a
    // value that does not match it.
    let budget = 4 * 1024 * 1024;
    let profile = ArenaProfile::SERVER;
    let pool = Arc::new(ChunkPool::new(profile, budget, 2));
    let arena = Arc::new(Arena::new(pool, budget, profile));
    let list = Arc::new(ArenaSkipList::new(arena).expect("head"));

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let readers: Vec<_> = (0..8)
        .map(|_| {
            let list = Arc::clone(&list);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let mut node = list.first();
                    while let Some(current) = node {
                        let key = current.key();
                        assert!(key.len() >= INTERNAL_KEY_SUFFIX_LEN);
                        let user = &key[..key.len() - INTERNAL_KEY_SUFFIX_LEN];
                        assert_eq!(
                            current.value(),
                            user,
                            "value must match the key it was written with"
                        );
                        node = current.next();
                    }
                }
            })
        })
        .collect();

    for i in 0..20_000u32 {
        let key = format!("key{i:08}");
        assert!(list.insert(
            key.as_bytes(),
            u64::from(i) + 1,
            VALUE_TYPE_VALUE,
            key.as_bytes()
        ));
    }
    stop.store(true, Ordering::Relaxed);
    for reader in readers {
        reader.join().expect("reader thread");
    }
    assert_eq!(list.len(), 20_000);
}

#[test]
#[cfg_attr(
    miri,
    ignore = "40k inserts against 4 spinning readers; \
              `a_reader_walks_the_list_while_a_writer_publishes` is the miri-sized form"
)]
fn a_seeded_key_stays_findable_while_a_writer_inserts_around_it() {
    // The two-step seek this replaced (descend, then re-read the
    // predecessor's level-0 link) could hand back a node inserted
    // after the descent finished, which a reader reads as "absent".
    let budget = 8 * 1024 * 1024;
    let profile = ArenaProfile::SERVER;
    let pool = Arc::new(ChunkPool::new(profile, budget, 2));
    let arena = Arc::new(Arena::new(pool, budget, profile));
    let list = Arc::new(ArenaSkipList::new(arena).expect("head"));
    assert!(list.insert(b"pinned", 1, VALUE_TYPE_VALUE, b"stable"));

    let probe = encode_internal_key(b"pinned", u64::MAX, VALUE_TYPE_DELETION);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let misses = Arc::new(AtomicUsize::new(0));
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let list = Arc::clone(&list);
            let stop = Arc::clone(&stop);
            let misses = Arc::clone(&misses);
            let probe = probe.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let found = list
                        .seek_ge(&probe)
                        .filter(|entry| entry.value() == b"stable");
                    if found.is_none() {
                        misses.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();

    for i in 0..40_000u64 {
        // Keys on both sides of "pinned", so inserts land right next
        // to the seek target at every level.
        let key = if i % 2 == 0 {
            format!("pinne{i:08}")
        } else {
            format!("pinnee{i:08}")
        };
        assert!(list.insert(key.as_bytes(), i + 2, VALUE_TYPE_VALUE, b"noise"));
    }
    stop.store(true, Ordering::Relaxed);
    for reader in readers {
        reader.join().expect("reader thread");
    }
    assert_eq!(
        misses.load(Ordering::Relaxed),
        0,
        "a present key must never read as absent"
    );
}

/// The publication protocol at a size an interpreter can finish.
///
/// The two stress tests above are the ones that catch a rare race by
/// volume; this one exists so miri's aliasing model and data-race
/// detector actually reach the same code. One writer publishes eight
/// nodes while one reader walks the list, reads every key and value,
/// and re-seeks a key that was already there: S1, S3, S5, S6 and S7
/// all sit on that path.
#[test]
fn a_reader_walks_the_list_while_a_writer_publishes() {
    let skiplist = Arc::new(list(64 * 1024));
    assert!(skiplist.insert(b"seed", 1, VALUE_TYPE_VALUE, b"seed"));
    let probe = encode_internal_key(b"seed", u64::MAX, VALUE_TYPE_DELETION);

    let reader = {
        let skiplist = Arc::clone(&skiplist);
        let probe = probe.clone();
        std::thread::spawn(move || {
            for _ in 0..8 {
                let mut cursor = skiplist.first();
                while let Some(current) = cursor {
                    let key = current.key();
                    assert!(key.len() >= INTERNAL_KEY_SUFFIX_LEN);
                    let user = &key[..key.len() - INTERNAL_KEY_SUFFIX_LEN];
                    assert_eq!(current.value(), user);
                    cursor = current.next();
                }
                assert!(
                    skiplist.seek_ge(&probe).is_some(),
                    "S3: a published key cannot be lost"
                );
            }
        })
    };

    for i in 0..8u64 {
        let key = format!("k{i}");
        assert!(skiplist.insert(key.as_bytes(), i + 2, VALUE_TYPE_VALUE, key.as_bytes()));
    }
    reader.join().expect("reader thread");
    assert_eq!(skiplist.len(), 9);
}

#[test]
fn the_start_level_follows_the_hinted_bracket() {
    let list = list(1024 * 1024);
    let mut hint = list.insert_hint();
    assert!(list.insert_with_hint(&mut hint, b"k10", 1, VALUE_TYPE_VALUE, b"v10"));

    let level_for = |key: &[u8], seq: u64| {
        let trailer = internal_trailer(seq, VALUE_TYPE_VALUE);
        list.hint_start_level(&hint, key, &trailer)
    };

    assert_eq!(
        level_for(b"k20", 1),
        0,
        "k20 sorts after the hinted predecessor with a null successor: bracketed at level 0"
    );
    assert_eq!(
        level_for(b"k05", 1),
        MAX_HEIGHT,
        "k05 sorts before the hinted predecessor and must restart from the head"
    );
    assert_eq!(
        level_for(b"k10", 1),
        MAX_HEIGHT,
        "a duplicate of the hinted key is not strictly after it and must restart from the head"
    );

    assert!(list.insert(b"k30", 2, VALUE_TYPE_VALUE, b"v30"));
    assert!(list.insert(b"k40", 3, VALUE_TYPE_VALUE, b"v40"));

    let level = level_for(b"k35", 1);
    assert!(
        level > 0 && level < MAX_HEIGHT,
        "the plain inserts made level 0 non-adjacent (k10's successor is now k30, not null), \
         so the search must climb past level 0 rather than restarting from the head: got {level}"
    );
}

#[test]
fn a_hint_from_another_list_restarts_from_that_lists_head() {
    let a = list(1024 * 1024);
    let b = list(1024 * 1024);
    let mut hint = a.insert_hint();
    assert!(a.insert_with_hint(&mut hint, b"k10", 1, VALUE_TYPE_VALUE, b"va10"));
    assert!(a.insert_with_hint(&mut hint, b"k20", 2, VALUE_TYPE_VALUE, b"va20"));
    let a_before = collect(&a);

    // "k15" would bracket between k10 and k20 in A; using A's hint on B
    // must restart from B's own head rather than link into A's tower.
    assert!(b.insert_with_hint(&mut hint, b"k15", 3, VALUE_TYPE_VALUE, b"vb15"));

    assert_eq!(
        collect(&b),
        vec![(
            encode_internal_key(b"k15", 3, VALUE_TYPE_VALUE),
            b"vb15".to_vec()
        )]
    );
    assert_eq!(
        collect(&a),
        a_before,
        "the hint from A must never touch A after being used on B"
    );
    a.assert_towers_consistent();
    b.assert_towers_consistent();
}

#[test]
fn a_hinted_insert_of_a_key_already_held_links_nothing_like_a_plain_insert() {
    let list = list(1024 * 1024);
    let mut hint = list.insert_hint();
    assert!(list.insert_with_hint(&mut hint, b"dup", 5, VALUE_TYPE_VALUE, b"first"));
    assert!(list.insert_with_hint(&mut hint, b"dup", 5, VALUE_TYPE_VALUE, b"second"));
    // The hint still works for the keys after the duplicate.
    assert!(list.insert_with_hint(&mut hint, b"dup", 4, VALUE_TYPE_VALUE, b"older"));
    assert!(list.insert_with_hint(&mut hint, b"e", 6, VALUE_TYPE_VALUE, b"next"));

    assert_eq!(collect(&list).len(), 3);
    let probe = encode_internal_key(b"dup", 5, VALUE_TYPE_VALUE);
    let found = list.seek_ge(&probe).expect("present");
    assert_eq!(
        found.value(),
        b"first",
        "the entry that got there first stays"
    );
    list.assert_towers_consistent();
}

proptest! {
    #[test]
    fn insert_then_seek_round_trips(
        entries in proptest::collection::vec(
            (proptest::collection::vec(any::<u8>(), 0..24), 1u64..1000, any::<Vec<u8>>()),
            1..80,
        ),
    ) {
        let list = list(1024 * 1024);
        let mut model: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for (key, seq, value) in &entries {
            prop_assert!(list.insert(key, *seq, VALUE_TYPE_VALUE, value));
            let internal = encode_internal_key(key, *seq, VALUE_TYPE_VALUE);
            // S2: an internal key the list already holds keeps the value it
            // was first inserted with.
            if !model.iter().any(|(held, _)| held == &internal) {
                model.push((internal, value.clone()));
            }
        }
        model.sort_by(|a, b| compare_internal_keys(&a.0, &b.0));

        let got = collect(&list);
        prop_assert_eq!(&got, &model);
        prop_assert_eq!(list.len(), model.len());

        // Every stored key is findable by an exact seek.
        for (key, _) in &model {
            let found = list.seek_ge(key).expect("stored key is findable");
            prop_assert!(compare_internal_keys(found.key(), key).is_eq());
        }
    }

    #[test]
    fn seeks_agree_with_a_linear_scan(
        keys in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..12), 1..40),
        probe in proptest::collection::vec(any::<u8>(), 0..12),
    ) {
        let list = list(1024 * 1024);
        for (i, key) in keys.iter().enumerate() {
            prop_assert!(list.insert(key, i as u64 + 1, VALUE_TYPE_VALUE, key));
        }
        let sorted = collect(&list);
        let target = encode_internal_key(&probe, u64::MAX, VALUE_TYPE_DELETION);

        let want_ge = sorted.iter().find(|(k, _)| compare_internal_keys(k, &target).is_ge());
        let got_ge = list.seek_ge(&target).map(|n| n.key().to_vec());
        prop_assert_eq!(got_ge.as_deref(), want_ge.map(|(k, _)| k.as_slice()));

        let want_lt = sorted.iter().rev().find(|(k, _)| compare_internal_keys(k, &target).is_lt());
        let got_lt = list.seek_lt(&target).map(|n| n.key().to_vec());
        prop_assert_eq!(got_lt.as_deref(), want_lt.map(|(k, _)| k.as_slice()));

        let want_le = sorted.iter().rev().find(|(k, _)| compare_internal_keys(k, &target).is_le());
        let got_le = list.seek_le(&target).map(|n| n.key().to_vec());
        prop_assert_eq!(got_le.as_deref(), want_le.map(|(k, _)| k.as_slice()));

        let want_gt = sorted.iter().find(|(k, _)| compare_internal_keys(k, &target).is_gt());
        let got_gt = list.seek_gt(&target).map(|n| n.key().to_vec());
        prop_assert_eq!(got_gt.as_deref(), want_gt.map(|(k, _)| k.as_slice()));
    }

    #[test]
    fn node_accounting_matches_the_layout(
        key in proptest::collection::vec(any::<u8>(), 0..2048),
        value in proptest::collection::vec(any::<u8>(), 0..4096),
    ) {
        let list = list(1024 * 1024);
        prop_assert!(list.insert(&key, 5, VALUE_TYPE_VALUE, &value));
        let node = list.first().expect("present");
        prop_assert_eq!(node.key().len(), key.len() + INTERNAL_KEY_SUFFIX_LEN);
        prop_assert_eq!(node.value(), value.as_slice());
        // The arena charged the full node, not just key + value.
        let used = list.arena().used_bytes();
        prop_assert!(used >= key.len() + value.len() + INTERNAL_KEY_SUFFIX_LEN + NODE_HEADER);
    }

    /// Every hinted insert against a `BTreeMap<OrdKey, Vec<u8>>` oracle,
    /// cross-checked against a plain-insert list built from the same
    /// entries.
    ///
    /// The value is `seq.to_le_bytes()`, so a duplicate `(key, seq)`
    /// always carries the identical value regardless of which insert
    /// wrote it last, which is what makes the oracle exact: collapsing
    /// consecutive identical pairs out of the hinted list's scan
    /// recovers precisely the oracle's key set.
    #[test]
    fn hinted_runs_match_the_head_descent_and_a_btreemap_oracle(
        runs in proptest::collection::vec(
            (
                proptest::collection::vec(
                    (proptest::collection::vec(any::<u8>(), 0..24), 1u64..1000),
                    1..20,
                ),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
            ),
            1..8,
        ),
        probes in proptest::collection::vec(
            (proptest::collection::vec(any::<u8>(), 0..24), any::<u64>()),
            8,
        ),
    ) {
        let hinted = list(1024 * 1024);
        let plain = list(1024 * 1024);
        let mut oracle: BTreeMap<OrdKey, Vec<u8>> = BTreeMap::new();
        let mut hint = hinted.insert_hint();

        for (entries, sorted, fresh_hint, plain_between) in &runs {
            if *fresh_hint {
                hint = hinted.insert_hint();
            }
            let mut entries = entries.clone();
            if *sorted {
                entries.sort_by(|(ak, aseq), (bk, bseq)| {
                    let a = encode_internal_key(ak, *aseq, VALUE_TYPE_VALUE);
                    let b = encode_internal_key(bk, *bseq, VALUE_TYPE_VALUE);
                    compare_internal_keys(&a, &b)
                });
            }
            for (key, seq) in &entries {
                let value = seq.to_le_bytes().to_vec();
                prop_assert!(hinted.insert_with_hint(&mut hint, key, *seq, VALUE_TYPE_VALUE, &value));
                hinted.assert_towers_consistent();
                prop_assert!(plain.insert(key, *seq, VALUE_TYPE_VALUE, &value));
                let internal = encode_internal_key(key, *seq, VALUE_TYPE_VALUE);
                oracle.insert(OrdKey(internal), value);
            }
            if *plain_between
                && let Some((first_key, first_seq)) = entries.first()
            {
                let seq = first_seq + 1000;
                let value = seq.to_le_bytes().to_vec();
                prop_assert!(hinted.insert(first_key, seq, VALUE_TYPE_VALUE, &value));
                prop_assert!(plain.insert(first_key, seq, VALUE_TYPE_VALUE, &value));
                let internal = encode_internal_key(first_key, seq, VALUE_TYPE_VALUE);
                oracle.insert(OrdKey(internal), value);
            }
        }

        let mut got = collect(&hinted);
        got.dedup();
        let want: Vec<(Vec<u8>, Vec<u8>)> =
            oracle.iter().map(|(k, v)| (k.0.clone(), v.clone())).collect();
        prop_assert_eq!(got, want);
        prop_assert_eq!(tower_shape(&hinted), tower_shape(&plain));

        let mut probe_internals: Vec<Vec<u8>> = oracle.keys().map(|k| k.0.clone()).collect();
        for (key, seq) in &probes {
            probe_internals.push(encode_internal_key(key, *seq, VALUE_TYPE_VALUE));
        }
        for probe in probe_internals {
            let probe_key = OrdKey(probe.clone());
            let want_ge = oracle.range(probe_key.clone()..).next().map(|(k, _)| k.0.clone());
            let got_ge = hinted.seek_ge(&probe).map(|n| n.key().to_vec());
            prop_assert_eq!(got_ge, want_ge);

            let want_lt = oracle.range(..probe_key).next_back().map(|(k, _)| k.0.clone());
            let got_lt = hinted.seek_lt(&probe).map(|n| n.key().to_vec());
            prop_assert_eq!(got_lt, want_lt);
        }
    }
}
