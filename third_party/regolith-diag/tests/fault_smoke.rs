//! Smoke tests for the fault-injection substrate itself.
//!
//! Every other durability, crash and corruption test rests on this
//! machinery, so the machinery gets its own tests. A harness that silently
//! does nothing would make every test built on it a false green, so most
//! of these assert that the fault actually fired, not merely that nothing
//! blew up.
//!
//! Runtime: the whole file finishes in a few seconds. Run it on its own
//! with `just test-fault`.

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

#[path = "fault_smoke/cut_ack.rs"]
mod cut_ack;
#[path = "fault_smoke/cut_record.rs"]
mod cut_record;
#[path = "fault_smoke/open_variadic.rs"]
mod open_variadic;
#[path = "fault_smoke/rename_pairs.rs"]
mod rename_pairs;

use std::path::Path;
use std::time::Duration;

use common::fault::{
    self, ChildSpec, CrashRun, CutPoint, History, OpValue, Phase, PowerLossOptions, TearMode,
    Trigger,
};
use regolith::{Db, DurabilityMode, Options};
use tempfile::TempDir;

/// Child process entry point. Returns immediately unless this process was
/// re-executed by the crash harness, so a normal `cargo test` run never
/// executes a workload here.
#[test]
fn crash_child() {
    fault::child_entrypoint(fault::builtin_workload);
}

fn small_opts() -> Options {
    Options::default().write_buffer_size(1 << 20)
}

fn reopen(dir: &Path) -> Db {
    Db::open(dir, small_opts()).expect("reopen after crash")
}

/// Proves the `LD_PRELOAD` interposer compiles here and actually observes
/// regolith's I/O: it must record writes to the WAL, writes to an SSTable, and
/// at least one `fsync`.
///
/// Catches: a shim that loads but interposes nothing (wrong symbol names,
/// a `rustix` raw-syscall path that bypasses glibc, or a future regolith
/// change to `mmap` writes). Without this, every power-loss test would
/// reconstruct from an empty journal and pass vacuously.
#[test]
fn the_shim_records_regoliths_real_file_io() {
    assert!(
        fault::shim::available(),
        "fault shim did not build: {:?}",
        fault::shim::build().err(),
    );
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let spec = ChildSpec::new(Phase::CleanExit, &db)
        .ops(3000)
        .write_buffer_size(8 * 1024);
    let out = CrashRun::new(spec).run();
    out.assert_clean();

    assert!(
        !out.journal.is_empty(),
        "shim recorded nothing; stderr:\n{}",
        out.stderr,
    );
    assert_eq!(out.journal.malformed, 0, "journal had unparseable lines");
    assert!(
        !out.journal.writes_to("/wal/").is_empty(),
        "no WAL writes recorded\n{}",
        out.journal,
    );
    assert!(
        !out.journal.writes_to(".sst").is_empty(),
        "no SSTable writes recorded; the workload never flushed\n{}",
        out.journal,
    );
    assert!(
        !out.journal.sync_seqs().is_empty(),
        "no fsync recorded, so nothing could ever be called durable\n{}",
        out.journal,
    );
}

/// Proves builders racing on a cold cache each get a working shim: every
/// path a builder returns exports every interposer, however their source
/// writes, compiles and installs interleave.
///
/// Catches: builders sharing one source path, where one builder's
/// truncating write lands while another's `rustc` reads it, so an empty
/// source compiles into a library that interposes nothing. Such a library
/// once ran a crash test whose fault never fired and whose journal stayed
/// empty. Builders arrive spread over the time `rustc` takes to start and
/// read its source, as test threads reaching their first crash run do.
#[test]
fn builders_racing_on_a_cold_cache_each_get_a_working_shim() {
    const BUILDERS: usize = 8;
    for round in 0..6u64 {
        let cold = TempDir::new().unwrap();
        let start = std::sync::Barrier::new(BUILDERS);
        std::thread::scope(|scope| {
            let builders: Vec<_> = (0..BUILDERS as u64)
                .map(|b| {
                    let (cold, start) = (cold.path(), &start);
                    scope.spawn(move || {
                        start.wait();
                        std::thread::sleep(Duration::from_millis((b * 37 + round * 11) % 150));
                        fault::shim::build_in(cold)
                    })
                })
                .collect();
            for builder in builders {
                let lib = builder.join().unwrap().expect("a racing builder failed");
                assert_eq!(
                    fault::shim::missing_interposer(&lib).unwrap(),
                    None,
                    "round {round}: a racing builder returned a shim that interposes nothing"
                );
            }
        });
        // Each builder removed its own source; only the installed object
        // remains.
        let left: Vec<_> = std::fs::read_dir(cold.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left.len(), 1, "builders left files behind: {left:?}");
    }
}

/// Proves a cached shim that interposes nothing is rebuilt rather than
/// used, and that the check tells an export from an import.
///
/// Catches: a cache that trusts any file at the shim's path, so one bad
/// install poisons every later run until someone deletes it by hand.
#[test]
fn a_cached_shim_that_interposes_nothing_is_rebuilt() {
    let dir = TempDir::new().unwrap();
    let lib = fault::shim::build_in(dir.path()).unwrap();
    // A valid shared object that defines none of the interposers but, like
    // any Rust library, imports `write` and `close` from libc.
    let empty = dir.path().join("empty.rs");
    std::fs::write(&empty, "#[no_mangle]\npub extern \"C\" fn unrelated() {}\n").unwrap();
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let status = std::process::Command::new(rustc)
        .args(["--edition", "2021", "--crate-type", "cdylib", "-o"])
        .arg(&lib)
        .arg(&empty)
        .status()
        .unwrap();
    assert!(status.success(), "setup: the empty library compiles");
    assert_eq!(
        fault::shim::missing_interposer(&lib).unwrap(),
        Some(fault::shim::INTERPOSERS[0]),
        "an object that only imports `write` passed as exporting it"
    );
    let rebuilt = fault::shim::build_in(dir.path()).unwrap();
    assert_eq!(rebuilt, lib);
    assert_eq!(
        fault::shim::missing_interposer(&rebuilt).unwrap(),
        None,
        "the cached library that interposes nothing was used as is"
    );
}

/// Proves the cached shim belongs to the compiler that built it: another
/// compiler names another object, and the one this run builds carries the
/// key of the compiler this run uses.
///
/// Catches: a cache keyed on the shim's source alone. A shim built under
/// one toolchain was then preloaded into a test binary built by another,
/// fired no fault and recorded nothing, and every power-loss test failed
/// until someone deleted the cache by hand.
#[test]
fn the_cached_shim_is_keyed_on_the_compiler_that_builds_it() {
    let stable = "rustc 1.99.0 (b940084d7 2026-09-28)\nhost: x86_64-unknown-linux-gnu\n";
    let nightly = "rustc 1.99.0-nightly (c79e8f894 2026-08-04)\nhost: x86_64-unknown-linux-gnu\n";
    assert_eq!(
        fault::shim::cache_key(stable),
        fault::shim::cache_key(stable)
    );
    assert_ne!(
        fault::shim::cache_key(stable),
        fault::shim::cache_key(nightly),
        "two compilers would share one cached shim"
    );

    let identity = fault::shim::compiler_identity().unwrap();
    assert!(identity.contains("rustc"), "{identity}");
    let dir = TempDir::new().unwrap();
    let lib = fault::shim::build_in(dir.path()).unwrap();
    let name = lib.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.contains(&format!("{:016x}", fault::shim::cache_key(&identity))),
        "{name} is not named after this run's compiler"
    );
    // A shim left there by another compiler is not the one this run uses.
    let other = dir.path().join(format!(
        "libregolith_fault_shim_{:016x}.so",
        fault::shim::cache_key("some other compiler")
    ));
    std::fs::write(&other, b"not this compiler's shim").unwrap();
    assert_eq!(fault::shim::build_in(dir.path()).unwrap(), lib);
}

/// Proves a kill point is byte-exact rather than wall-clock: asking to die
/// on the second MANIFEST write must kill the child by signal, with the
/// last recorded operation being that MANIFEST write.
///
/// Catches: a trigger that fires late, fires on the wrong file, or does
/// not fire at all and lets the child exit cleanly.
#[test]
fn a_kill_point_lands_on_the_requested_syscall() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    // This probe checks that the harness dies *where it was asked to*,
    // so it names the write rather than inheriting the phase default:
    // a default that moves would otherwise look like the harness
    // missing its mark.
    const NTH_MANIFEST_WRITE: u64 = 2;
    let spec = ChildSpec::new(Phase::DuringManifestWrite, &db);
    let out = CrashRun::new(spec)
        .trigger(Trigger::manifest_write(NTH_MANIFEST_WRITE))
        .run();

    out.assert_killed();
    assert_eq!(out.signal, Some(9), "expected SIGKILL");
    let last = out
        .journal
        .records
        .last()
        .expect("journal must contain the fatal operation");
    assert!(
        last.path.to_string_lossy().contains("MANIFEST"),
        "died on {:?} instead of a MANIFEST write\n{}",
        last.path,
        out.journal,
    );
    assert_eq!(
        out.journal.writes_to("MANIFEST").len(),
        NTH_MANIFEST_WRITE as usize,
        "expected to die on MANIFEST write {NTH_MANIFEST_WRITE}\n{}",
        out.journal,
    );
}

/// The load-bearing test for the whole module: power-loss simulation must
/// discard bytes that a process kill leaves on disk. Runs the identical
/// workload twice, kills both children at the same byte, and compares the
/// directory a kill leaves against the directory a power cut leaves.
///
/// Catches: a "power loss" implementation that is really just `kill -9`
/// with extra steps. If this test ever reports zero discarded bytes, every
/// durability claim built on this harness is void.
#[test]
fn power_loss_discards_bytes_that_a_process_kill_keeps() {
    let tmp = TempDir::new().unwrap();
    let killed = tmp.path().join("killed");
    let cut = tmp.path().join("cut");

    let mut sizes = Vec::new();
    for db in [&killed, &cut] {
        let spec = ChildSpec::new(Phase::AfterNPuts, db)
            .ops(600)
            .durability(DurabilityMode::Eventual);
        let out = CrashRun::new(spec).run();
        out.assert_killed();
        sizes.push(fault::file_len(&fault::newest_wal(db)));
    }
    assert_eq!(
        sizes[0], sizes[1],
        "the two identical runs must leave identical bytes on disk",
    );

    let report = fault::simulate_power_loss(&cut, CutPoint::End);
    assert!(
        report.discarded_anything(),
        "power-loss simulation discarded nothing, so it modelled a process kill \
         and nothing more.\n{}",
        report.summary(),
    );

    let after_kill = fault::file_len(&fault::newest_wal(&killed));
    let after_cut = fault::file_len(&fault::newest_wal(&cut));
    assert!(
        after_cut < after_kill,
        "WAL after a power cut ({after_cut}) must be shorter than after a kill ({after_kill})\n{}",
        report.summary(),
    );
}

/// Proves the `DurabilityMode::Immediate` contract end to end: every write
/// that returned `Ok` is still there after the unsynced bytes are thrown
/// away.
///
/// Catches: an `Immediate` path that acknowledges a write before the WAL
/// `fsync` returns, which a `kill -9` test could never detect because the
/// page cache would hide it.
#[test]
fn immediate_durability_survives_a_power_cut() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let spec = ChildSpec::new(Phase::AfterNPuts, &db)
        .ops(150)
        .durability(DurabilityMode::Immediate);
    let out = CrashRun::new(spec).run();
    out.assert_killed();
    assert_eq!(
        out.acked_count(),
        150,
        "the workload should have acknowledged every write before killing itself",
    );

    fault::simulate_power_loss(&db, CutPoint::End);

    let recovered = reopen(&db);
    let report = fault::assert_valid_prefix(&recovered, &out.history);
    fault::assert_acked_survived(&report, &out.acked());
    assert_eq!(
        report.k,
        out.history.len(),
        "Immediate durability must lose nothing: {}",
        report.summary(),
    );
}

/// Proves the property that actually holds under the default
/// `DurabilityMode::Eventual`: writes may be lost, but the recovered state
/// must be a valid prefix of the history, with no gap, no half-applied
/// batch and an intact iteration order.
///
/// Catches: recovery that replays a WAL record past a torn one, that
/// applies part of a batch, or that leaves the key space out of order.
#[test]
fn eventual_durability_recovers_to_a_valid_prefix() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let spec = ChildSpec::new(Phase::AfterNPuts, &db)
        .ops(600)
        .durability(DurabilityMode::Eventual);
    let out = CrashRun::new(spec).run();
    out.assert_killed();

    let report = fault::simulate_power_loss(&db, CutPoint::End);
    assert!(report.discarded_anything(), "{}", report.summary());

    let recovered = reopen(&db);
    let prefix = fault::assert_valid_prefix(&recovered, &out.history);
    assert!(
        prefix.k <= out.history.len(),
        "prefix cannot exceed the history",
    );
}

/// Proves the harshest reconstruction is survivable: the unsynced tail is
/// replaced with garbage rather than removed, so the WAL reader meets a
/// record with a plausible length prefix and a broken payload.
///
/// The invariant asserted is the one that holds in every durability mode:
/// the engine either recovers a valid prefix, or it refuses to open and
/// says why. What it must never do is open and serve a state that is
/// neither. regolith today takes the second branch on a garbage WAL tail.
///
/// Catches: a WAL reader that trusts a length prefix before verifying the
/// checksum, and any recovery that applies a record whose bytes are wrong.
#[test]
fn a_garbage_tail_never_yields_a_state_that_is_not_a_prefix() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let spec = ChildSpec::new(Phase::AfterNPuts, &db).ops(600);
    let out = CrashRun::new(spec).run();
    out.assert_killed();

    let opts = PowerLossOptions::default()
        .tear(TearMode::Garbage)
        .seed(0xDEAD_BEEF);
    let report = fault::simulate_power_loss_with(&db, &out.journal, CutPoint::End, &opts);
    assert!(
        !report.torn.is_empty(),
        "nothing was torn\n{}",
        report.summary()
    );

    match fault::recover_and_validate(&db, small_opts(), &out.history) {
        fault::Recovery::Recovered(_) => {}
        fault::Recovery::RefusedToOpen(e) => assert!(
            e.contains("Corruption") || e.contains("corrupt"),
            "refusing to open is allowed, but the reason must name the corruption; got: {e}",
        ),
    }
}

/// Proves a cut can be placed at a sync boundary, not only at the end of
/// the run, and that the reconstruction honours it.
///
/// Catches: a `CutPoint` that is silently ignored, which would make every
/// mid-run power-loss test actually test the end of the run.
#[test]
fn a_cut_at_an_earlier_sync_discards_more_than_a_cut_at_the_end() {
    let tmp = TempDir::new().unwrap();
    let late = tmp.path().join("late");
    let early = tmp.path().join("early");

    let mut reports = Vec::new();
    let mut histories = Vec::new();
    for (db, early_cut) in [(&late, false), (&early, true)] {
        let spec = ChildSpec::new(Phase::AfterNPuts, db)
            .ops(600)
            .durability(DurabilityMode::Immediate);
        let out = CrashRun::new(spec).run();
        out.assert_killed();
        let syncs = out.journal.sync_seqs().len();
        assert!(syncs > 8, "Immediate mode should fsync often, saw {syncs}");
        let point = if early_cut {
            CutPoint::AfterNthSync(syncs / 2)
        } else {
            CutPoint::End
        };
        reports.push(fault::simulate_power_loss(db, point));
        histories.push(out.history);
    }
    assert!(
        reports[1].bytes_discarded > reports[0].bytes_discarded,
        "cutting halfway must discard more than cutting at the end\n{}\n{}",
        reports[0].summary(),
        reports[1].summary(),
    );

    let full = fault::recover_and_validate(&late, small_opts(), &histories[0]);
    let half = fault::recover_and_validate(&early, small_opts(), &histories[1]);
    assert!(
        half.k() < full.k(),
        "an earlier cut must leave a shorter surviving prefix: {} vs {}",
        half.k(),
        full.k(),
    );
}

/// Proves the validator returns the prefix length rather than a bare
/// pass/fail, and accepts a genuine prefix of a batched history.
#[test]
fn the_validator_accepts_a_prefix_and_reports_how_much_was_lost() {
    let mut h = History::new();
    h.batch(vec![
        (b"a".to_vec(), OpValue::Put(b"1".to_vec())),
        (b"b".to_vec(), OpValue::Put(b"2".to_vec())),
    ]);
    h.batch(vec![
        (b"c".to_vec(), OpValue::Put(b"3".to_vec())),
        (b"d".to_vec(), OpValue::Put(b"4".to_vec())),
    ]);

    let state = vec![
        (b"a".to_vec(), b"1".to_vec()),
        (b"b".to_vec(), b"2".to_vec()),
    ];
    let report = fault::validate_prefix_of_state(&state, &h).expect("this is a valid prefix");
    assert_eq!(report.k, 2);
    assert_eq!(report.lost, 2);
}

/// Proves the validator can actually fail: a state holding half of an
/// atomic `WriteBatch` must be rejected as `HalfAppliedBatch`.
///
/// Catches: a validator that always passes. This is the honesty check on
/// the check, without which the whole suite could be green and worthless.
#[test]
fn the_validator_rejects_a_half_applied_batch() {
    let mut h = History::new();
    h.batch(vec![
        (b"a".to_vec(), OpValue::Put(b"1".to_vec())),
        (b"b".to_vec(), OpValue::Put(b"2".to_vec())),
    ]);

    let torn = vec![(b"a".to_vec(), b"1".to_vec())];
    match fault::validate_prefix_of_state(&torn, &h) {
        Err(fault::PrefixViolation::HalfAppliedBatch { .. }) => {}
        other => panic!("expected HalfAppliedBatch, got {other:?}"),
    }
}

/// Proves the validator rejects a hole: a state missing a write from the
/// middle of the history while holding a later one is not a prefix.
///
/// Catches: recovery that skips a corrupt WAL record and carries on with
/// the next one, which loses a write silently and is the single most
/// damaging recovery bug an LSM can have.
#[test]
fn the_validator_rejects_a_gap_in_the_history() {
    let mut h = History::new();
    h.put(b"a".to_vec(), b"1".to_vec());
    h.put(b"b".to_vec(), b"2".to_vec());
    h.put(b"c".to_vec(), b"3".to_vec());

    let gapped = vec![
        (b"a".to_vec(), b"1".to_vec()),
        (b"c".to_vec(), b"3".to_vec()),
    ];
    match fault::validate_prefix_of_state(&gapped, &h) {
        Err(fault::PrefixViolation::NotAPrefix { .. }) => {}
        other => panic!("expected NotAPrefix, got {other:?}"),
    }
}

/// Proves the validator rejects a key the workload never wrote, which is
/// how a recovery that resurrects a deleted key or invents one is caught.
#[test]
fn the_validator_rejects_a_key_that_was_never_written() {
    let mut h = History::new();
    h.put(b"a".to_vec(), b"1".to_vec());

    let extra = vec![
        (b"a".to_vec(), b"1".to_vec()),
        (b"zz".to_vec(), b"9".to_vec()),
    ];
    match fault::validate_prefix_of_state(&extra, &h) {
        Err(fault::PrefixViolation::ForeignKeys { .. }) => {}
        other => panic!("expected ForeignKeys, got {other:?}"),
    }
}

/// Proves a clean run loses nothing, which is the baseline every crash
/// result is measured against. Without it, a validator bug that always
/// reported `k = 0` would look like a durability finding.
#[test]
fn a_clean_shutdown_loses_nothing() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let spec = ChildSpec::new(Phase::CleanExit, &db).ops(500);
    let out = CrashRun::new(spec).run();
    out.assert_clean();

    let recovered = reopen(&db);
    let report = fault::assert_valid_prefix(&recovered, &out.history);
    assert_eq!(
        report.k,
        out.history.len(),
        "clean shutdown must keep every write: {}",
        report.summary(),
    );
    fault::assert_acked_survived(&report, &out.acked());
}

/// Proves the byte-level mutators do exactly what their names say, on
/// known bytes, so a corruption test that uses them is aiming at the
/// offset it thinks it is.
#[test]
fn the_byte_mutators_hit_the_offsets_they_claim() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("f");
    std::fs::write(&p, b"0123456789").unwrap();

    fault::flip_bit(&p, 0, 0);
    assert_eq!(std::fs::read(&p).unwrap()[0], b'0' ^ 1);

    fault::overwrite_range(&p, 4, b"XY");
    assert_eq!(&std::fs::read(&p).unwrap()[4..6], b"XY");

    fault::truncate_at(&p, 6);
    assert_eq!(fault::file_len(&p), 6);

    let g1 = fault::garbage(7, 32);
    let g2 = fault::garbage(7, 32);
    let g3 = fault::garbage(8, 32);
    assert_eq!(g1, g2, "garbage must be reproducible from its seed");
    assert_ne!(g1, g3, "a different seed must give different bytes");
}

/// Proves the locators find the real WAL, MANIFEST and SSTable of a live
/// database, so a corruption test cannot silently target a path that does
/// not exist.
#[test]
fn the_locators_find_the_wal_manifest_and_sstable() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    {
        let d = Db::open(&db, Options::default().write_buffer_size(4 * 1024)).unwrap();
        for i in 0..2000u32 {
            // A write a stall stops waits for it to clear: on a busy host
            // the compaction worker falls behind two thousand small flushes.
            regolith::through_stalls(|| d.put(format!("k{i:06}").as_bytes(), &[b'v'; 64])).unwrap();
        }
        d.close().unwrap();
    }
    assert!(!fault::find_wals(&db).is_empty());
    assert!(fault::newest_wal(&db).is_file());
    assert!(fault::find_manifest(&db).is_file());
    assert!(
        !fault::find_ssts(&db).is_empty(),
        "the workload should have flushed at least one SSTable",
    );
    assert!(fault::first_sst(&db).is_file());
}

/// Proves a crash physically inside a single `WriteBatch` record leaves
/// either all of that batch or none of it after recovery, under both
/// realistic shapes of unsynced tail.
///
/// The setup is what makes this real rather than vacuous. `Immediate`
/// durability makes the batches before the crash genuinely durable, the
/// batch is far larger than the WAL's 8 KiB `BufWriter` so the kill lands
/// between two write syscalls of one record, and the reconstruction is run
/// twice: once with the tail dropped ([`TearMode::Truncate`], the common
/// filesystem outcome) and once with the tail left in place but zeroed
/// ([`TearMode::Zero`], what ext4 can expose when the size was journalled
/// but the data blocks were not).
///
/// Observed on this engine, and recorded here so it is not mistaken for a
/// harness fault: with the tail dropped, every acknowledged batch is
/// recovered. With the tail zeroed, regolith reports a WAL checksum mismatch
/// and refuses to open at all, so the assertion below allows a loud
/// refusal but never a torn state.
///
/// Catches: a WAL batch record that recovery applies operation by
/// operation as it decodes, instead of validating the whole record first;
/// and an `Immediate` path that loses an acknowledged batch when the tail
/// is merely truncated.
///
/// Runtime: measured at 0.04s of test time; it spawns two child processes.
#[test]
fn a_crash_inside_one_write_batch_applies_all_of_it_or_none() {
    let tmp = TempDir::new().unwrap();

    for tear in [TearMode::Truncate, TearMode::Zero] {
        let db = tmp.path().join(format!("db_{tear:?}"));
        let out = CrashRun::new(ChildSpec::new(Phase::MidWriteBatch, &db))
            .timeout(Duration::from_secs(180))
            .run();
        out.assert_killed();
        assert!(
            out.acked_count() > 0,
            "no batch was acknowledged before the crash, so there is no atomicity to test",
        );

        let opts = PowerLossOptions::default().tear(tear);
        let report = fault::simulate_power_loss_with(&db, &out.journal, CutPoint::End, &opts);
        assert!(
            report.discarded_anything(),
            "the in-flight batch was not disturbed at all\n{}",
            report.summary(),
        );

        let boundaries = out.history.batch_boundaries();
        match fault::recover_and_validate(&db, small_opts(), &out.history) {
            fault::Recovery::Recovered(r) => {
                assert!(
                    boundaries.contains(&r.k),
                    "{tear:?}: recovered prefix {} stops inside a batch; boundaries are {:?}",
                    r.k,
                    boundaries,
                );
                fault::assert_acked_survived(&r, &out.acked());
            }
            fault::Recovery::RefusedToOpen(e) => {
                assert_eq!(
                    tear,
                    TearMode::Zero,
                    "a merely truncated tail must still recover, but the database refused \
                     to open after acknowledging {} writes: {e}",
                    out.acked_count(),
                );
                assert!(
                    e.contains("corruption") || e.contains("Corruption"),
                    "{tear:?}: refusing to open is allowed, but the reason must name the \
                     corruption; got: {e}",
                );
            }
        }
    }
}

/// Proves a crash while a background thread is writing an SSTable still
/// recovers to a valid prefix, and confirms from the recorded thread ids
/// that a background write really was in flight rather than assuming it.
///
/// Catches: a flush or compaction that publishes an SSTable into the
/// MANIFEST before its bytes are durable.
///
/// Runtime: measured at 0.2s of test time; it spawns one child process.
#[test]
fn a_crash_during_a_background_sstable_write_recovers_to_a_valid_prefix() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let out = CrashRun::new(ChildSpec::new(Phase::DuringCompaction, &db))
        .timeout(Duration::from_secs(300))
        .run();
    out.assert_killed();

    let main_tid = out
        .journal
        .first_writer_tid()
        .expect("the run must have recorded a write");
    assert!(
        out.journal.background_sst_write_seen(main_tid),
        "no background SSTable write was recorded, so this run did not test what it claims\n{}",
        out.journal,
    );

    fault::simulate_power_loss(&db, CutPoint::End);
    let recovered = reopen(&db);
    fault::assert_valid_prefix(&recovered, &out.history);
}

/// Proves the harness refuses to pretend: asking for a syscall-triggered
/// kill that can never match must fail loudly instead of reporting a
/// clean, meaningless pass.
#[test]
#[should_panic(expected = "was expected to be killed")]
fn a_trigger_that_never_fires_fails_the_test() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let out = CrashRun::new(ChildSpec::new(Phase::CleanExit, &db).ops(20))
        .trigger(Trigger::Syscall {
            kind: fault::DieKind::Write,
            path_contains: "this-path-does-not-exist".to_string(),
            nth: 1,
            before: false,
        })
        .run();
    out.assert_killed();
}
