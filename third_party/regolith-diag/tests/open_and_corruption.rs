//! `Db::open` and the read path against hostile inputs.
//!
//! Every case here is driven through the public API. The contract
//! under test is uniform: a bad path, a bad permission, or a corrupt
//! file produces an `Err`, never a panic, never an unbounded
//! allocation, never an endless iterator, and never a silently empty
//! database when the tables are still on disk.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads, the filesystem or proptest, none of which exist there. The browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use regolith::{Db, DurabilityMode, Options, RateLimiter};
use tempfile::TempDir;

mod common;

fn opts() -> Options {
    Options::default().write_buffer_size(4 * 1024)
}

fn fill(db: &Db, n: usize) {
    for i in 0..n {
        regolith::through_stalls(|| {
            db.put(
                format!("key_{i:06}").as_bytes(),
                format!("val_{i:06}").as_bytes(),
            )
        })
        .unwrap();
    }
}

fn files_with_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut v: Vec<_> = fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some(ext))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// A populated database directory whose files are ready to be
/// tampered with.
fn seeded_db(keys: usize) -> TempDir {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts()).unwrap();
    fill(&db, keys);
    db.compact_range(None, None).wait().unwrap();
    db.close().unwrap();
    dir
}

/// Permission-based cases are meaningless for a process that ignores
/// permissions, so they are skipped rather than falsely passing.
#[cfg(unix)]
fn permissions_are_enforced(dir: &Path) -> bool {
    let probe = dir.join("write-probe");
    let enforced = fs::File::create(&probe).is_err();
    let _ = fs::remove_file(&probe);
    enforced
}

// ---- hostile filesystem shapes ----

#[test]
fn open_on_a_regular_file_path_errors() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("iam_a_file");
    fs::write(&path, b"not a directory").unwrap();
    assert!(Db::open(&path, Options::default()).is_err());
}

// POSIX permission bits: Windows has no equivalent of a mode that
// makes a directory unreadable or unsearchable to its owner.
#[cfg(unix)]
#[test]
fn open_inside_a_read_only_parent_errors() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let parent = dir.path().join("ro");
    fs::create_dir(&parent).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
    if permissions_are_enforced(&parent) {
        assert!(Db::open(parent.join("db"), Options::default()).is_err());
        assert!(Db::open(&parent, Options::default()).is_err());
    }
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
}

// POSIX permission bits: Windows has no equivalent of a mode that
// makes a directory unreadable or unsearchable to its owner.
#[cfg(unix)]
#[test]
fn open_under_an_unsearchable_parent_errors() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let parent = dir.path().join("locked");
    fs::create_dir(&parent).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o000)).unwrap();
    if permissions_are_enforced(&parent) {
        assert!(Db::open(parent.join("db"), Options::default()).is_err());
    }
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn open_when_a_required_subdirectory_is_a_regular_file_errors() {
    for name in ["sst", "wal"] {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(name), b"blocked").unwrap();
        assert!(
            Db::open(dir.path(), Options::default()).is_err(),
            "{name} as a regular file must fail the open"
        );
    }
}

#[test]
fn open_when_the_lock_path_is_a_directory_errors() {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("LOCK")).unwrap();
    assert!(Db::open(dir.path(), Options::default()).is_err());
}

#[test]
fn a_failed_open_leaves_no_stale_lock_file() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("sst"), b"blocked").unwrap();
    assert!(Db::open(dir.path(), Options::default()).is_err());
    fs::remove_file(dir.path().join("sst")).unwrap();
    // The next open must succeed: nothing may be left holding the lock.
    let db = Db::open(dir.path(), Options::default()).unwrap();
    regolith::through_stalls(|| db.put(b"k", b"v")).unwrap();
    assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
}

// ---- corrupt MANIFEST ----

#[test]
fn a_manifest_that_would_discard_live_tables_refuses_to_open() {
    for shape in ["zeroed", "garbage", "single-byte", "empty"] {
        let dir = seeded_db(2000);
        let manifest = dir.path().join("MANIFEST");
        let len = fs::metadata(&manifest).unwrap().len() as usize;
        let bytes = match shape {
            "zeroed" => vec![0u8; len],
            "garbage" => vec![0xABu8; len],
            "single-byte" => vec![0x01u8],
            _ => Vec::new(),
        };
        fs::write(&manifest, &bytes).unwrap();
        let tables = files_with_ext(&dir.path().join("sst"), "sst").len();
        assert!(tables > 0, "{shape}: the fixture must have tables on disk");

        match Db::open(dir.path(), opts()) {
            Err(_) => {}
            Ok(db) => {
                // Opening is only acceptable if nothing was lost.
                let readable = (0..2000)
                    .filter(|i| {
                        db.get(format!("key_{i:06}").as_bytes())
                            .map(|v| v.is_some())
                            .unwrap_or(false)
                    })
                    .count();
                panic!(
                    "{shape}: open succeeded with {tables} tables on disk but only {readable} of 2000 keys readable"
                );
            }
        }
    }
}

/// A database whose WAL holds every acknowledged write, copied while it
/// is still live so nothing has been flushed and the MANIFEST is still
/// at its durable length of zero.
fn wal_only_copy(keys: usize) -> TempDir {
    let live = TempDir::new().unwrap();
    let db = Db::open(
        live.path(),
        Options::default()
            .write_buffer_size(8 * 1024 * 1024)
            .durability(DurabilityMode::Immediate),
    )
    .unwrap();
    fill(&db, keys);

    let copy = TempDir::new().unwrap();
    copy_tree(live.path(), copy.path());
    db.close().unwrap();

    // What a power cut before the first `AddFile` leaves: the MANIFEST was
    // `fsync`ed empty and every edit since is still in the page cache.
    fs::write(copy.path().join("MANIFEST"), b"").unwrap();
    copy
}

/// Property: the discarded-table guard counts tables that could hold
/// data, not `.sst` files. A crash inside the first flush leaves a
/// zero-length table file next to a MANIFEST whose durable length is
/// zero; that file holds nothing, while the `fsync`ed WAL holds every
/// acknowledged write. Refusing on it would lose them all.
///
/// Catches a guard that goes back to counting directory entries.
#[test]
fn a_wiped_manifest_next_to_an_empty_orphan_table_still_opens_and_keeps_the_wal() {
    let dir = wal_only_copy(100);
    let orphan = dir.path().join("sst").join("000003.sst");
    fs::write(&orphan, b"").unwrap();

    let db = Db::open(dir.path(), opts()).expect("an empty orphan table must not block the open");
    for i in 0..100 {
        assert_eq!(
            db.get(format!("key_{i:06}").as_bytes()).unwrap(),
            Some(format!("val_{i:06}").into_bytes()),
            "key_{i:06} was acknowledged and must come back from the WAL",
        );
    }
}

/// Property: only a file that can be *proved* to hold nothing is
/// dismissed. A non-empty unreferenced table whose footer will not parse
/// cannot be proved empty, so the guard still refuses and leaves the file
/// on disk for repair rather than opening on top of it.
///
/// Catches the over-correction: relaxing the guard until any damaged
/// table is stepped over, which would let a wiped MANIFEST quietly
/// present a database missing everything those tables held.
#[test]
fn a_wiped_manifest_next_to_an_unreadable_orphan_table_refuses_to_open() {
    let dir = wal_only_copy(20);
    let orphan = dir.path().join("sst").join("000004.sst");
    fs::write(&orphan, vec![0xABu8; 4096]).unwrap();

    let err = match Db::open(dir.path(), opts()) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("an unreadable non-empty orphan table must refuse the open"),
    };
    assert!(
        err.contains("000004.sst"),
        "the refusal must name the file that caused it, got: {err}",
    );
    assert!(
        orphan.exists(),
        "a refused open must leave the suspect table on disk",
    );
}

#[test]
fn a_manifest_with_a_huge_length_prefix_does_not_panic() {
    let dir = seeded_db(200);
    let manifest = dir.path().join("MANIFEST");
    let mut bytes = fs::read(&manifest).unwrap();
    bytes[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
    fs::write(&manifest, &bytes).unwrap();
    let _ = Db::open(dir.path(), opts());
}

#[test]
fn manifest_single_byte_flips_do_not_panic() {
    let template = seeded_db(200);
    let pristine = fs::read(template.path().join("MANIFEST")).unwrap();
    let step = (pristine.len() / 64).max(1);
    for offset in (0..pristine.len()).step_by(step) {
        let dir = TempDir::new().unwrap();
        copy_tree(template.path(), dir.path());
        let mut bytes = pristine.clone();
        bytes[offset] ^= 0xFF;
        fs::write(dir.path().join("MANIFEST"), &bytes).unwrap();
        let _ = Db::open(dir.path(), opts());
    }
}

// ---- corrupt WAL ----

fn vm_peak_kib() -> u64 {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmPeak:")
            && let Some(n) = rest.split_whitespace().next()
        {
            return n.parse().unwrap_or(0);
        }
    }
    0
}

#[test]
fn a_wal_claiming_a_huge_record_does_not_allocate_it() {
    // The log is captured while the database is still open, with two
    // synced records: the second proves the first durable, so a bogus
    // length on the first is corruption rather than the short trailing
    // record every crash leaves behind, which replay is required to
    // accept as the end of the log.
    let dir = TempDir::new().unwrap();
    let (wal, mut bytes) = {
        let db = Db::open(dir.path(), opts().durability(DurabilityMode::Immediate)).unwrap();
        regolith::through_stalls(|| db.put(b"k", b"v")).unwrap();
        regolith::through_stalls(|| db.put(b"k2", b"v2")).unwrap();
        let wal = files_with_ext(&dir.path().join("wal"), "log")
            .pop()
            .expect("a wal file");
        let bytes = fs::read(&wal).unwrap();
        drop(db);
        (wal, bytes)
    };
    let first = common::wal_format::record_bounds(&bytes)[0];

    // The first record's header claims u32::MAX bytes of payload, with a
    // whole record behind it: replay must reject it without reserving
    // 4 GiB for it.
    bytes[first..first + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    fs::write(&wal, &bytes).unwrap();

    let before = vm_peak_kib();
    let result = Db::open(dir.path(), opts());
    let after = vm_peak_kib();
    assert!(result.is_err(), "a bogus record length must fail replay");
    assert!(
        after - before < 1024 * 1024,
        "replay reserved {} KiB of address space for a corrupt length",
        after - before
    );
}

#[test]
fn wal_single_byte_flips_do_not_panic() {
    let template = TempDir::new().unwrap();
    {
        let db = Db::open(template.path(), opts()).unwrap();
        for i in 0..40 {
            regolith::through_stalls(|| db.put(format!("k{i:03}").as_bytes(), b"value")).unwrap();
        }
        db.close().unwrap();
    }
    let wal_name = files_with_ext(&template.path().join("wal"), "log")
        .pop()
        .expect("a wal file");
    let pristine = fs::read(&wal_name).unwrap();
    let leaf = wal_name.file_name().unwrap().to_owned();
    let step = (pristine.len() / 48).max(1);
    for offset in (0..pristine.len()).step_by(step) {
        let dir = TempDir::new().unwrap();
        copy_tree(template.path(), dir.path());
        let mut bytes = pristine.clone();
        bytes[offset] ^= 0xFF;
        fs::write(dir.path().join("wal").join(&leaf), &bytes).unwrap();
        let _ = Db::open(dir.path(), opts());
    }
}

// ---- corrupt SSTable ----

#[test]
fn a_corrupt_sstable_never_makes_the_forward_scan_run_away() {
    const KEYS: usize = 2000;
    let template = seeded_db(KEYS);
    let sst_name = files_with_ext(&template.path().join("sst"), "sst")
        .pop()
        .expect("an sst file");
    let pristine = fs::read(&sst_name).unwrap();
    let leaf = sst_name.file_name().unwrap().to_owned();
    let step = (pristine.len() / 32).max(1);

    for offset in (0..pristine.len()).step_by(step) {
        let dir = TempDir::new().unwrap();
        copy_tree(template.path(), dir.path());
        let mut bytes = pristine.clone();
        bytes[offset] ^= 0xFF;
        fs::write(dir.path().join("sst").join(&leaf), &bytes).unwrap();

        let Ok(db) = Db::open(dir.path(), opts()) else {
            continue;
        };
        // Point lookups must terminate, with or without an error.
        for i in (0..KEYS).step_by(97) {
            let _ = db.get(format!("key_{i:06}").as_bytes());
        }
        // Forward iteration must terminate. A sorted merge yields
        // strictly increasing keys, so it cannot outlast the key count
        // by a wide margin however the file is damaged.
        let budget = KEYS * 4;
        let mut steps = 0usize;
        let mut it = db.iter();
        it.seek_to_first();
        while it.valid() {
            steps += 1;
            assert!(
                steps <= budget,
                "flip at {offset}: forward scan yielded {steps} entries from a {KEYS}-key database"
            );
            it.next();
        }
        // Reverse iteration too.
        let mut steps = 0usize;
        it.seek_to_last();
        while it.valid() {
            steps += 1;
            assert!(
                steps <= budget,
                "flip at {offset}: reverse scan yielded {steps} entries from a {KEYS}-key database"
            );
            it.prev();
        }
    }
}

#[test]
fn a_truncated_sstable_does_not_panic() {
    let template = seeded_db(500);
    let sst_name = files_with_ext(&template.path().join("sst"), "sst")
        .pop()
        .expect("an sst file");
    let pristine = fs::read(&sst_name).unwrap();
    let leaf = sst_name.file_name().unwrap().to_owned();
    let step = (pristine.len() / 32).max(1);

    for cut in (0..pristine.len()).step_by(step) {
        let dir = TempDir::new().unwrap();
        copy_tree(template.path(), dir.path());
        fs::write(dir.path().join("sst").join(&leaf), &pristine[..cut]).unwrap();
        let Ok(db) = Db::open(dir.path(), opts()) else {
            continue;
        };
        for i in (0..500).step_by(37) {
            let _ = db.get(format!("key_{i:06}").as_bytes());
        }
        let mut it = db.iter();
        it.seek_to_first();
        let mut steps = 0usize;
        while it.valid() && steps < 5000 {
            steps += 1;
            it.next();
        }
        assert!(steps < 5000, "truncation at {cut}: scan did not terminate");
    }
}

// ---- public constructors and shutdown ----

#[test]
fn a_rate_limiter_serves_a_high_priority_request_at_once() {
    let limiter = regolith::TokenBucketRateLimiter::new(1024, 4096);
    limiter.request(128, regolith::Priority::High);
    assert_eq!(
        limiter.get_total_bytes_through(regolith::Priority::High),
        128
    );
}

#[test]
fn close_does_not_wait_out_a_condvar_timeout_per_worker() {
    for workers in [1usize, 2, 4, 8] {
        let dir = TempDir::new().unwrap();
        let db = Db::open(
            dir.path(),
            Options::default().max_background_compactions(workers),
        )
        .unwrap();
        regolith::through_stalls(|| db.put(b"k", b"v")).unwrap();
        let started = Instant::now();
        drop(db);
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "closing with {workers} compaction workers took {elapsed:?}"
        );
    }
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).unwrap().filter_map(|e| e.ok()) {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&target).unwrap();
            copy_tree(&entry.path(), &target);
        } else if entry.file_name() != "LOCK" {
            let mut out = fs::File::create(&target).unwrap();
            out.write_all(&fs::read(entry.path()).unwrap()).unwrap();
        }
    }
}
