//! Adversarial probes for the discarded-table open guard (the discarded-table open guard).
//!
//! The guard's job is a two-sided one, and both sides are data loss when
//! they go wrong:
//!
//! * A manifest that lost its tail while unreferenced tables still hold
//!   the database must **refuse**, so the tables survive for repair.
//! * A zero-length table left by a crash inside the very first flush
//!   holds nothing, while the fsynced WAL holds every acknowledged
//!   write, so it must **open**.
//!
//! Every test here builds a real database, wipes the MANIFEST to the
//! durable length a power cut before the first `AddFile` leaves, and
//! then varies only the contents of `sst/`.

use std::fs;
use std::path::Path;

use regolith::{Db, DurabilityMode, Options, SstFileWriter};
use tempfile::TempDir;

fn opts() -> Options {
    Options::default()
        .write_buffer_size(8 * 1024 * 1024)
        .durability(DurabilityMode::Immediate)
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create_dir_all");
    for entry in fs::read_dir(from).expect("read_dir").flatten() {
        let name = entry.file_name();
        if name == "LOCK" {
            continue;
        }
        let src = entry.path();
        let dst = to.join(&name);
        if src.is_dir() {
            copy_tree(&src, &dst);
        } else {
            fs::copy(&src, &dst).expect("copy");
        }
    }
}

/// A database directory whose WAL holds `keys` acknowledged, fsynced
/// writes and whose MANIFEST is back at the durable length of zero: the
/// exact shape of a power cut before the first flush committed.
fn wal_only_copy(keys: usize) -> TempDir {
    let live = TempDir::new().expect("tempdir");
    let db = Db::open(live.path(), opts()).expect("open");
    for i in 0..keys {
        db.put(
            format!("key_{i:06}").as_bytes(),
            format!("val_{i:06}").as_bytes(),
        )
        .expect("put");
    }
    let copy = TempDir::new().expect("tempdir");
    copy_tree(live.path(), copy.path());
    db.close().expect("close");
    fs::write(copy.path().join("MANIFEST"), b"").expect("wipe manifest");
    copy
}

/// Write a genuine, non-empty SSTable through the public external-table
/// writer, then hand back its bytes.
fn real_table_bytes(entries: usize) -> Vec<u8> {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("external.sst");
    let mut w = SstFileWriter::create(&path, &Options::default()).expect("create");
    for i in 0..entries {
        w.put(
            format!("ext_{i:06}").as_bytes(),
            format!("extval_{i:06}").as_bytes(),
        )
        .expect("put");
    }
    let meta = w.finish().expect("finish");
    assert!(meta.num_entries > 0, "the fixture table must hold entries");
    fs::read(&path).expect("read")
}

fn keys_readable(db: &Db, keys: usize) {
    for i in 0..keys {
        assert_eq!(
            db.get(format!("key_{i:06}").as_bytes()).expect("get"),
            Some(format!("val_{i:06}").into_bytes()),
            "key_{i:06} was acknowledged and fsynced; it must come back",
        );
    }
}

/// A **valid, non-empty** unreferenced table is the case the guard
/// exists for. Relaxing it for the zero-length orphan must not have
/// relaxed it here: opening would present a database missing everything
/// that table holds.
#[test]
fn a_valid_non_empty_orphan_table_still_refuses_the_open() {
    let dir = wal_only_copy(20);
    let orphan = dir.path().join("sst").join("000007.sst");
    fs::write(&orphan, real_table_bytes(50)).expect("write orphan");

    let err = match Db::open(dir.path(), opts()) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a valid non-empty orphan table must refuse the open"),
    };
    assert!(
        err.contains("000007.sst"),
        "the refusal must name the file, got: {err}",
    );
    assert!(
        orphan.exists(),
        "a refused open must leave the file on disk"
    );
    println!("valid non-empty orphan: refused with {err}");
}

/// A zero-length orphan is a crash artifact and must not block the open,
/// and every acknowledged write must come back out of the WAL.
#[test]
fn a_zero_length_orphan_table_opens_and_keeps_every_acknowledged_write() {
    let dir = wal_only_copy(61);
    fs::write(dir.path().join("sst").join("000003.sst"), b"").expect("write orphan");
    let db = Db::open(dir.path(), opts()).expect("a zero-length orphan must not block the open");
    keys_readable(&db, 61);
    println!("zero-length orphan: opened, 61 acknowledged writes recovered");
}

/// Recovery deletes nothing, so repeating it converges.
///
/// The guard dismisses a zero-length orphan rather than refusing the open,
/// and recovery from the wiped manifest leaves it exactly as it found it:
/// a manifest that did not replay cleanly cannot say what is unreferenced.
///
/// Once recovery has rebuilt the manifest, a later writable open's orphan
/// sweep removes the file where the directory lock excludes other
/// processes, since no other writer can be flushing into that id. Where
/// the lock does not, the file is never touched.
#[test]
fn an_open_that_dismissed_a_zero_length_orphan_reaches_the_same_verdict_every_time() {
    let dir = wal_only_copy(61);
    let orphan = dir.path().join("sst").join("000003.sst");
    fs::write(&orphan, b"").expect("write orphan");
    let swept = regolith::env::Env::capabilities(&regolith::env::StdEnv::new()).file_lock;

    for attempt in 0..3 {
        let db = Db::open(dir.path(), opts()).unwrap_or_else(|e| {
            panic!("attempt {attempt}: a zero-length orphan must not block the open: {e}")
        });
        keys_readable(&db, 61);
        db.close().expect("close");
        drop(db);

        if swept && attempt > 0 {
            assert!(
                !orphan.exists(),
                "attempt {attempt}: a locked writable open must sweep the orphan",
            );
        } else {
            let len = fs::metadata(&orphan)
                .unwrap_or_else(|e| panic!("attempt {attempt}: recovery deleted the orphan: {e}"))
                .len();
            assert_eq!(
                len, 0,
                "attempt {attempt}: without a process lock, recovery must leave the orphan as it found it",
            );
        }
    }
}

/// A table truncated to any non-zero prefix cannot be proved empty, so
/// the guard must refuse at every one of those lengths. Sweeping the
/// whole file catches a relaxation that dismisses anything whose footer
/// merely fails to parse.
#[test]
fn a_truncated_but_non_empty_orphan_table_refuses_at_every_length() {
    let bytes = real_table_bytes(50);
    let mut bad = Vec::new();
    let mut refused = 0usize;
    // Sweep a bounded sample of lengths plus every length near both ends,
    // where the footer and the first data block live.
    let mut lengths: Vec<usize> = (1..=64.min(bytes.len())).collect();
    lengths.extend((bytes.len().saturating_sub(96)..bytes.len()).filter(|n| *n > 0));
    lengths.extend((1..bytes.len()).step_by(37));
    lengths.sort_unstable();
    lengths.dedup();

    for &n in &lengths {
        let dir = wal_only_copy(4);
        fs::write(dir.path().join("sst").join("000009.sst"), &bytes[..n]).expect("write orphan");
        match Db::open(dir.path(), opts()) {
            Err(_) => refused += 1,
            Ok(db) => {
                // Opening is only defensible if the truncated file
                // genuinely cannot hold data. Then the WAL must still
                // deliver every acknowledged write.
                for i in 0..4 {
                    let got = db.get(format!("key_{i:06}").as_bytes()).expect("get");
                    if got != Some(format!("val_{i:06}").into_bytes()) {
                        bad.push(format!(
                            "orphan truncated to {n} bytes: opened but key_{i:06} is gone",
                        ));
                    }
                }
                bad.push(format!(
                    "orphan truncated to {n} of {} bytes: opened instead of refusing; a \
                     non-empty table that will not parse cannot be proved discardable",
                    bytes.len(),
                ));
            }
        }
    }
    println!(
        "truncated orphan sweep: {} lengths, {refused} refused, {} violations",
        lengths.len(),
        bad.len(),
    );
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

/// The guard must converge: a refused open changes nothing on disk, so
/// repeating it gives the same verdict, and once the operator removes
/// the suspect file the same directory opens and serves every write.
#[test]
fn a_refused_open_is_idempotent_and_converges_once_the_orphan_is_removed() {
    let dir = wal_only_copy(30);
    let orphan = dir.path().join("sst").join("000011.sst");
    fs::write(&orphan, real_table_bytes(25)).expect("write orphan");

    let before = fs::read(&orphan).expect("read orphan");
    let mut messages = Vec::new();
    for attempt in 0..5 {
        match Db::open(dir.path(), opts()) {
            Err(e) => messages.push(e.to_string()),
            Ok(_) => panic!("attempt {attempt}: the guard stopped refusing without any change"),
        }
        assert_eq!(
            fs::read(&orphan).expect("read orphan"),
            before,
            "attempt {attempt}: a refused open modified the suspect table",
        );
    }
    assert!(
        messages.windows(2).all(|w| w[0] == w[1]),
        "the refusal message changed between attempts: {messages:?}",
    );

    fs::remove_file(&orphan).expect("remove orphan");
    let db = Db::open(dir.path(), opts()).expect("open after the orphan is removed");
    keys_readable(&db, 30);
    println!("idempotence: 5 identical refusals, then a clean open with all 30 writes");
}

/// Several orphans at once: the guard must name them and must not let a
/// zero-length one mask a real one.
#[test]
fn a_zero_length_orphan_does_not_mask_a_real_one() {
    let dir = wal_only_copy(10);
    let sst = dir.path().join("sst");
    for id in [1u32, 2, 3] {
        fs::write(sst.join(format!("{id:06}.sst")), b"").expect("write empty orphan");
    }
    fs::write(sst.join("000012.sst"), real_table_bytes(30)).expect("write real orphan");

    let err = match Db::open(dir.path(), opts()) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a real orphan hidden among empty ones must still refuse the open"),
    };
    assert!(
        err.contains("000012.sst"),
        "the refusal must name the real orphan, got: {err}",
    );
    println!("mixed orphans: refused, naming 000012.sst");
}
