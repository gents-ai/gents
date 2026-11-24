//! Adversarial probes on streaming WAL replay.
//!
//! Recovery must be exactly as strict as it was before the replay path
//! became an iterator: a corrupt log fails the open loud, an intact log
//! recovers every acknowledged write, and no corruption is ever silently
//! skipped. "Opened with the wrong data" is the failure these tests hunt.
#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use regolith::{Db, Options, WriteOptions};
use tempfile::TempDir;

mod common;

fn opts() -> Options {
    Options::default()
        // Large enough that nothing flushes: everything under test stays
        // in the WAL, which is the point.
        .write_buffer_size(8 * 1024 * 1024)
}

fn wal_files(db_dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(db_dir.join("wal"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("log"))
        .collect();
    entries.sort();
    entries
}

/// Fill `dir` with `count` durable writes and return the WAL bytes.
///
/// The log is read while the database is still open: `close` flushes
/// the memtable to an SSTable and leaves an empty WAL behind, which is
/// exactly the state these tests must not attack.
fn seed(dir: &TempDir, count: usize) -> Vec<u8> {
    let db = Db::open(dir.path(), opts()).unwrap();
    let wo = WriteOptions {
        sync: true,
        ..WriteOptions::default()
    };
    for i in 0..count {
        db.put_opt(
            &wo,
            format!("k{i:04}").as_bytes(),
            format!("v{i:04}").as_bytes(),
        )
        .unwrap();
    }
    let files = wal_files(dir.path());
    assert_eq!(files.len(), 1, "expected exactly one WAL to attack");
    let bytes = fs::read(&files[0]).unwrap();
    assert!(!bytes.is_empty(), "the seeded WAL is empty");
    drop(db);
    bytes
}

fn expected(count: usize) -> Vec<(Vec<u8>, Vec<u8>)> {
    (0..count)
        .map(|i| {
            (
                format!("k{i:04}").into_bytes(),
                format!("v{i:04}").into_bytes(),
            )
        })
        .collect()
}

/// A completely empty WAL file is not corruption: it is what a crash
/// between `create` and the first append leaves behind.
#[test]
fn a_zero_length_wal_opens_clean() {
    let dir = TempDir::new().unwrap();
    {
        let db = Db::open(dir.path(), opts()).unwrap();
        for i in 0..200 {
            db.put(format!("s{i:04}").as_bytes(), b"v").unwrap();
        }
        db.compact_range(None, None).wait().unwrap();
        db.close().unwrap();
    }
    for path in wal_files(dir.path()) {
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(0)
            .unwrap();
    }
    // A stray zero-length WAL with a fresh id, as an interrupted rotation
    // would leave.
    fs::write(dir.path().join("wal").join("wal_009999.log"), b"").unwrap();

    let db = Db::open(dir.path(), opts()).unwrap();
    for i in 0..200 {
        assert_eq!(
            db.get(format!("s{i:04}").as_bytes()).unwrap(),
            Some(b"v".to_vec()),
            "flushed data lost after a zero-length WAL"
        );
    }
}

/// Truncating the log at every byte offset must either open with the
/// exact prefix of writes the log still frames, or fail loud. It must
/// never open with data that was never written, and it must never lose
/// a record the surviving bytes still describe.
#[test]
fn every_truncation_offset_either_fails_or_recovers_a_clean_prefix() {
    const COUNT: usize = 24;
    let source = {
        let dir = TempDir::new().unwrap();
        seed(&dir, COUNT)
    };
    let full = source.len();
    let all = expected(COUNT);

    let mut opened = 0usize;
    let mut failed = 0usize;
    for cut in 0..full {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("wal")).unwrap();
        fs::create_dir_all(dir.path().join("sst")).unwrap();
        // A fresh DB writes its manifest; seed one and then swap in the
        // truncated log.
        {
            let db = Db::open(dir.path(), opts()).unwrap();
            db.close().unwrap();
        }
        for path in wal_files(dir.path()) {
            fs::remove_file(path).unwrap();
        }
        fs::write(
            dir.path().join("wal").join("wal_000001.log"),
            &source[..cut],
        )
        .unwrap();

        match Db::open(dir.path(), opts()) {
            Err(_) => failed += 1,
            Ok(db) => {
                opened += 1;
                let mut seen = 0usize;
                let mut hole = false;
                for (k, v) in &all {
                    match db.get(k).unwrap() {
                        Some(got) => {
                            assert_eq!(&got, v, "cut={cut}: recovered the wrong value for {k:?}");
                            assert!(!hole, "cut={cut}: recovered a hole then a later record");
                            seen += 1;
                        }
                        None => hole = true,
                    }
                }
                assert!(
                    seen <= COUNT,
                    "cut={cut}: recovered {seen} records from a {COUNT}-record log"
                );
            }
        }
    }
    println!("truncation offsets: {full} tried, {opened} opened, {failed} failed loud");
    assert_eq!(opened + failed, full);
    assert!(opened > 0, "no truncation offset recovered anything");
    // Deliberately not `failed > 0`. A cut is a torn tail, and for the
    // only log in the database every cut lands on a whole prefix of the
    // write history, which is exactly the durability rule. A cut inside
    // the format stamp is the same case: no record in that log can have
    // been acknowledged, because the `fsync` that would have
    // acknowledged one flushes the stamp written before it. Requiring
    // some offset to be *refused* would be requiring recovery to fail
    // on a state it can legitimately recover. Damage that is not a torn
    // tail, whole records after a hole, is covered by
    // `a_flip_in_the_middle_of_the_wal_never_silently_discards_the_records_after_it`
    // and by the earlier-log rule in `adversarial_wal`.
}

/// Flip one byte at every offset in the log. The open must either fail
/// or return exactly the data that was written: a silently wrong value
/// is the failure this hunts.
#[test]
fn no_single_byte_flip_can_produce_silently_wrong_data() {
    const COUNT: usize = 12;
    let source = {
        let dir = TempDir::new().unwrap();
        seed(&dir, COUNT)
    };
    let all = expected(COUNT);

    let mut opened_intact = 0usize;
    let mut opened_short = 0usize;
    let mut failed = 0usize;
    for offset in 0..source.len() {
        let mut bytes = source.clone();
        bytes[offset] ^= 0xFF;

        let dir = TempDir::new().unwrap();
        {
            let db = Db::open(dir.path(), opts()).unwrap();
            db.close().unwrap();
        }
        for path in wal_files(dir.path()) {
            fs::remove_file(path).unwrap();
        }
        fs::write(dir.path().join("wal").join("wal_000001.log"), &bytes).unwrap();

        match Db::open(dir.path(), opts()) {
            Err(_) => failed += 1,
            Ok(db) => {
                let mut seen = 0usize;
                for (k, v) in &all {
                    if let Some(got) = db.get(k).unwrap() {
                        assert_eq!(
                            &got, v,
                            "offset={offset}: a single byte flip produced a wrong value for {k:?}"
                        );
                        seen += 1;
                    }
                }
                // Nothing may appear that was never written.
                let scanned = db.scan(None, None).unwrap();
                assert!(
                    scanned.len() <= COUNT,
                    "offset={offset}: recovery invented {} entries",
                    scanned.len() - COUNT
                );
                for (k, v) in &scanned {
                    let found = all.iter().find(|(ek, _)| ek == k);
                    assert!(
                        found.is_some(),
                        "offset={offset}: recovery invented key {k:?}"
                    );
                    assert_eq!(
                        &found.unwrap().1,
                        v,
                        "offset={offset}: wrong value for {k:?}"
                    );
                }
                if seen == COUNT {
                    opened_intact += 1;
                } else {
                    opened_short += 1;
                }
            }
        }
    }
    println!(
        "byte flips: {} tried, {failed} failed loud, {opened_intact} opened with everything, \
         {opened_short} opened with a subset",
        source.len()
    );
    assert!(failed > 0, "no byte flip was detected");
}

/// A record header claiming far more bytes than the file holds must be
/// refused without trying to allocate for it.
#[test]
fn an_oversized_length_header_is_refused_not_allocated() {
    const COUNT: usize = 4;
    let source = {
        let dir = TempDir::new().unwrap();
        seed(&dir, COUNT)
    };

    let mut bytes = source.clone();
    // The first record header sits after the format stamp, not at offset
    // 0. Inflating the stamp instead would be a different failure with a
    // different rule, so the probe would pass on the wrong reason.
    const STAMP: usize = common::wal_format::STAMP_LEN;
    bytes[STAMP..STAMP + 4].copy_from_slice(&u32::MAX.to_le_bytes());

    let dir = TempDir::new().unwrap();
    {
        let db = Db::open(dir.path(), opts()).unwrap();
        db.close().unwrap();
    }
    for path in wal_files(dir.path()) {
        fs::remove_file(path).unwrap();
    }
    fs::write(dir.path().join("wal").join("wal_000001.log"), &bytes).unwrap();

    let err = Db::open(dir.path(), opts()).expect_err("a 4 GiB length header must be refused");
    // The writes were synced one by one, so the records after the damaged
    // one prove it durable: the open is refused, naming where.
    let text = format!("{err:?}");
    assert!(
        text.contains(&format!("damaged at offset {STAMP}")),
        "unexpected error for an oversized length header: {err:?}"
    );
}

/// Corruption in an *earlier* WAL must fail the open, not be skipped in
/// favour of the later one that replays cleanly. The two logs are one
/// seeded log split at a record boundary, as a rotation there would have
/// left them (`wal_format::split_at`); the earlier one was complete when
/// the newer one began, so any damage in it is loss.
#[test]
fn corruption_in_an_earlier_wal_is_not_skipped() {
    const COUNT: usize = 64;
    let dir = TempDir::new().unwrap();
    let bytes = seed(&dir, COUNT);
    let only = wal_files(dir.path()).pop().expect("one WAL");
    let bounds = common::wal_format::record_bounds(&bytes);
    let (mut earlier, later) = common::wal_format::split_at(&bytes, bounds[COUNT / 2]);
    fs::remove_file(&only).unwrap();
    let target = dir.path().join("wal").join("wal_000001.log");
    fs::write(dir.path().join("wal").join("wal_000002.log"), &later).unwrap();

    // The control: the split alone serves every write.
    fs::write(&target, &earlier).unwrap();
    let manifest = dir.path().join("MANIFEST");
    let before_control = fs::read(&manifest).unwrap();
    {
        let db = Db::open(dir.path(), opts()).expect("the split alone opens");
        assert_eq!(db.scan(None, None).unwrap(), expected(COUNT));
    }
    // That open rewrote the logs and recorded the pair as retired, so lay
    // the pair and the manifest down again, the pair damaged.
    fs::write(&manifest, &before_control).unwrap();
    for path in wal_files(dir.path()) {
        fs::remove_file(path).unwrap();
    }
    let at = earlier.len() / 2;
    earlier[at] ^= 0xFF;
    fs::write(&target, &earlier).unwrap();
    fs::write(dir.path().join("wal").join("wal_000002.log"), &later).unwrap();

    let err = Db::open(dir.path(), opts()).expect_err("corrupt WAL must fail the open");
    assert!(
        format!("{err}").contains("wal_000001.log"),
        "the refusal must name the damaged log: {err}"
    );
    assert!(
        target.exists(),
        "a failed open must leave the corrupt WAL on disk for inspection"
    );
}

/// A failed replay must leave the on-disk state untouched, so a second
/// attempt after the corruption is repaired recovers everything.
#[test]
fn a_failed_replay_is_not_destructive() {
    const COUNT: usize = 64;
    let dir = TempDir::new().unwrap();
    let good = seed(&dir, COUNT);
    let path = wal_files(dir.path())[0].clone();

    let mut broken = good.clone();
    let at = broken.len() / 2;
    broken[at] ^= 0xFF;
    fs::write(&path, &broken).unwrap();
    let _ = Db::open(dir.path(), opts()).expect_err("corrupt WAL must fail the open");
    // Failing again must be just as safe.
    let _ = Db::open(dir.path(), opts()).expect_err("corrupt WAL must fail the open twice");

    fs::write(&path, &good).unwrap();
    let db = Db::open(dir.path(), opts()).unwrap();
    for (k, v) in expected(COUNT) {
        assert_eq!(
            db.get(&k).unwrap(),
            Some(v),
            "a repaired WAL must recover everything the failed attempts saw"
        );
    }
}
