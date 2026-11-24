//! Independent adversarial review of the torn-WAL-tail contract.
//!
//! Written against the public API only, with its own fixture, its own
//! record-framing oracle and its own prefix check, so it shares no helper
//! with the suite it is checking. It asserts both halves of the
//! benign/malignant split the fix rests on:
//!
//! * benign, and therefore recoverable with every earlier write intact:
//!   the file ends inside its final record, at any offset, or its final
//!   record is damaged with nothing after it to prove it was synced; the
//!   discard is reported;
//! * malignant, and therefore an error: damage in the middle of the log
//!   with whole records still after it, which prove the damaged bytes
//!   were synced.
//!
//! A fix that turned the second class into "discard and carry on" would
//! silently lose acknowledged writes, so every case below pins the
//! direction as well as the outcome.
#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::path::{Path, PathBuf};

use std::sync::Arc;

use regolith::{Db, DurabilityMode, Options};
use tempfile::TempDir;

mod common;

use common::wal_format;

fn opts() -> Options {
    Options::default()
        .write_buffer_size(8 * 1024 * 1024)
        .durability(DurabilityMode::Immediate)
}

/// One file of a planted database directory: its path relative to the
/// database root, and its bytes.
type PlantedFile = (PathBuf, Vec<u8>);

/// A pristine database directory whose WAL holds `n` acknowledged
/// `Immediate` writes and whose MANIFEST references no SSTable, plus the
/// WAL's own path relative to the root.
fn fixture(n: usize) -> (Vec<PlantedFile>, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let db = dir.path().join("db");
    {
        let d = Db::open(&db, opts()).expect("open");
        for i in 0..n {
            d.put(format!("k{i:04}").as_bytes(), format!("v{i:04}").as_bytes())
                .expect("put");
        }
        // Closing would flush the memtable and empty the WAL, which is
        // exactly the state this test must not start from.
        std::mem::forget(d);
    }
    let mut files = Vec::new();
    collect(&db, &db, &mut files);
    let wal = files
        .iter()
        .map(|(p, _)| p.clone())
        .find(|p| p.starts_with("wal"))
        .expect("one WAL in the fixture");
    (files, wal)
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<PlantedFile>) {
    for e in fs::read_dir(dir).expect("read_dir").flatten() {
        let p = e.path();
        if p.file_name().is_some_and(|n| n == "LOCK") {
            continue;
        }
        if p.is_dir() {
            collect(root, &p, out);
        } else {
            out.push((
                p.strip_prefix(root).expect("under root").to_path_buf(),
                fs::read(&p).expect("read"),
            ));
        }
    }
}

/// Plant the fixture into `db`, overriding the WAL with `wal_bytes`.
fn plant(files: &[PlantedFile], wal: &Path, wal_bytes: &[u8], db: &Path) {
    let _ = fs::remove_dir_all(db);
    for (rel, bytes) in files {
        let p = db.join(rel);
        fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        let payload: &[u8] = if rel == wal { wal_bytes } else { bytes };
        fs::write(&p, payload).expect("write");
    }
}

/// The WAL bytes of a fixture.
fn wal_bytes(files: &[PlantedFile], wal: &Path) -> Vec<u8> {
    files
        .iter()
        .find(|(p, _)| p == wal)
        .map(|(_, b)| b.clone())
        .expect("wal present")
}

/// Reopen the planted database and return the indices of the keys it
/// serves, in order. `Err` carries the open or read failure.
fn reopen(db: &Path, n: usize) -> Result<Vec<usize>, String> {
    let d = Db::open(db, opts()).map_err(|e| e.to_string())?;
    let mut found = Vec::new();
    for i in 0..n {
        match d.get(format!("k{i:04}").as_bytes()) {
            Ok(Some(v)) => {
                if v != format!("v{i:04}").as_bytes() {
                    return Err(format!("k{i:04} served a value it was never given"));
                }
                found.push(i);
            }
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    drop(d);
    Ok(found)
}

/// The record boundaries of a WAL, walked with the framing the format
/// defines, from `common::wal_format` rather than the engine, so a change
/// to the engine's own framing cannot quietly move this test's oracle
/// along with it.
fn boundaries(bytes: &[u8]) -> Vec<usize> {
    wal_format::record_bounds(bytes)
}

/// A cut at any offset is a torn tail: every record that survived whole
/// comes back, and nothing after the cut is invented.
#[test]
fn a_cut_at_every_offset_of_the_wal_keeps_exactly_the_whole_records_before_it() {
    let (files, wal) = fixture(12);
    let full = wal_bytes(&files, &wal);
    let bounds = boundaries(&full);
    assert_eq!(bounds.len(), 13, "twelve records tile the WAL: {bounds:?}");

    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");
    for cut in 0..=full.len() {
        plant(&files, &wal, &full[..cut], &db);
        // `saturating_sub`: a cut inside the stamp leaves no boundary at
        // or below it, and zero whole records is the right answer there
        // rather than an underflow.
        let whole = bounds
            .iter()
            .filter(|b| **b <= cut)
            .count()
            .saturating_sub(1);
        let found = reopen(&db, 12)
            .unwrap_or_else(|e| panic!("a cut at {cut} must be a torn tail, not an error: {e}"));
        let expected: Vec<usize> = (0..whole).collect();
        assert_eq!(found, expected, "cut at {cut}");
    }
    println!("cuts swept: {} offsets, all torn tails", full.len() + 1);
}

/// Damage in the middle, with whole records after it, is corruption.
/// Every bit of every record but the last is flipped in turn; each one
/// must either refuse the open or be absorbed with the full history
/// intact, and must never silently drop the records that follow it.
#[test]
fn a_flip_in_the_middle_of_the_wal_never_silently_discards_the_records_after_it() {
    let (files, wal) = fixture(8);
    let full = wal_bytes(&files, &wal);
    let bounds = boundaries(&full);
    let last_record_start = bounds[bounds.len() - 2];

    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");
    let expected: Vec<usize> = (0..8).collect();
    let (mut refused, mut absorbed) = (0usize, 0usize);
    for byte in 0..last_record_start {
        for bit in 0..8u8 {
            let mut damaged = full.clone();
            damaged[byte] ^= 1 << bit;
            plant(&files, &wal, &damaged, &db);
            match reopen(&db, 8) {
                Err(_) => refused += 1,
                Ok(found) => {
                    assert_eq!(
                        found, expected,
                        "flip at byte {byte} bit {bit} opened but dropped records \
                         out of the middle of the log",
                    );
                    absorbed += 1;
                }
            }
        }
    }
    println!(
        "mid-log flips: {} trials, {refused} refused, {absorbed} absorbed with the \
         full history intact, 0 silent losses",
        last_record_start * 8,
    );
    assert!(refused > 0, "no mid-log flip was caught at all");
}

/// Damage to the whole final record, with nothing after it, cannot be told
/// from a crash: no surviving record proves that record was synced. The
/// open keeps every record before it, drops it, and reports the drop with
/// the file and the offset, so the loss of a rotted final record is never
/// silent (the residual case the format accepts in place of padding every
/// synced group to whole sectors).
#[test]
fn a_flip_in_the_whole_final_record_drops_only_it_and_reports_the_drop() {
    let (files, wal) = fixture(6);
    let full = wal_bytes(&files, &wal);
    let bounds = boundaries(&full);
    let last_start = bounds[bounds.len() - 2];
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");

    for byte in [last_start, last_start + 4, last_start + 20, full.len() - 1] {
        let mut damaged = full.clone();
        damaged[byte] ^= 0x01;
        plant(&files, &wal, &damaged, &db);
        let reports = wal_format::TailReports::new();
        let d = Db::open(
            &db,
            opts().listeners(vec![reports.clone() as Arc<dyn regolith::EventListener>]),
        )
        .unwrap_or_else(|e| panic!("flip at {byte}: {e}"));
        let found: Vec<usize> = (0..6)
            .filter(|i| d.get(format!("k{i:04}").as_bytes()).unwrap().is_some())
            .collect();
        assert_eq!(found, (0..5).collect::<Vec<_>>(), "flip at {byte}");
        let taken = reports.taken();
        assert_eq!(taken.len(), 1, "flip at {byte} must be reported");
        assert_eq!(taken[0].offset, last_start as u64);
        assert_eq!(taken[0].discarded_bytes, (full.len() - last_start) as u64);
    }
}

/// Garbage appended after a whole log frames as a trailing partial record
/// and is discarded, and the records before it always survive. Every
/// one-byte and two-byte suffix drawn from a spread of byte values is
/// tried.
#[test]
fn garbage_appended_to_a_whole_wal_never_loses_the_records_before_it() {
    let (files, wal) = fixture(6);
    let full = wal_bytes(&files, &wal);
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");
    let expected: Vec<usize> = (0..6).collect();

    let (mut refused, mut kept, mut trials) = (0usize, 0usize, 0usize);
    for a in [0x00u8, 0x01, 0x05, 0x7f, 0x80, 0xff] {
        for b in [0x00u8, 0x05, 0xff] {
            for suffix in [vec![a], vec![a, b], vec![a, b, a, b, a, b, a, b]] {
                let mut damaged = full.clone();
                damaged.extend_from_slice(&suffix);
                plant(&files, &wal, &damaged, &db);
                trials += 1;
                match reopen(&db, 6) {
                    Ok(found) => {
                        assert_eq!(found, expected, "appending {suffix:02x?} lost writes");
                        kept += 1;
                    }
                    Err(_) => refused += 1,
                }
            }
        }
    }
    println!("appended garbage: {trials} suffixes, {kept} kept every write, {refused} refused");
}

/// A zero-length WAL, and a WAL of one byte, are both the smallest torn
/// tail there is. Neither may error.
#[test]
fn a_zero_length_and_a_one_byte_wal_both_open_clean() {
    let (files, wal) = fixture(4);
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");

    plant(&files, &wal, b"", &db);
    assert_eq!(
        reopen(&db, 4).expect("an empty WAL is an empty log"),
        Vec::<usize>::new(),
        "an empty WAL must serve nothing, not stale data",
    );

    for byte in [0x00u8, 0x01, 0x05, 0x7f, 0xff] {
        plant(&files, &wal, &[byte], &db);
        assert_eq!(
            reopen(&db, 4)
                .unwrap_or_else(|e| panic!("a one-byte WAL of {byte:#04x} must open: {e}")),
            Vec::<usize>::new(),
        );
    }
    println!("empty WAL and five one-byte WALs all opened clean");
}

/// The zero-tail exception must not widen into "trailing zeros are always
/// fine". A zero run that has whole records after it is damage in the
/// middle of the log and must refuse.
#[test]
fn a_zero_run_with_whole_records_after_it_still_refuses() {
    let (files, wal) = fixture(8);
    let full = wal_bytes(&files, &wal);
    let bounds = boundaries(&full);
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");

    // Zero the whole of record 3, leaving records 4..8 whole behind it.
    let (from, to) = (bounds[3], bounds[4]);
    let mut damaged = full.clone();
    damaged[from..to].fill(0);
    plant(&files, &wal, &damaged, &db);

    let err = reopen(&db, 8).expect_err("a zero run followed by whole records must refuse");
    println!("zero run with records after it refused: {err}");
}

/// A length field that runs past the end of the file, with whole records
/// still behind it: the damaged record's length cannot be trusted to find
/// them, and the records after it must still be found, since they prove
/// the damaged one was synced. The open refuses rather than discard them.
#[test]
fn a_mangled_length_field_with_a_tiling_remainder_refuses_rather_than_discarding() {
    let (files, wal) = fixture(8);
    let full = wal_bytes(&files, &wal);
    let bounds = boundaries(&full);
    let root = TempDir::new().expect("tempdir");
    let db = root.path().join("db");

    // Overstate record 2's length so its frame runs past end-of-file
    // while records 3..8 still sit whole behind it.
    let start = bounds[2];
    let mut damaged = full.clone();
    damaged[start..start + 4].copy_from_slice(&(full.len() as u32).to_le_bytes());
    plant(&files, &wal, &damaged, &db);

    let err = reopen(&db, 8)
        .expect_err("a length that overruns the file with whole records after it must refuse");
    println!("mangled length with records after it refused: {err}");
    assert!(
        err.contains(&format!("damaged at offset {start}")),
        "the refusal must say where, got: {err}",
    );
}
