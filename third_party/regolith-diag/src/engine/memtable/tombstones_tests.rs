//! Tests of the append-only tombstone log: publication under racing
//! writers, the index against a linear model across rebuilds, and the
//! listing order the memtable has always answered in.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};

use proptest::prelude::*;

use super::*;
use crate::engine::range_tombstone::sort_dedup_tombstones;

fn tomb(start: &[u8], end: &[u8], seq: u64) -> RangeTombstone {
    RangeTombstone::new(start.to_vec(), end.to_vec(), seq)
}

/// The answer a linear walk over every tombstone gives.
fn model_covering(all: &[(Vec<u8>, Vec<u8>, u64)], key: &[u8], snapshot: u64) -> u64 {
    all.iter()
        .filter(|(s, e, q)| s.as_slice() <= key && key < e.as_slice() && *q <= snapshot)
        .map(|(_, _, q)| *q)
        .max()
        .unwrap_or(0)
}

#[test]
fn segments_cover_every_position_without_a_gap() {
    let mut expected = 0usize;
    for position in 0..100_000usize {
        let (segment, offset) = locate(position);
        assert!(offset < segment_len(segment), "position {position}");
        if offset == 0 && position != 0 {
            expected += 1;
        }
        assert_eq!(segment, expected, "position {position}");
    }
    assert!(locate(u32::MAX as usize).0 < SEGMENTS);
}

#[test]
fn an_empty_log_answers_nothing_and_holds_nothing() {
    let log = TombstoneLog::new();
    assert_eq!(log.len(), 0);
    assert_eq!(log.bytes(), 0);
    assert_eq!(log.covering_seq(b"k", u64::MAX), 0);
    assert!(log.first_newer_overlap(b"a", b"z", 0, u64::MAX).is_none());
    assert!(log.to_sorted_vec().is_empty());
}

#[test]
fn a_rebuild_keeps_every_answer_the_tail_gave() {
    let log = TombstoneLog::new();
    let mut all = Vec::new();
    for i in 0..(REBUILD as u64 * 3 + 5) {
        let start = format!("k{:03}", (i * 7) % 50).into_bytes();
        let end = format!("k{:03}", (i * 7) % 50 + 1 + i % 9).into_bytes();
        log.push(tomb(&start, &end, i + 1));
        all.push((start, end, i + 1));
        // Under miri every seventh key: all sixty after each append take ten
        // minutes in the interpreter, and the rebuilds they cross are the same.
        for probe in (0..60u64).step_by(if cfg!(miri) { 7 } else { 1 }) {
            let key = format!("k{probe:03}").into_bytes();
            for snapshot in [0, i / 2 + 1, u64::MAX] {
                assert_eq!(
                    log.covering_seq(&key, snapshot),
                    model_covering(&all, &key, snapshot),
                    "after {} appends, key {probe}, snapshot {snapshot}",
                    i + 1
                );
            }
        }
    }
    assert!(log.index.load().is_some(), "setup: an index was built");
}

#[test]
fn the_listing_is_sorted_and_deduplicated_as_before() {
    let log = TombstoneLog::new();
    for t in [
        tomb(b"m", b"p", 3),
        tomb(b"a", b"c", 1),
        tomb(b"a", b"c", 1),
        tomb(b"a", b"c", 2),
        tomb(b"a", b"b", 4),
    ] {
        log.push(t);
    }
    let listed: Vec<_> = log
        .to_sorted_vec()
        .into_iter()
        .map(|t| (t.start, t.end, t.seq))
        .collect();
    assert_eq!(
        listed,
        vec![
            (b"a".to_vec(), b"b".to_vec(), 4),
            (b"a".to_vec(), b"c".to_vec(), 2),
            (b"a".to_vec(), b"c".to_vec(), 1),
            (b"m".to_vec(), b"p".to_vec(), 3),
        ]
    );
}

/// The reader's half of T2, under a writer that never stops: every ready
/// position a reader acquires holds a whole tombstone, in append order, a
/// ready position stays ready, and a later read never sees fewer.
#[test]
fn readers_see_a_growing_prefix_of_whole_appends_while_a_writer_appends() {
    // Sized for the interpreter under miri: 100 appends still cross three
    // segment boundaries and six rebuilds of the index under four readers.
    const APPENDS: u64 = if cfg!(miri) { 100 } else { 4_000 };
    let log = Arc::new(TombstoneLog::new());
    let done = Arc::new(AtomicBool::new(false));
    let checked = Arc::new(AtomicU64::new(0));
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let (log, done, checked) = (Arc::clone(&log), Arc::clone(&done), Arc::clone(&checked));
            scope.spawn(move || {
                let mut last = 0usize;
                while !done.load(std::sync::atomic::Ordering::Acquire) || last < APPENDS as usize {
                    let len = log.len();
                    assert!(len >= last, "the prefix shrank from {last} to {len}");
                    // The one writer's appends become ready in order, so the
                    // ready positions are a prefix of the ones handed out.
                    let mut ready = last;
                    while ready < len {
                        let Some(t) = log.get(ready) else {
                            break;
                        };
                        let seq = ready as u64 + 1;
                        assert_eq!(t.seq, seq, "append {ready} out of order");
                        assert_eq!(t.start, format!("s{seq:06}").into_bytes(), "torn append");
                        assert_eq!(t.end, format!("t{seq:06}").into_bytes(), "torn append");
                        ready += 1;
                    }
                    // Every ready tombstone covers its own start, and a
                    // lookup never misses one a scan already read.
                    if ready > 0 {
                        let key = format!("s{ready:06}").into_bytes();
                        let seen = log.covering_seq(&key, u64::MAX);
                        assert!(seen >= ready as u64, "a ready tombstone was missed");
                    }
                    checked.fetch_add((ready - last) as u64, std::sync::atomic::Ordering::Relaxed);
                    last = ready;
                }
            });
        }
        for seq in 1..=APPENDS {
            log.push(tomb(
                format!("s{seq:06}").as_bytes(),
                format!("t{seq:06}").as_bytes(),
                seq,
            ));
        }
        done.store(true, std::sync::atomic::Ordering::Release);
    });
    assert_eq!(
        checked.load(std::sync::atomic::Ordering::Relaxed),
        4 * APPENDS,
        "every reader checked every append"
    );
}

/// T1: many threads append at once, each its own tombstones, and every one
/// of them is in the log afterwards, whole.
#[test]
fn concurrent_appenders_lose_nothing() {
    // Sized for the interpreter as the appends beside readers above are.
    const THREADS: u64 = if cfg!(miri) { 4 } else { 8 };
    const EACH: u64 = if cfg!(miri) { 12 } else { 500 };
    let log = TombstoneLog::new();
    std::thread::scope(|scope| {
        for t in 0..THREADS {
            let log = &log;
            scope.spawn(move || {
                for i in 0..EACH {
                    let seq = t * EACH + i + 1;
                    log.push(tomb(
                        format!("s{seq:06}").as_bytes(),
                        format!("t{seq:06}").as_bytes(),
                        seq,
                    ));
                }
            });
        }
    });
    let listed = log.to_sorted_vec();
    assert_eq!(listed.len() as u64, THREADS * EACH);
    for seq in 1..=THREADS * EACH {
        let key = format!("s{seq:06}");
        assert_eq!(log.covering_seq(key.as_bytes(), u64::MAX), seq, "{key}");
    }
}

/// T1: two threads applying one commit append the same tombstones. Whatever
/// the interleaving, every listing holds each of them once and every query
/// answers as if it had been appended once.
#[test]
fn the_same_tombstones_appended_by_two_threads_are_listed_once() {
    const ROUNDS: u64 = if cfg!(miri) { 3 } else { 200 };
    for round in 0..ROUNDS {
        let log = TombstoneLog::new();
        log.push(tomb(b"a", b"c", 1));
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let log = &log;
                scope.spawn(move || {
                    for seq in 2..10u64 {
                        log.push(tomb(b"b", format!("k{seq}").as_bytes(), seq));
                    }
                });
            }
        });
        let listed = log.to_sorted_vec();
        assert_eq!(listed.len(), 9, "round {round}: {listed:?}");
        assert_eq!(log.covering_seq(b"b", u64::MAX), 9);
        assert_eq!(log.covering_seq(b"b", 5), 5);
        assert_eq!(log.covering_seq(b"a", u64::MAX), 1);
    }
}

/// T1: an append of a tombstone the log already holds adds nothing.
#[test]
fn an_append_of_a_tombstone_already_held_adds_nothing() {
    let log = TombstoneLog::new();
    log.push(tomb(b"a", b"m", 4));
    log.push(tomb(b"c", b"z", 5));
    let (len, bytes) = (log.len(), log.bytes());
    log.push(tomb(b"c", b"z", 5));
    log.push(tomb(b"a", b"m", 4));
    assert_eq!((log.len(), log.bytes()), (len, bytes));
    // The same range at another sequence is another tombstone.
    log.push(tomb(b"a", b"m", 6));
    assert_eq!(log.len(), len + 1);
}

/// T2 and T4: a position handed out whose append has not finished is
/// skipped by every query, the index stops in front of it, and the appends
/// after it are still read.
#[test]
fn a_position_whose_append_has_not_finished_is_skipped_and_not_indexed() {
    let log = TombstoneLog::new();
    for seq in 1..=3u64 {
        log.push(tomb(b"a", b"b", seq));
    }
    // A thread took position 3 and was descheduled before writing it.
    let stuck = log.claimed.fetch_add(1, Ordering::AcqRel);
    for seq in 4..=(4 + 3 * REBUILD as u64) {
        log.push(tomb(b"a", b"b", seq));
    }
    let newest = 4 + 3 * REBUILD as u64;
    assert_eq!(log.covering_seq(b"a", u64::MAX), newest);
    assert_eq!(log.covering_seq(b"a", 3), 3);
    assert_eq!(log.to_sorted_vec().len() as u64, newest);
    assert!(log.any(|t| t.seq == newest));
    assert!(
        log.index.load().is_none_or(|index| index.covered <= stuck),
        "the index covered a position that is not ready"
    );
}

/// `count` range tombstones at one sequence, no two touching, in an order
/// that is neither their key order nor its reverse: `d00000a..d00000m`,
/// `d00001a..d00001m`, and so on. What an ingested table that deleted
/// `count` separate ranges carries, all at the table's one sequence.
fn disjoint_at_one_sequence(count: u64, seq: u64) -> Vec<RangeTombstone> {
    // Seven shares no factor with the counts used here, so this visits
    // every one of them once.
    assert!(!count.is_multiple_of(7));
    (0..count)
        .map(|i| (i * 7 + 3) % count)
        .map(|i| {
            tomb(
                format!("d{i:05}a").as_bytes(),
                format!("d{i:05}m").as_bytes(),
                seq,
            )
        })
        .collect()
}

/// The cost of a batch, counted: one index and no walk through the log for
/// a duplicate, where the same tombstones appended one at a time build an
/// index every [`REBUILD`] of them and walk the whole log for each. The
/// answers are the same.
#[test]
fn a_batch_of_thousands_builds_one_index_and_walks_the_log_for_no_duplicate() {
    // Under miri a hundred: the interpreter takes minutes for thousands, and
    // the counts below are exact at any size.
    const COUNT: u64 = if cfg!(miri) { 96 } else { 4_096 };
    let batch = disjoint_at_one_sequence(COUNT, 20);
    assert_eq!(batch.len() as u64, COUNT);
    assert!(
        batch.windows(2).any(|pair| pair[0].start > pair[1].start),
        "the batch arrives unsorted"
    );

    let one_by_one = TombstoneLog::new();
    work_since_last_call();
    for tombstone in batch.iter().cloned() {
        one_by_one.push(tombstone);
    }
    let (indexes, looks) = work_since_last_call();
    assert_eq!(indexes as u64, COUNT / REBUILD as u64);
    assert_eq!(
        looks as u64,
        COUNT * (COUNT - 1) / 2,
        "every append at one sequence walks the whole log"
    );

    let at_once = TombstoneLog::new();
    // With a copy of one of them in it twice: a batch may repeat itself.
    let mut repeated = batch.clone();
    repeated.push(batch[3].clone());
    at_once.extend(repeated);
    assert_eq!(work_since_last_call(), (1, 0));
    assert_eq!(at_once.len() as u64, COUNT);
    assert_eq!(at_once.bytes(), one_by_one.bytes());
    assert_eq!(
        at_once.index.load().map(|index| index.covered as u64),
        Some(COUNT),
        "the one index covers the whole batch"
    );

    let listing = |log: &TombstoneLog| -> Vec<(Vec<u8>, Vec<u8>, u64)> {
        log.to_sorted_vec()
            .into_iter()
            .map(|t| (t.start, t.end, t.seq))
            .collect()
    };
    assert_eq!(listing(&at_once), listing(&one_by_one));
    for i in (0..COUNT).step_by(if cfg!(miri) { 7 } else { 1 }) {
        let inside = format!("d{i:05}c").into_bytes();
        let between = format!("d{i:05}p").into_bytes();
        for log in [&at_once, &one_by_one] {
            assert_eq!(log.covering_seq(&inside, u64::MAX), 20);
            assert_eq!(log.covering_seq(&inside, 19), 0);
            assert_eq!(log.covering_seq(&between, u64::MAX), 0);
        }
    }
}

/// T1 for a batch: what the log already holds is not appended again, by
/// one look at the log, and a batch given twice adds nothing the second
/// time.
#[test]
fn a_batch_appends_only_what_the_log_does_not_hold() {
    const COUNT: u64 = if cfg!(miri) { 48 } else { 1_024 };
    let log = TombstoneLog::new();
    log.push(tomb(b"a", b"c", 3));
    log.push(tomb(b"d00002a", b"d00002m", 20));
    let batch = disjoint_at_one_sequence(COUNT, 20);
    work_since_last_call();

    log.extend(batch.clone());
    // One of the batch was there already.
    assert_eq!(log.len() as u64, 2 + COUNT - 1);
    assert_eq!(work_since_last_call(), (1, 0));
    let (len, bytes) = (log.len(), log.bytes());

    log.extend(batch);
    assert_eq!((log.len(), log.bytes()), (len, bytes));
    assert_eq!(
        work_since_last_call(),
        (0, 0),
        "nothing new: no index to build"
    );
    assert_eq!(log.to_sorted_vec().len(), len);
    assert_eq!(log.covering_seq(b"b", u64::MAX), 3);
    assert_eq!(log.covering_seq(b"d00002c", u64::MAX), 20);

    // The same range at another sequence is another tombstone, and an
    // empty batch is nothing at all.
    log.extend(vec![tomb(b"a", b"c", 4)]);
    assert_eq!(log.len(), len + 1);
    log.extend(Vec::new());
    assert_eq!(log.len(), len + 1);
    // A commit applied after a batch still finds its own tombstone.
    log.push(tomb(b"q", b"r", 30));
    log.push(tomb(b"q", b"r", 30));
    assert_eq!(log.len(), len + 2);
}

/// T4: an append that claims its position after a rebuild found every
/// position below its bound ready is left out of the view the rebuild
/// publishes, so once it finishes every query finds it. The view covering
/// it as an empty placeholder would hide it: a lookup stops at the old
/// ends, and the scan past the view starts above it.
#[test]
fn a_position_claimed_while_a_rebuild_scans_is_not_indexed() {
    let log = Arc::new(TombstoneLog::new());
    for seq in 1..REBUILD as u64 {
        log.push(tomb(b"b", b"m", seq));
    }
    // A second helper claims the next position right after the rebuild the
    // REBUILD-th append starts has found every position below its bound
    // ready, and pauses before it writes the slot.
    let helper = Arc::clone(&log);
    after_next_ready_scan(move || {
        let claimed = helper.claimed.fetch_add(1, Ordering::AcqRel);
        assert_eq!(
            claimed, REBUILD,
            "setup: the claim lands right past the scan"
        );
    });
    log.push(tomb(b"b", b"m", REBUILD as u64));
    assert_eq!(
        log.len(),
        REBUILD + 1,
        "setup: the helper claimed during the rebuild"
    );
    assert!(
        log.index.load().is_some(),
        "setup: the append rebuilt the index"
    );
    // The helper resumes and finishes its range delete.
    let newest = REBUILD as u64 + 1;
    // SAFETY: the helper's claim above handed it position REBUILD, and only
    // this call fills it.
    unsafe { log.fill(REBUILD, tomb(b"a", b"zz", newest)) };
    assert_eq!(
        log.covering_seq(b"y", u64::MAX),
        newest,
        "the delete of y was hidden"
    );
    assert_eq!(log.covering_seq(b"c", u64::MAX), newest);
    assert_eq!(
        log.first_newer_overlap(b"x", b"z", REBUILD as u64, u64::MAX)
            .map(|t| t.seq),
        Some(newest)
    );
    let listed = log.to_sorted_vec();
    assert_eq!(listed.len(), REBUILD + 1);
    assert_eq!(listed[0].seq, newest);
    assert!(
        log.index
            .load()
            .is_some_and(|index| index.covered <= REBUILD),
        "the index covered a position that was not ready"
    );
}

fn key() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(0u8..6, 0..3)
}

proptest! {
    /// A batch, whatever the log held before it and whatever is appended
    /// after it, answers every query as the same tombstones appended one at
    /// a time do.
    #[test]
    fn a_batch_answers_what_its_tombstones_appended_one_at_a_time_answer(
        before in proptest::collection::vec((key(), key(), 1u64..40), 0..24),
        batch in proptest::collection::vec((key(), key(), 1u64..40), 0..60),
        after in proptest::collection::vec((key(), key(), 40u64..60), 0..24),
        probes in proptest::collection::vec((key(), key(), 0u64..70), 1..24),
    ) {
        let whole = |list: &[(Vec<u8>, Vec<u8>, u64)]| -> Vec<RangeTombstone> {
            list.iter()
                .filter(|(start, end, _)| start < end)
                .map(|(start, end, seq)| tomb(start, end, *seq))
                .collect()
        };
        let (batched, pushed) = (TombstoneLog::new(), TombstoneLog::new());
        for tombstone in whole(&before) {
            batched.push(tombstone.clone());
            pushed.push(tombstone);
        }
        batched.extend(whole(&batch));
        for tombstone in whole(&batch) {
            pushed.push(tombstone);
        }
        for tombstone in whole(&after) {
            batched.push(tombstone.clone());
            pushed.push(tombstone);
        }
        let listing = |log: &TombstoneLog| -> Vec<(Vec<u8>, Vec<u8>, u64)> {
            log.to_sorted_vec().into_iter().map(|t| (t.start, t.end, t.seq)).collect()
        };
        prop_assert_eq!(listing(&batched), listing(&pushed));
        for (a, b, n) in &probes {
            prop_assert_eq!(batched.covering_seq(a, *n), pushed.covering_seq(a, *n));
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            prop_assert_eq!(
                batched.first_newer_overlap(lo, hi, *n, u64::MAX).map(|t| (&t.start, &t.end, t.seq)),
                pushed.first_newer_overlap(lo, hi, *n, u64::MAX).map(|t| (&t.start, &t.end, t.seq))
            );
        }
    }

    /// Every query the memtable makes of its tombstones, against a linear
    /// model over the same appends, across rebuilds of the index.
    #[test]
    fn every_query_matches_a_linear_model(
        appends in proptest::collection::vec((key(), key(), 1u64..60), 0..80),
        probes in proptest::collection::vec((key(), key(), 0u64..70), 1..24),
    ) {
        let log = TombstoneLog::new();
        let mut all = Vec::new();
        for (start, end, seq) in &appends {
            if start < end {
                log.push(tomb(start, end, *seq));
                all.push((start.clone(), end.clone(), *seq));
            }
        }
        // T1: an append of a tombstone already held may add nothing.
        prop_assert!(log.len() <= all.len());
        let mut sorted: Vec<RangeTombstone> =
            all.iter().map(|(s, e, q)| tomb(s, e, *q)).collect();
        sort_dedup_tombstones(&mut sorted);
        let listed = log.to_sorted_vec();
        prop_assert_eq!(
            listed.iter().map(|t| (&t.start, &t.end, t.seq)).collect::<Vec<_>>(),
            sorted.iter().map(|t| (&t.start, &t.end, t.seq)).collect::<Vec<_>>()
        );
        for (a, b, n) in &probes {
            prop_assert_eq!(log.covering_seq(a, *n), model_covering(&all, a, *n));
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            let expected = sorted.iter().find(|t| t.seq > *n && t.overlaps(lo, hi));
            let got = log.first_newer_overlap(lo, hi, *n, u64::MAX);
            prop_assert_eq!(
                got.map(|t| (&t.start, &t.end, t.seq)),
                expected.map(|t| (&t.start, &t.end, t.seq))
            );
        }
    }
}
