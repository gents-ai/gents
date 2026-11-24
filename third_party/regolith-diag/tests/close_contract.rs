//! The `close` contract (plan 3.0): close resolves every ticket the final
//! sync covers, delivers `Closed` to every queue with a pending unit, runs
//! the `on_abort` callbacks of transactions still open on the closing
//! thread, then joins regolith's threads.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::ThreadId;

use common::device_env::{Device, cold_db};
use common::faulty_env::FaultyEnv;
use common::{eventually, returns};
use regolith::{
    AbortReason, CompactionOutcome, Db, DurabilityMode, Error, IoBudget, OptimisticTransactionDb,
    Options, ReadMode, TransactionError, TxnOptions, WouldBlock,
};
use tempfile::TempDir;

/// A database with four cold L0 tables, opened with `workers` compaction
/// workers and an L0 compaction trigger of `trigger`. It was filled with
/// compaction off, so the tables are all still in L0.
fn cold_l0(workers: usize, trigger: usize) -> (TempDir, Arc<Db>, Arc<Device>) {
    let filling = std::sync::atomic::AtomicBool::new(true);
    let (dir, db, device) = cold_db(
        |env| {
            let trigger = if filling.swap(false, Ordering::SeqCst) {
                1000
            } else {
                trigger
            };
            Options::default()
                .env(env)
                .max_background_compactions(workers)
                .l0_compaction_trigger(trigger)
        },
        |db| {
            for i in 0..200u32 {
                db.put(format!("k{i:05}").as_bytes(), &[7u8; 256]).unwrap();
                if i % 50 == 49 {
                    db.flush().unwrap();
                }
            }
        },
    );
    (dir, Arc::new(db), device)
}

/// Close has begun: a write is refused as closed.
fn closing(db: &Db) -> bool {
    matches!(db.put(b"probe", b"v"), Err(Error::Closed))
}

/// A commit whose group still owed its fsync when close began is resolved
/// by close: it committed, its ticket says so at the queue's next poll, and
/// the commit survives a reopen.
#[test]
fn close_resolves_every_ticket_the_final_sync_covers() {
    let dir = TempDir::new().unwrap();
    {
        let db = OptimisticTransactionDb::open(
            dir.path(),
            Options::default().durability(DurabilityMode::Immediate),
        )
        .unwrap();
        let mut queue = db.db().io_queue();
        let txn = db.begin(&TxnOptions::new().io_queue(queue.id()));
        txn.put(b"k", b"v").unwrap();
        let ticket = txn.commit_nowait();
        assert!(!ticket.is_ready());
        db.db().close().unwrap();
        queue.poll(IoBudget::ALL);
        assert!(ticket.is_ready());
        let receipt = queue.block_on(ticket).expect("the final sync covered it");
        assert!(receipt.seq() > 0);
    }
    let db = Db::open(dir.path(), Options::default()).unwrap();
    assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
}

/// Every queue with a pending unit is told at close: a read's wait, after
/// which the read answers `Closed`; a foreground job's ticket, which fails
/// with `Closed`; a stalled write's wait.
#[test]
fn close_delivers_closed_to_every_queue_with_a_pending_unit() {
    let (_dir, db, _device) = cold_db(
        |env| {
            Options::default()
                .env(env)
                .block_size(256)
                .max_background_compactions(0)
        },
        |db| {
            for i in 0..2_000u32 {
                db.put(format!("k{i:05}").as_bytes(), b"value").unwrap();
            }
        },
    );
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    let read = match snapshot.get(b"k01000") {
        Err(Error::WouldBlock(WouldBlock::Io(wait))) => wait,
        other => panic!("a cold read must miss, got {other:?}"),
    };
    let job = db.compact_range(None, None);
    assert!(!read.is_ready() && !job.is_ready());

    db.close().unwrap();
    queue.poll(IoBudget::ALL);
    assert!(read.is_ready(), "the read's wait was told");
    assert!(matches!(snapshot.get(b"k01000"), Err(Error::Closed)));
    assert!(job.is_ready());
    assert!(matches!(job.wait(), Err(Error::Closed)));
}

/// A transaction still open when close begins ends there: its `on_abort`
/// callbacks run on the closing thread, with `Closed`, and its later
/// `commit_nowait` fails with `Closed` and runs nothing more.
#[test]
fn close_runs_the_on_abort_of_open_transactions_on_the_closing_thread() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
    let mut txn = db.begin(&TxnOptions::new());
    txn.put(b"k", b"v").unwrap();
    let ran: Arc<std::sync::Mutex<Vec<ThreadId>>> = Arc::default();
    let seen = Arc::clone(&ran);
    txn.on_abort(move |reason| {
        assert!(matches!(reason, AbortReason::Closed));
        seen.lock().unwrap().push(std::thread::current().id());
    });
    let closing_thread = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                db.db().close().unwrap();
                std::thread::current().id()
            })
            .join()
            .unwrap()
    });
    assert_eq!(*ran.lock().unwrap(), vec![closing_thread]);
    let mut queue = db.db().io_queue();
    assert!(matches!(
        queue.block_on(txn.commit_nowait()),
        Err(TransactionError::Engine(Error::Closed))
    ));
    assert_eq!(ran.lock().unwrap().len(), 1, "exactly one outcome");
}

/// Close joins regolith's threads only after the rest: a foreground job the
/// worker never ran fails with `Closed`, and no job runs after close.
#[test]
fn close_settles_the_jobs_the_worker_did_not_run() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();
    db.put(b"k", b"v").unwrap();
    let ran = Arc::new(AtomicUsize::new(0));
    let tickets: Vec<_> = (0..8)
        .map(|_| {
            let ticket = db.compact_range(None, None);
            let count = Arc::clone(&ran);
            ticket.on_complete(move |_| {
                count.fetch_add(1, Ordering::SeqCst);
            });
            ticket
        })
        .collect();
    db.close().unwrap();
    for ticket in tickets {
        // Run before close, or settled with `Closed`: never left pending.
        assert!(ticket.is_ready());
        let _ = ticket.wait();
    }
    assert_eq!(ran.load(Ordering::SeqCst), 8, "every callback ran once");
    assert!(matches!(
        db.compact_range(None, None).wait(),
        Err(Error::Closed)
    ));
}

/// Holds every compaction at its start, on the thread that runs it, until
/// the test lets it go: a pass in flight for as long as a test needs one.
#[derive(Default)]
struct HoldCompactions {
    /// Compactions waiting at their start.
    held: std::sync::Mutex<usize>,
    changed: std::sync::Condvar,
    released: std::sync::atomic::AtomicBool,
}

impl HoldCompactions {
    /// Wait until a compaction sits at its start.
    fn wait_held(&self) {
        let held = self.held.lock().unwrap();
        let (_held, timeout) = self
            .changed
            .wait_timeout_while(held, common::STUCK, |held| *held == 0)
            .unwrap();
        assert!(!timeout.timed_out(), "no compaction pass ever started");
    }

    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        let _held = self.held.lock().unwrap();
        self.changed.notify_all();
    }
}

impl regolith::EventListener for HoldCompactions {
    fn on_compaction_begin(&self, _info: &regolith::CompactionJobInfo) {
        let mut held = self.held.lock().unwrap();
        *held += 1;
        self.changed.notify_all();
        while !self.released.load(Ordering::SeqCst) {
            held = self.changed.wait(held).unwrap();
        }
    }
}

/// Close while a compaction pass is in flight on the worker, held at its
/// start with the gate shared: close waits for the pass, joins the worker
/// and returns, and nothing the database held is lost.
#[test]
fn close_waits_out_a_pass_in_flight_on_the_worker() {
    let dir = TempDir::new().unwrap();
    let hold = Arc::new(HoldCompactions::default());
    let db = Arc::new(
        Db::open(
            dir.path(),
            Options::default()
                .l0_compaction_trigger(2)
                .listeners(vec![Arc::clone(&hold) as Arc<dyn regolith::EventListener>]),
        )
        .unwrap(),
    );
    // Two tables reach the trigger, and the worker starts a pass.
    for table in 0..2u32 {
        for i in 0..50u32 {
            db.put(format!("k{table}{i:04}").as_bytes(), &[7u8; 64])
                .unwrap();
        }
        db.flush().unwrap();
    }
    hold.wait_held();

    let closer = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || db.close())
    };
    eventually("close begins", || closing(&db));
    hold.release();
    returns("close", move || closer.join()).unwrap().unwrap();

    drop(db);
    let db = Db::open(dir.path(), Options::default()).unwrap();
    for table in 0..2u32 {
        for i in (0..50u32).step_by(7) {
            assert_eq!(
                db.get(format!("k{table}{i:04}").as_bytes()).unwrap(),
                Some(vec![7u8; 64])
            );
        }
    }
}

/// Close while a job holds the gate alone on the worker, held at the device,
/// with another job queued behind it: close waits for the first, settles the
/// second, and both tickets are told.
#[test]
fn close_waits_out_a_job_that_holds_the_gate_and_settles_the_rest() {
    let (_dir, db, device) = cold_l0(1, 1000);
    device.hold();
    let first = db.compact_range(None, None);
    device.wait_held(1);
    let second = db.compact_range(None, None);

    let closer = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || db.close())
    };
    eventually("close begins", || closing(&db));
    assert!(!first.is_ready(), "the job is still at the device");
    device.release();
    returns("close", move || closer.join()).unwrap().unwrap();

    for (name, ticket) in [("first", first), ("second", second)] {
        assert!(ticket.is_ready(), "the {name} job's ticket was never told");
        let result = ticket.wait();
        assert!(
            matches!(result, Ok(()) | Err(Error::Closed)),
            "the {name} job ended with {result:?}"
        );
    }
}

/// Close while a job is parked on its queue, waiting for a gate a pass on
/// another thread holds: close settles the job with `Closed`, gives back its
/// place at the gate, and does not wait for the pass.
#[test]
fn close_settles_a_job_parked_on_the_gate() {
    let (_dir, db, device) = cold_l0(0, 2);
    device.hold();
    let pass = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || db.compact_step())
    };
    device.wait_held(1);

    let outcome = {
        let db = Arc::clone(&db);
        returns("the owner of the queue", move || {
            let mut queue = db.io_queue();
            let ticket = db.compact_range(None, None);
            assert_eq!(queue.poll(IoBudget::ALL).completed, 0, "the job parked");
            db.close().unwrap();
            queue.poll(IoBudget::ALL);
            assert!(ticket.is_ready(), "close told the parked job's ticket");
            ticket.wait()
        })
    };
    assert!(matches!(outcome, Err(Error::Closed)), "got {outcome:?}");

    // The pass was never waited for; it ends once the device lets it go.
    device.release();
    let passed = pass.join().unwrap();
    assert!(
        matches!(passed, Ok(CompactionOutcome::DidWork) | Err(Error::Closed)),
        "the pass ended with {passed:?}"
    );
}

/// Two closes at once: one runs, the other waits for it on its own thread,
/// and both report the database closed.
#[test]
fn two_closes_at_once_both_succeed() {
    for round in 0..32 {
        let dir = TempDir::new().unwrap();
        let db = Arc::new(Db::open(dir.path(), Options::default()).unwrap());
        db.put(b"k", b"v").unwrap();
        let start = Arc::new(std::sync::Barrier::new(2));
        let closers: Vec<_> = (0..2)
            .map(|_| {
                let (db, start) = (Arc::clone(&db), Arc::clone(&start));
                std::thread::spawn(move || {
                    start.wait();
                    db.close()
                })
            })
            .collect();
        for closer in closers {
            returns("a close", move || closer.join())
                .unwrap()
                .unwrap_or_else(|e| panic!("round {round}: a close failed: {e:?}"));
        }
        db.close().unwrap();
        drop(db);
        let db = Db::open(dir.path(), Options::default()).unwrap();
        assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
    }
}

/// Close while the background is failing: the flush the worker owes keeps
/// failing, so the close's own flush fails too. Close reports it and leaves
/// the database open and readable; once the fault clears, close succeeds and
/// every write is there after a reopen.
#[test]
fn a_close_that_fails_behind_a_failing_flush_leaves_the_database_open() {
    let dir = TempDir::new().unwrap();
    let env = Arc::new(FaultyEnv::default());
    let options = || {
        Options::default()
            .env(Arc::clone(&env) as Arc<dyn regolith::env::Env>)
            .write_buffer_size(4 * 1024)
    };
    let db = Db::open(dir.path(), options()).unwrap();
    env.fail_tables(true);
    // Enough to seal a memtable, which the worker then fails to flush.
    let mut written = 0u32;
    while db.get_int_property("regolith.background-errors").unwrap() == 0 {
        assert!(written < 100_000, "no flush ever failed");
        match db.put(format!("k{written:06}").as_bytes(), &[5u8; 128]) {
            Ok(()) => written += 1,
            // Stopped behind the failing flush, or told why: both are the
            // state this test wants.
            Err(_) => std::thread::yield_now(),
        }
    }

    let failed = db.close().expect_err("the flush close owes cannot succeed");
    assert!(
        !matches!(failed, Error::Closed),
        "close reported the database closed: {failed:?}"
    );
    assert_eq!(
        db.get(b"k000000").unwrap(),
        Some(vec![5u8; 128]),
        "the database is still open after the failed close"
    );

    env.fail_tables(false);
    db.close().unwrap();
    drop(db);
    let db = Db::open(dir.path(), options()).unwrap();
    for i in 0..written {
        assert_eq!(
            db.get(format!("k{i:06}").as_bytes()).unwrap(),
            Some(vec![5u8; 128]),
            "write {i} was lost"
        );
    }
}

/// The error an open reports when the directory is still held.
fn held(result: Result<Db, Error>) -> bool {
    matches!(result, Err(Error::Io(io)) if io.kind() == std::io::ErrorKind::AlreadyExists)
}

/// Close ends the database's work; it does not give the directory back. The
/// directory stays held until the handle that was closed, and every snapshot
/// taken from it, has been dropped, and only then can it be opened again. On
/// a file system and in memory alike.
#[test]
fn a_closed_database_holds_its_directory_until_its_handles_are_dropped() {
    let dir = TempDir::new().unwrap();
    let memory = Options::memory();
    let memory = || Options::memory().env(Arc::clone(memory.get_env()));
    let cases: [(&std::path::Path, &dyn Fn() -> Options); 2] = [
        (dir.path(), &Options::default),
        (std::path::Path::new("held-in-memory"), &memory),
    ];
    for (path, options) in cases {
        let db = Db::open(path, options()).unwrap();
        db.put(b"k", b"v").unwrap();
        let snapshot = db.snapshot();
        db.close().unwrap();
        assert!(matches!(db.put(b"k", b"w"), Err(Error::Closed)));

        assert!(held(Db::open(path, options())), "closed, handle alive");
        drop(db);
        assert!(held(Db::open(path, options())), "a snapshot is alive");
        drop(snapshot);

        let reopened = Db::open(path, options()).unwrap();
        assert_eq!(reopened.get(b"k").unwrap(), Some(b"v".to_vec()));
    }
}
