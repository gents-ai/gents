//! The compaction gate is fair both ways and no call waits on it (plan 4.6,
//! E28).
//!
//! - **E28.** An endless stream of exclusive users (`compact_range`, an
//!   ingest, a checkpoint) never starves the compaction worker, and writers
//!   never stall behind it: the worker takes turns between the jobs and its
//!   own flushes and passes.
//! - **A poll never waits.** A job on a caller's queue that finds the gate
//!   held by a pass parks there; the poll returns at once, and the queue's
//!   owner is woken once the job may go on.
//! - **A caller's pass never waits.** `compact_step` reports `Contended`
//!   while a job holds the gate.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::device_env::{Device, cold_db};
use common::{STUCK, eventually, returns};
use regolith::{CompactionOutcome, Db, Error, IoBudget, JobTicket, Options, WouldBlock};
use tempfile::TempDir;

fn l0(db: &Db) -> u64 {
    db.get_int_property("regolith.num-files-at-level0").unwrap()
}

/// A database with no worker and four cold L0 tables past its compaction
/// trigger, so a compaction pass has work and reads the device to do it.
fn cold_l0(workers: usize) -> (TempDir, Arc<Db>, Arc<Device>) {
    // Filled with compaction off, so the tables are still in L0 when the
    // database is opened again with the trigger that makes them work.
    let filling = AtomicBool::new(true);
    let (dir, db, device) = cold_db(
        |env| {
            let trigger = if filling.swap(false, Ordering::SeqCst) {
                1000
            } else {
                2
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

/// Jobs handed over and not yet ended, at most: what bounds the memory the
/// stream of jobs holds.
const IN_FLIGHT: usize = 4_096;
/// The threads handing the jobs over.
const PRODUCERS: usize = 4;

/// Sets its flag when dropped.
struct StopOnDrop<'a>(&'a AtomicBool);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// E28's stream. Four threads hand `db` exclusive jobs as fast as they can,
/// far faster than it runs them, for as long as the writer runs: the job
/// queue is kept thousands deep and is never empty. Each job flushes what the
/// writer wrote into L0 and compacts nothing, its range holding no key.
/// Meanwhile `put` writes keys `0..writes` in order, waiting out every stall,
/// up to [`STUCK`]; any other error fails the writer. Returns how many keys
/// went in and how many jobs were handed over. Each job counts itself out of
/// `in_flight` when it ends, and into `failed` when it failed.
fn stream_jobs_beside_a_writer(
    db: &Db,
    writes: u32,
    in_flight: &Arc<AtomicUsize>,
    failed: &Arc<AtomicUsize>,
    put: impl Fn(u32) -> regolith::Result<()>,
) -> (u32, usize) {
    let writer_done = AtomicBool::new(false);
    let handed = AtomicUsize::new(0);
    let deadline = Instant::now() + STUCK;
    let written = std::thread::scope(|scope| {
        for _ in 0..PRODUCERS {
            scope.spawn(|| {
                while !writer_done.load(Ordering::SeqCst) {
                    if in_flight.load(Ordering::SeqCst) >= IN_FLIGHT {
                        std::thread::yield_now();
                        continue;
                    }
                    in_flight.fetch_add(1, Ordering::SeqCst);
                    let (in_flight, failed) = (Arc::clone(in_flight), Arc::clone(failed));
                    db.compact_range(Some(b"zz0"), Some(b"zz1"))
                        .on_complete(move |result| {
                            if result.is_err() {
                                failed.fetch_add(1, Ordering::SeqCst);
                            }
                            in_flight.fetch_sub(1, Ordering::SeqCst);
                        });
                    handed.fetch_add(1, Ordering::SeqCst);
                }
            });
        }

        // Stops the producers however the writer ends, by a return or a
        // panic: a writer that failed and left them running would leave the
        // scope waiting on them for ever, and the test would hang where it
        // should report the failure.
        let _stop = StopOnDrop(&writer_done);
        let mut written = 0;
        'writes: for i in 0..writes {
            loop {
                match put(i) {
                    Ok(()) => break,
                    Err(Error::WouldBlock(WouldBlock::Stall(stall))) => {
                        // The stop clears once the worker compacts. Bounded
                        // backoff up to the deadline, never a fixed sleep.
                        let mut backoff = Duration::from_micros(50);
                        while !stall.is_ready() {
                            if Instant::now() >= deadline {
                                break 'writes;
                            }
                            std::thread::sleep(backoff);
                            backoff = (backoff * 2).min(Duration::from_millis(5));
                        }
                    }
                    Err(other) => panic!("a write failed: {other:?}"),
                }
            }
            written += 1;
        }
        written
    });
    (written, handed.load(Ordering::SeqCst))
}

/// E28. The stream never starves the compaction worker: it must still flush
/// and compact between the jobs, so the writer gets through. A stop that
/// held it would last as long as the stream.
///
/// The worker this replaced ran every queued job before anything else, so
/// under this stream it never compacted, L0 reached the stop trigger and the
/// writer stayed stopped until the deadline.
#[test]
fn an_endless_stream_of_jobs_never_starves_the_compaction_worker() {
    const WRITES: u32 = 3_000;
    const STOP: usize = 6;
    let dir = TempDir::new().unwrap();
    let db = Db::open(
        dir.path(),
        Options::default()
            .write_buffer_size(16 * 1024)
            .l0_compaction_trigger(2)
            .level0_slowdown_writes_trigger(4)
            .level0_stop_writes_trigger(STOP),
    )
    .unwrap();
    let in_flight = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let (written, handed) = stream_jobs_beside_a_writer(&db, WRITES, &in_flight, &failed, |i| {
        db.put(format!("k{i:06}").as_bytes(), &[3u8; 1024])
    });

    assert_eq!(
        written, WRITES,
        "the writer stayed stopped behind the stream of jobs: the worker never compacted"
    );
    // The stream has stopped: what it left queued runs out.
    eventually("the queued jobs end", || {
        in_flight.load(Ordering::SeqCst) == 0
    });
    assert!(
        handed >= IN_FLIGHT,
        "only {handed} jobs were handed over while the writer ran: the stream was no stream"
    );
    assert_eq!(failed.load(Ordering::SeqCst), 0, "a job failed");
    assert!((l0(&db) as usize) < STOP, "L0 was left at the stop trigger");
    for i in (0..WRITES).step_by(97) {
        assert_eq!(
            db.get(format!("k{i:06}").as_bytes()).unwrap(),
            Some(vec![3u8; 1024])
        );
    }
}

/// A writer that fails stops the stream on its way out, so the test reports
/// the failure. Before, the producers ran on until the writer was done, and a
/// writer that panicked was never done: the scope waited on them for ever.
#[test]
fn a_failing_writer_stops_the_stream_of_jobs() {
    let dir = TempDir::new().unwrap();
    let db = Arc::new(Db::open(dir.path(), Options::default()).unwrap());
    let in_flight = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let outcome = {
        let (db, in_flight, failed) =
            (Arc::clone(&db), Arc::clone(&in_flight), Arc::clone(&failed));
        returns("a failing writer ends the stream", move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                stream_jobs_beside_a_writer(&db, 100, &in_flight, &failed, |i| {
                    if i == 10 {
                        return Err(Error::InvalidArgument("a planted failure".into()));
                    }
                    db.put(format!("k{i:06}").as_bytes(), b"v")
                })
            }))
        })
    };
    assert!(outcome.is_err(), "the planted failure was not reported");
    eventually("the queued jobs end", || {
        in_flight.load(Ordering::SeqCst) == 0
    });
}

/// A job on a caller's queue never makes its poll wait for the gate. While
/// a compaction pass on another thread holds the gate shared, held at the
/// device mid-read, the poll parks the job and returns at once, as often as
/// it is called. Once the pass ends the gate wakes the queue's owner, and
/// the next poll runs the job.
#[test]
fn a_poll_never_waits_for_the_gate() {
    let (_dir, db, device) = cold_l0(0);
    device.hold();
    let pass = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || db.compact_step())
    };
    device.wait_held(1);

    // The queue lives on one thread, as a queue does; what that thread
    // learned at each step comes back over a channel.
    let (step, steps) = mpsc::channel();
    let (released, go_on) = mpsc::channel::<()>();
    let owner = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            let mut queue = db.io_queue();
            let ticket = db.compact_range(None, None);
            let first = queue.poll(IoBudget::ALL);
            let second = queue.poll(IoBudget::ALL);
            step.send((
                first.completed + second.completed,
                second.more_pending,
                ticket.is_ready(),
            ))
            .unwrap();
            go_on.recv().unwrap();
            queue.block_on(ticket)
        })
    };
    let (completed, more_pending, ready) = steps
        .recv_timeout(STUCK)
        .expect("a poll waited for the pass that holds the gate");
    assert_eq!(completed, 0, "nothing completed");
    assert!(more_pending, "the job is still owed");
    assert!(!ready, "the job has not run");

    device.release();
    assert_eq!(pass.join().unwrap().unwrap(), CompactionOutcome::DidWork);
    released.send(()).unwrap();
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(owner.join());
    });
    result
        .recv_timeout(STUCK)
        .expect("the parked job was never woken")
        .unwrap()
        .unwrap();
    assert_eq!(l0(&db), 0, "the job ran once the gate freed");
}

/// The wake is not lost when the gate frees while the owner is idle: the
/// owner, parked in `block_on` with only the parked job on its queue, is
/// woken by the gate's release.
#[test]
fn an_idle_owner_is_woken_when_the_gate_frees() {
    let (_dir, db, device) = cold_l0(0);
    device.hold();
    let pass = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || db.compact_step())
    };
    device.wait_held(1);

    let (parked, when_parked) = mpsc::channel();
    let owner = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            let mut queue = db.io_queue();
            let ticket = db.compact_range(None, None);
            assert_eq!(queue.poll(IoBudget::ALL).completed, 0);
            parked.send(()).unwrap();
            // Idles here: the job is parked and nothing else is owed.
            queue.block_on(ticket)
        })
    };
    when_parked
        .recv_timeout(STUCK)
        .expect("the poll waited for the gate");
    device.release();
    assert_eq!(pass.join().unwrap().unwrap(), CompactionOutcome::DidWork);
    returns("the owner's block_on", move || owner.join())
        .unwrap()
        .unwrap();
    assert_eq!(l0(&db), 0);
}

/// A pass a caller runs never waits for the gate: while a job holds it
/// alone, held at the device, `compact_step` reports `Contended` at once.
#[test]
fn compact_step_is_contended_while_a_job_holds_the_gate() {
    let (_dir, db, device) = cold_l0(0);
    device.hold();
    // No worker and no queue: the call runs the job itself, holding the
    // gate alone, and sits at the device.
    let job = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || db.compact_range(None, None).wait())
    };
    device.wait_held(1);

    let outcome = {
        let db = Arc::clone(&db);
        returns("compact_step", move || db.compact_step())
    };
    assert_eq!(outcome.unwrap(), CompactionOutcome::Contended);

    device.release();
    job.join().unwrap().unwrap();
    assert_eq!(l0(&db), 0);
    assert_eq!(db.compact_step().unwrap(), CompactionOutcome::Idle);
}

/// A blocking caller that runs a parked job itself waits for the gate on its
/// own thread, and the job runs once.
#[test]
fn wait_runs_a_parked_job_once_the_gate_frees() {
    let (_dir, db, device) = cold_l0(0);
    device.hold();
    let pass = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || db.compact_step())
    };
    device.wait_held(1);

    let (parked, when_parked) = mpsc::channel();
    let owner = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            let mut queue = db.io_queue();
            let ticket = db.compact_range(None, None);
            assert_eq!(queue.poll(IoBudget::ALL).completed, 0);
            parked.send(()).unwrap();
            let result = ticket.wait();
            // Delivered at the queue's poll, as any ticket on a queue is.
            queue.poll(IoBudget::ALL);
            result
        })
    };
    when_parked
        .recv_timeout(STUCK)
        .expect("the poll waited for the gate");
    device.release();
    assert_eq!(pass.join().unwrap().unwrap(), CompactionOutcome::DidWork);
    returns("JobTicket::wait", move || owner.join())
        .unwrap()
        .unwrap();
    assert_eq!(l0(&db), 0);
}

/// With two workers, jobs on one and passes on the other meet at the gate:
/// every job ends, the passes still compact L0 away, and close finds both
/// threads joinable.
#[test]
fn two_workers_share_the_gate_between_jobs_and_passes() {
    let (_dir, db, _device) = cold_l0(2);
    let jobs: Vec<JobTicket> = (0..32)
        .map(|_| db.compact_range(Some(b"zz0"), Some(b"zz1")))
        .collect();
    for job in jobs {
        job.wait().unwrap();
    }
    eventually("a pass runs beside the jobs", || l0(&db) == 0);
    db.close().unwrap();
}
