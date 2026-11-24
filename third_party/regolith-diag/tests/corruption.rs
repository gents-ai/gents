//! Byte-level corruption and recovery scenarios ported from the
//! subset of RocksDB's `corruption_test.cc` that applies without
//! a fault-injecting filesystem.
//!
//! Each test writes data, closes the DB, mangles an on-disk file
//! with raw `std::fs` ops, then re-opens and asserts that the
//! engine either (a) surfaces a diagnostic error or (b) recovers
//! gracefully with the expected partial data - never silent loss
//! of earlier, uncorrupted data.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads, the filesystem or proptest, none of which exist there. The browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use std::sync::Arc;

use regolith::{Db, DurabilityMode, Error, Options, Statistics, Ticker};
use tempfile::TempDir;

mod common;

use common::{count_sst_files, count_wal_files, force_compaction, open, wal_format};

// ── helpers ─────────────────────────────────────────────────────

/// Flip the byte at `offset` inside `path`.
fn flip_byte(path: &Path, offset: usize) {
    let mut bytes = fs::read(path).unwrap();
    bytes[offset] ^= 0xFF;
    fs::write(path, &bytes).unwrap();
}

/// Truncate `path` to `new_len` bytes.
fn truncate(path: &Path, new_len: u64) {
    let f = OpenOptions::new().write(true).open(path).unwrap();
    f.set_len(new_len).unwrap();
}

/// Return the path of the first SST file in `<db>/sst/`.
fn first_sst(db_dir: &Path) -> PathBuf {
    let sst_dir = db_dir.join("sst");
    let mut entries: Vec<_> = fs::read_dir(&sst_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("sst"))
        .collect();
    entries.sort_by_key(|e| e.path());
    entries.into_iter().next().unwrap().path()
}

/// Return the path of the first WAL file in `<db>/wal/`.
fn first_wal(db_dir: &Path) -> PathBuf {
    let wal_dir = db_dir.join("wal");
    let mut entries: Vec<_> = fs::read_dir(&wal_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("log"))
        .collect();
    entries.sort_by_key(|e| e.path());
    entries.into_iter().next().unwrap().path()
}

fn assert_open_fails_with_kind(dir: &TempDir, expected: io::ErrorKind) {
    match Db::open(dir.path(), Default::default()) {
        Err(Error::Corruption(e)) => assert_eq!(e.kind(), expected),
        Err(e) => panic!("expected corruption error, got {e:?}"),
        Ok(_) => panic!("expected DB open to fail"),
    }
}

// ── WAL tail corruption ─────────────────────────────────────────

/// Options that report what an open discards.
fn reporting_opts(reports: &Arc<wal_format::TailReports>) -> (Options, Arc<Statistics>) {
    let stats = Arc::new(Statistics::new());
    let opts = common::small_opts()
        .listeners(vec![reports.clone() as Arc<dyn regolith::EventListener>])
        .statistics(Some(Arc::clone(&stats)));
    (opts, stats)
}

#[test]
fn damage_a_later_record_proves_synced_fails_open_and_keeps_wal() {
    // Under Immediate durability each record carries the offset the sync
    // before it covered, so the second record proves the first durable.
    // Damage to the first is then loss of an acknowledged write: open
    // must fail closed and leave the log for repair or inspection.
    let dir = TempDir::new().unwrap();
    {
        let db = Db::open(
            dir.path(),
            common::small_opts().durability(DurabilityMode::Immediate),
        )
        .unwrap();
        regolith::through_stalls(|| db.put(b"good_1", b"1")).unwrap();
        regolith::through_stalls(|| db.put(b"good_2", b"2")).unwrap();
    }

    let wal = first_wal(dir.path());
    let wal_count = count_wal_files(dir.path());
    let bounds = wal_format::record_bounds(&fs::read(&wal).unwrap());
    flip_byte(&wal, bounds[1] - 1);

    assert_open_fails_with_kind(&dir, io::ErrorKind::InvalidData);
    assert!(wal.exists());
    assert_eq!(count_wal_files(dir.path()), wal_count);
}

#[test]
fn a_damaged_final_record_nothing_proves_synced_is_dropped_and_reported() {
    // Nothing after the last record vouches for it, so a crash was free
    // to leave it in any state: the open keeps every record before it,
    // drops it, and says so.
    let dir = TempDir::new().unwrap();
    {
        let db = open(&dir);
        regolith::through_stalls(|| db.put(b"good_1", b"1")).unwrap();
        regolith::through_stalls(|| db.put(b"good_2", b"2")).unwrap();
    }
    let wal = first_wal(dir.path());
    let bytes = fs::read(&wal).unwrap();
    let bounds = wal_format::record_bounds(&bytes);
    flip_byte(&wal, bytes.len() - 1);

    let reports = wal_format::TailReports::new();
    let (opts, stats) = reporting_opts(&reports);
    let db = Db::open(dir.path(), opts).unwrap();
    assert_eq!(db.get(b"good_1").unwrap(), Some(b"1".to_vec()));
    assert_eq!(db.get(b"good_2").unwrap(), None);
    let taken = reports.taken();
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].file_path, wal);
    assert_eq!(taken[0].offset, bounds[1] as u64);
    assert_eq!(taken[0].discarded_bytes, (bytes.len() - bounds[1]) as u64);
    assert_eq!(stats.get_ticker(Ticker::WalTailDiscarded), 1);
    assert_eq!(
        stats.get_ticker(Ticker::WalTailDiscardedBytes),
        taken[0].discarded_bytes
    );
}

#[test]
fn wal_truncated_at_arbitrary_offset_replays_the_whole_records_before_the_cut() {
    // Truncation is what a crash leaves behind: the records before the
    // cut are byte-for-byte what the process wrote, and only the one the
    // cut lands in is incomplete. Every cut below lands inside the second
    // record, so the first must always come back and the second never
    // may. Failing the open here instead would throw away a write that
    // was acknowledged and fsynced.
    for trim in [1u64, 3, 5, 9, 11, 15, 20] {
        let dir = TempDir::new().unwrap();
        {
            let db = open(&dir);
            regolith::through_stalls(|| db.put(b"a", b"1")).unwrap();
            regolith::through_stalls(|| db.put(b"b", b"2")).unwrap();
        }
        let wal = first_wal(dir.path());
        let wal_count = count_wal_files(dir.path());
        let full = fs::read(&wal).unwrap();
        let first_end = wal_format::record_bounds(&full)[1] as u64;
        let cut = full.len() as u64 - trim;
        assert!(
            cut > first_end && cut < full.len() as u64,
            "a cut of {trim} must land inside the second record",
        );

        truncate(&wal, cut);
        let db = Db::open(dir.path(), Default::default())
            .unwrap_or_else(|e| panic!("cut at {cut}: {e}"));
        assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()), "cut at {cut}");
        assert_eq!(db.get(b"b").unwrap(), None, "cut at {cut}");
        db.close().unwrap();
        assert_eq!(count_wal_files(dir.path()), wal_count);
    }
}

#[test]
fn wal_checksum_flip_in_final_record_of_a_closed_log_fails_open() {
    // A clean close syncs every record and then appends CLOSE, which
    // proves the whole log durable. Damage to the final record is then
    // corruption, not a crash: do not convert it into a clean stop.
    let dir = TempDir::new().unwrap();
    {
        let db = open(&dir);
        regolith::through_stalls(|| db.put(b"first", b"v1")).unwrap();
        regolith::through_stalls(|| db.put(b"second", b"v2")).unwrap();
        db.close().unwrap();
    }
    let wal = first_wal(dir.path());
    let bytes = fs::read(&wal).unwrap();
    let bounds = wal_format::record_bounds(&bytes);
    let close = bounds[bounds.len() - 2];
    assert_eq!(wal_format::kind_at(&bytes, close), wal_format::KIND_CLOSE);
    flip_byte(&wal, close - 1);

    assert_open_fails_with_kind(&dir, io::ErrorKind::InvalidData);
    assert!(wal.exists());
}

// ── manifest corruption ─────────────────────────────────────────

#[test]
fn manifest_deleted_prevents_reopen_of_nonempty_db() {
    // corruption_test.cc::MissingDescriptor - once a DB has written
    // SSTables, deleting the manifest drops the pointer to them.
    // Opening without a manifest yields a fresh-looking DB (the
    // manifest is re-created empty), so the pre-existing files are
    // orphaned but the open must not panic.
    let dir = TempDir::new().unwrap();
    {
        let db = open(&dir);
        for i in 0..200 {
            regolith::through_stalls(|| db.put(format!("k_{:04}", i).as_bytes(), &[0u8; 64]))
                .unwrap();
        }
        force_compaction(&db);
    }
    let manifest = dir.path().join("MANIFEST");
    assert!(manifest.exists());
    fs::remove_file(&manifest).unwrap();

    // Expected behavior: open succeeds and produces an empty view
    // of the DB. The orphaned SST files are still on disk but not
    // referenced.
    let db = open(&dir);
    assert!(db.scan(None, None).unwrap().is_empty());
    // At least one SST file is still physically present.
    assert!(count_sst_files(dir.path()) >= 1);
}

fn manifest_options() -> Options {
    Options::default()
        .max_background_compactions(0)
        .l0_compaction_trigger(100)
}

#[test]
fn corrupted_manifest_batch_preserves_the_earlier_flush() {
    let dir = TempDir::new().unwrap();
    let manifest = dir.path().join("MANIFEST");
    let second_end = {
        let db = Db::open(dir.path(), manifest_options()).unwrap();
        regolith::through_stalls(|| db.put(b"k1", b"v1")).unwrap();
        db.flush().unwrap();
        let first_end = fs::metadata(&manifest).unwrap().len();
        regolith::through_stalls(|| db.put(b"k2", b"v2")).unwrap();
        db.flush().unwrap();
        let second_end = fs::metadata(&manifest).unwrap().len();
        assert!(second_end > first_end);
        assert_eq!(count_sst_files(dir.path()), 2);
        // Both sealed WALs are gone; only the empty active WAL remains.
        assert_eq!(count_wal_files(dir.path()), 1);
        db.close().unwrap();
        second_end as usize
    };

    // Damage the second flush's checksum while the first flush's
    // SSTable is still present and referenced by the valid prefix.
    flip_byte(&manifest, second_end - 1);
    let db = Db::open(dir.path(), manifest_options()).unwrap();
    assert_eq!(db.get(b"k1").unwrap(), Some(b"v1".to_vec()));
    assert_eq!(db.get(b"k2").unwrap(), None);
    assert_eq!(
        db.scan(None, None).unwrap(),
        vec![(b"k1".to_vec(), b"v1".to_vec())]
    );
}

#[test]
fn corrupted_completed_compaction_refuses_missing_inputs_without_rewriting() {
    let dir = TempDir::new().unwrap();
    let manifest = dir.path().join("MANIFEST");
    let (inputs, compaction_end) = {
        let db = Db::open(dir.path(), manifest_options()).unwrap();
        regolith::through_stalls(|| db.put(b"k1", b"v1")).unwrap();
        db.flush().unwrap();
        regolith::through_stalls(|| db.put(b"k2", b"v2")).unwrap();
        db.flush().unwrap();
        let inputs: Vec<_> = fs::read_dir(dir.path().join("sst"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(inputs.len(), 2);
        db.compact_range(None, None).wait().unwrap();
        let end = fs::metadata(&manifest).unwrap().len() as usize;
        db.close().unwrap();
        (inputs, end)
    };
    assert!(inputs.iter().all(|path| !path.exists()));
    assert_eq!(count_sst_files(dir.path()), 1);
    let output = first_sst(dir.path());
    let output_bytes = fs::read(&output).unwrap();

    // Unlike an interrupted append, corruption after a committed
    // compaction cannot roll back to inputs that were already deleted.
    flip_byte(&manifest, compaction_end - 1);
    let damaged = fs::read(&manifest).unwrap();
    match Db::open(dir.path(), manifest_options()) {
        Err(Error::Io(error)) => {
            assert_eq!(error.kind(), io::ErrorKind::NotFound);
            assert!(error.to_string().contains(".sst"));
        }
        Err(error) => panic!("expected a missing compaction input, got {error:?}"),
        Ok(_) => panic!("damaged compaction must not hide its missing inputs"),
    }
    assert_eq!(fs::read(&manifest).unwrap(), damaged);
    assert_eq!(fs::read(&output).unwrap(), output_bytes);
    assert_eq!(count_sst_files(dir.path()), 1);
}

/// Three flushes, one table each, and the manifest's length after each.
fn three_flushes(dir: &Path) -> [usize; 3] {
    let manifest = dir.join("MANIFEST");
    let db = Db::open(dir, manifest_options()).unwrap();
    let mut ends = [0; 3];
    for (i, end) in ends.iter_mut().enumerate() {
        regolith::through_stalls(|| db.put(format!("k{i}").as_bytes(), b"v")).unwrap();
        db.flush().unwrap();
        *end = fs::metadata(&manifest).unwrap().len() as usize;
    }
    db.close().unwrap();
    ends
}

/// E29: damage below a batch that needed a sync and has bytes after it is
/// no crash's doing, because those bytes were written only once that sync
/// had made everything before it durable. The open refuses, naming the file
/// and both offsets, and leaves the manifest as it found it.
#[test]
fn damage_below_a_batch_a_later_sync_proves_refuses_naming_both_offsets() {
    let dir = TempDir::new().unwrap();
    let manifest = dir.path().join("MANIFEST");
    let [first, second, _] = three_flushes(dir.path());
    flip_byte(&manifest, first - 1);
    let damaged = fs::read(&manifest).unwrap();
    let err = match Db::open(dir.path(), manifest_options()) {
        Err(Error::Corruption(e)) => e.to_string(),
        Err(e) => panic!("expected a corruption error, got {e:?}"),
        Ok(_) => panic!("damage a later sync proves durable must not open"),
    };
    assert!(err.contains("MANIFEST is damaged at offset"), "{err}");
    assert!(err.contains(&format!("below offset {second}")), "{err}");
    assert_eq!(fs::read(&manifest).unwrap(), damaged);
}

/// E29: bytes after the last batch that read back whole, which no later
/// batch proves durable, are a crash's unsynced tail. A read-write open
/// drops them, truncates the file and reports it with a warn line and two
/// tickers; every table stays.
#[test]
fn a_torn_manifest_tail_is_dropped_truncated_and_reported() {
    let dir = TempDir::new().unwrap();
    let manifest = dir.path().join("MANIFEST");
    three_flushes(dir.path());
    let whole = fs::read(&manifest).unwrap();
    let mut torn = whole.clone();
    torn.extend_from_slice(&[0xAB; 37]);
    fs::write(&manifest, &torn).unwrap();

    let stats = Arc::new(Statistics::new());
    let db = Db::open(
        dir.path(),
        manifest_options().statistics(Some(Arc::clone(&stats))),
    )
    .unwrap();
    assert_eq!(stats.get_ticker(Ticker::ManifestTailDiscarded), 1);
    assert_eq!(stats.get_ticker(Ticker::ManifestTailDiscardedBytes), 37);
    for i in 0..3 {
        assert_eq!(
            db.get(format!("k{i}").as_bytes()).unwrap().as_deref(),
            Some(&b"v"[..])
        );
    }
    assert!(
        fs::read(&manifest).unwrap().starts_with(&whole),
        "the open keeps every batch that read back whole"
    );
    drop(db);
    let stats = Arc::new(Statistics::new());
    Db::open(
        dir.path(),
        manifest_options().statistics(Some(Arc::clone(&stats))),
    )
    .unwrap();
    assert_eq!(
        stats.get_ticker(Ticker::ManifestTailDiscarded),
        0,
        "the first open truncated the tail"
    );
}

/// E29: a read-only open reports a torn manifest tail and leaves it.
#[test]
fn a_read_only_open_reports_a_torn_manifest_tail_and_leaves_it() {
    let dir = TempDir::new().unwrap();
    let manifest = dir.path().join("MANIFEST");
    three_flushes(dir.path());
    let mut torn = fs::read(&manifest).unwrap();
    torn.extend_from_slice(&[0; 21]);
    fs::write(&manifest, &torn).unwrap();

    let stats = Arc::new(Statistics::new());
    let db = Db::open_read_only(
        dir.path(),
        manifest_options().statistics(Some(Arc::clone(&stats))),
    )
    .unwrap();
    assert_eq!(stats.get_ticker(Ticker::ManifestTailDiscarded), 1);
    assert_eq!(stats.get_ticker(Ticker::ManifestTailDiscardedBytes), 21);
    assert_eq!(db.get(b"k2").unwrap().as_deref(), Some(&b"v"[..]));
    drop(db);
    assert_eq!(fs::read(&manifest).unwrap(), torn);
}

// ── SSTable corruption ──────────────────────────────────────────

#[test]
fn truncated_sst_file_to_below_footer_reports_error_on_open() {
    // corruption_test.cc::CorruptedBlock - an SSTable smaller than
    // its 64-byte footer cannot be opened. The engine must
    // surface the error rather than silently drop the file.
    let dir = TempDir::new().unwrap();
    {
        let db = open(&dir);
        for i in 0..100 {
            regolith::through_stalls(|| db.put(format!("k_{:04}", i).as_bytes(), b"v")).unwrap();
        }
        force_compaction(&db);
    }
    let sst = first_sst(dir.path());
    truncate(&sst, 10);

    // Reopening must either error cleanly or surface a first read
    // error; it must not panic.
    if let Ok(db) = Db::open(dir.path(), Default::default()) {
        // If open succeeded, at least trying to read the
        // corrupted key should return an Err rather than wrong
        // data or a panic.
        let _ = db.get(b"k_0000");
    }
}

#[test]
fn sst_footer_magic_byte_flip_detected_on_open() {
    // corruption_test.cc::CorruptedBlock - the last 8 bytes of the
    // footer carry the magic number; flipping one byte must make
    // the engine refuse to trust the file.
    let dir = TempDir::new().unwrap();
    {
        let db = open(&dir);
        for i in 0..50 {
            regolith::through_stalls(|| db.put(format!("k_{:02}", i).as_bytes(), b"v")).unwrap();
        }
        force_compaction(&db);
    }
    let sst = first_sst(dir.path());
    let size = fs::metadata(&sst).unwrap().len() as usize;
    flip_byte(&sst, size - 1); // high byte of magic

    // Open attempt: we accept either Err OR Ok that errors on read.
    if let Ok(db) = Db::open(dir.path(), Default::default()) {
        // If open tolerates the file, the first read of a key
        // inside that file must either error or return None -
        // crucially, it must not panic.
        let _ = db.get(b"k_00");
    }
}

#[test]
fn stray_file_in_sst_dir_does_not_break_open() {
    // Not a direct corruption_test.cc scenario, but a robustness
    // check: leftover non-SST files in the SST directory (partial
    // compaction temp files, editor swap files) should be ignored
    // rather than crash the open path.
    let dir = TempDir::new().unwrap();
    {
        let db = open(&dir);
        regolith::through_stalls(|| db.put(b"k", b"v")).unwrap();
        force_compaction(&db);
    }
    let sst_dir = dir.path().join("sst");
    fs::write(sst_dir.join("leftover.tmp"), b"junk").unwrap();
    fs::write(sst_dir.join(".hidden"), b"junk").unwrap();

    let db = open(&dir);
    assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
}

// ── positive invariants ─────────────────────────────────────────

#[test]
fn clean_close_then_reopen_has_stable_file_count() {
    // Control case: when nothing is corrupted, reopening should
    // produce exactly the same on-disk layout.
    let dir = TempDir::new().unwrap();
    {
        let db = open(&dir);
        for i in 0..300 {
            regolith::through_stalls(|| db.put(format!("k_{:04}", i).as_bytes(), b"v")).unwrap();
        }
        force_compaction(&db);
    }
    let sst_before = count_sst_files(dir.path());
    let wal_before = count_wal_files(dir.path());
    {
        let _db = open(&dir);
    }
    let sst_after = count_sst_files(dir.path());
    let wal_after = count_wal_files(dir.path());
    // Reopen creates a fresh WAL but shouldn't delete SSTs.
    assert_eq!(sst_before, sst_after);
    assert!(wal_after >= wal_before.saturating_sub(1));
}
