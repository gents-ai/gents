//! Exactly one outcome per transaction, whichever way it ends (plan 3.16,
//! items 1 and 5): through a synchronous commit, a `commit_nowait` ticket
//! delivered at its queue's poll, a dropped ticket, a dropped queue, a
//! rollback, a drop, or `close`. Each transaction's `on_commit` or
//! `on_abort` callbacks run once, never both and never neither, the
//! database's hooks run one outcome per transaction too, and a commit that
//! ran `on_commit` is in the database after a reopen. The tickets are
//! delivered before close, by the queue's drop, or only at a poll after
//! close, so a commit still owed its delivery when close runs is covered.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use proptest::prelude::*;
use regolith::{
    AbortReason, CommitInfo, Db, DurabilityMode, IoBudget, OptimisticTransactionDb, Options,
    Transaction, TransactionHooks, TxnOptions,
};
use tempfile::TempDir;

/// How one transaction ends.
#[derive(Clone, Copy, Debug)]
enum End {
    /// `commit`, on this thread.
    Commit,
    /// `commit_nowait`, the ticket kept and its queue polled.
    Nowait,
    /// `commit_nowait`, the ticket dropped at once.
    NowaitDropTicket,
    /// `rollback`.
    Rollback,
    /// The transaction dropped.
    Drop,
    /// Left open until the database closes.
    Open,
}

fn end() -> impl Strategy<Value = End> {
    prop_oneof![
        Just(End::Commit),
        Just(End::Nowait),
        Just(End::NowaitDropTicket),
        Just(End::Rollback),
        Just(End::Drop),
        Just(End::Open),
    ]
}

/// When the tickets are delivered.
#[derive(Clone, Copy, Debug)]
enum Delivery {
    /// The queue is dropped before close: its drop delivers them.
    DropQueue,
    /// The queue is polled before close.
    PollBefore,
    /// The queue is polled only after close.
    PollAfter,
}

fn delivery() -> impl Strategy<Value = Delivery> {
    prop_oneof![
        Just(Delivery::DropQueue),
        Just(Delivery::PollBefore),
        Just(Delivery::PollAfter),
    ]
}

/// The database's hooks, counting the outcomes they are told.
#[derive(Default)]
struct Hooks {
    outcomes: AtomicUsize,
}

impl TransactionHooks for Hooks {
    fn on_commit(&self, _: &CommitInfo) {
        self.outcomes.fetch_add(1, Ordering::SeqCst);
    }
    fn on_abort(&self, _: &AbortReason<'_>) {
        self.outcomes.fetch_add(1, Ordering::SeqCst);
    }
}

/// What one transaction's callbacks saw.
#[derive(Default)]
struct Seen {
    committed: AtomicUsize,
    aborted: AtomicUsize,
}

fn watched(txn: &mut Transaction, seen: &Arc<Seen>) {
    let on_commit = Arc::clone(seen);
    txn.on_commit(move |_| {
        on_commit.committed.fetch_add(1, Ordering::SeqCst);
    });
    let on_abort = Arc::clone(seen);
    txn.on_abort(move |_| {
        on_abort.aborted.fetch_add(1, Ordering::SeqCst);
    });
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn every_transaction_ends_exactly_once(
        ends in proptest::collection::vec((end(), 0usize..4), 1..10),
        immediate in any::<bool>(),
        delivery in delivery(),
    ) {
        let dir = TempDir::new().unwrap();
        let durability = if immediate { DurabilityMode::Immediate } else { DurabilityMode::Eventual };
        let options = || Options::default().durability(durability);
        let hooks = Arc::new(Hooks::default());
        let mut committed_keys = Vec::new();
        {
            let db = OptimisticTransactionDb::open(
                dir.path(),
                options().transaction_hooks(Arc::clone(&hooks) as Arc<dyn TransactionHooks>),
            )
            .unwrap();
            let mut queue = Some(db.db().io_queue());
            let id = queue.as_ref().map(|q| q.id()).unwrap();
            let mut seen = Vec::new();
            let mut open = Vec::new();
            let mut tickets = Vec::new();
            // Every transaction begins before any ends, and they write a few
            // shared keys: a later commit of a key an earlier one committed
            // loses to it.
            let mut begun = Vec::new();
            for (at, (_, key)) in ends.iter().enumerate() {
                let mut txn = db.begin(&TxnOptions::new().io_queue(id));
                let witness = Arc::new(Seen::default());
                watched(&mut txn, &witness);
                txn.put(format!("key{key}").as_bytes(), format!("txn{at}").as_bytes()).unwrap();
                seen.push((at, Arc::clone(&witness)));
                begun.push(txn);
            }
            for (txn, (end, _)) in begun.into_iter().zip(&ends) {
                match end {
                    End::Commit => { let _ = txn.commit(); }
                    End::Nowait => tickets.push(txn.commit_nowait()),
                    End::NowaitDropTicket => drop(txn.commit_nowait()),
                    End::Rollback => txn.rollback(),
                    End::Drop => drop(txn),
                    End::Open => open.push(txn),
                }
            }
            match delivery {
                // The queue's drop delivers what it holds, on this thread.
                Delivery::DropQueue => drop(queue.take()),
                Delivery::PollBefore => {
                    if let Some(queue) = queue.as_mut() {
                        queue.poll(IoBudget::ALL);
                    }
                }
                // Close comes first: it lands what is owed, and the poll
                // after it delivers.
                Delivery::PollAfter => {}
            }
            db.db().close().unwrap();
            // Open transactions ended at close; ending them now runs nothing.
            for txn in open {
                drop(txn);
            }
            if let Some(mut queue) = queue.take() {
                queue.poll(IoBudget::ALL);
            }
            for ticket in &tickets {
                prop_assert!(ticket.is_ready(), "a ticket left pending");
            }
            for (at, witness) in &seen {
                let committed = witness.committed.load(Ordering::SeqCst);
                let aborted = witness.aborted.load(Ordering::SeqCst);
                prop_assert_eq!(committed + aborted, 1, "transaction {} ended {} times", at, committed + aborted);
                if committed == 1 {
                    committed_keys.push(*at);
                }
            }
            prop_assert_eq!(
                hooks.outcomes.load(Ordering::SeqCst),
                ends.len(),
                "the hooks are told one outcome per transaction"
            );
        }
        // Every commit that ran `on_commit` is durable: the key holds the
        // value of the last committed writer of it.
        let db = Db::open(dir.path(), options()).unwrap();
        for key in 0..4usize {
            let last = committed_keys
                .iter()
                .rev()
                .find(|at| ends[**at].1 == key)
                .map(|at| format!("txn{at}").into_bytes());
            let stored = db.get(format!("key{key}").as_bytes()).unwrap();
            if let Some(last) = last {
                prop_assert_eq!(stored, Some(last));
            }
        }
    }
}
