//! Process-crash recovery: `kill -9` at every meaningful point in the
//! write path.
//!
//! # Why this file is separate from the power-loss tests
//!
//! A process kill and a power cut are different failures and prove
//! different things. When a process is killed, every byte it handed to the
//! kernel is still in the page cache and the kernel goes on to write it
//! out; nothing is lost that the process had not already lost. A power cut
//! additionally discards everything that was never `fsync`ed. This file is
//! only about the first failure, which makes its bar the strict one:
//!
//! **a process kill must lose nothing that reached the kernel.**
//!
//! regolith hands every WAL record to the kernel before the write is
//! acknowledged, in both durability modes: `Immediate` calls `Wal::sync`
//! and `Eventual` calls `Wal::flush`, which empties the `BufWriter` with a
//! real `write` syscall (`src/engine/mod.rs:1389`). So under a process
//! kill, and unlike under a power cut, "every acknowledged write survives"
//! is the correct expectation even in the default `Eventual` mode, and
//! that is what these tests assert. A test here that tolerated losing an
//! acknowledged write would be measuring nothing.
//!
//! # Determinism
//!
//! Every kill point is byte-exact: either the workload killing itself once
//! a chosen write has returned `Ok`, or the `LD_PRELOAD` shim raising
//! `SIGKILL` on the nth matching syscall. There is no wall clock and no
//! sleep in any kill path, so a fast machine and a slow one crash at the
//! same byte. Where a background thread makes a count non-deterministic (a
//! flush or a compaction racing the foreground writer), the test asserts
//! the invariant and never the count.
//!
//! # Runtime
//!
//! Every test here is in the default run: the whole file spawns 41 child
//! processes and is measured at 0.6s with the default test parallelism
//! (0.9s serial). The only non-test function
//! is `crash_child`, which is the child entry point and not a test. Run
//! the file on its own with `just test-crash`.

//! # Linux only
//!
//! Every test here re-executes a child under the `LD_PRELOAD` fault
//! shim so the harness can record which bytes actually reached the
//! kernel. `LD_PRELOAD` interposition is a glibc mechanism, so on any
//! other target the shim cannot be built and the child cannot be
//! observed. The file is compiled out there rather than failing at
//! run time: a test that panics because the platform cannot host its
//! mechanism reports a defect that does not exist.
#![cfg(target_os = "linux")]

mod common;

use std::collections::BTreeSet;
use std::path::Path;

use common::fault::{self, ChildSpec, CrashRun, DieKind, History, Phase, Trigger};
use regolith::{Db, DurabilityMode, Options, WriteBatch};
use tempfile::TempDir;

/// Child process entry point. Returns immediately unless this process was
/// re-executed by the crash harness, so a normal `cargo test` run never
/// executes a workload here.
#[test]
fn crash_child() {
    fault::child_entrypoint(dispatch);
}

/// Phase name of the crash-loop workload, which needs keys that are unique
/// per cycle and so cannot use the shared built-in workload.
const CYCLE_PHASE: &str = "cycle";

fn dispatch(spec: &ChildSpec) {
    if spec.phase == Phase::Custom(CYCLE_PHASE.to_string()) {
        cycle_workload(spec);
    } else {
        fault::builtin_workload(spec);
    }
}

// ── helpers ─────────────────────────────────────────────────────────────

fn reopen(db_dir: &Path, opts: Options) -> Db {
    Db::open(db_dir, opts).unwrap_or_else(|e| {
        panic!(
            "the database refused to open after a process kill: {e}\n\
             A kill -9 discards no bytes that reached the kernel, so recovery \
             had everything the running process had.\ndirectory: {}",
            db_dir.display(),
        )
    })
}

/// File ids the recovered version considers live, read back from the
/// engine rather than guessed from the directory listing.
fn live_file_ids(db: &Db) -> BTreeSet<u64> {
    let prop = db
        .get_property("regolith.sstables")
        .expect("regolith.sstables is a supported property");
    prop.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _level: u64 = fields.next()?.parse().ok()?;
            fields.next()?.parse().ok()
        })
        .collect()
}

fn sst_id_of(path: &Path) -> u64 {
    path.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("{} is not an SSTable path", path.display()))
}

/// Every live file must exist on disk. A version that names a file the
/// crash never finished writing, or one a compaction already unlinked, is
/// a dangling reference and the next read of that key range fails.
///
/// The recovered database has a compaction worker, which may install a new
/// version while the directory is listed: a file it adds after the listing,
/// or one it removes after the live set was read, is no dangling reference.
/// So the live set is read before and after the listing, and only a listing
/// the version did not change across is judged: a file is removed only once
/// no version names it, so every file of a version live throughout the
/// listing is in it.
fn assert_no_dangling_files(db: &Db, db_dir: &Path, tag: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut backoff = std::time::Duration::from_millis(1);
    let (live, on_disk) = loop {
        let before = live_file_ids(db);
        let on_disk: BTreeSet<u64> = fault::find_ssts(db_dir)
            .iter()
            .map(|p| sst_id_of(p))
            .collect();
        if live_file_ids(db) == before {
            break (before, on_disk);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{tag}: the version kept changing while the table directory was listed",
        );
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(std::time::Duration::from_millis(50));
    };
    for id in live {
        assert!(
            on_disk.contains(&id),
            "{tag}: the recovered version claims file {id:06}.sst is live but it is not on disk \
             (present: {on_disk:?})",
        );
    }
}

/// Exercise the whole public surface against a recovered database.
///
/// "It reopened" is a much weaker statement than "it works". A recovery
/// that leaves a stale WAL handle, a poisoned version or an unwritable
/// memtable can still answer a point lookup, so every crash test drives a
/// put, an overwrite, a batch, a scan, a seek, a compaction and a delete
/// before it is willing to call the recovery good. The probe keys sort
/// after every workload key and are deleted again, so a later prefix
/// validation still sees only workload keys.
fn assert_functional(db: &Db, tag: &str) {
    let probe: &[u8] = b"zzz_probe_point";
    let batched: &[u8] = b"zzz_probe_batch";

    regolith::through_stalls(|| db.put(probe, b"v1"))
        .unwrap_or_else(|e| panic!("{tag}: put on a recovered database failed: {e}"));
    assert_eq!(
        db.get(probe).unwrap(),
        Some(b"v1".to_vec()),
        "{tag}: a value written after recovery could not be read back",
    );
    regolith::through_stalls(|| db.put(probe, b"v2")).unwrap();
    assert_eq!(
        db.get(probe).unwrap(),
        Some(b"v2".to_vec()),
        "{tag}: overwriting a key after recovery did not take effect",
    );

    let mut batch = WriteBatch::new();
    batch.put(batched, b"b1");
    regolith::through_stalls(|| db.write(batch.clone()))
        .unwrap_or_else(|e| panic!("{tag}: WriteBatch on a recovered database failed: {e}"));
    assert_eq!(
        db.get(batched).unwrap(),
        Some(b"b1".to_vec()),
        "{tag}: a batch written after recovery was not applied",
    );

    let scanned = db
        .scan(None, None)
        .unwrap_or_else(|e| panic!("{tag}: scan on a recovered database failed: {e}"));
    assert!(
        scanned.windows(2).all(|w| w[0].0 < w[1].0),
        "{tag}: scan returned keys out of ascending order after recovery",
    );
    assert!(
        scanned.iter().any(|(k, _)| k.as_slice() == probe),
        "{tag}: scan did not see a key written after recovery",
    );

    let mut it = db.iter();
    it.seek(probe);
    assert_eq!(
        it.key(),
        Some(probe),
        "{tag}: iterator seek missed a key written after recovery",
    );
    drop(it);

    db.compact_range(None, None)
        .wait()
        .unwrap_or_else(|e| panic!("{tag}: compaction on a recovered database failed: {e}"));
    assert_eq!(
        db.get(probe).unwrap(),
        Some(b"v2".to_vec()),
        "{tag}: compacting a recovered database lost a live value",
    );

    regolith::through_stalls(|| db.delete(probe)).unwrap();
    regolith::through_stalls(|| db.delete(batched)).unwrap();
    assert_eq!(
        db.get(probe).unwrap(),
        None,
        "{tag}: delete on a recovered database did not take effect",
    );
    assert!(
        !db.scan(None, None)
            .unwrap()
            .iter()
            .any(|(k, _)| k.as_slice() == probe),
        "{tag}: a deleted key was still returned by a scan",
    );
}

// ── kill after N writes ─────────────────────────────────────────────────

/// Proves that a `kill -9` after N acknowledged writes loses none of them,
/// for N chosen on both sides of the memtable-rotation boundary, in the
/// default `DurabilityMode::Eventual` where nothing is ever `fsync`ed.
///
/// The rotation boundary is where this could plausibly go wrong: rotation
/// freezes the memtable, opens a fresh WAL and hands the old one to the
/// background flush, so writes on either side of it live in different
/// files and different memtables at the moment of death. The larger N are
/// checked to have really crossed it (an L0 file exists) rather than
/// assumed to have.
///
/// Catches: a rotation that retires the old WAL before its data is
/// reachable elsewhere, recovery that replays only the newest WAL, and any
/// acknowledgement that happens before the WAL `write` syscall.
#[test]
fn a_process_kill_after_n_writes_keeps_every_one_of_them() {
    for n in [1usize, 15, 31, 64, 200, 800] {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("db");
        let spec = ChildSpec::new(Phase::AfterNPuts, &db)
            .ops(n)
            .value_len(96)
            .write_buffer_size(4 * 1024)
            .durability(DurabilityMode::Eventual);
        let opts = spec.options();
        let out = CrashRun::new(spec).run();
        out.assert_killed();
        assert_eq!(
            out.acked_count(),
            n,
            "the workload should have acknowledged all {n} writes before killing itself",
        );
        if n >= 64 {
            assert!(
                !fault::find_ssts(&db).is_empty(),
                "N={n} was supposed to cross a memtable rotation but no L0 file was written, \
                 so this case did not test the boundary it claims to",
            );
        }

        let tag = format!("N={n}");
        let recovered = reopen(&db, opts);
        let report = fault::assert_valid_prefix(&recovered, &out.history);
        fault::assert_acked_survived(&report, &out.acked());
        assert_eq!(
            report.k,
            n,
            "N={n}: a process kill must not discard bytes the kernel already has: {}",
            report.summary(),
        );
        assert_no_dangling_files(&recovered, &db, &tag);
        assert_functional(&recovered, &tag);
        recovered.close().unwrap();
    }
}

// ── batch atomicity ─────────────────────────────────────────────────────

/// Proves a `WriteBatch` is durable as an indivisible unit *before* it is
/// acknowledged: killing the process after the batch's WAL record has been
/// written but before the memtable apply and the return to the caller must
/// recover the whole batch, never part of it.
///
/// This is the reachable half of batch atomicity under a process kill. The
/// crash is placed on the 12th WAL `write` syscall, and the run is checked
/// to have issued exactly one syscall per batch, so the 12th batch record
/// is physically complete on disk while its caller never saw `Ok`. The
/// recovered prefix must therefore be exactly the 88 acknowledged writes
/// plus the whole 8-write batch that was in flight.
///
/// Catches: a WAL batch record that recovery applies operation by
/// operation as it decodes, so a crash could expose a partial batch; and
/// the reverse bug of acknowledging a batch before its record is written.
/// The WAL write syscall that carries record `n`, counting from 1.
///
/// `Wal::create` writes the 12-byte format stamp with its own
/// `write_all`, so the first write to a log is not a record and every
/// record is one syscall later than its ordinal. A trigger that ignores
/// that lands one record early and the probe measures the wrong crash
/// point. Matches `WAL_STAMP_LEN` in `src/engine/wal.rs`.
fn wal_write_of_record(n: u64) -> u64 {
    const STAMP_WRITES: u64 = 1;
    STAMP_WRITES + n
}

#[test]
fn a_process_kill_between_a_batch_and_its_acknowledgement_recovers_the_whole_batch() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let batch_size = 8usize;
    let spec = ChildSpec::new(Phase::AfterNPuts, &db)
        .ops(400)
        .batch_size(batch_size)
        .delete_every(0)
        .value_len(96)
        .write_buffer_size(1 << 30)
        .durability(DurabilityMode::Eventual);
    let opts = spec.options();
    let kill_on = wal_write_of_record(12) as usize;
    let out = CrashRun::new(spec)
        .trigger(Trigger::wal_write(kill_on as u64))
        .run();
    out.assert_killed();
    assert_eq!(
        out.journal.writes_to("/wal/").len(),
        kill_on,
        "the kill was meant to land on the 12th batch, WAL write {kill_on}\n{}",
        out.journal,
    );
    assert_eq!(
        out.acked_count(),
        11 * batch_size,
        "11 batches should have been acknowledged before the 12th was written",
    );

    let recovered = reopen(&db, opts);
    let report = fault::assert_valid_prefix(&recovered, &out.history);
    fault::assert_acked_survived(&report, &out.acked());
    assert!(
        out.history.batch_boundaries().contains(&report.k),
        "recovered prefix {} stops inside a batch; boundaries are {:?}",
        report.k,
        out.history.batch_boundaries(),
    );
    assert_eq!(
        report.k,
        12 * batch_size,
        "the batch whose WAL record was complete at the crash must be recovered whole: {}",
        report.summary(),
    );
    assert_functional(&recovered, "batch-before-ack");
    recovered.close().unwrap();
}

/// Proves the invariant that a crash part way through writing one WAL
/// record costs at most that record: the records written and acknowledged
/// before it must still be there.
///
/// A record larger than the WAL's 8 KiB `BufWriter` is emitted as several
/// `write` syscalls, so a process kill between two of them leaves a
/// complete prefix of the log followed by an incomplete trailing record.
/// That is the ordinary shape of every crash, not corruption: the bytes on
/// disk are exactly the bytes the process wrote. Both shapes that produce
/// it are exercised, because their blast radii are very different:
///
/// * a `WriteBatch` of 64 writes with 1 KiB values, and
/// * a single `put` with a 64 KiB value, which needs no batch at all and
///   is well inside the 64 MiB `Options::max_value_size` default.
///
/// Both run under `DurabilityMode::Immediate`, so every write before the
/// torn one returned `Ok` only after its own `fsync` returned.
///
/// Catches: a WAL reader that treats a short trailing record as a fatal
/// error instead of as the end of the log, which turns the cheapest and
/// most common failure there is into total, unrecoverable data loss.
#[test]
fn a_process_kill_part_way_through_a_wal_record_must_not_lose_the_records_before_it() {
    let tmp = TempDir::new().unwrap();

    // The trigger counts WAL *write syscalls*, and group commit means a
    // syscall is not a record: one vectored write carries a whole group,
    // and the 12-byte format stamp is a write of its own. So these
    // numbers are read off the journal rather than derived from an
    // operation count. Both land inside the record stream of the first
    // log, which is what "part way through a record" needs.
    let cases: [(&str, ChildSpec, Trigger); 2] = [
        (
            "batch of 64 writes with 1 KiB values",
            ChildSpec::new(Phase::MidWriteBatch, tmp.path().join("batch")),
            Trigger::wal_write(3),
        ),
        (
            "single put with a 64 KiB value",
            ChildSpec::new(Phase::AfterNPuts, tmp.path().join("value"))
                .ops(10)
                .batch_size(1)
                .delete_every(0)
                .value_len(64 * 1024)
                .write_buffer_size(1 << 30)
                .durability(DurabilityMode::Immediate),
            Trigger::wal_write(4),
        ),
    ];

    // Both cases run before anything is reported, so a reviewer sees every
    // shape that reproduces rather than only the first one to fail.
    let mut lost = Vec::new();
    for (label, spec, trigger) in cases {
        let db = spec.db_path.clone();
        let opts = spec.options();
        let out = CrashRun::new(spec).trigger(trigger).run();
        out.assert_killed();
        assert!(
            out.acked_count() > 0,
            "{label}: nothing was acknowledged before the crash, so there is nothing to lose",
        );
        let wal_writes = out.journal.writes_to("/wal/").len();
        assert!(
            wal_writes > 1,
            "{label}: the record was written in a single syscall, so the kill could not land \
             inside it and this case proves nothing\n{}",
            out.journal,
        );

        let recovered = match Db::open(&db, opts) {
            Ok(d) => d,
            Err(e) => {
                lost.push(format!(
                    "  case {label:?}: {acked} acknowledged and fsynced writes are \
                     unrecoverable. The crash landed between two of the {wal_writes} write \
                     syscalls of one WAL record. Engine error: {e}",
                    acked = out.acked_count(),
                ));
                continue;
            }
        };
        let report = fault::assert_valid_prefix(&recovered, &out.history);
        fault::assert_acked_survived(&report, &out.acked());
        assert!(
            out.history.batch_boundaries().contains(&report.k),
            "{label}: recovered prefix {} stops inside a batch; boundaries are {:?}",
            report.k,
            out.history.batch_boundaries(),
        );
        assert_functional(&recovered, label);
        recovered.close().unwrap();
    }

    assert!(
        lost.is_empty(),
        "the database refused to open after an ordinary process kill, so every write it had \
         already acknowledged is gone:\n{}\n\
         Nothing here is corruption: the bytes on disk are exactly the bytes the process \
         wrote, and a short trailing record is what every crash leaves behind. `Wal::replay` \
         must treat a record the file ends inside as the end of the log and return every \
         whole record before it.",
        lost.join("\n"),
    );
}

// ── the WAL-append / memtable-apply window ──────────────────────────────

/// Proves the ordering of the write path from the outside: a crash between
/// the WAL `write` syscall and the memtable apply can only ever leave
/// *more* durable data than was acknowledged, never less.
///
/// The crash is placed on the 37th WAL write syscall, after the syscall
/// returns. The run is first checked to issue exactly one syscall per
/// operation, so that syscall is operation 37's record and nothing else.
/// Its caller never reached the memtable apply, never published
/// `visible_seq` and never returned, so exactly 36 writes were
/// acknowledged while 37 must be recovered.
///
/// Catches: an acknowledgement, a `visible_seq` publication or a memtable
/// apply that happens before the WAL record reaches the kernel. Any of
/// those would make the recovered prefix shorter than the acknowledged
/// one, which is the direction that loses a caller's data.
#[test]
fn a_process_kill_between_the_wal_append_and_the_memtable_apply_can_only_gain_data() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let spec = ChildSpec::new(Phase::BetweenWalAndApply, &db);
    let opts = spec.options();
    let out = CrashRun::new(spec).run();
    out.assert_killed();
    assert_eq!(
        out.journal.writes_to("/wal/").len(),
        wal_write_of_record(36) as usize,
        "the first write to a log is the format stamp, so 36 operation records are 37 \
         writes and the fatal one is operation 36's\n{}",
        out.journal,
    );
    assert_eq!(
        out.acked_count(),
        35,
        "the operation whose WAL record was the fatal write must not have been acknowledged",
    );

    let recovered = reopen(&db, opts);
    let report = fault::assert_valid_prefix(&recovered, &out.history);
    fault::assert_acked_survived(&report, &out.acked());
    assert_eq!(
        report.k,
        out.acked_count() + 1,
        "the write whose WAL record was durable but unacknowledged must be recovered: {}",
        report.summary(),
    );
    assert_functional(&recovered, "wal-before-apply");
    recovered.close().unwrap();
}

// ── flush and compaction ────────────────────────────────────────────────

/// Proves a crash while a memtable flush is writing its L0 file loses
/// nothing and installs nothing: the data is still reachable through the
/// WAL the flush had not yet retired, and the half-written SSTable is not
/// in the recovered version.
///
/// The crash lands on the 3rd `.sst` write syscall, so the file exists on
/// disk with a partial data block and no footer. The run has no compaction
/// worker, so the writing thread runs every flush (the step a write owes
/// with no worker, or the flush a rotation runs when the one before it is
/// still pending), and three L0 files is below the default compaction
/// trigger, so exactly one thread has written SSTables at that point and
/// the third write is unambiguously the torn file. With a worker, the
/// worker and the writer can both be inside a flush when the third write
/// lands, and the journal cannot say which file was torn. A flush removes
/// the old WAL only after the `AddFile` edit is applied
/// (`Flusher::flush_oldest`), which is the ordering this test checks from
/// the outside.
///
/// Catches: a flush that publishes an SSTable into the version before its
/// bytes are complete, and a flush that retires the WAL before the file
/// that replaces it exists. Either one loses every write in that memtable.
#[test]
fn a_process_kill_during_a_flush_installs_no_truncated_sstable() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let spec = ChildSpec::new(Phase::DuringFlush, &db).workers(0);
    let opts = spec.options();
    let out = CrashRun::new(spec).run();
    out.assert_killed();

    let sst_writes = out.journal.writes_to(".sst");
    assert_eq!(
        sst_writes.len(),
        3,
        "the crash was meant to land on the 3rd SSTable write\n{}",
        out.journal,
    );
    let writers: BTreeSet<i64> = sst_writes.iter().map(|r| r.tid).collect();
    assert_eq!(
        writers.len(),
        1,
        "more than one thread wrote SSTables, so the last recorded write is not necessarily \
         the interrupted one\n{}",
        out.journal,
    );
    let fatal = sst_writes
        .last()
        .expect("the run recorded three SSTable writes")
        .path
        .clone();
    let torn = sst_id_of(&fatal);
    assert!(
        fatal.is_file() && fault::file_len(&fatal) > 0,
        "the half-written SSTable {fatal:?} should be on disk, so recovery has to decide about it",
    );

    let recovered = reopen(&db, opts);
    let report = fault::assert_valid_prefix(&recovered, &out.history);
    fault::assert_acked_survived(&report, &out.acked());
    assert!(
        !live_file_ids(&recovered).contains(&torn),
        "the truncated SSTable {torn:06}.sst was installed into the recovered version: {:?}",
        live_file_ids(&recovered),
    );
    assert_no_dangling_files(&recovered, &db, "flush");
    assert_functional(&recovered, "flush");
    recovered.close().unwrap();
}

/// Proves a crash while a compaction is writing its output keeps the
/// compaction inputs live and discards the output: every acknowledged
/// write is still readable, at least one SSTable the crash left on disk is
/// not referenced by the recovered version, and no file the version does
/// reference is missing.
///
/// The crash's files are listed before the reopen, because a writable
/// open removes the SSTables its manifest does not name.
///
/// The run is confirmed from the recorded thread ids to have had a
/// background SSTable write in flight at the crash, and confirmed from the
/// recovered version to hold a file above L0, so a compaction really did
/// run rather than a flush alone. Both a background compaction and a
/// foreground flush write SSTables here, so the interrupted file is
/// identified by the version rather than by the journal's last record,
/// which two racing writers make ambiguous. The recovered database is then
/// fully compacted, which is what forces every input the version still
/// names to be opened and read.
///
/// Catches: a compaction that applies its `RemoveFile` edits before the
/// output file is complete, which would drop the only copy of every key in
/// the inputs, and a version that survives a crash naming a file that was
/// never finished.
#[test]
fn a_process_kill_during_a_compaction_keeps_the_inputs_and_discards_the_output() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let spec = ChildSpec::new(Phase::DuringCompaction, &db);
    let opts = spec.options();
    let out = CrashRun::new(spec).run();
    out.assert_killed();

    let main_tid = out
        .journal
        .first_writer_tid()
        .expect("the run must have recorded a write");
    assert!(
        out.journal.background_sst_write_seen(main_tid),
        "no background SSTable write was recorded, so nothing was in flight to interrupt\n{}",
        out.journal,
    );

    let on_disk: BTreeSet<u64> = fault::find_ssts(&db).iter().map(|p| sst_id_of(p)).collect();
    let recovered = reopen(&db, opts.clone());
    let live = live_file_ids(&recovered);
    let discarded: Vec<u64> = on_disk.difference(&live).copied().collect();
    assert!(
        !discarded.is_empty(),
        "every SSTable the crash left on disk was installed into the recovered version, so \
         the output the crash interrupted was published rather than discarded (on disk \
         {on_disk:?})",
    );
    assert_no_dangling_files(&recovered, &db, "compaction");
    let report = fault::assert_valid_prefix(&recovered, &out.history);
    fault::assert_acked_survived(&report, &out.acked());

    let promoted = (1..7).any(|level| {
        recovered
            .get_int_property(&format!("regolith.num-files-at-level{level}"))
            .unwrap_or(0)
            > 0
    });
    assert!(
        promoted,
        "no file above L0 survived, so this run exercised flushes and not a compaction:\n{}",
        recovered.get_property("regolith.sstables").unwrap(),
    );

    assert_functional(&recovered, "compaction");
    recovered.close().unwrap();
    // The handle holds the directory lock until it is dropped, so a
    // reopen in the same scope would fail on the lock rather than on
    // anything the crash did.
    drop(recovered);

    let again = reopen(&db, opts);
    let second = fault::assert_valid_prefix(&again, &out.history);
    assert_eq!(
        second.k, report.k,
        "a full compaction and a second reopen changed the recovered state",
    );
    again.close().unwrap();
}

// ── the MANIFEST ────────────────────────────────────────────────────────

/// Proves a crash with a `VersionEdit` written but never `fsync`ed leaves
/// a version that is internally consistent: it names only files that
/// exist, it holds every acknowledged write, and replaying it a second
/// time yields the same state.
///
/// The kill lands immediately after a MANIFEST `write` syscall and before
/// anything else, which the journal confirms by the fatal record being
/// that write and by fewer MANIFEST `fsync`s than writes having completed.
///
/// A *partial* `VersionEdit` is deliberately not attempted here, because a
/// process kill cannot produce one: `VersionSet::apply` encodes a whole
/// edit batch into one buffer and flushes an empty 8 KiB `BufWriter`
/// (`src/engine/manifest.rs:377`), so every edit reaches the kernel in a
/// single `write` syscall and a crash can only land between records, never
/// inside one. Tearing a MANIFEST record needs a power cut or deliberate
/// corruption, which belong to the power-loss and corruption suites.
///
/// Catches: a version edit applied in memory before it is written,
/// recovery that stops replaying the MANIFEST early and silently drops a
/// live file, and a replay that is not idempotent.
#[test]
fn a_process_kill_on_an_unsynced_version_edit_leaves_a_consistent_version() {
    for nth in [2u64, 3, 5] {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("db");
        let spec = ChildSpec::new(Phase::DuringManifestWrite, &db);
        let opts = spec.options();
        let out = CrashRun::new(spec)
            .trigger(Trigger::manifest_write(nth))
            .run();
        out.assert_killed();

        assert_eq!(
            out.journal.writes_to("MANIFEST").len(),
            nth as usize,
            "nth={nth}: the kill landed on the wrong MANIFEST write\n{}",
            out.journal,
        );
        assert!(
            out.journal.syncs_to("MANIFEST").len() < nth as usize,
            "nth={nth}: every MANIFEST write had already been fsynced, so no unsynced edit was \
             in flight and this case proves nothing\n{}",
            out.journal,
        );

        let tag = format!("manifest nth={nth}");
        let (state, k) = {
            let recovered = reopen(&db, opts.clone());
            let report = fault::assert_valid_prefix(&recovered, &out.history);
            fault::assert_acked_survived(&report, &out.acked());
            assert_no_dangling_files(&recovered, &db, &tag);
            let state = fault::recovered_state(&recovered).expect("recovered state");
            recovered.close().unwrap();
            (state, report.k)
        };

        let again = reopen(&db, opts);
        let second = fault::assert_valid_prefix(&again, &out.history);
        assert_eq!(
            second.k, k,
            "nth={nth}: replaying the same MANIFEST a second time recovered a different prefix",
        );
        assert_eq!(
            fault::recovered_state(&again).expect("recovered state"),
            state,
            "nth={nth}: replaying the same MANIFEST a second time recovered different contents",
        );
        assert_no_dangling_files(&again, &db, &tag);
        assert_functional(&again, &tag);
        again.close().unwrap();
    }
}

// ── shutdown ────────────────────────────────────────────────────────────

/// Proves a crash inside `Db::close`, while the closing memtable flush is
/// writing its SSTable, loses nothing and installs nothing.
///
/// The write buffer is 1 GiB so no flush can happen during the workload,
/// and `close` flushes any non-empty memtable regardless of size
/// (`src/engine/mod.rs:86`), so the first `.sst` write of the whole run is
/// provably the closing one. The journal is checked to hold exactly one
/// SSTable write, so there is no ambiguity about which file was cut short.
///
/// Catches: a shutdown path that retires the WAL before the closing flush
/// is complete, and a half-written closing SSTable that recovery installs.
#[test]
fn a_process_kill_during_the_closing_flush_loses_nothing() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let spec = ChildSpec::new(Phase::CleanExit, &db)
        .ops(400)
        .value_len(96)
        .write_buffer_size(1 << 30)
        .durability(DurabilityMode::Eventual);
    let opts = spec.options();
    let out = CrashRun::new(spec).trigger(Trigger::sst_write(1)).run();
    out.assert_killed();
    assert_eq!(
        out.acked_count(),
        400,
        "the workload must have finished writing before it reached close",
    );
    let sst_writes = out.journal.writes_to(".sst");
    assert_eq!(
        sst_writes.len(),
        1,
        "with a 1 GiB write buffer the only SSTable write should be the closing flush\n{}",
        out.journal,
    );
    let fatal = sst_writes[0].path.clone();

    let recovered = reopen(&db, opts);
    let report = fault::assert_valid_prefix(&recovered, &out.history);
    fault::assert_acked_survived(&report, &out.acked());
    assert_eq!(
        report.k,
        out.history.len(),
        "a crash during shutdown must not lose a write the caller was told was applied: {}",
        report.summary(),
    );
    assert!(
        !live_file_ids(&recovered).contains(&sst_id_of(&fatal)),
        "the half-written closing SSTable was installed into the recovered version",
    );
    assert_functional(&recovered, "closing-flush");
    recovered.close().unwrap();
}

/// Proves a crash inside `Db::close` at the final WAL `fsync`, before that
/// `fsync` runs, still loses nothing: the durability of a process kill
/// comes from the `write` syscalls that already happened, not from the
/// sync that never did.
///
/// The kill is placed *before* the syscall, and the journal is checked to
/// hold no successful WAL `fsync` at all, which is only possible if the
/// process died in the closing sync: `DurabilityMode::Eventual` issues
/// none during the workload.
///
/// Catches: a close path that defers WAL data to shutdown time, and any
/// write path that leaves acknowledged records in a userspace buffer.
#[test]
fn a_process_kill_before_the_closing_wal_fsync_loses_nothing() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let spec = ChildSpec::new(Phase::CleanExit, &db)
        .ops(400)
        .value_len(96)
        .write_buffer_size(1 << 30)
        .durability(DurabilityMode::Eventual);
    let opts = spec.options();
    let out = CrashRun::new(spec)
        .trigger(Trigger::Syscall {
            kind: DieKind::Fsync,
            path_contains: "/wal/".to_string(),
            nth: 1,
            before: true,
        })
        .run();
    out.assert_killed();
    assert_eq!(
        out.acked_count(),
        400,
        "the workload must have finished writing before it reached close",
    );
    assert!(
        out.journal.syncs_to("/wal/").is_empty(),
        "a WAL fsync completed, so the process did not die in the closing sync\n{}",
        out.journal,
    );

    let recovered = reopen(&db, opts);
    let report = fault::assert_valid_prefix(&recovered, &out.history);
    fault::assert_acked_survived(&report, &out.acked());
    assert_eq!(
        report.k,
        out.history.len(),
        "every write reached the kernel before it was acknowledged, so a kill before the \
         closing fsync must lose nothing: {}",
        report.summary(),
    );
    assert_functional(&recovered, "closing-fsync");
    recovered.close().unwrap();
}

// ── repeated crash and recovery ─────────────────────────────────────────

fn cycle_entries(spec: &ChildSpec) -> Vec<(Vec<u8>, Vec<u8>)> {
    (0..spec.ops)
        .map(|i| {
            let key = format!("cycle{:04}_key{:06}", spec.seed, i).into_bytes();
            let mut value = format!("cycle{:04}_val{:06}#", spec.seed, i).into_bytes();
            value.resize(spec.value_len.max(value.len()), b'v');
            (key, value)
        })
        .collect()
}

/// Workload for the crash loop: keys carry the cycle number, so every
/// cycle writes a disjoint key range and "the data accumulated" is a
/// statement about the union rather than about overwrites.
fn cycle_workload(spec: &ChildSpec) {
    let db = Db::open(&spec.db_path, spec.options()).expect("child: open db");
    let mut acks = fault::Acks::open(&spec.ack_path);
    for (i, (key, value)) in cycle_entries(spec).into_iter().enumerate() {
        regolith::through_stalls(|| db.put(&key, &value)).expect("child: put");
        acks.ack([i as u64]);
    }
    fault::kill_self();
}

/// Proves that 24 consecutive crash-and-recover cycles accumulate data
/// monotonically: after every crash the database holds every write from
/// every earlier cycle plus every write from this one, in order, with the
/// recovered state a valid prefix of the whole cumulative history.
///
/// Each cycle opens the directory the previous crash left behind, writes a
/// disjoint range of keys, acknowledges each one and kills itself with
/// `SIGKILL`. The write buffer is 4 KiB so flushes and compactions run in
/// the background across cycles and each recovery starts from the wreckage
/// of the last. Every cycle drives the full functional workout, and every
/// fourth cycle compacts, so later cycles recover from a database whose
/// files were rewritten after an earlier crash.
///
/// Catches: recovery that is only correct the first time, a sequence
/// number that does not survive repeated recovery and lets a later write
/// be shadowed by an earlier one, a WAL retired without its replacement,
/// and any slow leak of state that only shows after several generations.
///
/// Runtime: measured at 0.66s, spawning 24 child processes, so it stays in
/// the default run.
#[test]
fn twenty_four_crash_and_reopen_cycles_accumulate_data_monotonically() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let cycles = 24u64;
    let per_cycle = 40usize;
    let mut cumulative = History::new();
    let mut previous_live = 0usize;

    for cycle in 0..cycles {
        let spec = ChildSpec::new(Phase::Custom(CYCLE_PHASE.to_string()), &db)
            .seed(cycle)
            .ops(per_cycle)
            .value_len(96)
            .write_buffer_size(4 * 1024)
            .durability(DurabilityMode::Eventual);
        let opts = spec.options();
        let entries = cycle_entries(&spec);
        let out = CrashRun::new(spec).run();
        out.assert_killed();
        assert_eq!(
            out.acked_count(),
            entries.len(),
            "cycle {cycle}: the workload should have acknowledged every write before dying",
        );
        for (key, value) in &entries {
            cumulative.put(key.clone(), value.clone());
        }

        let tag = format!("cycle {cycle}");
        let recovered = reopen(&db, opts);
        let report = fault::assert_valid_prefix(&recovered, &cumulative);
        assert_eq!(
            report.k,
            cumulative.len(),
            "cycle {cycle}: recovery number {} lost data that recovery number {} had kept: {}",
            cycle + 1,
            cycle,
            report.summary(),
        );
        assert!(
            report.live_keys >= previous_live,
            "cycle {cycle}: live key count went backwards, {} after {previous_live}",
            report.live_keys,
        );
        previous_live = report.live_keys;
        assert_no_dangling_files(&recovered, &db, &tag);
        assert_functional(&recovered, &tag);
        if cycle % 4 == 3 {
            recovered.compact_range(None, None).wait().unwrap();
            let after = fault::assert_valid_prefix(&recovered, &cumulative);
            assert_eq!(
                after.k, report.k,
                "cycle {cycle}: compacting a recovered database changed its contents",
            );
        }
        recovered.close().unwrap();
    }

    assert_eq!(
        previous_live,
        cycles as usize * per_cycle,
        "every cycle's keys should still be live after the last recovery",
    );
}
