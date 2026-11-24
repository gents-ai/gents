//! What a queue keeps landed for calls run again: a call that needs more
//! than the queue's bound at once holds its blocks until it is seen again,
//! lets them go when it finishes, and a call abandoned after a wait lets
//! them go after `KEEP_ROUNDS` landing polls.

use super::KEEP_ROUNDS;
use crate::{CompressionType, Db, Error, IoBudget, IoQueue, Options, ReadMode, WouldBlock};

/// Two MiB: one value fills a data block, twice the queue's 1 MiB bound.
const BIG: usize = 2 << 20;

/// 4 KiB blocks (a 1 MiB bound), no block cache, no compression: a read run
/// again finds its blocks only where they landed for its queue.
fn big_values(keys: &[&[u8]]) -> (tempfile::TempDir, Db) {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Db::open(
        dir.path(),
        Options::default()
            .block_size(4096)
            .block_cache_size(0)
            .compression(CompressionType::None),
    )
    .unwrap();
    for (i, key) in keys.iter().enumerate() {
        db.put(key, &vec![i as u8; BIG]).unwrap();
    }
    db.flush().unwrap();
    (dir, db)
}

fn waits<T: std::fmt::Debug>(read: crate::Result<T>) -> bool {
    match read {
        Err(Error::WouldBlock(WouldBlock::Io(_))) => true,
        Ok(_) => false,
        other => panic!("{other:?}"),
    }
}

/// Run `get(key)` through `queue` until it answers.
fn get_through(db: &Db, queue: &mut IoQueue, key: &[u8]) {
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    for _ in 0..8 {
        if !waits(snapshot.get(key)) {
            return;
        }
        queue.poll(IoBudget::ALL);
    }
    panic!("the read never finished");
}

#[test]
fn a_call_past_the_bound_holds_its_blocks_until_it_finishes() {
    let (_dir, db) = big_values(&[b"a", b"b", b"c"]);
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    // a misses; then a is read and b misses: the wait holds a.
    assert!(waits(snapshot.multi_get(&[b"a", b"b"])));
    queue.poll(IoBudget::ALL);
    let a = queue.landed_keys()[0];
    assert!(waits(snapshot.multi_get(&[b"a", b"b"])));
    queue.poll(IoBudget::ALL);
    // Both landed and held, past the bound.
    let landed = queue.landed_keys();
    assert_eq!(landed.len(), 2);
    let b = landed[1];
    assert!(queue.holds(&a) && queue.holds(&b));
    assert!(queue.landed_bytes() > 2 * BIG - 1);
    // The run that reads both finishes, and says so.
    assert!(!waits(snapshot.multi_get(&[b"a", b"b"])));
    queue.poll(IoBudget::ALL);
    assert!(!queue.holds(&a) && !queue.holds(&b));
    // The next landing lets both go: nothing holds them past the bound.
    get_through(&db, &mut queue, b"c");
    let landed = queue.landed_keys();
    assert!(!landed.contains(&a) && !landed.contains(&b), "{landed:?}");
    assert_eq!(landed.len(), 1);
}

#[test]
fn a_call_never_run_again_lets_go_after_its_rounds() {
    let (_dir, db) = big_values(&[b"a", b"c0", b"c1", b"c2", b"c3", b"c4", b"c5"]);
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    assert!(waits(snapshot.get(b"a")));
    queue.poll(IoBudget::ALL);
    let a = queue.landed_keys()[0];
    // The call is never run again. Each read below lands in a poll of its
    // own: a round each.
    let mut rounds = 0;
    for key in [b"c0", b"c1", b"c2", b"c3", b"c4", b"c5"] {
        get_through(&db, &mut queue, key);
        rounds += 1;
        let kept = queue.landed_keys().contains(&a);
        if rounds < KEEP_ROUNDS {
            assert!(kept, "a was let go after {rounds} rounds");
        }
    }
    assert!(rounds > KEEP_ROUNDS);
    assert!(!queue.holds(&a));
    assert!(!queue.landed_keys().contains(&a));
}

#[test]
fn a_ready_live_caller_keeps_progress_across_other_landing_rounds() {
    let (_dir, db) = big_values(&[b"a", b"b", b"c0", b"c1", b"c2", b"c3", b"c4", b"c5"]);
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    for _ in 0..4 {
        let wait = match snapshot.multi_get(&[b"a", b"b"]) {
            Ok(values) => {
                assert_eq!(values.len(), 2);
                return;
            }
            Err(Error::WouldBlock(WouldBlock::Io(wait))) => wait,
            other => panic!("{other:?}"),
        };
        queue.poll(IoBudget::ALL);
        assert!(wait.is_ready());
        for key in [b"c0", b"c1", b"c2", b"c3", b"c4", b"c5"] {
            get_through(&db, &mut queue, key);
        }
        drop(wait);
    }
    panic!("a live reader lost progress while other ready tasks ran");
}

#[test]
fn abandoned_landings_expire_behind_a_live_completed_wait() {
    let (_dir, db) = big_values(&[b"a", b"b", b"c0", b"c1", b"c2", b"c3", b"c4", b"c5"]);
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    let live = match snapshot.get(b"a") {
        Err(Error::WouldBlock(WouldBlock::Io(wait))) => wait,
        other => panic!("{other:?}"),
    };
    queue.poll(IoBudget::ALL);
    let a = queue.landed_keys()[0];
    assert!(waits(snapshot.get(b"b")));
    queue.poll(IoBudget::ALL);
    let b = *queue.landed_keys().iter().find(|key| **key != a).unwrap();
    for key in [b"c0", b"c1", b"c2", b"c3", b"c4", b"c5"] {
        get_through(&db, &mut queue, key);
    }
    assert!(queue.holds(&a), "the live wait still owns a");
    assert!(
        !queue.holds(&b),
        "an older live wait must not prevent reclaiming b"
    );
    drop(live);
    get_through(&db, &mut queue, b"b");
    assert!(!queue.holds(&a), "the abandoned wait is now reclaimable");
}

#[test]
fn another_reader_cannot_release_a_live_callers_landing() {
    let (_dir, db) = big_values(&[b"a", b"c0", b"c1", b"c2", b"c3", b"c4", b"c5"]);
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    let live = match snapshot.get(b"a") {
        Err(Error::WouldBlock(WouldBlock::Io(wait))) => wait,
        other => panic!("{other:?}"),
    };
    queue.poll(IoBudget::ALL);
    let a = queue.landed_keys()[0];
    get_through(&db, &mut queue, b"a");
    queue.poll(IoBudget::ALL);
    for key in [b"c0", b"c1", b"c2", b"c3", b"c4", b"c5"] {
        get_through(&db, &mut queue, key);
    }
    assert!(live.is_ready());
    assert!(
        queue.holds(&a),
        "another reader consumed this caller's hold"
    );
}

/// A landing for ledger tests that charge by hand: a failure, which holds
/// no block.
fn failed() -> crate::engine::io::shared::Landing {
    crate::engine::io::shared::Landing::Failed(std::sync::Arc::new(std::io::Error::other(
        "synthetic",
    )))
}

fn synthetic_key(offset: u64) -> crate::engine::io::unit::UnitKey {
    crate::engine::io::unit::UnitKey {
        file_id: 1,
        offset,
        guard: None,
    }
}

#[test]
fn forgotten_live_handle_expires_at_the_finite_window() {
    use super::{LIVE_KEEP_ROUNDS, Ledger};
    use crate::engine::io::shared::{QueueShared, WaitSlot, test_id};
    use std::sync::Arc;

    let queue = QueueShared::new(test_id(), 100);
    let mut ledger = Ledger::default();
    let key = synthetic_key(0);
    let caller = Arc::new(WaitSlot::new());
    caller.complete();
    ledger.land(key, 200, failed());
    ledger.completed(Some(key), caller.clone());
    for round in 1..=LIVE_KEEP_ROUNDS {
        ledger.land(synthetic_key(round), 10, failed());
        ledger.settle(&queue);
        assert!(ledger.is_held(&key), "lost progress at round {round}");
        assert_eq!(ledger.live_bytes(), 200);
    }
    ledger.land(synthetic_key(LIVE_KEEP_ROUNDS + 1), 10, failed());
    ledger.settle(&queue);
    assert!(caller.is_ready(), "expiry must not invalidate the wait");
    assert!(!ledger.is_held(&key));
    assert!(!ledger.keys().contains(&key));
    assert!(ledger.bytes() <= 100);
    assert_eq!(ledger.live_bytes(), 0);
}

#[test]
fn live_retention_counts_shared_blocks_once_and_resets_after_drop() {
    use super::Ledger;
    use crate::engine::io::shared::{QueueShared, WaitSlot, test_id};
    use std::sync::Arc;

    let queue = QueueShared::new(test_id(), 100);
    let mut ledger = Ledger::default();
    let key = synthetic_key(0);
    let first = Arc::new(WaitSlot::new());
    let second = Arc::new(WaitSlot::holding(vec![key].into_boxed_slice()));
    ledger.land(key, 60, failed());
    ledger.land(key, 80, failed());
    ledger.completed(Some(key), first.clone());
    ledger.hold(&second);
    ledger.completed(Some(key), second.clone());
    ledger.settle(&queue);
    assert_eq!(
        ledger.live_bytes(),
        80,
        "shared blocks are not per-wait charges"
    );
    assert!(ledger.retention_warning);
    drop(first);
    ledger.land(synthetic_key(1), 10, failed());
    ledger.settle(&queue);
    assert_eq!(ledger.live_bytes(), 80, "the other caller still retains it");
    drop(second);
    ledger.land(synthetic_key(2), 10, failed());
    ledger.settle(&queue);
    assert_eq!(ledger.live_bytes(), 0);
    assert!(
        !ledger.retention_warning,
        "the next episode must warn again"
    );
}

#[test]
fn duplicate_landing_eviction_keeps_the_latest_entry_until_last_removal() {
    use super::Ledger;
    use crate::engine::io::shared::{Landing, QueueShared, test_id};
    use std::{io, sync::Arc};

    let queue = QueueShared::new(test_id(), 100);
    let mut ledger = Ledger::default();
    let key = synthetic_key(0);
    queue.land(key, &Landing::Failed(Arc::new(io::Error::other("latest"))));
    ledger.land(key, 80, failed());
    ledger.land(key, 80, failed());
    ledger.settle(&queue);
    assert_eq!(ledger.bytes(), 80);
    assert!(
        queue.landed(&key).is_some(),
        "the remaining entry must stay available"
    );
    ledger.land(synthetic_key(1), 100, failed());
    ledger.settle(&queue);
    assert!(!ledger.keys().contains(&key));
}

#[test]
fn an_idle_poll_reports_the_retained_live_landing() {
    let (_dir, db) = big_values(&[b"a"]);
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    let live = match snapshot.get(b"a") {
        Err(Error::WouldBlock(WouldBlock::Io(wait))) => wait,
        other => panic!("{other:?}"),
    };
    let landed = queue.poll(IoBudget::ALL);
    assert!(live.is_ready());
    assert!(landed.retained_live_bytes >= BIG);
    assert!(landed.landed_bytes >= landed.retained_live_bytes);
    let idle = queue.poll(IoBudget::ALL);
    assert_eq!(idle.retained_live_bytes, landed.retained_live_bytes);
    assert_eq!(idle.landed_bytes, landed.landed_bytes);
    assert_eq!(idle.completed, 0);
    assert!(!idle.more_pending);
}

#[test]
fn a_late_drop_does_not_receive_the_rest_of_the_live_window() {
    use super::Ledger;
    use crate::engine::io::shared::{QueueShared, WaitSlot, test_id};
    use std::sync::Arc;

    let queue = QueueShared::new(test_id(), 100);
    let mut ledger = Ledger::default();
    let key = synthetic_key(0);
    let caller = Arc::new(WaitSlot::new());
    ledger.land(key, 200, failed());
    ledger.completed(Some(key), caller.clone());
    for round in 1..=10 {
        ledger.land(synthetic_key(round), 10, failed());
        ledger.settle(&queue);
    }
    assert!(ledger.is_held(&key));
    drop(caller);
    ledger.land(synthetic_key(11), 10, failed());
    ledger.settle(&queue);
    assert!(!ledger.is_held(&key));
    assert_eq!(ledger.live_bytes(), 0);
    assert!(ledger.bytes() <= 100);
}

/// A decoded data block of about 44 KiB, the size the landings that stayed
/// resident had, and a weak handle that says when it is freed.
fn big_block(
    seed: u8,
) -> (
    crate::engine::io::Landed,
    std::sync::Weak<crate::engine::block::Block>,
) {
    use crate::engine::block::{Block, BlockBuilder};
    let mut builder = BlockBuilder::new(16);
    builder.add(&[seed], &vec![seed; 44 << 10]);
    let block = std::sync::Arc::new(Block::decode(builder.finish()).unwrap());
    let weak = std::sync::Arc::downgrade(&block);
    (crate::engine::io::Landed::Data(block), weak)
}

#[test]
fn a_forgotten_landing_frees_its_block_while_its_thread_idles() {
    use super::Ledger;
    use crate::engine::io::shared::{Landing, QueueShared, test_id};
    use std::sync::{Arc, Barrier, mpsc};

    const THREADS: usize = 4;
    const BLOCKS: u64 = 16;
    // Each owner lands its blocks, lets them all go, and then idles without
    // flushing kovan: its retired map entries stay unreclaimed until the
    // barrier releases it.
    let idle = Arc::new(Barrier::new(THREADS + 1));
    let (tx, rx) = mpsc::channel();
    let owners: Vec<_> = (0..THREADS)
        .map(|t| {
            let idle = Arc::clone(&idle);
            let tx = tx.clone();
            std::thread::spawn(move || {
                let queue = QueueShared::new(test_id(), 0);
                let mut ledger = Ledger::default();
                let mut weaks = Vec::new();
                for offset in 0..BLOCKS {
                    let (landed, weak) = big_block(t as u8);
                    let key = synthetic_key(offset);
                    let landing = Landing::Ready(landed);
                    queue.land(key, &landing);
                    ledger.land(key, 1, landing);
                    assert!(matches!(queue.landed(&key), Some(Landing::Ready(_))));
                    weaks.push((key, weak));
                }
                // A bound of zero: the settle lets every unheld landing go.
                ledger.settle(&queue);
                assert!(ledger.keys().is_empty());
                for (key, _) in &weaks {
                    assert!(queue.landed(key).is_none(), "a let-go landing is a miss");
                }
                tx.send(weaks.into_iter().map(|(_, weak)| weak).collect::<Vec<_>>())
                    .unwrap();
                idle.wait();
                drop(queue);
            })
        })
        .collect();
    drop(tx);
    let weaks: Vec<_> = rx.iter().take(THREADS).flatten().collect();
    let alive = weaks.iter().filter(|weak| weak.strong_count() > 0).count();
    idle.wait();
    for owner in owners {
        owner.join().unwrap();
    }
    assert_eq!(
        alive,
        0,
        "{alive} of {} forgotten blocks stayed alive while their owners idled",
        weaks.len()
    );
}

#[test]
fn a_read_block_is_freed_once_its_queue_lets_it_go() {
    use crate::engine::io::Landed;
    use crate::engine::io::shared::Landing;

    let (_dir, db) = big_values(&[b"a", b"b", b"c"]);
    let mut queue = db.io_queue();
    get_through(&db, &mut queue, b"a");
    let a = queue.landed_keys()[0];
    let block = match queue.shared().landed(&a) {
        Some(Landing::Ready(Landed::Data(block))) => std::sync::Arc::downgrade(&block),
        _ => panic!("a landed as a data block"),
    };
    // Each later read lands past the bound, and nothing holds a: it goes.
    get_through(&db, &mut queue, b"b");
    get_through(&db, &mut queue, b"c");
    assert!(!queue.landed_keys().contains(&a));
    // Neither the landing table nor the unit table, whose removed entries
    // kovan has yet to reclaim, keeps the block.
    assert_eq!(block.strong_count(), 0, "the let-go block is still alive");
}
