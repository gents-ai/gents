use std::sync::Arc;

use regolith::{Db, MergeOperator, Options};
use tempfile::TempDir;

struct Concat;

impl MergeOperator for Concat {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(|b| b.to_vec()).unwrap_or_default();
        for op in operands {
            out.extend_from_slice(op);
        }
        Some(out)
    }
    fn name(&self) -> &'static str {
        "concat"
    }
}

fn opts() -> Options {
    Options::default().merge_operator(Some(Arc::new(Concat)))
}

#[test]
fn repro_a() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts()).unwrap();
    db.merge(b"k0001", b"abc").unwrap();

    assert_eq!(db.get(b"k0001").unwrap(), Some(b"abc".to_vec()), "get");
    assert_eq!(db.scan(None, None).unwrap().len(), 1, "scan");

    let mut it = db.iter();
    it.seek_to_first();
    assert!(it.valid(), "seek_to_first");
    assert_eq!(it.key().unwrap(), b"k0001");

    let mut it = db.iter();
    it.seek(b"k0001");
    assert!(it.valid(), "seek");

    let mut it = db.iter();
    it.seek_to_last();
    assert!(it.valid(), "seek_to_last");

    let mut it = db.iter();
    it.seek_for_prev(b"k0001");
    it.status().unwrap();
    assert!(
        it.valid(),
        "seek_for_prev on a pure merge chain landed nowhere"
    );
    assert_eq!(it.key().unwrap(), b"k0001");
    assert_eq!(it.value().unwrap(), b"abc");
}

#[test]
fn repro_b_with_a_base_value() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts()).unwrap();
    db.put(b"k0001", b"base").unwrap();
    db.merge(b"k0001", b"abc").unwrap();

    let mut it = db.iter();
    it.seek_for_prev(b"k0001");
    it.status().unwrap();
    assert!(it.valid(), "seek_for_prev over base+merge landed nowhere");
    assert_eq!(it.value().unwrap(), b"baseabc");
}

#[test]
fn repro_c_after_flush() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts()).unwrap();
    db.merge(b"k0001", b"abc").unwrap();
    db.compact_range(None, None).wait().unwrap();

    let mut it = db.iter();
    it.seek_for_prev(b"k0001");
    it.status().unwrap();
    assert!(
        it.valid(),
        "seek_for_prev over a flushed merge chain landed nowhere"
    );
}

#[test]
fn repro_d_seek_for_prev_past_the_key() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts()).unwrap();
    db.merge(b"k0001", b"abc").unwrap();

    let mut it = db.iter();
    it.seek_for_prev(b"k9999");
    it.status().unwrap();
    assert!(it.valid(), "seek_for_prev past the end landed nowhere");
    assert_eq!(it.key().unwrap(), b"k0001");
}

#[test]
fn repro_e_no_merge_operator_configured() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();
    db.put(b"k0001", b"abc").unwrap();

    let mut it = db.iter();
    it.seek_for_prev(b"k0001");
    it.status().unwrap();
    assert!(it.valid(), "seek_for_prev on a plain put landed nowhere");
}
