//! `tests/fixtures/v0_1_9_buried_delete`: a database the release tagged
//! `v0.1.9` left with an ingested table in its bottom level whose range
//! delete is newer than keys it covers in level 1, a level every read
//! consults first. The table's recorded range meets no other table of its
//! level, so no level overlaps: nothing an open moves (E14) touches it
//! (E37).
//!
//! Written by that release, not by this version and not by hand: the
//! defect is in where that release put an ingested table. With the
//! concatenating merge operator `concat` of `overlap_fixture.rs`,
//! `Immediate` durability, level 0 compacted at one table, and every put's
//! value `"<tag>:"` followed by 200 bytes derived from the tag
//! (`overlap_fixture::value`):
//!
//! 1. `put w x y = old`, `flush`, `compact_range(None, None)`: one table in
//!    level 6;
//! 2. reopened with a 512 byte level 1 budget and a worker: `put c d =
//!    old`, `merge a1 x`, `flush`. The worker compacted it to level 1;
//! 3. reopened with no worker: `ingest_external_files` of a table another
//!    0.1.9 database flushed after `put m n p = ingested` and
//!    `delete_range [a, g)`. Its keys `m..p` meet no table, so 0.1.9 put it
//!    in level 6, recorded as `a..p`, which meets no table of level 6: its
//!    range delete sits below `a1`, `c` and `d` in level 1;
//! 4. reopened with no worker: `put d = new`, `put e = l0`, `flush`; then
//!    `put f = wal`, `delete x`, `merge a1 y`, and the process exited without
//!    closing, so those three writes are only in the format 1 log.
//!
//! That leaves one table in level 0, one in level 1 and two in level 6.
//! 0.1.9 itself answered `get(c) = old` and `get(a1) = xy`, while its scan,
//! which gathers every range delete first, left `c` out and gave `a1 = y`.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use regolith::Db;
use tempfile::TempDir;

use super::overlap_fixture::{assert_db_reads_of, value};

/// Range deletes the first open of the fixture copies: the ingested
/// table's one.
pub const COPIED: u64 = 1;

/// Every key the fixture's steps name, and two they do not.
pub const KEYS: [&[u8]; 14] = [
    b"a", b"a1", b"b", b"c", b"d", b"e", b"f", b"g", b"m", b"n", b"p", b"w", b"x", b"y",
];

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A copy of the fixture at `db_dir`, with the lock file every database
/// directory has and the fixture leaves out.
pub fn copy_to(db_dir: &Path) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v0_1_9_buried_delete");
    copy_tree(&fixture, db_dir);
    fs::write(db_dir.join("LOCK"), []).unwrap();
}

/// A fresh copy of the fixture in a directory of its own.
pub fn fresh() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let db_dir = tmp.path().join("db");
    copy_to(&db_dir);
    (tmp, db_dir)
}

/// The newest version of every key, by the sequence each write took: the
/// ingested range delete over `[a, g)` is newer than `merge a1 x`, `c` and
/// the first `d`, and older than `merge a1 y`, the second `d`, `e` and `f`.
pub fn state() -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut state = BTreeMap::new();
    let mut put = |key: &[u8], value: Vec<u8>| state.insert(key.to_vec(), value);
    put(b"a1", b"y".to_vec());
    put(b"d", value("new"));
    put(b"e", value("l0"));
    put(b"f", value("wal"));
    put(b"m", value("ingested"));
    put(b"n", value("ingested"));
    put(b"p", value("ingested"));
    put(b"w", value("old"));
    put(b"y", value("old"));
    state
}

/// Range deletes `db` copied in front of older keys when it opened.
pub fn copied(db: &Db) -> u64 {
    db.get_int_property("regolith.manifest.range_deletes_copied")
        .unwrap()
}

/// Every way a caller reads the fixture's keys, checked against `expected`.
pub fn assert_db_reads(read: &str, db: &Db, expected: &BTreeMap<Vec<u8>, Vec<u8>>) {
    assert_db_reads_of(&KEYS, read, db, expected);
}
