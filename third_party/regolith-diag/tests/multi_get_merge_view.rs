//! A `multi_get` on a database with a merge operator must resolve every key
//! against the one read view it loaded.
//!
//! The batch picks its read sequence against that view. Resolving a key
//! against a view loaded later lets a compaction that ran in between drop the
//! version the sequence admits in favour of a newer one it does not, and the
//! key reads back as absent: a read that travels backwards.
//!
//! The merge operator is the interleaving point. It runs while the first key
//! is resolved, so what it does there lands between the batch's keys.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// the filesystem, which does not exist there.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use regolith::{Db, MergeOperator, Options};
use tempfile::TempDir;

/// Concatenates operands onto the base. The first time it runs it rewrites
/// `"b"` and compacts, which replaces the view the batch loaded.
struct Interfering {
    db: Arc<OnceLock<Weak<Db>>>,
    fired: AtomicBool,
}

impl MergeOperator for Interfering {
    fn name(&self) -> &'static str {
        "interfering"
    }

    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        if !self.fired.swap(true, Ordering::SeqCst)
            && let Some(db) = self.db.get().and_then(Weak::upgrade)
        {
            db.put(b"b", b"new").unwrap();
            db.flush().unwrap();
            db.compact_range(None, None).wait().unwrap();
        }
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        operands.iter().for_each(|op| out.extend_from_slice(op));
        Some(out)
    }
}

#[test]
fn a_multi_get_resolves_every_key_against_one_view() {
    let dir = TempDir::new().unwrap();
    let slot = Arc::new(OnceLock::new());
    let options = Options::default()
        .max_background_compactions(0)
        .merge_operator(Some(Arc::new(Interfering {
            db: Arc::clone(&slot),
            fired: AtomicBool::new(false),
        })));
    let db = Arc::new(Db::open(dir.path(), options).unwrap());
    slot.set(Arc::downgrade(&db)).unwrap();

    db.put(b"a", b"base").unwrap();
    db.merge(b"a", b"+op").unwrap();
    db.put(b"b", b"old").unwrap();

    let got = db.multi_get(&[b"a", b"b"]).unwrap();

    assert_eq!(got[0].as_deref(), Some(&b"base+op"[..]));
    assert_eq!(
        got[1].as_deref(),
        Some(&b"old"[..]),
        "the second key was resolved against a view loaded after the first key's merge"
    );
    assert_eq!(db.get(b"b").unwrap().as_deref(), Some(&b"new"[..]));
}
