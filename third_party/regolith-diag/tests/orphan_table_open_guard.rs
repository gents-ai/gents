//! Cross-fix probe over the open guard's rule that an orphan table
//! recording no entry is a crash artifact.
//!
//! The rule reads the footer's `num_entries` and `range_tombstone_size`.
//! Every format regolith reads carries a footer checksum, so a damaged
//! footer is refused before the guard ever sees it, and a forged entry
//! count cannot talk the open into discarding a table that holds data.

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
    fs::create_dir_all(to).expect("mkdir");
    for e in fs::read_dir(from).expect("read_dir").flatten() {
        if e.file_name() == "LOCK" {
            continue;
        }
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if src.is_dir() {
            copy_tree(&src, &dst);
        } else {
            fs::copy(&src, &dst).expect("copy");
        }
    }
}

/// A directory whose WAL holds `keys` fsynced writes and whose MANIFEST
/// is back at the zero length a cut before the first `AddFile` leaves.
fn wal_only_copy(keys: usize) -> TempDir {
    let live = TempDir::new().expect("tempdir");
    let db = Db::open(live.path(), opts()).expect("open");
    for i in 0..keys {
        db.put(format!("key_{i:06}").as_bytes(), b"v").expect("put");
    }
    let copy = TempDir::new().expect("tempdir");
    copy_tree(live.path(), copy.path());
    db.close().expect("close");
    drop(db);
    fs::write(copy.path().join("MANIFEST"), b"").expect("wipe manifest");
    copy
}

/// Zero the footer's `num_entries` and `range_tombstone_size`, the two
/// fields the guard reads. Footer offsets are documented in
/// `src/engine/sstable.rs`: the seven fixed fields start at the footer's
/// first byte in every version, and the magic is always the last eight.
fn lie_about_the_contents(bytes: &mut [u8], footer_size: usize) {
    let start = bytes.len() - footer_size;
    bytes[start + 8..start + 16].fill(0); // range_tombstone_size
    bytes[start + 48..start + 56].fill(0); // num_entries
}

fn modern_table(entries: usize) -> Vec<u8> {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("modern.sst");
    let mut w = SstFileWriter::create(&path, &Options::default()).expect("create");
    for i in 0..entries {
        w.put(format!("ext_{i:06}").as_bytes(), b"v").expect("put");
    }
    w.finish().expect("finish");
    fs::read(&path).expect("read")
}

#[test]
fn a_modern_table_that_lies_about_its_entry_count_is_still_refused() {
    let mut bytes = modern_table(50);
    // Either footer will do: v5 is the flat REGOSST magic and v6 the
    // partitioned one, and both share the 72-byte layout the offsets
    // below assume. What the probe needs is a footer the reader *can*
    // verify, so that refusing the forgery is the checksum doing its job
    // rather than the version byte.
    let magic = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().expect("8"));
    assert!(
        matches!(
            magic,
            0x4C41524B_53535403 | 0x4C41524B_53535404 | 0x5245474F_53535405 | 0x5245474F_53535406
        ),
        "this probe needs a checksummed table, got {magic:#018x}",
    );
    lie_about_the_contents(&mut bytes, 72);

    let dir = wal_only_copy(10);
    fs::write(dir.path().join("sst").join("000042.sst"), &bytes).expect("write orphan");
    let err = match Db::open(dir.path(), opts()) {
        Err(e) => e.to_string(),
        Ok(_) => {
            panic!("a checksummed footer with a forged entry count must not pass the guard")
        }
    };
    assert!(err.contains("000042.sst"), "got: {err}");
    println!("checksummed lying footer: refused ({err})");
}
