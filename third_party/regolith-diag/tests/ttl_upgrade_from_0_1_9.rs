//! A TTL database regolith 0.1.9 wrote, puts and merges, reads on this
//! version exactly as 0.1.9 read it, and neither a compaction nor a reopen
//! changes a read.
//!
//! `tests/fixtures/v0_1_9_ttl` was written by the release tagged `v0.1.9`
//! through `DbWithTtl` with a TTL of 100 s, on a `MemEnv` whose wall clock
//! the program set, and copied out without closing. Its merge operator,
//! named `mix`, puts an operand that starts with `<` in front of the value
//! and any other at the end. 0.1.9 stamped puts and passed merge operands
//! through unstamped. The steps, at seconds from `T0`:
//!
//! 1. at 0: `put old = o`, `p-expired = E`, `a-expired = E`; flush;
//! 2. at 60: `put p-compacted = C`, `merge p-compacted <z`,
//!    `put a-compacted = C`, `merge a-compacted z`; flush; compact the
//!    whole database, which folded each of the two keys into one value;
//! 3. at 120: `put put-new = n`, `p-live = B`, `a-live = B`, and through the
//!    inner database `put raw-put = r`; flush;
//! 4. at 125: `merge p-live <x`, `merge a-live x`;
//! 5. at 130: `merge p-live <y`, `a-live y`, `p-expired <x`, `a-expired x`,
//!    `ops-only <x`, `ops-only y`, `raw-put <q`; `put deleted = d` and
//!    through the inner database `delete deleted`. Every write of steps 4
//!    and 5 is only in the log.
//!
//! [`READ_BY_0_1_9`] is what that program printed 0.1.9 read, by point
//! reads and by a scan, at 150 s and at 200 s.

// Native-only, like every test that reads a fixture from the filesystem.
#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::path::Path;
use std::sync::Arc;

use regolith::{DbWithTtl, Env, MemEnv, MergeOperator, Options, WriteBatch};

const T0: u64 = 1_000_000;
const TTL: u64 = 100;

/// Every key the fixture wrote.
const KEYS: [&[u8]; 11] = [
    b"a-compacted",
    b"a-expired",
    b"a-live",
    b"deleted",
    b"old",
    b"ops-only",
    b"p-compacted",
    b"p-expired",
    b"p-live",
    b"put-new",
    b"raw-put",
];

/// What 0.1.9 read at each time: the keys that read as a value, in key
/// order. Every other key read as absent.
type Reads = &'static [(&'static [u8], &'static [u8])];

const READ_BY_0_1_9: [(u64, Reads); 2] = [
    (
        T0 + 150,
        &[
            (b"p-compacted", b"<zC"),
            (b"p-live", b"<y<xB"),
            (b"put-new", b"n"),
        ],
    ),
    (T0 + 200, &[(b"p-live", b"<y<xB"), (b"put-new", b"n")]),
];

/// The operator the fixture was written with.
struct Mix;

impl MergeOperator for Mix {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        for operand in operands {
            if operand.first() == Some(&b'<') {
                out.splice(0..0, operand.iter().copied());
            } else {
                out.extend_from_slice(operand);
            }
        }
        Some(out)
    }

    fn name(&self) -> &'static str {
        "mix"
    }
}

const DB: &str = "/ttl";

/// A fresh environment holding the fixture.
fn fixture_env() -> Arc<MemEnv> {
    let env = Arc::new(MemEnv::new());
    let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v0_1_9_ttl");
    load(&env, &from, Path::new(DB));
    env
}

fn load(env: &MemEnv, from: &Path, to: &Path) {
    env.create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            load(env, &entry.path(), &target);
        } else {
            env.write(&target, &fs::read(entry.path()).unwrap())
                .unwrap();
        }
    }
}

fn at(env: &MemEnv, secs: u64) {
    env.set_clocks(Some(secs * 1_000_000), Some(secs));
}

fn open(env: &Arc<MemEnv>) -> DbWithTtl {
    let options = Options::memory()
        .env(env.clone())
        .merge_operator(Some(Arc::new(Mix)));
    DbWithTtl::open(DB, options, TTL).expect("a 0.1.9 TTL database opens")
}

/// Every key reads as `expected` says, by a point read and by a scan.
fn assert_reads(db: &DbWithTtl, expected: Reads, when: &str) {
    for key in KEYS {
        let want = expected
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_vec());
        assert_eq!(
            db.get(key).unwrap(),
            want,
            "{} {when}",
            String::from_utf8_lossy(key)
        );
    }
    let scan: Vec<(Vec<u8>, Vec<u8>)> = expected
        .iter()
        .map(|(k, v)| (k.to_vec(), v.to_vec()))
        .collect();
    assert_eq!(db.scan(None, None).unwrap(), scan, "the scan {when}");
}

#[test]
fn a_0_1_9_ttl_database_reads_as_0_1_9_read_it() {
    for (secs, expected) in READ_BY_0_1_9 {
        let env = fixture_env();
        at(&env, secs);
        let db = open(&env);
        assert_reads(&db, expected, &format!("at {secs}, as opened"));
        db.compact_range(None, None).wait().unwrap();
        assert_reads(&db, expected, &format!("at {secs}, compacted"));
        db.close().unwrap();
        drop(db);
        let db = open(&env);
        assert_reads(&db, expected, &format!("at {secs}, reopened"));
    }
}

#[test]
fn a_compaction_of_it_changes_no_later_read() {
    let env = fixture_env();
    at(&env, T0 + 150);
    let db = open(&env);
    db.compact_range(None, None).wait().unwrap();
    // The puts that had expired are gone from the tables, not only hidden.
    assert_eq!(db.inner().get(b"old").unwrap(), None);
    let (secs, expected) = READ_BY_0_1_9[1];
    at(&env, secs);
    assert_reads(&db, expected, "after a compaction at 150");
    db.compact_range(None, None).wait().unwrap();
    assert_reads(&db, expected, "after a second compaction");
}

#[test]
fn a_merge_after_the_upgrade_applies_to_what_the_key_reads_as() {
    let env = fixture_env();
    at(&env, T0 + 150);
    let db = open(&env);
    let mut batch = WriteBatch::new();
    batch.merge(b"p-live", b"<w");
    db.write(batch).unwrap();
    assert_eq!(db.get(b"p-live").unwrap(), Some(b"<w<y<xB".to_vec()));
    db.compact_range(None, None).wait().unwrap();
    assert_eq!(db.get(b"p-live").unwrap(), Some(b"<w<y<xB".to_vec()));
    // The put has expired by 230: the 0.1.9 merges read as nothing without
    // it, and the new one, which lives to 250, reads alone.
    at(&env, T0 + 230);
    assert_eq!(db.get(b"p-live").unwrap(), Some(b"<w".to_vec()));
    db.compact_range(None, None).wait().unwrap();
    assert_eq!(db.get(b"p-live").unwrap(), Some(b"<w".to_vec()));
    at(&env, T0 + 251);
    assert_eq!(db.get(b"p-live").unwrap(), None);
}
