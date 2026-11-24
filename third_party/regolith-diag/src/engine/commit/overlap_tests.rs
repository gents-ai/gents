//! Concurrent durable writers share an fsync, through the public API, with
//! the overlap made certain instead of left to the scheduler.
//!
//! One sync of the log covers every record written before it, so writers
//! that are inside the commit path at the same time share one. Threads
//! started together need luck to overlap: on one or two cores, or on a
//! filesystem whose fsync returns at once, each write can finish before the
//! next begins, and every write then pays for a sync of its own. So the test
//! holds the round, the one claim on the log's device step, as a thread in
//! the middle of a sync does, until every writer's first write is decided
//! and waits behind it, and only then lets go. That first sync is shared on
//! any core count and any filesystem.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::pipeline::Pipeline;
use crate::{Db, DurabilityMode, Options, Statistics, Ticker};

/// Wait, with a deadline and a bounded backoff, until the decided frontier
/// has passed `through`: every writer's first write is decided, and each of
/// them waits for the round.
pub(super) fn wait_until_decided(pipe: &Pipeline, through: u64) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut pause = Duration::from_micros(50);
    while pipe.ring.decided() < through {
        assert!(
            Instant::now() < deadline,
            "the writers never queued behind the held round"
        );
        thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(20));
    }
}

#[test]
fn concurrent_durable_writers_cost_far_fewer_fsyncs_than_writes() {
    let dir = TempDir::new().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = Arc::new(
        Db::open(
            dir.path(),
            Options::default()
                .durability(DurabilityMode::Immediate)
                .statistics(Some(Arc::clone(&stats))),
        )
        .unwrap(),
    );

    const WRITERS: usize = 8;
    // About two thousand durable writes in all, and at least two a writer so
    // writers go on after the held round.
    let per_writer = 2000usize.div_ceil(WRITERS).max(2);

    let pipe = db.engine.pipe().unwrap();
    let before = pipe.ring.decided();
    // Held as a thread holds it while it syncs: a write that arrives is
    // decided and written, and then waits for the round.
    let held = db.engine.hold_round(pipe, super::park::Help::Wait);
    let handles: Vec<_> = (0..WRITERS)
        .map(|w| {
            let db = Arc::clone(&db);
            thread::spawn(move || {
                for i in 0..per_writer {
                    db.put(format!("w{w}k{i:05}").as_bytes(), b"value").unwrap();
                }
            })
        })
        .collect();
    wait_until_decided(pipe, before + WRITERS as u64);
    assert_eq!(
        stats.get_ticker(Ticker::WalSyncCount),
        0,
        "nothing is synced while the round is held"
    );
    drop(held);
    for handle in handles {
        handle.join().unwrap();
    }

    let writes = (WRITERS * per_writer) as u64;
    let syncs = stats.get_ticker(Ticker::WalSyncCount);
    assert!(syncs >= 1, "durable writes must reach stable storage");

    // Every write that returned Ok must be readable.
    for w in 0..WRITERS {
        for i in 0..per_writer {
            let key = format!("w{w}k{i:05}");
            assert_eq!(
                db.get(key.as_bytes()).unwrap(),
                Some(b"value".to_vec()),
                "every write that returned Ok must be readable"
            );
        }
    }

    // The writes that waited behind the held round were covered by syncs of
    // two records or more, so that wave alone saved a sync for every second
    // writer.
    assert!(
        syncs <= writes - (WRITERS as u64 / 2),
        "{WRITERS} writers waiting on one round must share fsyncs: \
         {syncs} syncs for {writes} durable writes"
    );
    assert!(
        syncs < writes,
        "no fsync was ever shared by {WRITERS} concurrent writers: \
         every write paid for its own"
    );
    println!("commit pipeline: {syncs} syncs for {writes} durable writes");
}
