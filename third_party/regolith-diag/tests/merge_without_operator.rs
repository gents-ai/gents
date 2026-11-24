//! A merge written to a database with no merge operator is refused, so a
//! read can never disagree with another about an operand nothing can fold.
//!
//! The refusal comes from every way to write a merge: the point calls, a
//! batch (checked where it is applied, since a batch does not know its
//! database) and a transaction. Nothing is written when it fires.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// the filesystem. The browser suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use regolith::{Db, Error, MergeOperator, Options, WriteBatch, WriteOptions};

/// Appends every operand to the base in order.
struct AppendMerge;

impl MergeOperator for AppendMerge {
    fn name(&self) -> &'static str {
        "append"
    }

    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        operands
            .iter()
            .for_each(|operand| out.extend_from_slice(operand));
        Some(out)
    }
}

fn refused(result: regolith::Result<impl std::fmt::Debug>) -> bool {
    matches!(result, Err(Error::NoMergeOperator))
}

#[test]
fn the_point_merges_are_refused_and_write_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();
    let before = db.latest_sequence();
    let cf = db.default_cf();

    assert!(refused(db.merge(b"k", b"op")));
    assert!(refused(db.merge_opt(&WriteOptions::sync(), b"k", b"op")));
    assert!(refused(db.merge_cf(&cf, b"k", b"op")));

    assert_eq!(
        db.latest_sequence(),
        before,
        "a refused merge used a sequence"
    );
    assert_eq!(db.get(b"k").unwrap(), None);
    assert!(db.scan(None, None).unwrap().is_empty());
}

#[test]
fn a_batch_with_a_merge_is_refused_when_applied_and_none_of_it_lands() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();
    let before = db.latest_sequence();
    let cf = db.default_cf();
    let batch = || {
        let mut batch = WriteBatch::new();
        batch.put(b"put", b"v");
        batch.merge(b"k", b"op");
        batch
    };
    let cf_batch = || {
        let mut batch = WriteBatch::new();
        batch.put(b"put", b"v");
        batch.merge_cf(&cf, b"k", b"op");
        batch
    };

    assert!(refused(db.write(batch())));
    assert!(refused(db.write(cf_batch())));
    assert!(refused(db.write_opt(&WriteOptions::sync(), batch())));
    assert!(refused(db.write_sequenced(batch())));
    assert!(refused(db.write_with_durability(
        batch(),
        regolith::DurabilityMode::Immediate
    )));

    assert_eq!(
        db.latest_sequence(),
        before,
        "a refused batch used a sequence"
    );
    assert_eq!(db.get(b"put").unwrap(), None, "the batch is atomic");
    assert_eq!(db.get(b"k").unwrap(), None);
}

#[test]
fn a_batch_without_a_merge_is_unaffected() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();
    let mut batch = WriteBatch::new();
    batch.put(b"a", b"1");
    batch.delete(b"b");
    db.write(batch).unwrap();
    assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()));
}

#[test]
fn with_an_operator_every_merge_write_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let options = Options::default().merge_operator(Some(Arc::new(AppendMerge)));
    let db = Db::open(dir.path(), options).unwrap();
    let cf = db.default_cf();

    db.merge(b"k", b"a").unwrap();
    db.merge_opt(&WriteOptions::sync(), b"k", b"b").unwrap();
    db.merge_cf(&cf, b"k", b"c").unwrap();
    let mut batch = WriteBatch::new();
    batch.merge(b"k", b"d");
    batch.merge_cf(&cf, b"k", b"e");
    db.write(batch).unwrap();

    assert_eq!(db.get(b"k").unwrap(), Some(b"abcde".to_vec()));
}

#[test]
fn a_closed_handle_reports_that_before_the_missing_operator() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();
    db.close().unwrap();
    assert!(matches!(db.merge(b"k", b"op"), Err(Error::Closed)));
    assert!(matches!(
        db.write({
            let mut batch = WriteBatch::new();
            batch.merge(b"k", b"op");
            batch
        }),
        Err(Error::Closed)
    ));
}
