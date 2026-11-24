//! A foreground job and the stop its hold on the compaction gate kept
//! stopped writers from relieving: a job run by the call, a job parked on
//! a queue and run by its owner's poll, and a parked job the gate was
//! handed to and that never ran.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use tempfile::TempDir;

use crate::{Db, Error, IngestOptions, IoBudget, Options, SstFileWriter, StallWait, WouldBlock};

/// Long enough that only a hang meets it.
const STUCK: Duration = Duration::from_secs(60);

/// One L0 table stops every write; no worker, inline compaction on, so a
/// stopped writer relieves the stop with compaction steps of its own.
fn stopped_db(dir: &std::path::Path) -> Arc<Db> {
    let options = Options::default()
        .max_background_compactions(0)
        .inline_compaction(true)
        .l0_compaction_trigger(1)
        .level0_slowdown_writes_trigger(1)
        .level0_stop_writes_trigger(1);
    let db = Arc::new(Db::open(dir, options).unwrap());
    db.put(b"m", b"in L0").unwrap();
    db.flush().unwrap();
    assert_eq!(
        db.get_int_property("regolith.num-files-at-level0"),
        Some(1),
        "setup: one table in L0"
    );
    db
}

/// A writer stopped while an ingest holds the compaction gate finds its
/// relief contended and waits for the thread holding the gate. The ingest,
/// once it releases the gate, relieves the stop, so the wait completes. With
/// no relief after the job, nothing would ever compact and the writer would
/// wait for good.
#[test]
fn a_writer_stopped_during_an_ingest_is_relieved_when_the_ingest_ends() {
    let dir = TempDir::new().unwrap();
    let db = stopped_db(dir.path());
    // Keys far from the L0 table's, so the ingest lands below L0 and adds
    // nothing to the stop.
    let source = dir.path().join("external.sst");
    let mut writer = SstFileWriter::create(&source, &Options::default()).unwrap();
    writer.put(b"z-ingested", b"v").unwrap();
    writer.finish().unwrap();

    let (sent, received) = mpsc::channel();
    {
        let db = Arc::clone(&db);
        // Runs on this thread, under the gate and the pipeline, after the
        // ingest took its sequence.
        super::super::ingest::after_next_ingest_seq(move || {
            let (put, outcome) = mpsc::channel();
            std::thread::spawn(move || put.send(db.put(b"late", b"v")));
            // A write the stop did not refuse would wait for the pipeline this
            // thread holds: bounded, so that fails the test instead.
            let outcome = outcome
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("setup: the write was not stopped and waits for the pipeline");
            sent.send(outcome).unwrap();
        });
    }
    db.ingest_external_files(std::slice::from_ref(&source), IngestOptions::default())
        .wait()
        .unwrap();
    let wait = match received.recv().unwrap() {
        Err(Error::WouldBlock(WouldBlock::Stall(wait))) => wait,
        other => panic!("setup: the write met the stop while the gate was held: {other:?}"),
    };
    assert!(
        wait.is_ready(),
        "the stop the ingest's gate kept the writer from relieving was left in place"
    );
    db.put(b"late", b"v").unwrap();
    assert_eq!(db.get(b"z-ingested").unwrap(), Some(b"v".to_vec()));
}

/// The outcome of a write made on another thread while this thread holds
/// the compaction gate and the pipeline. A write the stop did not refuse
/// would wait for the pipeline: bounded, so that fails the test instead.
fn write_on_another_thread(db: &Arc<Db>) -> crate::Result<()> {
    let (put, outcome) = mpsc::channel();
    let db = Arc::clone(db);
    std::thread::spawn(move || put.send(db.put(b"late", b"v")));
    outcome
        .recv_timeout(Duration::from_secs(10))
        .expect("setup: the write was not stopped and waits for the pipeline")
}

fn stall_wait(outcome: crate::Result<()>) -> StallWait {
    match outcome {
        Err(Error::WouldBlock(WouldBlock::Stall(wait))) => wait,
        other => panic!("setup: the write did not meet the stop: {other:?}"),
    }
}

/// The same relief when the job ran from a queue's poll after it parked
/// there behind a pass: the gated part ends on the owner's thread, which
/// then relieves the stop a writer waited on.
#[test]
fn a_writer_stopped_while_a_once_parked_ingest_runs_is_relieved_when_it_ends() {
    let dir = TempDir::new().unwrap();
    let db = stopped_db(dir.path());
    let source = dir.path().join("external.sst");
    let mut writer = SstFileWriter::create(&source, &Options::default()).unwrap();
    writer.put(b"z-ingested", b"v").unwrap();
    writer.finish().unwrap();

    // A pass in flight: the ingest finds the gate held and parks.
    let pass = db
        .engine()
        .compaction_gate()
        .try_shared()
        .expect("setup: the gate is free");
    let (sent, received) = mpsc::channel();
    let (parked, when_parked) = mpsc::channel();
    let (go, gone) = mpsc::channel::<()>();
    let owner = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            let mut queue = db.io_queue();
            let writer_db = Arc::clone(&db);
            // On this thread, where the poll runs the ingest.
            super::super::ingest::after_next_ingest_seq(move || {
                sent.send(write_on_another_thread(&writer_db)).unwrap();
            });
            let ticket =
                db.ingest_external_files(std::slice::from_ref(&source), IngestOptions::default());
            assert_eq!(
                queue.poll(IoBudget::ALL).completed,
                0,
                "setup: the job parked behind the pass"
            );
            parked.send(()).unwrap();
            gone.recv().unwrap();
            queue.block_on(ticket)
        })
    };
    when_parked
        .recv_timeout(STUCK)
        .expect("the poll waited for the gate");
    drop(pass);
    go.send(()).unwrap();
    owner.join().unwrap().unwrap();

    let wait = stall_wait(received.recv().unwrap());
    assert!(
        wait.is_ready(),
        "the stop the parked job's gate kept the writer from relieving was left in place"
    );
    db.put(b"late", b"v").unwrap();
    assert_eq!(db.get(b"z-ingested").unwrap(), Some(b"v".to_vec()));
}

/// A job parked on a queue is handed the gate once passes barged past it
/// too often, and holds it from then until its owner polls: a stopped
/// writer's step is contended meanwhile, and the writer waits. When the
/// owner drops the queue instead, the job never runs; the gate it gives
/// back must reach the writer, which then relieves the stop itself.
#[test]
fn a_parked_job_handed_the_gate_and_dropped_hands_the_stop_back() {
    let dir = TempDir::new().unwrap();
    let db = stopped_db(dir.path());
    let gate = db.engine().compaction_gate();
    let mut pass = Some(gate.try_shared().expect("setup: the gate is free"));

    let (polled, when_polled) = mpsc::channel();
    let (poll, to_poll) = mpsc::channel::<bool>();
    let owner = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            let mut queue = db.io_queue();
            let ticket = db.compact_range(Some(b"y0"), Some(b"y1"));
            // Each poll steps the job once: it finds the gate held and
            // parks again, one more time passed over.
            loop {
                assert_eq!(queue.poll(IoBudget::ALL).completed, 0, "the job ran");
                polled.send(()).unwrap();
                if !to_poll.recv().unwrap() {
                    break;
                }
            }
            drop(queue);
            ticket.wait()
        })
    };
    when_polled
        .recv_timeout(STUCK)
        .expect("the poll waited for the gate");

    // Free the gate and barge straight back in, then let the job look: it
    // loses again. Past the bound it is owed the gate, and the next release
    // hands it over, with the job still parked.
    let mut handed = false;
    for _ in 0..crate::sync::MAX_BYPASS + 2 {
        drop(pass.take());
        match gate.try_shared() {
            Some(again) => pass = Some(again),
            None => {
                handed = true;
                break;
            }
        }
        poll.send(true).unwrap();
        when_polled
            .recv_timeout(STUCK)
            .expect("the poll waited for the gate");
    }
    assert!(handed, "setup: the parked job was never handed the gate");

    let wait = stall_wait(db.put(b"late", b"v"));
    assert!(!wait.is_ready(), "setup: nothing relieved the stop yet");

    poll.send(false).unwrap();
    let told = owner.join().unwrap();
    assert!(told.is_err(), "the job never ran, got {told:?}");
    assert!(
        wait.is_ready(),
        "the gate a dropped job gave back never reached the writer it held off"
    );
    db.put(b"late", b"v").unwrap();
}
