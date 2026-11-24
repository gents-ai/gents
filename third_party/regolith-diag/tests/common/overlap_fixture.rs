//! `tests/fixtures/v0_1_9_overlap`: a database the release tagged `v0.1.9`
//! left with two tables of overlapping key ranges in its bottom level, and
//! what its newest versions are.
//!
//! It was written by that release, not by this version and not by hand,
//! because the defect is in what that release wrote: it placed an ingested
//! table by the range of its keys and recorded it with the wider range of
//! its range tombstone. Under `Immediate` durability and a concatenating
//! merge operator named `concat`, with every put's value `"<tag>:"`
//! followed by 200 bytes derived from the tag ([`value`]):
//!
//! 1. `put c d e f h i j = old`, `merge a1 x`, `merge j2 1`, `flush`,
//!    `compact_range(None, None)`: one table in level 6;
//! 2. `put b k = old`, `flush`: one table in level 0. Then
//!    `ingest_external_files` of a table another 0.1.9 database flushed
//!    after `put m n p = ingested` and `delete_range [a, g)`. Its keys
//!    `m..p` meet no table, so 0.1.9 put it in level 6, recorded as `a..p`:
//!    it overlaps the table of step 1 there, and its range tombstone is
//!    newer than `b` in level 0;
//! 3. reopened with a 512 byte level 1 budget and a worker: `put d h n =
//!    new`, `delete p`, `merge a1 y`, `merge j2 2`, `flush`. The worker
//!    compacted level 0 to level 1 and on to level 2;
//! 4. reopened with the default budget: `put h = newer`, `put i = new`,
//!    `flush`, `put j m = new`, `merge j2 3`, `flush`. The worker compacted
//!    level 0 to level 1;
//! 5. reopened with no worker: `put f i = l0`, `flush`; then `put e = wal`,
//!    `delete k`, `put q = wal`, `merge j2 4`, and the process exited
//!    without closing, so those four writes are only in the format 1 log.
//!
//! That leaves one table in level 0, one in level 1, one in level 2 and two
//! in level 6. 0.1.9 itself answered `get(b) = old`, from the level it read
//! before the range tombstone's, and failed every scan with "SSTable
//! iteration went backwards".

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use regolith::{Db, MergeOperator, Options};
use tempfile::TempDir;

/// Tables the fixture holds below level 0: one in level 1, one in level 2,
/// two in level 6. An open moves them all.
pub const MOVED: u64 = 4;

/// Every key the fixture's steps name.
pub const KEYS: [&[u8]; 17] = [
    b"a", b"a1", b"b", b"c", b"d", b"e", b"f", b"g", b"h", b"i", b"j", b"j2", b"k", b"m", b"n",
    b"p", b"q",
];

/// The fixture's merge operator.
pub struct Concat;

impl MergeOperator for Concat {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        for op in operands {
            out.extend_from_slice(op);
        }
        Some(out)
    }

    fn name(&self) -> &'static str {
        "concat"
    }
}

/// `options` with the fixture's merge operator and no worker, so only the
/// calls a test makes move a table.
pub fn with_operator(options: Options) -> Options {
    options
        .merge_operator(Some(Arc::new(Concat)))
        .max_background_compactions(0)
}

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
/// directory has and the fixture leaves out (a read-only open takes it).
pub fn copy_to(db_dir: &Path) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v0_1_9_overlap");
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

/// The value the fixture's writer gave a put tagged `tag`.
pub fn value(tag: &str) -> Vec<u8> {
    let mut out = format!("{tag}:").into_bytes();
    let mut x: u32 = tag
        .bytes()
        .fold(0x9E37_79B9, |h, b| h.rotate_left(5) ^ u32::from(b));
    for _ in 0..200 {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        out.push((x >> 24) as u8);
    }
    out
}

/// The newest version of every key, by the sequence each write took: the
/// ingest's range tombstone over `[a, g)` is newer than the first `a1`, `b`,
/// `c`, `d`, `e` and `f`, and older than the later `a1`, `d`, `e` and `f`.
pub fn state() -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut state = BTreeMap::new();
    let mut put = |key: &[u8], value: Vec<u8>| state.insert(key.to_vec(), value);
    put(b"a1", b"y".to_vec());
    put(b"d", value("new"));
    put(b"e", value("wal"));
    put(b"f", value("l0"));
    put(b"h", value("newer"));
    put(b"i", value("l0"));
    put(b"j", value("new"));
    put(b"j2", b"1234".to_vec());
    put(b"m", value("new"));
    put(b"n", value("new"));
    put(b"q", value("wal"));
    state
}

/// Tables `db` moved out of overlapping levels when it opened.
pub fn moved(db: &Db) -> u64 {
    db.get_int_property("regolith.manifest.tables_demoted")
        .unwrap()
}

/// Every way a caller reads, checked against `expected`: one key at a time,
/// a batch, and a scan. `read` names the reader in a failure.
pub fn assert_reads(
    read: &str,
    get: impl Fn(&[u8]) -> Option<Vec<u8>>,
    multi_get: impl Fn(&[&[u8]]) -> Vec<Option<Vec<u8>>>,
    scan: impl Fn() -> Vec<(Vec<u8>, Vec<u8>)>,
    expected: &BTreeMap<Vec<u8>, Vec<u8>>,
) {
    assert_reads_of(&KEYS, read, get, multi_get, scan, expected);
}

/// [`assert_reads`] over the keys `keys`.
pub fn assert_reads_of(
    keys: &[&[u8]],
    read: &str,
    get: impl Fn(&[u8]) -> Option<Vec<u8>>,
    multi_get: impl Fn(&[&[u8]]) -> Vec<Option<Vec<u8>>>,
    scan: impl Fn() -> Vec<(Vec<u8>, Vec<u8>)>,
    expected: &BTreeMap<Vec<u8>, Vec<u8>>,
) {
    for key in keys.iter().copied() {
        assert_eq!(
            get(key).as_ref(),
            expected.get(key),
            "{read}: get({})",
            String::from_utf8_lossy(key)
        );
    }
    let batch = multi_get(keys);
    for (key, got) in keys.iter().zip(&batch) {
        assert_eq!(
            got.as_ref(),
            expected.get(*key),
            "{read}: multi_get({})",
            String::from_utf8_lossy(key)
        );
    }
    let scanned: BTreeMap<Vec<u8>, Vec<u8>> = scan().into_iter().collect();
    assert_eq!(&scanned, expected, "{read}: scan");
}

/// [`assert_reads`] through `db` itself.
pub fn assert_db_reads(read: &str, db: &Db, expected: &BTreeMap<Vec<u8>, Vec<u8>>) {
    assert_db_reads_of(&KEYS, read, db, expected);
}

/// [`assert_reads_of`] through `db` itself.
pub fn assert_db_reads_of(
    keys: &[&[u8]],
    read: &str,
    db: &Db,
    expected: &BTreeMap<Vec<u8>, Vec<u8>>,
) {
    assert_reads_of(
        keys,
        read,
        |key| db.get(key).unwrap().map(|v| v.to_vec()),
        |keys| {
            db.multi_get(keys)
                .unwrap()
                .into_iter()
                .map(|v| v.map(|v| v.to_vec()))
                .collect()
        },
        || db.scan(None, None).unwrap(),
        expected,
    );
}
