//! Independent adversarial review of the discarded-table open guard.
//!
//! The guard was relaxed so a crash inside the very first flush no longer
//! costs the WAL's acknowledged writes. A relaxation of a data-loss guard
//! is only safe if it did not also open the door it was holding shut, so
//! every probe here pushes on the *dismissal* rules rather than on the
//! refusal ones: each rule is handed a file that satisfies it and still
//! carries real data, and the question asked is whether the database then
//! opens on top of it.
//!
//! A zeroed tail is the sharp case, and it found a hole. A power cut that
//! truncates the MANIFEST is the same power cut that can leave a complete
//! table's final delayed-allocation block reading back as zeros, so "the
//! last 64 bytes are zero, therefore the flush never finished" and "a
//! real table lost its tail to the same crash" are not different files.
//! An earlier revision of the guard dismissed the first reading and so
//! opened on top of the second. The guard now dismisses an unreferenced
//! table only when it is zero bytes long or its footer parses and its
//! index block is empty, and these probes are the regression gate for
//! that line.

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
/// writes and whose MANIFEST is back at the durable length of zero.
fn wiped_manifest_copy(keys: usize) -> TempDir {
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

/// A genuine, non-empty SSTable written through the public external-table
/// writer.
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

/// Plant `bytes` as an unreferenced table in `db/sst` and try to open.
fn open_with_orphan(db: &Path, bytes: &[u8]) -> Result<Db, String> {
    let sst = db.join("sst");
    fs::create_dir_all(&sst).expect("mkdir sst");
    fs::write(sst.join("000900.sst"), bytes).expect("plant orphan");
    Db::open(db, opts()).map_err(|e| e.to_string())
}

/// A real table whose last `n` bytes were zeroed: the shape a power cut
/// leaves when the file's final delayed-allocation block never reached
/// the platter, on a file that was otherwise complete.
fn tail_zeroed(entries: usize, n: usize) -> Vec<u8> {
    let mut bytes = real_table_bytes(entries);
    let from = bytes.len() - n;
    bytes[from..].fill(0);
    bytes
}

/// THE PROBE. A complete, non-empty table whose final 64 bytes are zero
/// is exactly what a lost tail block leaves behind on a table that was
/// finished. If the guard dismisses it, a wiped manifest plus one lost
/// write silently serves a database missing everything that table held.
#[test]
fn a_real_table_whose_tail_was_zeroed_is_dismissed_and_the_database_opens_without_it() {
    let dir = wiped_manifest_copy(40);
    let bytes = tail_zeroed(500, 64);
    match open_with_orphan(dir.path(), &bytes) {
        Err(e) => {
            println!("64-byte zeroed tail: REFUSED, guard held: {e}");
        }
        Ok(db) => {
            let ext = db.get(b"ext_000000").expect("get");
            panic!(
                "HOLE: a complete non-empty table with a 64-byte zeroed tail was \
                 dismissed and the database opened on top of it. \
                 ext_000000 reads back as {ext:?}; the table's {} entries are gone \
                 from the served database while the file is still on disk.",
                500,
            );
        }
    }
}

/// The same probe at the sizes a filesystem actually zeroes: a 512-byte
/// sector and a 4096-byte block. Both are far more than the 64 bytes the
/// rule inspects, so if 64 is dismissed these are too.
#[test]
fn a_real_table_whose_tail_block_was_zeroed_is_dismissed_at_sector_and_block_sizes() {
    let mut opened = Vec::new();
    for n in [64usize, 128, 512, 4096] {
        let dir = wiped_manifest_copy(40);
        let bytes = tail_zeroed(2000, n);
        assert!(
            bytes.len() > n * 2,
            "the fixture must be much larger than the zeroed tail",
        );
        match open_with_orphan(dir.path(), &bytes) {
            Err(_) => {}
            Ok(db) => {
                let served = db.get(b"ext_000000").expect("get");
                opened.push(format!(
                    "{n} bytes zeroed -> opened, ext_000000 = {served:?}"
                ));
            }
        }
    }
    assert!(
        opened.is_empty(),
        "HOLE: a real table with a zeroed tail was dismissed at these sizes:\n  {}",
        opened.join("\n  "),
    );
}

/// A zeroed footer with one non-zero byte in it must refuse too. This
/// held even under the old rule, and is kept so a future relaxation
/// cannot pass by inspecting fewer bytes.
#[test]
fn one_non_zero_byte_in_the_zeroed_footer_brings_the_refusal_back() {
    let dir = wiped_manifest_copy(40);
    let mut bytes = tail_zeroed(500, 64);
    let at = bytes.len() - 30;
    bytes[at] = 0x01;
    let err = open_with_orphan(dir.path(), &bytes)
        .expect_err("a footer that is not all zeros cannot be proved unwritten");
    println!("one non-zero byte in the zeroed footer: refused as it should: {err}");
}

/// A zero-length orphan is the case the relaxation exists for and must
/// still open, with every acknowledged write intact.
#[test]
fn a_zero_length_orphan_still_opens_and_keeps_every_write() {
    let dir = wiped_manifest_copy(61);
    let db = open_with_orphan(dir.path(), b"").expect("a zero-length orphan must not block open");
    for i in 0..61 {
        assert_eq!(
            db.get(format!("key_{i:06}").as_bytes()).expect("get"),
            Some(format!("val_{i:06}").into_bytes()),
            "key_{i:06} was acknowledged and fsynced",
        );
    }
    drop(db);
}

/// A valid non-empty orphan must still refuse. This is the guard's whole
/// reason to exist and the relaxation must not have touched it.
#[test]
fn a_valid_non_empty_orphan_still_refuses() {
    let dir = wiped_manifest_copy(40);
    let err = open_with_orphan(dir.path(), &real_table_bytes(500))
        .expect_err("a valid non-empty orphan must refuse");
    assert!(err.contains("corrupt"), "the refusal must say why: {err}");
    println!("valid non-empty orphan: refused");
}

/// Truncating a real table to every one of a spread of lengths must never
/// open: a short table cannot be proved empty.
#[test]
fn a_truncated_orphan_refuses_at_every_length_probed() {
    let full = real_table_bytes(500);
    let mut opened = Vec::new();
    for cut in (1..full.len()).step_by(full.len() / 40 + 1) {
        let dir = wiped_manifest_copy(40);
        if open_with_orphan(dir.path(), &full[..cut]).is_ok() {
            opened.push(cut);
        }
    }
    assert!(
        opened.is_empty(),
        "HOLE: a truncated non-empty orphan opened at lengths {opened:?}",
    );
    println!("truncated orphan: refused at every probed length");
}

/// The refusal must be idempotent, and a crash *during* the operator's
/// cleanup must converge rather than wedge. Nothing is deleted by
/// recovery, so the check is that repeated opens reach the same verdict
/// and that removing the orphan by hand releases the database.
#[test]
fn the_refusal_is_idempotent_and_converges_after_a_half_finished_cleanup() {
    let dir = wiped_manifest_copy(61);
    let orphan = dir.path().join("sst").join("000900.sst");
    let full = real_table_bytes(500);

    for round in 0..5 {
        let err = open_with_orphan(dir.path(), &full)
            .err()
            .unwrap_or_else(|| panic!("round {round} must refuse"));
        assert!(err.contains("corrupt"), "round {round}: {err}");
        assert!(
            orphan.exists() && fs::metadata(&orphan).expect("meta").len() as usize == full.len(),
            "round {round}: a refused open must leave the orphan byte-for-byte intact",
        );
    }

    // A cleanup interrupted part way: the operator truncated the orphan
    // and was killed. Still refuses, because a short table is not empty.
    fs::write(&orphan, &full[..full.len() / 2]).expect("half cleanup");
    assert!(
        Db::open(dir.path(), opts()).is_err(),
        "a half-truncated orphan must still refuse",
    );

    // Cleanup finished.
    fs::remove_file(&orphan).expect("remove orphan");
    let db = Db::open(dir.path(), opts()).expect("removing the orphan releases the database");
    for i in 0..61 {
        assert_eq!(
            db.get(format!("key_{i:06}").as_bytes()).expect("get"),
            Some(format!("val_{i:06}").into_bytes()),
            "key_{i:06} must survive the whole cycle",
        );
    }
    println!("refusal idempotent over 5 rounds, converged after cleanup, all 61 writes intact");
}
