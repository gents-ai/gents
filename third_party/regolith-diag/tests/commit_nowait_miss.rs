//! A `commit_nowait` through a `CacheOnly` handle whose check needs a block
//! the cache does not hold (plan 4.10, D53, `proofs/tla/NowaitMiss.tla`).
//!
//! The miss never ends the commit: the transaction was consumed by it and
//! cannot commit a second time. The committing call touches no table on the
//! device, counted at the `Env`; the block is read at a poll of the commit's
//! queue, and the commit, checked again there, lands, conflicts or fails as
//! a commit made then would.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::device_env::{Device, DeviceEnv};
use regolith::{
    CommitTicket, Error, IoQueue, OptimisticTransactionDb, Options, ReadMode, Transaction,
    TransactionError, TxnOptions,
};
use tempfile::TempDir;

/// The key every test commits to: its only version is in a table no block
/// of which is cached.
const COLD: &[u8] = b"k01000";

/// A database over one table of small blocks, reopened so its cache holds
/// none of them, and the device it reads through.
fn cold(dir: &TempDir) -> (OptimisticTransactionDb, Arc<Device>) {
    let (env, device) = DeviceEnv::new();
    let options = || {
        Options::default()
            .env(Arc::<DeviceEnv>::clone(&env))
            .block_size(256)
    };
    {
        let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
        for i in 0..2_000u32 {
            db.db().put(format!("k{i:05}").as_bytes(), b"x").unwrap();
        }
        db.db().flush().unwrap();
        db.db().close().unwrap();
    }
    let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
    (db, device)
}

/// A transaction through `queue`'s `CacheOnly` handle that writes `COLD`
/// blindly, with its callbacks counted: `on_commit` in `committed`,
/// `on_abort` in `aborted`.
fn blind_put(
    db: &OptimisticTransactionDb,
    queue: &IoQueue,
    committed: &Arc<AtomicUsize>,
    aborted: &Arc<AtomicUsize>,
) -> Transaction {
    let mut txn = db.begin(&TxnOptions::new().read_mode(ReadMode::CacheOnly(queue.id())));
    txn.put(COLD, b"mine").unwrap();
    let count = Arc::clone(committed);
    txn.on_commit(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
    });
    let count = Arc::clone(aborted);
    txn.on_abort(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
    });
    txn
}

/// Commit `txn` after another commit moved the horizon past its snapshot,
/// so its check must look `COLD` up in the cold table. The call touches no
/// table, takes no sequence, and leaves the commit on its queue.
fn owed(db: &OptimisticTransactionDb, device: &Device, txn: Transaction) -> CommitTicket {
    db.db().put(b"other", b"x").unwrap();
    db.db().flush().unwrap();
    let (touches, seq) = (device.touches(), db.db().latest_sequence());
    let ticket = txn.commit_nowait();
    assert_eq!(
        device.touches(),
        touches,
        "the committing call touched the device"
    );
    assert!(!ticket.is_ready(), "the miss ended the commit");
    assert_eq!(
        db.db().latest_sequence(),
        seq,
        "the commit took a sequence before its check"
    );
    ticket
}

fn counter() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

#[test]
fn an_early_check_miss_is_waited_out_on_the_queue_and_lands_once() {
    let dir = TempDir::new().unwrap();
    let (db, device) = cold(&dir);
    let mut queue = db.db().io_queue();
    let (committed, aborted, completed) = (counter(), counter(), counter());
    let ticket = owed(&db, &device, blind_put(&db, &queue, &committed, &aborted));
    assert_eq!(
        db.db().wait_for_snapshots().pinned(),
        1,
        "the owed commit let its snapshot go before it landed"
    );
    let count = Arc::clone(&completed);
    ticket.on_complete(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
    });
    let before = db.db().latest_sequence();

    let receipt = queue
        .block_on(ticket)
        .expect("the commit lands once its queue read the block");
    assert!(receipt.seq() > before);
    assert_eq!(committed.load(Ordering::SeqCst), 1, "on_commit ran once");
    assert_eq!(aborted.load(Ordering::SeqCst), 0, "on_abort ran");
    assert_eq!(completed.load(Ordering::SeqCst), 1, "on_complete ran once");
    assert_eq!(db.db().wait_for_snapshots().pinned(), 0);
    assert_eq!(db.db().get(COLD).unwrap(), Some(b"mine".to_vec()));
}

/// The check runs again when the commit is submitted again, at the horizon
/// of then: a write to the key made while the commit was owed is a conflict.
#[test]
fn an_owed_commit_is_checked_again_and_loses_to_a_write_made_meanwhile() {
    let dir = TempDir::new().unwrap();
    let (db, device) = cold(&dir);
    let mut queue = db.db().io_queue();
    let (committed, aborted) = (counter(), counter());
    let ticket = owed(&db, &device, blind_put(&db, &queue, &committed, &aborted));
    db.db().put(COLD, b"theirs").unwrap();

    match queue.block_on(ticket) {
        Err(TransactionError::Conflict(conflict)) => assert_eq!(conflict.key(), COLD),
        other => panic!("the commit landed over a write made after its snapshot: {other:?}"),
    }
    assert_eq!(committed.load(Ordering::SeqCst), 0);
    assert_eq!(aborted.load(Ordering::SeqCst), 1);
    assert_eq!(db.db().get(COLD).unwrap(), Some(b"theirs".to_vec()));
}

/// A database closed while a commit is owed ends the commit, at its queue's
/// next poll, as it refuses a commit made after it closed.
#[test]
fn an_owed_commit_ends_closed_when_the_database_closes() {
    let dir = TempDir::new().unwrap();
    let (db, device) = cold(&dir);
    let mut queue = db.db().io_queue();
    let (committed, aborted) = (counter(), counter());
    let ticket = owed(&db, &device, blind_put(&db, &queue, &committed, &aborted));
    db.db().close().unwrap();

    assert!(matches!(
        queue.block_on(ticket),
        Err(TransactionError::Engine(Error::Closed))
    ));
    assert_eq!(committed.load(Ordering::SeqCst), 0);
    assert_eq!(aborted.load(Ordering::SeqCst), 1, "on_abort ran once");
}

/// A queue dropped while a commit is owed polls no more: the commit is
/// brought through on the dropping thread, reading the device there as a
/// blocking commit does, and its ticket is ready when the drop returns.
#[test]
fn a_dropped_queue_brings_its_owed_commit_through() {
    let dir = TempDir::new().unwrap();
    let (db, device) = cold(&dir);
    let queue = db.db().io_queue();
    let (committed, aborted, landed) = (counter(), counter(), counter());
    let ticket = owed(&db, &device, blind_put(&db, &queue, &committed, &aborted));
    drop(queue);

    assert!(
        ticket.is_ready(),
        "the dropped queue left the commit undelivered"
    );
    let count = Arc::clone(&landed);
    ticket.on_complete(move |outcome| {
        if outcome.is_ok() {
            count.fetch_add(1, Ordering::SeqCst);
        }
    });
    assert_eq!(landed.load(Ordering::SeqCst), 1, "the commit did not land");
    assert_eq!(committed.load(Ordering::SeqCst), 1);
    assert_eq!(aborted.load(Ordering::SeqCst), 0);
    assert_eq!(db.db().get(COLD).unwrap(), Some(b"mine".to_vec()));
}

#[test]
fn early_validation_does_not_read_tables_older_than_its_observation() {
    let dir = TempDir::new().unwrap();
    let (env, device) = DeviceEnv::new();
    let options = || {
        Options::default()
            .env(Arc::<DeviceEnv>::clone(&env))
            .block_size(256)
            .block_cache_size(0)
    };
    {
        let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
        for i in 0..2_000u32 {
            db.db()
                .put(format!("k{i:05}").as_bytes(), &[b'x'; 64])
                .unwrap();
        }
        db.db().flush().unwrap();
        db.db().close().unwrap();
    }
    let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
    let mut queue = db.db().io_queue();
    let txn = db.begin(&TxnOptions::new().read_mode(ReadMode::CacheOnly(queue.id())));
    for i in 0..2_000u32 {
        txn.put(format!("k{i:05}").as_bytes(), b"mine").unwrap();
    }
    db.db().put(b"other", b"x").unwrap();
    let touches = device.touches();
    let ticket = txn.commit_nowait();
    assert!(ticket.is_ready(), "old tables caused a validation read");
    queue.block_on(ticket).unwrap();
    assert_eq!(device.touches(), touches, "validation touched old tables");
    assert_eq!(db.db().get(b"k01000").unwrap(), Some(b"mine".to_vec()));
}

#[test]
fn skipping_old_tables_still_detects_a_newer_memtable_write() {
    let dir = TempDir::new().unwrap();
    let (db, _) = cold(&dir);
    let mut queue = db.db().io_queue();
    let txn = db.begin(&TxnOptions::new().read_mode(ReadMode::CacheOnly(queue.id())));
    queue
        .block_on(regolith::ready(|| txn.get_for_update(COLD)))
        .unwrap();
    txn.put(b"result", b"mine").unwrap();
    db.db().put(COLD, b"theirs").unwrap();
    assert!(matches!(
        queue.block_on(txn.commit_nowait()),
        Err(TransactionError::Conflict(_))
    ));
    assert_eq!(db.db().get(b"result").unwrap(), None);
}

#[test]
fn validation_detects_a_newer_ingested_table() {
    let dir = TempDir::new().unwrap();
    let (db, _) = cold(&dir);
    let mut queue = db.db().io_queue();
    let txn = db.begin(&TxnOptions::new().read_mode(ReadMode::CacheOnly(queue.id())));
    queue
        .block_on(regolith::ready(|| txn.get_for_update(COLD)))
        .unwrap();
    txn.put(b"result", b"mine").unwrap();
    let staging = TempDir::new().unwrap();
    let path = staging.path().join("newer.sst");
    let mut writer = regolith::SstFileWriter::create(&path, &Options::default()).unwrap();
    writer.put(COLD, b"ingested").unwrap();
    writer.finish().unwrap();
    db.db()
        .ingest_external_files(
            &[path],
            regolith::IngestOptions {
                snapshot_consistency: false,
                ..regolith::IngestOptions::default()
            },
        )
        .wait()
        .unwrap();
    assert!(matches!(
        queue.block_on(txn.commit_nowait()),
        Err(TransactionError::Conflict(_))
    ));
    assert_eq!(db.db().get(COLD).unwrap(), Some(b"ingested".to_vec()));
    assert_eq!(db.db().get(b"result").unwrap(), None);
}
