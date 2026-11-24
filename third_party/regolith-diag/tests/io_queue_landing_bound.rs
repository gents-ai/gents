//! `CacheOnly` reads whose blocks together pass the queue's byte bound.
//!
//! With no block cache, a read run again finds its blocks only where they
//! landed for its queue, and that landing table is bounded by bytes. A call
//! that needs several blocks at once (a `multi_get`, a collecting scan, a
//! read that merges operands from several tables) starts over each time it
//! is run, and needs every block it found before: each one here is larger
//! than half the bound, so two of them never fit. The call must still
//! finish when it is run again after each poll, and answer what a blocking
//! read answers.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use regolith::{
    CompressionType, Db, Error, IoBudget, MergeOperator, Options, ReadMode, Snapshot, WouldBlock,
};
use tempfile::TempDir;

/// Runs of a call before it must have finished: one per block it needs,
/// several times over. A call past it never finished.
const MOST_RUNS: usize = 64;

/// Two MiB: one value fills a data block on its own, twice the queue's bound
/// at a 4 KiB block size (256 blocks).
const BIG: usize = 2 << 20;

/// Six hundred KiB: one is under the bound, two are past it.
const MID: usize = 600 << 10;

/// Append-only merge: the value is every operand in order.
struct Concat;

impl MergeOperator for Concat {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        for op in operands {
            out.extend_from_slice(op);
        }
        Some(out)
    }

    fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        let mut out = left.to_vec();
        out.extend_from_slice(right);
        Some(out)
    }

    fn name(&self) -> &'static str {
        "landing-bound-concat"
    }
}

/// The reviewer's layout: 4 KiB blocks, no block cache, no compression.
fn options() -> Options {
    Options::default()
        .block_size(4096)
        .block_cache_size(0)
        .compression(CompressionType::None)
        .merge_operator(Some(Arc::new(Concat)))
}

fn value(tag: u8, len: usize) -> Vec<u8> {
    vec![tag; len]
}

/// Run `read` through a `CacheOnly` snapshot on a queue of this thread,
/// polling the queue after every `WouldBlock`, and return its answer.
fn through_queue<T: std::fmt::Debug>(
    db: &Db,
    read: impl Fn(&Snapshot) -> regolith::Result<T>,
) -> T {
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    for _ in 0..MOST_RUNS {
        match read(&snapshot) {
            Err(Error::WouldBlock(WouldBlock::Io(_))) => {
                queue.poll(IoBudget::ALL);
            }
            other => return other.unwrap(),
        }
    }
    panic!("the read never finished: every run lost a block an earlier run found");
}

#[test]
fn a_multi_get_whose_blocks_pass_the_bound_finishes() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), options()).unwrap();
    db.put(b"a", &value(1, BIG)).unwrap();
    db.put(b"b", &value(2, BIG)).unwrap();
    db.flush().unwrap();

    let got = through_queue(&db, |snapshot| snapshot.multi_get(&[b"a", b"b"]));
    assert_eq!(got, vec![Some(value(1, BIG)), Some(value(2, BIG))]);
    assert_eq!(got, db.multi_get(&[b"a", b"b"]).unwrap());
}

#[test]
fn a_scan_whose_blocks_pass_the_bound_finishes() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), options()).unwrap();
    for (i, key) in [b"s1", b"s2", b"s3"].into_iter().enumerate() {
        db.put(key, &value(i as u8 + 1, MID)).unwrap();
    }
    db.flush().unwrap();

    let got = through_queue(&db, |snapshot| snapshot.scan(Some(b"s"), Some(b"t")));
    assert_eq!(got.len(), 3);
    assert_eq!(got, db.scan(Some(b"s"), Some(b"t")).unwrap());
}

#[test]
fn a_merge_read_whose_operands_pass_the_bound_finishes() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), options()).unwrap();
    // One operand per table, so the read merges three blocks of three
    // tables.
    for tag in 1..=3u8 {
        db.merge(b"m", &value(tag, MID)).unwrap();
        db.flush().unwrap();
    }

    let got = through_queue(&db, |snapshot| snapshot.get(b"m"));
    let mut want = value(1, MID);
    want.extend(value(2, MID));
    want.extend(value(3, MID));
    assert_eq!(got.as_deref(), Some(&want[..]));
    assert_eq!(got, db.get(b"m").unwrap());
}
