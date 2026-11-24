//! Power-loss durability tests.
//!
//! These are the tests behind the README's "WAL crash recovery" claim.
//! They are deliberately not process-kill tests: killing a process leaves
//! every byte it wrote sitting in the OS page cache, and the kernel writes
//! those bytes out afterwards, so a `kill -9` proves the engine survives
//! losing its *memory* and nothing more. A power cut discards everything
//! that was never `fsync`ed, which is a strictly harsher failure and the
//! only one that can tell an honest durability claim from a lucky one.
//!
//! # The mechanism used here, stated so nobody over-reads the results
//!
//! Every test in this file crashes a real child process at a byte-exact
//! point and then reconstructs the directory with
//! [`common::fault::simulate_power_loss`]. That reconstruction is driven by
//! a pure-Rust `LD_PRELOAD` interposer (`tests/common/fault/preload_shim.rs`)
//! which records the child's real `write`/`pwrite`/`writev`/`fsync`/
//! `fdatasync`/`open`/`ftruncate`/`rename`/`unlink` stream with byte offsets
//! and thread ids. The reconstruction replays that stream, computes the byte
//! ranges never followed by a successful `fsync` on their file, and rewrites
//! the directory the way the filesystem would have left it. It is the ALICE
//! model driven by what regolith actually did, not by an assumption about what
//! regolith does.
//!
//! Interposition is sound for this engine because regolith performs all data
//! I/O through `std::fs` (glibc symbols); it uses `rustix` raw syscalls only
//! for `flock` and `fadvise`, which move no file data, and it has no `mmap`
//! write path.
//!
//! ## What these tests therefore DO prove
//!
//! * That a write acknowledged under `DurabilityMode::Immediate` was really
//!   on stable storage, not merely in the page cache.
//! * That the state regolith recovers is a valid prefix of the write history:
//!   some `k` writes applied in order, no gap, no half-applied `WriteBatch`,
//!   no key the workload never wrote, and an intact iteration order.
//! * That the above still holds when the cut lands inside a memtable flush,
//!   inside a compaction, inside a MANIFEST append, and inside a recovery
//!   from an earlier cut.
//!
//! ## What they do NOT prove
//!
//! * Nothing about resurrection of an `unlink`ed file whose directory was
//!   never synced. Undoing an unlink needs the file's contents, which the
//!   journal does not carry; not resurrecting is the milder outcome, so
//!   these tests are weaker than reality on that one axis.
//! * Nothing about write reordering below the granularity of a single
//!   `write` call, nor about the cache behaviour of a specific device.
//! * Nothing about a filesystem that exposes a tear these tests do not
//!   model. Two realistic tears are exercised ([`TearMode::Truncate`], the
//!   usual journalling-filesystem outcome, and [`TearMode::TornSector`]);
//!   which one a given filesystem produces changes the outcome, and
//!   [`a_write_batch_is_all_or_nothing_at_every_cut_point`] records that.
//!
//! # Determinism
//!
//! Every workload is generated from a fixed seed, every kill point is the
//! nth matching syscall counted by the shim rather than a wall-clock
//! deadline, and every byte of injected garbage comes from a seeded stream.
//! There is no `sleep` anywhere in this file. A fast machine and a slow one
//! crash at the same byte.
//!
//! # Acknowledgement bookkeeping
//!
//! The harness records the index of every write that returned `Ok` in a file
//! outside the database directory, so a power-loss reconstruction cannot
//! rewrite it. That record ends where the child died, so it may only be
//! compared against a reconstruction taken at [`CutPoint::End`]. Tests that
//! cut earlier assert the prefix property, never the acknowledgement
//! contract.
//!
//! # Runtime
//!
//! Each test's doc comment states its measured runtime. Every test spawns
//! at least one child process, and the slowest takes 0.31s, so none of them
//! is switched off: the whole file runs in well under a second
//! and stays in the default `cargo test`. `just test-power` runs it with
//! output shown, which is how the measured `Eventual` loss is read off.

//! # Linux only
//!
//! Every test here drives a child process under the `LD_PRELOAD` fault
//! shim, which is how unsynced bytes are discarded to model a power
//! cut rather than a process kill. `LD_PRELOAD` interposition is a
//! glibc mechanism, so on any other target the shim cannot be built.
//! The file is compiled out there rather than failing at run time: a
//! test that panics because the platform cannot host its mechanism
//! reports a defect that does not exist.
#![cfg(target_os = "linux")]

mod common;

use std::path::Path;
use std::time::Duration;

use common::fault::{
    self, ChildOutcome, ChildSpec, CrashRun, CutPoint, History, OpKind, Phase, PowerLossOptions,
    PowerLossReport, Recovery, TearMode, Trigger,
};
use regolith::{Db, DurabilityMode, Options};
use tempfile::TempDir;

/// Child process entry point. Returns immediately unless this process was
/// re-executed by the crash harness, so a normal `cargo test` run never
/// executes a workload here.
#[test]
fn crash_child() {
    fault::child_entrypoint(dispatch);
}

/// Every phase except [`COMPACT_ONLY`] runs the harness's built-in
/// workload; that one needs an explicit `compact_range` call, which the
/// built-in workload has no reason to make.
fn dispatch(spec: &ChildSpec) {
    match &spec.phase {
        Phase::Custom(name) if name == COMPACT_ONLY => compact_only(spec),
        _ => fault::builtin_workload(spec),
    }
}

/// Phase name for the child that opens an existing database and compacts
/// it synchronously.
const COMPACT_ONLY: &str = "compact_only";

/// Phase name for a child that only writes; the kill point is supplied by
/// the test rather than by the phase.
const WAL_KILL: &str = "wal_kill";

/// Phase name for a child that only opens a database, so the kill lands
/// inside recovery.
const OPEN_ONLY: &str = "open_only";

/// An L0 trigger no test workload can reach, which turns background
/// compaction off so a manual `compact_range` is the only compaction in the
/// run and is therefore identifiable in the recorded syscall stream.
const NO_AUTO_COMPACTION: usize = 1_000_000;

const CHILD_TIMEOUT: Duration = Duration::from_secs(180);

fn write_buffered_opts(write_buffer_size: usize) -> Options {
    Options::default().write_buffer_size(write_buffer_size)
}

/// The option set shared by the parent and the child of the compaction
/// test, so the two can never disagree about which compactions run.
fn manual_compaction_opts(write_buffer_size: usize) -> Options {
    Options::default()
        .write_buffer_size(write_buffer_size)
        .durability(DurabilityMode::Immediate)
        .l0_compaction_trigger(NO_AUTO_COMPACTION)
        .level0_slowdown_writes_trigger(NO_AUTO_COMPACTION)
        .level0_stop_writes_trigger(NO_AUTO_COMPACTION)
}

fn compact_only(spec: &ChildSpec) {
    let db = Db::open(
        &spec.db_path,
        manual_compaction_opts(spec.write_buffer_size),
    )
    .expect("child: open db");
    db.compact_range(None, None)
        .wait()
        .expect("child: compact_range");
    db.close().expect("child: close");
}

/// Run one child to its kill point, then throw away everything it never
/// made durable. Every test goes through here so the two halves of a power
/// cut, the crash and the discard, can never drift apart.
fn crash_and_cut(
    spec: ChildSpec,
    trigger: Trigger,
    cut: CutPoint,
    tear: TearMode,
) -> (ChildOutcome, PowerLossReport) {
    let out = CrashRun::new(spec)
        .trigger(trigger)
        .timeout(CHILD_TIMEOUT)
        .run();
    out.assert_killed();
    let opts = PowerLossOptions::default().tear(tear);
    let report = fault::simulate_power_loss_with(&out.spec.db_path, &out.journal, cut, &opts);
    (out, report)
}

/// Distinct `.sst` files the child wrote to, by file name.
fn sst_paths_written(journal: &fault::Journal) -> Vec<String> {
    let mut names: Vec<String> = journal
        .writes_to(".sst")
        .iter()
        .filter_map(|r| r.path.file_name().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names.dedup();
    names
}

fn sst_names_on_disk(db: &Path) -> Vec<String> {
    let mut names: Vec<String> = fault::find_ssts(db)
        .iter()
        .filter_map(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names
}

/// The recovered prefix length, failing loudly when the database refused to
/// open for a reason other than the corruption it met.
fn recovered_prefix(db: &Path, opts: Options, history: &History, context: &str) -> usize {
    match fault::recover_and_validate(db, opts, history) {
        Recovery::Recovered(r) => r.k,
        Recovery::RefusedToOpen(e) => panic!("{context}: database refused to open: {e}"),
    }
}

/// Proves the `DurabilityMode::Immediate` contract at many independent cut
/// points: a write that returned `Ok` is on stable storage, so throwing away
/// every unsynced byte cannot lose it.
///
/// Five children are crashed at five different byte-exact points in the WAL
/// write stream, and each is reconstructed at [`CutPoint::End`], which is
/// the only cut the acknowledgement record can be compared against. Each run
/// also asserts that the reconstruction actually discarded something, so a
/// pass can never mean "the power cut modelled nothing".
///
/// Catches: an `Immediate` path that returns `Ok` before the WAL `fsync`
/// completes, or that fsyncs the file but never its parent directory. A
/// `kill -9` test cannot detect either, because the page cache hides both.
/// If this test ever fails, acknowledged data is being lost on power loss
/// and that is a critical defect, not a test to be relaxed.
///
/// Runtime: measured at 0.03s; it spawns five child processes.
#[test]
fn immediate_durability_loses_no_acknowledged_write_at_any_cut_point() {
    let tmp = TempDir::new().unwrap();
    let mut summary = Vec::new();

    for nth in [3u64, 11, 29, 67, 131] {
        let db = tmp.path().join(format!("db_{nth}"));
        let spec = ChildSpec::new(Phase::Custom(WAL_KILL.into()), &db)
            .ops(200)
            .durability(DurabilityMode::Immediate);
        let (out, report) = crash_and_cut(
            spec,
            Trigger::wal_write(nth),
            CutPoint::End,
            TearMode::Truncate,
        );
        assert!(
            report.discarded_anything(),
            "cut after WAL write {nth} discarded nothing, so it modelled a process kill \
             and proved nothing about durability\n{}",
            report.summary(),
        );
        assert!(
            out.acked_count() > 0,
            "no write was acknowledged before the crash, so there is no contract to check",
        );

        match fault::recover_and_validate(&db, write_buffered_opts(1 << 20), &out.history) {
            Recovery::Recovered(r) => {
                fault::assert_acked_survived(&r, &out.acked());
                summary.push(format!(
                    "kill at WAL write {nth}: acked {}, recovered prefix {}",
                    out.acked_count(),
                    r.k,
                ));
            }
            Recovery::RefusedToOpen(e) => panic!(
                "the database refused to open after acknowledging {} writes under \
                 Immediate durability: {e}\n{}",
                out.acked_count(),
                report.summary(),
            ),
        }
    }
    println!("immediate durability:\n  {}", summary.join("\n  "));
}

/// Proves the property that actually holds under the default
/// `DurabilityMode::Eventual`, and measures what the default costs.
///
/// Eventual mode flushes the WAL's buffer on every write but never `fsync`s
/// it, so nothing a user writes is durable on its own; durability arrives
/// only when a memtable flush `fsync`s its SSTable and the MANIFEST
/// `AddFile` record that publishes it. Recent writes are therefore allowed
/// to vanish, and the run is sized so that several flushes complete before
/// each cut, making the measured loss the interesting quantity ("everything
/// since the last completed flush") rather than the trivial one
/// ("everything"). What is never allowed is a torn state: each of five
/// independent cuts must leave a valid prefix of the write history, with no
/// gap, no half-applied batch, no foreign key, and a forward scan, reverse
/// scan and point lookup that all agree.
///
/// The loss at each cut is printed rather than asserted. Asserting a
/// quantity that depends on buffer sizes would be asserting a rate; the
/// invariant is the prefix, and the number belongs in the README so users
/// know what the default costs them.
///
/// Catches: recovery that skips a torn WAL record and applies the next one
/// (a silent hole, the most damaging recovery bug an LSM can have), and any
/// cut whose surviving state is not reachable by replaying a prefix.
///
/// Runtime: measured at 0.31s; it spawns five child processes.
#[test]
fn eventual_durability_recovers_to_a_valid_prefix_and_the_loss_is_measured() {
    let tmp = TempDir::new().unwrap();
    let mut measured = Vec::new();

    // A 8 KiB write buffer holds about 55 of these writes, so every cut
    // below lands after several completed flushes.
    for nth in [113u64, 457, 911, 1373, 1777] {
        let db = tmp.path().join(format!("db_{nth}"));
        let spec = ChildSpec::new(Phase::Custom(WAL_KILL.into()), &db)
            .ops(2000)
            .value_len(128)
            .write_buffer_size(8 * 1024)
            .durability(DurabilityMode::Eventual);
        let (out, report) = crash_and_cut(
            spec,
            Trigger::wal_write(nth),
            CutPoint::End,
            TearMode::Truncate,
        );
        assert!(
            report.discarded_anything(),
            "cut after WAL write {nth} discarded nothing\n{}",
            report.summary(),
        );

        let k = recovered_prefix(
            &db,
            write_buffered_opts(8 * 1024),
            &out.history,
            &format!("eventual cut after WAL write {nth}"),
        );
        assert!(
            k <= out.history.len(),
            "recovered prefix {k} exceeds the {} writes the workload issued",
            out.history.len(),
        );
        measured.push((
            nth,
            out.acked_count(),
            k,
            out.acked_count().saturating_sub(k),
        ));
    }

    println!("eventual (default) durability, 2000 writes per run, 8 KiB write buffer:");
    for (nth, acked, k, acked_lost) in &measured {
        println!(
            "  kill at WAL write {nth}: {acked} acknowledged, {k} recovered, \
             {acked_lost} acknowledged writes lost"
        );
    }
    let worst = measured.iter().map(|m| m.3).max().unwrap_or(0);
    println!(
        "  worst case across the five cuts: {worst} acknowledged writes lost, which is \
         the work since the last completed flush and is bounded by one write buffer"
    );
}

/// Crash a fresh database inside its `nth` memtable flush and cut the
/// power, proving from the recorded stream that the cut really landed in a
/// flush rather than assuming it.
///
/// The proof: the whole run wrote to exactly `nth` SSTable files, which is
/// fewer than `l0_compaction_trigger` (4 by default), so no level compaction
/// can have started and every SSTable write in the run belongs to a flush;
/// and the fatal record is a write to the newest of those files. A rotation
/// seals the memtable and the compaction worker flushes it (E9), so the
/// writer can record a log write or two between the worker's fatal write and
/// the process's death: the fatal record is the last SSTable record, and the
/// cut lands with that SSTable written but not yet `fsync`ed and its
/// MANIFEST `AddFile` record not yet appended.
fn cut_power_inside_flush(db: &Path, nth: u64) -> (ChildOutcome, PowerLossReport) {
    let spec = ChildSpec::new(Phase::DuringFlush, db).durability(DurabilityMode::Immediate);
    let (out, report) = crash_and_cut(
        spec,
        Trigger::sst_write(nth),
        CutPoint::End,
        TearMode::Truncate,
    );

    let written = sst_paths_written(&out.journal);
    assert_eq!(
        written.len(),
        nth as usize,
        "expected the crash to land in flush {nth}, but {} SSTable files had been \
         written: {written:?}\n{}",
        written.len(),
        out.journal,
    );
    assert!(
        written.len() < 4,
        "the run produced {} SSTables, enough for a level compaction to have started, \
         so this cut can no longer be attributed to a flush\n{}",
        written.len(),
        out.journal,
    );
    let last = out
        .journal
        .records
        .iter()
        .rev()
        .find(|r| r.path.to_string_lossy().ends_with(".sst"))
        .expect("journal must contain the fatal operation");
    let newest = written.last().expect("written is non-empty");
    assert!(
        last.path.to_string_lossy().ends_with(newest) && last.kind == OpKind::Write,
        "the crash landed on {:?} ({:?}) rather than inside the flush's SSTable write\n{}",
        last.path,
        last.kind,
        out.journal,
    );
    assert!(
        !out.journal
            .records
            .iter()
            .any(|r| r.seq > last.seq && r.path.to_string_lossy().ends_with("MANIFEST")),
        "the manifest moved after the fatal SSTable write\n{}",
        out.journal,
    );
    assert!(
        out.acked_count() > 0,
        "no write was acknowledged before the flush crashed",
    );
    (out, report)
}

/// Proves a power cut landing inside a memtable flush cannot lose an
/// acknowledged write, once the MANIFEST already holds a durable record.
///
/// The cut is placed in the third flush, so two `AddFile` edits have already
/// been `fsync`ed into the MANIFEST (`VersionEdit::requires_manifest_sync`
/// forces a sync for `AddFile`). What the cut discards is the third
/// SSTable's unsynced bytes and the MANIFEST tail written after the last
/// sync. Every write acknowledged under `Immediate` durability must still be
/// there, because all of them are in the `fsync`ed WAL.
///
/// Catches: a flush that lets the WAL holding the same data be dropped,
/// rotated past or trimmed before the SSTable and its MANIFEST record are
/// durable. That is the classic LSM data-loss window, and it is invisible to
/// a process-kill test.
///
/// Runtime: measured at 0.03s; it spawns one child process.
#[test]
fn a_power_cut_during_a_memtable_flush_keeps_every_acknowledged_write() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let (out, report) = cut_power_inside_flush(&db, 3);

    match fault::recover_and_validate(&db, write_buffered_opts(8 * 1024), &out.history) {
        Recovery::Recovered(r) => fault::assert_acked_survived(&r, &out.acked()),
        Recovery::RefusedToOpen(e) => panic!(
            "a power cut inside a flush left a database that refuses to open, after {} \
             acknowledged writes: {e}\n{}",
            out.acked_count(),
            report.summary(),
        ),
    }
}

/// Proves a power cut inside the *first* memtable flush of a newly created
/// database cannot lose an acknowledged write, which is the one window the
/// sibling third-flush test cannot reach.
///
/// The shape of that window, all of it observed rather than assumed:
///
/// 1. `VersionSet::open` creates the MANIFEST, `fsync`s it while it is still
///    zero bytes, and appends `VersionEdit`s afterwards. Only edits for
///    which `requires_manifest_sync` holds (`AddFile`, `RemoveFile`,
///    `SetLastSeq`) `fsync` the file, and the first flush is cut before its
///    `AddFile`. So the MANIFEST's durable length is 0.
/// 2. The flush has created and written `sst/NNNNNN.sst` but not yet
///    `fsync`ed it, so the power cut leaves that file present and empty.
///    This is the classic ext4 outcome: the directory entry reaches the
///    journal on a periodic commit while the delayed-allocated data blocks
///    do not.
/// 3. `VersionSet::reject_discarded_tables` sees a MANIFEST that references
///    no SSTable while a `.sst` file exists on disk.
///
/// The guard is right in general (`tests/open_and_corruption.rs`
/// deliberately locks it in: a wiped MANIFEST next to real tables must
/// still refuse) and it stays right here, because it now counts tables that
/// could hold data rather than files. A zero-length orphan, or one whose
/// footer records no entry and no range tombstone, is logged and skipped;
/// anything else, including a table whose footer will not parse, still
/// refuses the open. Every acknowledged write is intact in the `fsync`ed
/// WAL, so once the orphan is dismissed the recovery finds all of them.
///
/// Catches a regression in either direction: a guard that counts files
/// again and loses the WAL's writes, or one relaxed far enough to open on a
/// real unreferenced table and discard it.
///
/// Runtime: measured at 0.03s; it spawns one child process.
#[test]
fn a_power_cut_during_the_first_memtable_flush_keeps_every_acknowledged_write() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let (out, report) = cut_power_inside_flush(&db, 1);

    match fault::recover_and_validate(&db, write_buffered_opts(8 * 1024), &out.history) {
        Recovery::Recovered(r) => fault::assert_acked_survived(&r, &out.acked()),
        Recovery::RefusedToOpen(e) => {
            let salvage = tmp.path().join("salvage");
            fault::copy_tree(&db, &salvage);
            let mut removed = Vec::new();
            for sst in fault::find_ssts(&salvage) {
                if fault::file_len(&sst) == 0 {
                    std::fs::remove_file(&sst).expect("salvage: remove empty SSTable");
                    removed.push(sst);
                }
            }
            let salvaged =
                fault::recover_and_validate(&salvage, write_buffered_opts(8 * 1024), &out.history);
            panic!(
                "{} write(s) were acknowledged under Immediate durability and then lost to \
                 a power cut inside the first flush: the database refuses to open.\n\
                 error: {e}\n{}\n\
                 The data itself is durable: after removing {} empty orphan SSTable(s) \
                 {removed:?} the same directory opens and recovers {} write(s). The open \
                 guard in VersionSet::reject_discarded_tables, not the WAL, is what lost \
                 them.",
                out.acked_count(),
                report.summary(),
                removed.len(),
                salvaged.k(),
            )
        }
    }
}

/// Proves a power cut landing inside a compaction cannot lose data the
/// compaction was merging.
///
/// The setup is what makes the claim rigorous rather than probabilistic. The
/// parent writes every key itself with background compaction switched off,
/// closes cleanly, and records the exact set of SSTable files on disk. The
/// child then opens that database and calls `compact_range`, which is
/// synchronous, so the only new SSTable in the child's recorded stream is
/// the compaction's output. The test asserts exactly that before drawing any
/// conclusion: one new `.sst` file, absent from the pre-crash listing.
///
/// Every write was acknowledged under `Immediate` durability before the
/// child ever started, so the whole history must survive; a compaction is a
/// rearrangement and is never allowed to lose a byte of it.
///
/// Catches: a compaction that installs its output into the MANIFEST before
/// the output file is durable, that deletes or stops consulting its input
/// files before the output is durable, or a recovery that trusts a
/// half-written orphan SSTable left behind by the cut.
///
/// Runtime: measured at 0.06s; it spawns one child process after writing
/// 1200 keys in-process.
#[test]
fn a_power_cut_during_a_compaction_keeps_every_write_it_was_merging() {
    const KEYS: usize = 1200;
    const STRIDE: usize = 7919;
    const WRITE_BUFFER: usize = 8 * 1024;

    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");

    let mut history = History::new();
    {
        let d = Db::open(&db, manual_compaction_opts(WRITE_BUFFER)).expect("open for fill");
        for i in 0..KEYS {
            let key = format!("key_{:06}", (i * STRIDE) % KEYS).into_bytes();
            let value = fault::garbage(i as u64, 128);
            regolith::through_stalls(|| d.put(&key, &value)).expect("fill put");
            history.put(key, value);
        }
        d.close().expect("close after fill");
    }
    let before = sst_names_on_disk(&db);
    assert!(
        before.len() > 4,
        "the fill should leave several L0 files to compact, got {before:?}",
    );

    let spec = ChildSpec::new(Phase::Custom(COMPACT_ONLY.into()), &db)
        .ops(0)
        .write_buffer_size(WRITE_BUFFER)
        .durability(DurabilityMode::Immediate);
    let (out, report) = crash_and_cut(
        spec,
        Trigger::sst_write(2),
        CutPoint::End,
        TearMode::Truncate,
    );

    let written = sst_paths_written(&out.journal);
    assert_eq!(
        written.len(),
        1,
        "expected the compaction to be the only SSTable writer in the child, got {written:?}\n{}",
        out.journal,
    );
    assert!(
        !before.contains(&written[0]),
        "{} already existed before the compaction, so the crash did not land on a \
         compaction output\n{}",
        written[0],
        out.journal,
    );

    let k = recovered_prefix(
        &db,
        manual_compaction_opts(WRITE_BUFFER),
        &history,
        "power cut inside a compaction",
    );
    assert_eq!(
        k,
        history.len(),
        "every one of the {} writes was acknowledged under Immediate durability and \
         flushed before the compaction started, so a cut inside the compaction must lose \
         none of them\n{}",
        history.len(),
        report.summary(),
    );
}

/// Proves a power cut landing inside a MANIFEST append cannot lose an
/// acknowledged write.
///
/// The crash is placed on the second `VersionEdit` write, which is the
/// record that publishes the first flushed SSTable. regolith writes and flushes
/// `VersionEdit`s but only fsyncs the MANIFEST when the edit requires it
/// (`src/engine/manifest.rs`), so the reconstruction genuinely discards a
/// MANIFEST tail here; the test asserts that it did, rather than assuming
/// it. What must survive is the data, which is still in the WAL: a lost
/// MANIFEST record may orphan an SSTable, but it may never orphan a write
/// that returned `Ok`.
///
/// Catches: a flush that trims or drops the WAL on the strength of a
/// MANIFEST record that was never made durable, and a recovery that cannot
/// open a database whose MANIFEST ends mid-record.
///
/// Runtime: measured at 0.02s; it spawns one child process.
#[test]
fn a_power_cut_during_a_manifest_append_keeps_every_acknowledged_write() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let spec =
        ChildSpec::new(Phase::DuringManifestWrite, &db).durability(DurabilityMode::Immediate);
    // The first write to a MANIFEST is its 12-byte REGOMAN stamp, not a
    // record, so a `VersionEdit` append is one write later than its
    // ordinal. Triggering on write 2 lands on the stamp's successor
    // before anything is appended, and the cut then has no unsynced
    // record to discard. Matches `MANIFEST_STAMP_LEN`.
    const MANIFEST_STAMP_WRITES: u64 = 1;
    let (out, report) = crash_and_cut(
        spec,
        Trigger::manifest_write(MANIFEST_STAMP_WRITES + 2),
        CutPoint::End,
        TearMode::Truncate,
    );

    let last = out
        .journal
        .records
        .last()
        .expect("journal must contain the fatal operation");
    assert!(
        last.path.to_string_lossy().contains("MANIFEST"),
        "the crash landed on {:?} rather than a MANIFEST append\n{}",
        last.path,
        out.journal,
    );
    let manifest_touched = report
        .truncated
        .iter()
        .any(|(p, _, _)| p.to_string_lossy().contains("MANIFEST"))
        || report
            .torn
            .iter()
            .any(|(p, _, _)| p.to_string_lossy().contains("MANIFEST"));
    assert!(
        manifest_touched,
        "the cut discarded no MANIFEST bytes, so the half-written VersionEdit survived \
         and this run does not test what it claims\n{}",
        report.summary(),
    );
    assert!(
        out.acked_count() > 0,
        "no write was acknowledged before the MANIFEST append crashed",
    );

    match fault::recover_and_validate(&db, write_buffered_opts(8 * 1024), &out.history) {
        Recovery::Recovered(r) => fault::assert_acked_survived(&r, &out.acked()),
        Recovery::RefusedToOpen(e) => panic!(
            "a power cut inside a MANIFEST append left a database that refuses to open, \
             after {} acknowledged writes: {e}\n{}",
            out.acked_count(),
            report.summary(),
        ),
    }
}

/// Proves recovery is re-entrant: a power cut *during* the recovery from an
/// earlier power cut may not lose anything the first recovery would have
/// kept.
///
/// regolith's open path replays the surviving WALs into a memtable, creates a
/// new WAL, rewrites the recovered memtable into it, fsyncs it and only then
/// removes the old WALs (`src/engine/mod.rs`). The second cut is placed on
/// the first write of that rewrite, which is the window in which two copies
/// of the same data exist and one of them is not durable. A reference copy
/// of the directory is taken before the second child runs, recovered
/// separately, and its prefix length is the yardstick: the twice-cut
/// database must recover to exactly the same length, and a third reopen must
/// not move it again.
///
/// Catches: a recovery that removes or truncates the source WAL before its
/// replacement is durable, one that is not idempotent across repeated
/// replays, and one whose second pass drops records the first pass kept.
/// This is where real engines break.
///
/// Runtime: measured at 0.03s; it spawns two child processes.
#[test]
fn a_power_cut_during_recovery_from_an_earlier_cut_loses_nothing_more() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let reference = tmp.path().join("reference");

    let spec = ChildSpec::new(Phase::Custom(WAL_KILL.into()), &db)
        .ops(300)
        .durability(DurabilityMode::Immediate);
    let (first, first_report) = crash_and_cut(
        spec,
        Trigger::wal_write(200),
        CutPoint::End,
        TearMode::Truncate,
    );
    assert!(
        first_report.discarded_anything(),
        "the first cut discarded nothing\n{}",
        first_report.summary(),
    );
    let acked = first.acked();
    assert!(!acked.is_empty(), "the first run acknowledged no write");

    // Recover a copy, so the yardstick is measured without disturbing the
    // directory the second crash has to start from.
    fault::copy_tree(&db, &reference);
    let expected = recovered_prefix(
        &reference,
        write_buffered_opts(1 << 20),
        &first.history,
        "recovery of the reference copy",
    );

    let interrupted = ChildSpec::new(Phase::Custom(OPEN_ONLY.into()), &db)
        .ops(0)
        .durability(DurabilityMode::Immediate);
    let (second, second_report) = crash_and_cut(
        interrupted,
        Trigger::wal_write(1),
        CutPoint::End,
        TearMode::Truncate,
    );
    assert!(
        second.journal.writes_to("/wal/").len() == 1,
        "the second crash was meant to land on the first WAL write of the recovery, but \
         {} WAL writes were recorded\n{}",
        second.journal.writes_to("/wal/").len(),
        second.journal,
    );

    let after = recovered_prefix(
        &db,
        write_buffered_opts(1 << 20),
        &first.history,
        "recovery after a cut inside a recovery",
    );
    assert_eq!(
        after,
        expected,
        "an interrupted recovery changed what the database recovers: {expected} writes \
         before, {after} after\n{}",
        second_report.summary(),
    );

    let again = recovered_prefix(
        &db,
        write_buffered_opts(1 << 20),
        &first.history,
        "second reopen after an interrupted recovery",
    );
    assert_eq!(
        again, after,
        "recovery is not idempotent: reopening the same directory moved the recovered \
         prefix from {after} to {again}",
    );
    match fault::recover_and_validate(&db, write_buffered_opts(1 << 20), &first.history) {
        Recovery::Recovered(r) => fault::assert_acked_survived(&r, &acked),
        Recovery::RefusedToOpen(e) => panic!("final reopen refused: {e}"),
    }
}

/// Proves a `WriteBatch` is atomic across a power cut, at four independent
/// cut points and under two realistic tears.
///
/// A batch is atomic by contract, so a recovered state holding some of one
/// batch's writes and not the rest is a bug in every durability mode. The
/// child is killed physically inside a single batch's WAL record: the batch
/// is far larger than the WAL's 8 KiB `BufWriter`, so the kill lands between
/// two write syscalls of one record rather than tidily between records, and
/// `Immediate` durability makes the batches before it genuinely durable.
/// Each cut is taken on its own fresh child, because a reconstruction is
/// driven by the recorded stream of the run it belongs to.
///
/// The assertion is the invariant that holds in every mode: the recovered
/// prefix stops on a batch boundary, or the database refuses to open and
/// names the corruption. Opening and serving half a batch is never allowed.
/// The acknowledgement contract is only checked at [`CutPoint::End`], the
/// only cut the acknowledgement record lines up with, and a refusal is
/// rejected outright for [`TearMode::Truncate`], the common filesystem
/// outcome, which must always recover.
///
/// The per-cut outcome is printed, because it records what a power cut
/// actually costs. Replay treats a trailing record the file ends inside as
/// the end of the log and keeps every whole record before it, so both
/// tears recover. Measured consequence, from this test at `CutPoint::End`:
/// `Truncate` and `TornSector` each come back with all 192 acknowledged
/// writes. What still fails closed is damage the record checksum can
/// prove, which
/// `tests/corruption.rs::torn_wal_tail_checksum_flip_fails_open_and_keeps_wal`
/// locks in: a *whole* record whose bytes are wrong cannot be proven to be
/// anything, so open refuses rather than serve it. A refusal therefore
/// stays a legal outcome for a tear that rewrites bytes, and is rejected
/// outright only for `TearMode::Truncate`.
///
/// Catches: a WAL batch record applied operation by operation as it decodes
/// rather than validated whole first, and a `visible_seq` published before
/// the whole batch is applied.
///
/// Runtime: measured at 0.11s; it spawns five child processes (one probe run
/// and one per cut point).
#[test]
fn a_write_batch_is_all_or_nothing_at_every_cut_point() {
    let tmp = TempDir::new().unwrap();

    // The sync count of this workload is fixed by its seed, so the two
    // mid-run cuts land in the same place on every machine.
    let probe = CrashRun::new(ChildSpec::new(
        Phase::MidWriteBatch,
        tmp.path().join("probe"),
    ))
    .timeout(CHILD_TIMEOUT)
    .run();
    probe.assert_killed();
    let syncs = probe.journal.sync_seqs().len();
    assert!(
        syncs >= 4,
        "Immediate durability should have fsynced several times before the crash, saw {syncs}",
    );

    let cuts = [
        (CutPoint::End, TearMode::Truncate),
        (CutPoint::End, TearMode::TornSector),
        (CutPoint::AfterNthSync(syncs / 2), TearMode::Truncate),
        (CutPoint::BeforeNthSync(syncs / 2), TearMode::Truncate),
    ];

    let mut outcomes = Vec::new();
    for (i, (cut, tear)) in cuts.into_iter().enumerate() {
        let db = tmp.path().join(format!("db_{i}"));
        let spec = ChildSpec::new(Phase::MidWriteBatch, &db);
        // The trigger counts WAL write syscalls, and group commit means
        // a syscall is not a record: one vectored write carries a whole
        // group, so this phase produces a handful of writes rather than
        // one per operation. Read off the journal, not derived from an
        // operation count; a number past the end never fires and the
        // probe silently proves nothing.
        let (out, report) = crash_and_cut(spec, Trigger::wal_write(3), cut, tear);
        assert!(
            report.discarded_anything(),
            "{cut:?}/{tear:?}: the in-flight batch was not disturbed at all\n{}",
            report.summary(),
        );

        let boundaries = out.history.batch_boundaries();
        match fault::recover_and_validate(&db, write_buffered_opts(1 << 20), &out.history) {
            Recovery::Recovered(r) => {
                outcomes.push(if cut == CutPoint::End {
                    format!(
                        "  {cut:?}/{tear:?}: opened, recovered {} of the {} acknowledged \
                         writes",
                        r.k,
                        out.acked_count(),
                    )
                } else {
                    format!(
                        "  {cut:?}/{tear:?}: opened, recovered {} writes; this cut predates \
                         the crash, so the acknowledgement contract does not reach it",
                        r.k,
                    )
                });
                assert!(
                    boundaries.contains(&r.k),
                    "{cut:?}/{tear:?}: recovered prefix {} stops inside a batch; the \
                     boundaries are {boundaries:?}",
                    r.k,
                );
                if cut == CutPoint::End {
                    fault::assert_acked_survived(&r, &out.acked());
                }
            }
            Recovery::RefusedToOpen(e) => {
                outcomes.push(format!(
                    "  {cut:?}/{tear:?}: refused to open; the {} acknowledged writes need \
                     manual repair ({e})",
                    out.acked_count(),
                ));
                assert!(
                    e.to_lowercase().contains("corrupt"),
                    "{cut:?}/{tear:?}: refusing to open is allowed, but the reason must \
                     name the corruption; got: {e}",
                );
                assert_ne!(
                    tear,
                    TearMode::Truncate,
                    "a merely truncated tail is the common filesystem outcome and must \
                     still recover, but the database refused to open after acknowledging \
                     {} writes: {e}",
                    out.acked_count(),
                );
            }
        }
    }
    println!(
        "WriteBatch atomicity under Immediate durability, 4 batches of 64 writes:\n{}",
        outcomes.join("\n"),
    );
}
