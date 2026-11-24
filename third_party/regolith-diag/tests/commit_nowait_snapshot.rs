//! A `commit_nowait` commit left behind a write stall keeps its snapshot
//! pinned until it lands. Until then the check of what landed above its
//! early horizon is still to come, and a compaction that ran meanwhile with
//! nothing pinned would retire a deletion newer than the snapshot: the
//! commit would land over a write it conflicts with.
#![cfg(not(target_arch = "wasm32"))]

use regolith::{
    CommitTicket, CompactionOutcome, Error, IoBudget, IoQueue, OptimisticTransactionDb, Options,
    TransactionError, TxnOptions, WouldBlock,
};
use tempfile::TempDir;

/// No worker, and a stop at one L0 table that one compaction step relieves:
/// every flush stops writers until the test runs the step.
fn open(dir: &TempDir) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(
        dir.path(),
        Options::default()
            .max_background_compactions(0)
            .inline_compaction(false)
            .l0_compaction_trigger(1)
            .level0_slowdown_writes_trigger(1)
            .level0_stop_writes_trigger(1),
    )
    .unwrap()
}

fn stopped(db: &OptimisticTransactionDb) -> bool {
    matches!(
        db.db().put(b"probe", b"v"),
        Err(Error::WouldBlock(WouldBlock::Stall(_)))
    )
}

/// Poll `queue` until the ticket is delivered, a bounded number of times.
fn delivered(queue: &mut IoQueue, ticket: CommitTicket) -> Result<u64, TransactionError> {
    for _ in 0..64 {
        if ticket.is_ready() {
            break;
        }
        queue.poll(IoBudget::ALL);
    }
    assert!(ticket.is_ready(), "the commit's ticket was never completed");
    queue.block_on(ticket).map(|receipt| receipt.seq())
}

#[test]
fn a_nowait_commit_behind_a_stall_still_conflicts_with_a_delete_compacted_meanwhile() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let mut queue = db.db().io_queue();
    db.db().put(b"k", b"v1").unwrap();

    // The transaction writes k from its snapshot, and its commit is left
    // behind the stop the flush makes, before it takes a slot.
    let tx = db.begin(&TxnOptions::new().io_queue(queue.id()));
    tx.put(b"k", b"tx").unwrap();
    db.db().flush().unwrap();
    assert!(stopped(&db));
    let ticket = tx.commit_nowait();
    assert!(!ticket.is_ready(), "the commit waits for the stop to clear");

    // The stop clears, but the commit is driven again only at its queue's
    // poll. Meanwhile another writer deletes k, and the deletion is flushed
    // and compacted onto the old value.
    assert_eq!(db.db().compact_step().unwrap(), CompactionOutcome::DidWork);
    assert!(!stopped(&db));
    db.db().delete(b"k").unwrap();
    db.db().flush().unwrap();
    assert_eq!(db.db().compact_step().unwrap(), CompactionOutcome::DidWork);
    assert_eq!(db.db().get(b"k").unwrap(), None);

    // k changed after the transaction's snapshot: the commit must lose.
    match delivered(&mut queue, ticket) {
        Err(TransactionError::Conflict(conflict)) => assert_eq!(conflict.key(), b"k"),
        other => panic!(
            "the commit landed over a delete of k that came after its snapshot: {other:?}; \
             k now reads {:?}",
            db.db().get(b"k").unwrap()
        ),
    }
    assert_eq!(db.db().get(b"k").unwrap(), None);
}
