//! `Db::allocate` does no I/O on the caller's thread (plan 3.7): the record
//! joins the write-ahead log in the ordered step and becomes durable with the
//! next sync, so the caller neither writes it, nor reads a block for it, nor
//! waits for a sync another commit asked for.
//!
//! Each test pins one of the three on an `Env` that sees the log and the
//! tables, by counts and by which thread made the call. The first holds a
//! sync at the device, so a caller that waits for it is seen to wait.
//!
//! A counter the database has not read yet is the one exception, decided
//! with the plan: the blocking call may read its block on the caller's
//! thread the first time, as a blocking `get` does; the non-blocking call
//! hands back a wait its queue's poll completes instead.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads and the filesystem.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use common::STUCK;
use common::device_env::cold_db;
use common::watched_env::WatchedEnv;
use regolith::{Db, DurabilityMode, Error, Options, WouldBlock};
use tempfile::TempDir;

/// A commit at `Immediate` durability is inside its sync, and stays there.
/// An allocation made meanwhile is ordered after that commit, and its caller
/// must still get its range without waiting for the device.
#[test]
fn an_allocation_returns_while_another_commits_sync_is_at_the_device() {
    let dir = TempDir::new().unwrap();
    let (env, watch) = WatchedEnv::on_host();
    let db = Arc::new(
        Db::open(
            dir.path(),
            Options::default()
                .env(env)
                .durability(DurabilityMode::Immediate)
                .max_background_compactions(0),
        )
        .unwrap(),
    );
    assert_eq!(db.allocate(b"ids", 1).unwrap(), 1..2);

    watch.hold_syncs();
    let committer = thread::spawn({
        let db = Arc::clone(&db);
        move || db.put(b"k", b"v")
    });
    watch.wait_held(1);

    // On a thread of its own, so a wait reads as a wait and the test can
    // still let the sync go.
    let (done, returned) = mpsc::channel();
    let allocator = thread::spawn({
        let db = Arc::clone(&db);
        move || {
            let _ = done.send(db.allocate(b"ids", 2));
        }
    });
    let outcome = returned.recv_timeout(STUCK);

    watch.release_syncs();
    committer.join().unwrap().unwrap();
    allocator.join().unwrap();
    let range = outcome
        .expect("the allocation waited for a sync that another commit had asked for")
        .unwrap();
    assert_eq!(range, 2..4);
    assert_eq!(db.allocate(b"ids", 1).unwrap(), 4..5);
}

/// The record of a reservation is written to the log by the ordered step,
/// not by the thread that called `allocate`.
#[test]
fn an_allocation_writes_nothing_to_the_log_on_the_callers_thread() {
    for durability in [DurabilityMode::Eventual, DurabilityMode::Immediate] {
        let dir = TempDir::new().unwrap();
        let (env, watch) = WatchedEnv::on_host();
        let db = Db::open(
            dir.path(),
            Options::default()
                .env(env)
                .durability(durability)
                .max_background_compactions(0),
        )
        .unwrap();
        db.put(b"warm", b"x").unwrap();

        let before = watch.log_writes_here();
        let range = db.allocate(b"ids", 3).unwrap();
        assert_eq!(range, 1..4);
        assert_eq!(
            watch.log_writes_here() - before,
            0,
            "{durability:?}: the calling thread wrote the reservation to the log itself"
        );

        // It is in the log no later than the commit that uses a value.
        db.put(b"used", &range.start.to_be_bytes()).unwrap();
        drop(db);
        let db = Db::open(dir.path(), Options::default()).unwrap();
        assert_eq!(db.allocate(b"ids", 1).unwrap(), 4..5, "{durability:?}");
    }
}

/// A database whose counter `ids` lives only in a table, at 7, with a cold
/// block cache.
fn cold_counter() -> (TempDir, Db, Arc<common::device_env::Device>) {
    cold_db(
        |env| {
            Options::default()
                .env(env)
                .block_size(256)
                .max_background_compactions(0)
        },
        |db: &Db| {
            // Keys enough that the counter is not in the one block an open
            // reads.
            for i in 0..2_000u32 {
                db.put(format!("a/{i:05}").as_bytes(), b"value").unwrap();
            }
            assert_eq!(db.allocate(b"ids", 7).unwrap(), 1..8);
        },
    )
}

/// The blocking call is a blocking call: the first reservation of a counter
/// that lives only in a table may read the counter's block on the caller's
/// thread, as a blocking `get` does. The database keeps the counter from
/// then on, so no later reservation of the key reads the device, not even
/// once the counter's record has left the memtables again.
#[test]
fn a_blocking_allocation_reads_a_cold_counter_once_and_never_again() {
    let (_dir, db, device) = cold_counter();
    let before = device.reads();
    assert_eq!(db.allocate(b"ids", 1).unwrap(), 8..9);
    let cold = device.reads() - before;
    db.flush().unwrap();
    let flushed = device.reads();
    assert_eq!(db.allocate(b"ids", 1).unwrap(), 9..10);
    assert_eq!(db.allocate(b"ids", 1).unwrap(), 10..11);
    let warm = device.reads() - flushed;
    assert!(cold <= 1, "the cold reservation read {cold} blocks");
    assert_eq!(warm, 0, "a later reservation read the device");
}

/// The non-blocking call never reads the device on the caller's thread: a
/// counter that lives only in a table hands back a wait, the queue's poll
/// reads the block, and the call run again reserves, reading nothing.
#[test]
fn a_non_blocking_allocation_reads_no_block_on_the_callers_thread() {
    let (_dir, db, device) = cold_counter();
    let mut queue = db.io_queue();
    let mut waits = 0;
    let range = loop {
        let before = device.reads();
        let outcome = db.allocate_nowait(queue.id(), b"ids", 1);
        assert_eq!(
            device.reads() - before,
            0,
            "the call read a block at the device on the caller's thread"
        );
        match outcome {
            Err(Error::WouldBlock(WouldBlock::Io(wait))) => {
                waits += 1;
                assert!(waits <= 4, "the poll never read the counter in");
                queue.block_on(wait);
            }
            other => break other.unwrap(),
        }
    };
    assert!(waits >= 1, "a cold counter is read at the queue's poll");
    assert_eq!(range, 8..9);
    let before = device.reads();
    assert_eq!(db.allocate_nowait(queue.id(), b"ids", 2).unwrap(), 9..11);
    assert_eq!(device.reads() - before, 0);
}
