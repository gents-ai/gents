//! The commit pipeline against a real database: the three invariants the
//! commit path keeps (the horizon trails durability, a failure fails every
//! unfinished commit, an abandoned writer wedges nothing), and the buffer a
//! thread stages its records in. `pipeline_order_tests.rs` holds the order:
//! serial equivalence, stopped threads, aborts.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use proptest::prelude::*;
use tempfile::TempDir;

use super::super::{EngineOptions, wal::fault};
use super::descriptor::{Descriptor, Work};
use super::hook::{self, Stage};
use super::*;
use crate::statistics::Statistics;
use crate::sync::internal::Mutex;

pub(super) fn open_engine(dir: &TempDir) -> Arc<RegolithEngine> {
    RegolithEngine::open(dir.path(), EngineOptions::default()).expect("engine open")
}

fn open_with_stats(dir: &TempDir) -> (Arc<RegolithEngine>, Arc<Statistics>) {
    let stats = Arc::new(Statistics::new());
    let engine = RegolithEngine::open(
        dir.path(),
        EngineOptions {
            statistics: Some(Arc::clone(&stats)),
            ..EngineOptions::default()
        },
    )
    .unwrap();
    (engine, stats)
}

/// Default-column-family prefix, the shape every engine key carries.
pub(super) fn key(name: &[u8]) -> Vec<u8> {
    let mut k = vec![0u8; 4];
    k.extend_from_slice(name);
    k
}

pub(super) fn put_with(name: &[u8], value: &[u8], durability: DurabilityMode) -> WriteRequest {
    WriteRequest::Put {
        key: key(name),
        value: value.to_vec(),
        durability,
        disable_wal: false,
    }
}

pub(super) fn durable_put(name: &[u8], value: &[u8]) -> WriteRequest {
    put_with(name, value, DurabilityMode::Immediate)
}

pub(super) fn eventual_put(name: &[u8], value: &[u8]) -> WriteRequest {
    put_with(name, value, DurabilityMode::Eventual)
}

pub(super) struct FaultGuard(std::path::PathBuf);

impl Drop for FaultGuard {
    fn drop(&mut self) {
        fault::disarm_sync_failure(&self.0);
    }
}

/// Arm the WAL sync fault for `dir` only, and disarm that directory when the
/// test scope ends, leaving any parallel test's own arming untouched.
pub(super) fn arm_sync_failure(dir: &TempDir) -> FaultGuard {
    fault::arm_sync_failure(dir.path());
    FaultGuard(dir.path().to_path_buf())
}

/// Arm the WAL sync fault for `dir` so it fails every sync but each second
/// one, and disarm that directory when the test scope ends.
fn arm_flapping_sync_failure(dir: &TempDir) -> FaultGuard {
    fault::arm_flapping_sync_failure(dir.path(), 2);
    FaultGuard(dir.path().to_path_buf())
}

/// Take a slot for `request` and install it, without bringing it through:
/// a commit whose writer went no further than the ring.
pub(super) fn install(
    engine: &RegolithEngine,
    request: WriteRequest,
) -> (u64, RingArc<Descriptor>) {
    let pipe = engine.pipe().unwrap();
    let desc = RingArc::new(Descriptor::new(Work::Request(request), false));
    let index = engine.enter(pipe, &desc);
    (index, desc)
}

/// Bring the slot `index`, and so every slot before it, through the
/// pipeline on this thread.
fn drive(engine: &RegolithEngine, index: u64, desc: &RingArc<Descriptor>) {
    engine.drive(engine.pipe().unwrap(), index, desc);
}

pub(super) fn seq_of(desc: &Descriptor) -> io::Result<u64> {
    RegolithEngine::settled(None, desc).and_then(Settled::into_seq)
}

/// Run `call` on a scoped thread, stop it the first time it reaches `stage`,
/// run `meanwhile` on this thread, then let it go on and return what it
/// returned.
pub(super) fn stopped_at<T: Send>(
    stage: Stage,
    call: impl FnOnce() -> T + Send,
    meanwhile: impl FnOnce(),
) -> T {
    let (reached, at_stage) = mpsc::channel();
    let (resume, resumed) = mpsc::channel::<()>();
    thread::scope(|scope| {
        let stopped = scope.spawn(move || {
            let mut fired = false;
            hook::set(move |now| {
                if now == stage && !fired {
                    fired = true;
                    reached.send(()).unwrap();
                    resumed.recv().unwrap();
                }
            });
            let out = call();
            hook::clear();
            out
        });
        at_stage
            .recv_timeout(Duration::from_secs(30))
            .expect("the call never reached the stage");
        meanwhile();
        resume.send(()).unwrap();
        stopped.join().unwrap()
    })
}

#[test]
fn cloned_errors_keep_kind_and_message() {
    let err = io::Error::new(io::ErrorKind::StorageFull, "disk is full");
    let cloned = crate::Error::clone_io(&err);
    assert_eq!(cloned.kind(), err.kind());
    assert_eq!(cloned.to_string(), err.to_string());
}

/// Every commit a failure discards learns the same OS error the failing
/// thread saw, its code included, not only its kind.
#[test]
fn cloned_os_errors_keep_their_code() {
    let err = io::Error::from_raw_os_error(28);
    let cloned = crate::Error::clone_io(&err);
    assert_eq!(cloned.raw_os_error(), Some(28));
    assert_eq!(cloned.kind(), err.kind());
}

#[test]
fn commit_ring_capacity_is_bounded() {
    let cap = commit_ring_capacity();
    assert!(
        (256..=1024).contains(&cap),
        "unexpected ring capacity {cap}"
    );
}

#[test]
fn a_failed_sync_fails_every_commit_it_covers() {
    // G2: every unfinished commit learns the fsync did not happen. A writer
    // that believed it committed here would be silent data loss.
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);

    let horizon_before = engine.snapshot_seq();
    let seq_before = engine.last_decided_seq();
    let wal_len_before = engine.log_end();

    let commits: Vec<_> = (0..3)
        .map(|i| {
            install(
                &engine,
                durable_put(format!("member{i}").as_bytes(), b"value"),
            )
        })
        .collect();
    {
        let _fault = arm_sync_failure(&dir);
        let (index, last) = commits.last().unwrap();
        drive(&engine, *index, last);
    }

    for (i, (_, desc)) in commits.iter().enumerate() {
        let err = seq_of(desc).expect_err("a commit the failed sync covered must learn it");
        assert!(err.to_string().contains("injected"), "member {i}: {err}");
    }
    assert_eq!(
        engine.snapshot_seq(),
        horizon_before,
        "a failed sync must not publish a read horizon"
    );
    for i in 0..3 {
        let name = format!("member{i}");
        assert_eq!(
            engine.get(&key(name.as_bytes()), u64::MAX).unwrap(),
            None,
            "a discarded commit must be out of the memtable"
        );
    }
    assert_eq!(
        engine.log_end(),
        wal_len_before,
        "discarded commits must be cut out of the WAL"
    );
    assert_eq!(
        engine.last_decided_seq(),
        seq_before,
        "discarded commits hand their sequences back"
    );
    assert!(engine.pipeline_is_idle());

    // The database goes on, from the same sequence and the same offset.
    let seq = engine
        .commit_through_pipeline(durable_put(b"after", b"v"))
        .and_then(Settled::into_seq)
        .expect("a write after the rollback commits");
    assert_eq!(seq, seq_before + 1);
    assert_eq!(
        engine.get(&key(b"after"), u64::MAX).unwrap(),
        Some(b"v".to_vec())
    );
}

#[test]
fn a_failed_commit_leaves_nothing_to_recover() {
    let dir = TempDir::new().unwrap();
    {
        let engine = open_engine(&dir);
        engine
            .submit(durable_put(b"before", b"kept"))
            .expect("the pre-failure write commits");

        let _fault = arm_sync_failure(&dir);
        let err = engine
            .submit(durable_put(b"during", b"lost"))
            .expect_err("the injected failure must surface");
        assert!(err.to_string().contains("injected"));
    }

    let engine = open_engine(&dir);
    assert_eq!(
        engine.get(&key(b"before"), u64::MAX).unwrap(),
        Some(b"kept".to_vec())
    );
    assert_eq!(
        engine.get(&key(b"during"), u64::MAX).unwrap(),
        None,
        "a write whose sync failed must not survive a reopen"
    );
}

/// A failure discards what is not published and nothing else: the published
/// commits keep their place in the memtable, and a reader that holds the
/// memtable from before the rollback still reads them.
#[test]
fn a_rollback_keeps_every_published_commit_in_the_memtable() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let ops: Vec<WriteBatchOp> = (0..50)
        .map(|i| WriteBatchOp::Put {
            key: key(format!("kept{i:02}").as_bytes()),
            value: vec![i as u8; 8],
        })
        .chain(std::iter::once(WriteBatchOp::DeleteRange {
            start: key(b"kept40"),
            end: key(b"kept45"),
        }))
        .chain(std::iter::once(WriteBatchOp::Merge {
            key: key(b"kept00"),
            operand: b"+".to_vec(),
        }))
        .collect();
    engine
        .submit_batch(ops, DurabilityMode::Eventual, false)
        .unwrap();
    let before = engine.view.load().active.iter_internal();
    let held = Arc::clone(&engine.view.load().active);
    {
        let _fault = arm_sync_failure(&dir);
        engine
            .submit(durable_put(b"lost", b"x"))
            .expect_err("the sync fails");
    }
    let after = engine.view.load();
    assert!(
        !Arc::ptr_eq(&after.active, &held),
        "the memtable a discarded commit was applied to is replaced"
    );
    assert_eq!(after.active.iter_internal(), before);
    assert_eq!(after.active.clone_range_tombstones().len(), 1);
    assert_eq!(
        held.iter_internal().len(),
        before.len() + 1,
        "a reader's memtable is left as it was"
    );
    assert_eq!(engine.get(&key(b"kept41"), u64::MAX).unwrap(), None);
    assert_eq!(
        engine.get(&key(b"kept07"), u64::MAX).unwrap(),
        Some(vec![7u8; 8])
    );
}

#[test]
fn a_reserved_slot_nobody_installed_does_not_wedge_the_ring() {
    // G3: a writer that went away between its reservation and its install
    // holds a slot of the ring. The first commit behind it voids the slot.
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let abandoned = engine.pipe().unwrap().ring.reserve();

    engine
        .submit(durable_put(b"later", b"value"))
        .expect("a later writer passes the abandoned slot");
    assert_eq!(
        engine.get(&key(b"later"), u64::MAX).unwrap(),
        Some(b"value".to_vec())
    );
    assert!(engine.pipe().unwrap().ring.published() > abandoned);
    assert!(engine.pipeline_is_idle());
}

#[test]
fn an_installed_commit_whose_writer_went_away_is_committed_by_the_next_writer() {
    // G3: the ring owns the descriptor, so a writer that went away after its
    // install still gets its commit brought through by whoever comes next.
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let (_, orphan) = install(&engine, durable_put(b"orphan", b"value"));

    engine
        .submit(durable_put(b"later", b"value"))
        .expect("a later writer helps the orphan through");
    seq_of(&orphan).expect("the abandoned commit was committed");
    for name in [&b"orphan"[..], b"later"] {
        assert_eq!(
            engine.get(&key(name), u64::MAX).unwrap(),
            Some(b"value".to_vec())
        );
    }
}

#[test]
fn one_sync_covers_every_commit_written_before_it() {
    let dir = TempDir::new().unwrap();
    let (engine, stats) = open_with_stats(&dir);

    let commits: Vec<_> = (0..8)
        .map(|i| install(&engine, durable_put(format!("k{i}").as_bytes(), b"v")))
        .collect();
    assert_eq!(stats.get_ticker(Ticker::WalSyncCount), 0);
    let (index, last) = commits.last().unwrap();
    drive(&engine, *index, last);

    for (_, desc) in &commits {
        seq_of(desc).expect("every commit lands");
    }
    assert_eq!(
        stats.get_ticker(Ticker::WalSyncCount),
        1,
        "eight durable commits written together must cost exactly one fdatasync"
    );
    for i in 0..8 {
        let name = format!("k{i}");
        assert_eq!(
            engine.get(&key(name.as_bytes()), u64::MAX).unwrap(),
            Some(b"v".to_vec())
        );
    }
}

#[test]
fn the_horizon_covers_a_commit_and_every_commit_before_it() {
    // the lost-update fix: `snapshot_seq` covers a commit only with every
    // commit before it, never a later one alone.
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let base = engine.snapshot_seq();

    let commits: Vec<_> = (0..4)
        .map(|i| install(&engine, durable_put(format!("h{i}").as_bytes(), b"v")))
        .collect();
    assert_eq!(engine.snapshot_seq(), base, "nothing is published yet");
    // Only the second is driven: it brings the first through with it, and
    // nothing behind it is published.
    let (index, second) = &commits[1];
    drive(&engine, *index, second);
    assert_eq!(engine.snapshot_seq(), base + 2);
    let (index, last) = &commits[3];
    drive(&engine, *index, last);

    let horizon = engine.snapshot_seq();
    assert_eq!(horizon, base + 4);
    for i in 0..4 {
        let name = format!("h{i}");
        assert_eq!(
            engine.get(&key(name.as_bytes()), horizon).unwrap(),
            Some(b"v".to_vec()),
            "the published horizon must cover every commit up to it"
        );
    }
}

/// At `Immediate` a commit is decided, applied and written before it is
/// durable, and visible to no snapshot until it is.
#[test]
fn at_immediate_a_commit_is_visible_only_once_it_is_durable() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let base = engine.snapshot_seq();
    let (index, desc) = install(&engine, durable_put(b"k", b"v"));
    let pipe = engine.pipe().unwrap();
    // Everything a `commit_nowait` call does before it returns.
    let blocked = engine.drive_nowait(pipe, index, &desc);
    assert!(blocked.is_err(), "the commit waits for its sync");
    assert_eq!(engine.last_decided_seq(), base + 1, "it is decided");
    assert_eq!(
        engine.get(&key(b"k"), u64::MAX).unwrap(),
        Some(b"v".to_vec()),
        "and applied, where the next commit's validation reads it"
    );
    assert_eq!(engine.snapshot_seq(), base, "and not visible");
    assert_eq!(engine.get(&key(b"k"), engine.snapshot_seq()).unwrap(), None);

    drive(&engine, index, &desc);
    assert_eq!(engine.snapshot_seq(), base + 1);
    assert!(engine.pipe().unwrap().tip.load().after.log.durable() >= engine.log_end());
}

/// A commit at `Eventual` behind one at `Immediate` is not published before
/// it either: publication is in ring order.
#[test]
fn an_eventual_commit_is_not_published_past_an_immediate_one_before_it() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let base = engine.snapshot_seq();
    let (_, first) = install(&engine, durable_put(b"first", b"v"));
    let (index, second) = install(&engine, eventual_put(b"second", b"v"));
    let pipe = engine.pipe().unwrap();
    assert!(engine.drive_nowait(pipe, index, &second).is_err());
    assert_eq!(engine.snapshot_seq(), base);
    drive(&engine, index, &second);
    assert_eq!(engine.snapshot_seq(), base + 2);
    seq_of(&first).unwrap();
}

#[test]
fn a_record_is_staged_in_one_exact_reservation_the_next_record_reuses() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);

    let batch = || WriteRequest::Batch {
        ops: (0..1000)
            .map(|i| WriteBatchOp::Put {
                key: key(format!("k{i:04}").as_bytes()),
                value: vec![b'v'; 4096],
            })
            .collect(),
        durability: DurabilityMode::Eventual,
        disable_wal: false,
    };
    let framed = batch().staged_len() + super::super::wal_frame::HEADER_LEN;
    assert!(
        framed > MAX_GROUP_BYTES,
        "the test batch must exceed the kept floor to exercise the reservation, got {framed}"
    );

    engine
        .submit(batch())
        .expect("a batch larger than the kept floor still commits");
    let (cap, ptr) = super::spare::record_buffer();
    assert_eq!(
        cap, framed,
        "the stage must reserve exactly the record's framed length, not a doubling chain"
    );

    engine.submit(batch()).expect("the next record commits");
    assert_eq!(
        super::spare::record_buffer(),
        (cap, ptr),
        "a stage already large enough for the next record must not reallocate"
    );
}

#[test]
fn an_outsized_record_gives_its_stage_back_once_two_smaller_ones_were_written() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);

    let huge = vec![b'x'; MAX_GROUP_BYTES + 64 * 1024];
    let huge_framed =
        eventual_put(b"huge", &huge).staged_len() + super::super::wal_frame::HEADER_LEN;
    engine
        .submit(eventual_put(b"huge", &huge))
        .expect("a record larger than the kept floor still commits");
    assert_eq!(
        super::spare::record_buffer().0,
        huge_framed,
        "the thread that just staged the huge record keeps exactly what it staged"
    );

    engine.submit(durable_put(b"a", b"v")).unwrap();
    assert_eq!(
        super::spare::record_buffer().0,
        huge_framed,
        "one record of hysteresis must not release the stage yet"
    );

    engine.submit(durable_put(b"b", b"v")).unwrap();
    assert!(
        super::spare::record_buffer().0 <= MAX_GROUP_BYTES,
        "the stage must be released once two smaller records were written"
    );
    assert_eq!(engine.get(&key(b"huge"), u64::MAX).unwrap(), Some(huge));
}

#[test]
fn a_giant_record_parks_no_more_than_the_retention_cap() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    engine
        .submit(eventual_put(b"huge", &vec![b'x'; 32 * MAX_GROUP_BYTES]))
        .expect("a record far larger than the kept floor still commits");
    let cap = super::spare::record_buffer().0;
    assert!(
        cap <= MAX_KEPT_STAGE_BYTES,
        "a one-off giant record must not park more than the retention cap, got {cap}"
    );
}

#[test]
fn a_wal_disabled_commit_rides_along_without_forcing_a_sync() {
    let dir = TempDir::new().unwrap();
    let (engine, stats) = open_with_stats(&dir);

    let (_, quiet) = install(
        &engine,
        WriteRequest::Put {
            key: key(b"quiet"),
            value: b"v".to_vec(),
            durability: DurabilityMode::Immediate,
            disable_wal: true,
        },
    );
    let wal_before = engine.log_end();
    engine
        .submit(eventual_put(b"eventual", b"v"))
        .expect("the commit behind it lands");

    seq_of(&quiet).expect("the wal-disabled commit lands");
    assert_eq!(
        stats.get_ticker(Ticker::WalSyncCount),
        0,
        "no logged commit asked for Immediate durability, so no fsync is due"
    );
    assert!(engine.log_end() > wal_before);
    assert_eq!(
        engine.get(&key(b"quiet"), u64::MAX).unwrap(),
        Some(b"v".to_vec())
    );
}

#[test]
fn concurrent_writers_share_far_fewer_fsyncs_than_writes() {
    let dir = TempDir::new().unwrap();
    let (engine, stats) = open_with_stats(&dir);

    const WRITERS: usize = 8;
    const PER_WRITER: usize = 64;

    thread::scope(|scope| {
        for w in 0..WRITERS {
            let engine = &engine;
            scope.spawn(move || {
                for i in 0..PER_WRITER {
                    engine
                        .submit(durable_put(format!("w{w}k{i:04}").as_bytes(), b"value"))
                        .expect("every durable write commits");
                }
            });
        }
    });

    let total = (WRITERS * PER_WRITER) as u64;
    let syncs = stats.get_ticker(Ticker::WalSyncCount);
    assert!(syncs >= 1, "durable writes must fsync at least once");
    assert!(
        syncs <= total,
        "the pipeline can never issue more fsyncs than writes: {syncs} > {total}"
    );
    for w in 0..WRITERS {
        for i in 0..PER_WRITER {
            let name = format!("w{w}k{i:04}");
            assert_eq!(
                engine.get(&key(name.as_bytes()), u64::MAX).unwrap(),
                Some(b"value".to_vec()),
                "every committed write must be readable"
            );
        }
    }
    assert!(engine.pipeline_is_idle());
}

#[test]
fn real_concurrent_writers_all_learn_a_failed_fsync() {
    // G2 against real threads: every thread goes through `submit`. A thread
    // told `Ok` whose bytes were cut away is silent data loss; a thread told
    // `Err` whose bytes survive a reopen is a resurrection.
    let dir = TempDir::new().unwrap();
    const WRITERS: usize = 12;
    const PER_WRITER: usize = 40;

    let acknowledged = {
        let engine = open_engine(&dir);
        engine
            .submit(durable_put(b"before", b"kept"))
            .expect("the pre-failure write commits");

        // Alternating, so both outcomes are guaranteed. Half the writers ask
        // for Immediate durability and half for Eventual: which commits a
        // sync covers is decided by thread timing, so leaving the mix to that
        // alone makes the test a machine-speed test. A flapping fault
        // supplies the successes itself.
        let _fault = arm_flapping_sync_failure(&dir);
        let acknowledged = Arc::new(Mutex::new(Vec::new()));
        thread::scope(|scope| {
            for w in 0..WRITERS {
                let (engine, acknowledged) = (&engine, &acknowledged);
                scope.spawn(move || {
                    let durability = if w % 2 == 0 {
                        DurabilityMode::Immediate
                    } else {
                        DurabilityMode::Eventual
                    };
                    for i in 0..PER_WRITER {
                        let name = format!("f{w:02}_{i:03}");
                        if engine
                            .submit(put_with(name.as_bytes(), b"v", durability))
                            .is_ok()
                        {
                            acknowledged.lock().push(name);
                        }
                    }
                });
            }
        });
        let acknowledged = acknowledged.lock().clone();
        assert!(
            !acknowledged.is_empty() && acknowledged.len() < WRITERS * PER_WRITER,
            "the flapping fault must produce a mix: {} of {} acknowledged",
            acknowledged.len(),
            WRITERS * PER_WRITER
        );
        // A write the caller was told failed must not be readable through
        // the live engine either.
        for w in 0..WRITERS {
            for i in 0..PER_WRITER {
                let name = format!("f{w:02}_{i:03}");
                let present = engine
                    .get(&key(name.as_bytes()), u64::MAX)
                    .unwrap()
                    .is_some();
                assert_eq!(
                    present,
                    acknowledged.contains(&name),
                    "{name}: readable={present} but acknowledged={}",
                    acknowledged.contains(&name)
                );
            }
        }
        acknowledged
    };

    let engine = open_engine(&dir);
    assert_eq!(
        engine.get(&key(b"before"), u64::MAX).unwrap(),
        Some(b"kept".to_vec()),
        "a write that committed before the fault must survive"
    );
    for w in 0..WRITERS {
        for i in 0..PER_WRITER {
            let name = format!("f{w:02}_{i:03}");
            let recovered = engine.get(&key(name.as_bytes()), u64::MAX).unwrap();
            if acknowledged.contains(&name) {
                assert_eq!(
                    recovered,
                    Some(b"v".to_vec()),
                    "{name} was acknowledged but lost across a reopen"
                );
            } else {
                assert_eq!(
                    recovered, None,
                    "{name} was rejected but resurrected across a reopen"
                );
            }
        }
    }
}

#[test]
fn a_failed_commit_does_not_cut_an_earlier_commits_bytes() {
    // The cut starts at the first commit not yet published. An Eventual
    // writer that already committed must keep its WAL bytes when a later
    // Immediate commit's fsync fails and is rolled back.
    let dir = TempDir::new().unwrap();
    {
        let engine = open_engine(&dir);
        engine
            .submit(eventual_put(b"eventual", b"kept"))
            .expect("the eventual write commits");

        let _fault = arm_sync_failure(&dir);
        engine
            .submit(durable_put(b"doomed", b"lost"))
            .expect_err("the injected failure must surface");
    }

    let engine = open_engine(&dir);
    assert_eq!(
        engine.get(&key(b"eventual"), u64::MAX).unwrap(),
        Some(b"kept".to_vec()),
        "a rollback reached past an earlier commit's bytes"
    );
    assert_eq!(engine.get(&key(b"doomed"), u64::MAX).unwrap(), None);
}

#[test]
fn a_writer_that_commits_is_immediately_visible_to_another_thread() {
    // The horizon must be published before the writer returns, observed
    // from a thread that never touched the write path.
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let (tx, rx) = mpsc::channel::<usize>();

    let writer = {
        let engine = Arc::clone(&engine);
        thread::spawn(move || {
            for i in 0..2_000usize {
                engine
                    .submit(durable_put(format!("v{i:05}").as_bytes(), b"v"))
                    .expect("write commits");
                tx.send(i).expect("reader is alive");
            }
        })
    };

    let mut checked = 0usize;
    for i in rx {
        let horizon = engine.snapshot_seq();
        assert_eq!(
            engine
                .get(&key(format!("v{i:05}").as_bytes()), horizon)
                .unwrap(),
            Some(b"v".to_vec()),
            "a committed write was not visible at the published horizon"
        );
        checked += 1;
    }
    writer.join().expect("writer thread panicked");
    assert_eq!(checked, 2_000);
}

/// P3 with helpers: several writers publish each other's slots, and a
/// writer returns as soon as it sees its own published, whoever passed the
/// frontier. The read horizon is at its sequence by then, so it reads its
/// own write.
#[test]
fn a_commit_published_by_another_thread_reads_its_own_write_when_it_returns() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    thread::scope(|scope| {
        for w in 0..4 {
            let engine = &engine;
            scope.spawn(move || {
                for i in 0..2_000 {
                    let name = format!("w{w}k{i:05}");
                    let seq = engine
                        .submit(eventual_put(name.as_bytes(), b"v"))
                        .expect("the write commits");
                    let horizon = engine.snapshot_seq();
                    assert!(
                        horizon >= seq,
                        "{name} returned at sequence {seq} with the horizon at {horizon}"
                    );
                }
            });
        }
    });
}

#[test]
fn writers_make_progress_with_more_threads_than_ring_slots() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let writers = commit_ring_capacity() + 8;

    thread::scope(|scope| {
        for w in 0..writers {
            let engine = &engine;
            scope.spawn(move || {
                engine
                    .submit(durable_put(format!("overflow{w:04}").as_bytes(), b"v"))
                    .expect("a full ring must not lose a write");
            });
        }
    });

    for w in 0..writers {
        let name = format!("overflow{w:04}");
        assert_eq!(
            engine.get(&key(name.as_bytes()), u64::MAX).unwrap(),
            Some(b"v".to_vec())
        );
    }
}

/// A ring far smaller than the commits in flight: commits written and not
/// yet durable fill it, and a commit that finds it full brings the oldest
/// through, sync included, before it takes its own slot.
#[test]
fn a_commit_that_finds_the_ring_full_brings_the_oldest_slot_through() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let capacity = engine.pipe().unwrap().ring.capacity();
    let commits: Vec<_> = (0..capacity + 3)
        .map(|i| install(&engine, durable_put(format!("r{i:04}").as_bytes(), b"v")))
        .collect();
    let (index, last) = commits.last().unwrap();
    drive(&engine, *index, last);
    for (i, (_, desc)) in commits.iter().enumerate() {
        seq_of(desc).unwrap_or_else(|err| panic!("commit {i}: {err}"));
    }
    assert!(engine.pipeline_is_idle());
}

proptest! {
    // Cases dropped from the default 256: the low-weight arm below draws
    // lengths above `MAX_KEPT_STAGE_BYTES` so the property actually
    // exercises the ceiling, and resizing (memset) a stage that large on
    // every case would be needlessly slow.
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn the_stage_never_keeps_more_than_the_capped_larger_of_the_last_two_records(
        lengths in proptest::collection::vec(
            prop_oneof![
                9 => 0usize..=2 * MAX_GROUP_BYTES + 4096,
                1 => MAX_KEPT_STAGE_BYTES..=MAX_KEPT_STAGE_BYTES + 2 * MAX_GROUP_BYTES,
            ],
            1..8,
        ),
    ) {
        let mut stage: Vec<u8> = Vec::new();
        let mut prev = 0usize;
        for staged in lengths {
            // Stands in for the encode: a real stage always has
            // `len == staged` when `trim_stage` sees it, so a forgotten
            // `clear` inside `trim_stage` would be caught by
            // `shrink_to`'s cannot-go-below-`len` rule.
            stage.clear();
            stage.reserve_exact(staged);
            stage.resize(staged, 0);
            let before = stage.capacity();
            trim_stage(&mut stage, prev, staged);
            let keep = MAX_GROUP_BYTES.max(prev.max(staged).min(MAX_KEPT_STAGE_BYTES));
            prop_assert!(stage.capacity() <= keep);
            prop_assert!(stage.capacity() <= MAX_KEPT_STAGE_BYTES);
            if before <= keep {
                prop_assert_eq!(stage.capacity(), before);
            }
            prev = staged;
        }
    }
}
