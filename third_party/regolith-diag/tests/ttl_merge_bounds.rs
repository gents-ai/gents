//! A TTL database keeps every write of a merged key that has not expired,
//! so the value it stores for the key grows with how often the key is
//! merged. These tests merge one key until that value crosses each bound
//! the engine has for one value or one table, and check that the key still
//! reads what its writes fold to, after a flush, compactions and a reopen:
//! no bound is one a merged value wedges on, and no merge taken is lost.
//!
//! - `max_value_size` bounds what a write takes: each operand is checked
//!   when it is written, and a value a fold makes is stored past it, as on
//!   any database with a merge operator.
//! - The write buffer holds operands, each as it is written; the value a
//!   fold makes is only ever in a table.
//! - A compaction's output file size is a target: a value larger than it
//!   gets a file of its own.
//! - The one call with a per-block limit, `get_size_with_limit`, refuses a
//!   database with a merge operator before it reads a block.
//! - A compressed block's frame records its length in four bytes; a block
//!   longer than that is written uncompressed, which the table writer's own
//!   tests cross (it takes a value of 4 GiB here).

// Native-only. wasm-pack builds every test target for wasm32, and the browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use regolith::{DbWithTtl, Error, MemEnv, MergeOperator, Options, WriteBatch, through_stalls};

/// Nothing expires while a test runs.
const TTL: u64 = 3600;

const KEY: &[u8] = b"merged";

struct Concat;

impl MergeOperator for Concat {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        for operand in operands {
            out.extend_from_slice(operand);
        }
        Some(out)
    }

    fn name(&self) -> &'static str {
        "concat"
    }
}

fn open(env: &Arc<MemEnv>, options: &Options) -> DbWithTtl {
    let options = options
        .clone()
        .env(env.clone())
        .merge_operator(Some(Arc::new(Concat)));
    DbWithTtl::open("bounds", options, TTL).unwrap()
}

/// Every read of the merged key returns `want`.
fn assert_reads(db: &DbWithTtl, want: &[u8], when: &str) {
    assert_eq!(db.get(KEY).unwrap().as_deref(), Some(want), "get {when}");
    assert_eq!(
        db.get_slice(KEY).unwrap().map(|s| s.to_vec()).as_deref(),
        Some(want),
        "get_slice {when}"
    );
    let scan = db.scan(None, None).unwrap();
    let found = scan.iter().find(|(k, _)| k == KEY).map(|(_, v)| &v[..]);
    assert_eq!(found, Some(want), "scan {when}");
}

/// Merges `count` operands of `len` bytes into the key, with other keys
/// written around it, and checks its reads as it is flushed, compacted and
/// reopened. Hands back the database and what the key reads.
fn merge_past(options: &Options, count: usize, len: usize) -> (Arc<MemEnv>, DbWithTtl, Vec<u8>) {
    let env = Arc::new(MemEnv::new());
    let db = open(&env, options);
    let mut want = Vec::new();
    for i in 0..count {
        let operand = vec![b'a' + (i % 26) as u8; len];
        let mut batch = WriteBatch::new();
        batch.merge(KEY, &operand);
        through_stalls(|| db.write(batch.clone())).unwrap();
        through_stalls(|| db.put(format!("k{i:05}").as_bytes(), &operand)).unwrap();
        want.extend_from_slice(&operand);
    }
    assert_reads(&db, &want, "as written");
    through_stalls(|| db.inner().flush()).unwrap();
    assert_reads(&db, &want, "flushed");
    db.compact_range(None, None).wait().unwrap();
    assert_reads(&db, &want, "compacted");
    db.close().unwrap();
    drop(db);
    let db = open(&env, options);
    assert_reads(&db, &want, "reopened");
    db.compact_range(None, None).wait().unwrap();
    assert_reads(&db, &want, "compacted again");
    (env, db, want)
}

/// What the database stores for the key: every write it holds, apart.
fn stored_len(db: &DbWithTtl) -> usize {
    db.inner()
        .get(KEY)
        .unwrap()
        .expect("the key is stored")
        .len()
}

#[test]
fn a_merged_value_past_the_maximum_value_size_is_stored_and_read() {
    let options = Options::memory().max_value_size(64);
    let (_env, db, want) = merge_past(&options, 40, 40);
    assert!(stored_len(&db) > 64, "the stored value crossed the bound");
    // The bound is on writes: a put of what the key reads is refused.
    assert!(matches!(
        db.put(b"copy", &want),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn a_merged_value_past_the_write_buffer_is_stored_and_read() {
    let options = Options::memory().write_buffer_size(4096);
    let (_env, db, _) = merge_past(&options, 200, 64);
    assert!(stored_len(&db) > 4096, "the stored value crossed the bound");
}

#[test]
fn a_merged_value_past_the_compaction_file_size_is_stored_and_read() {
    let options = Options::memory()
        .target_file_size(1024)
        .level_base_bytes(1024);
    let (env, db, want) = merge_past(&options, 100, 32);
    assert!(stored_len(&db) > 1024, "the stored value crossed the bound");
    // More merges onto the compacted value, compacted again.
    let mut want = want;
    for _ in 0..20 {
        let mut batch = WriteBatch::new();
        batch.merge(KEY, b"tail");
        through_stalls(|| db.write(batch.clone())).unwrap();
        want.extend_from_slice(b"tail");
    }
    db.compact_range(None, None).wait().unwrap();
    assert_reads(&db, &want, "merged onto and compacted");
    db.close().unwrap();
    drop(db);
    assert_reads(&open(&env, &options), &want, "reopened");
}

#[test]
fn the_per_block_limit_is_refused_before_a_block_is_read() {
    let options = Options::memory();
    let (_env, db, want) = merge_past(&options, 64, 64);
    let limit = 64;
    assert!(stored_len(&db) > limit);
    assert!(matches!(
        db.inner().get_size_with_limit(KEY, limit),
        Err(Error::InvalidArgument(_))
    ));
    assert_eq!(db.get(KEY).unwrap(), Some(want));
}
