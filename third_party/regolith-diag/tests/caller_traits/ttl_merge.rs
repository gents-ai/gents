//! A caller's `MergeOperator` on a TTL database: it is shown the values and
//! operands the caller wrote, never the time the database keeps behind
//! them, and only the writes that have not expired: each write expires by
//! its own time.

use std::sync::{Arc, Mutex};

use regolith::{DbWithTtl, MemEnv, MergeOperator, Options, WriteBatch};

/// The base and the operands of one full merge.
type Fold = (Option<Vec<u8>>, Vec<Vec<u8>>);

/// Concatenates, and records what each method is handed.
#[derive(Default)]
struct Concat {
    full: Mutex<Vec<Fold>>,
    partial: Mutex<Vec<(Vec<u8>, Vec<u8>)>>,
}

impl MergeOperator for Concat {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        self.full.lock().unwrap().push((
            base.map(<[u8]>::to_vec),
            operands.iter().map(|operand| operand.to_vec()).collect(),
        ));
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        for operand in operands {
            out.extend_from_slice(operand);
        }
        Some(out)
    }

    fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        self.partial
            .lock()
            .unwrap()
            .push((left.to_vec(), right.to_vec()));
        Some([left, right].concat())
    }

    fn name(&self) -> &'static str {
        "concat"
    }
}

fn merge(db: &DbWithTtl, key: &[u8], operand: &[u8]) {
    let mut batch = WriteBatch::new();
    batch.merge(key, operand);
    db.write(batch).unwrap();
}

const HOUR: u64 = 3_600 * 1_000_000;

#[test]
fn a_ttl_database_shows_the_merge_operator_what_the_caller_wrote() {
    let env = Arc::new(MemEnv::new());
    let operator = Arc::new(Concat::default());
    let options = Options::memory()
        .env(env.clone())
        .merge_operator(Some(operator.clone()));
    let db = DbWithTtl::open("ttl-merge", options, 3_600).unwrap();

    db.put(b"k", b"base").unwrap();
    merge(&db, b"k", b"+one");
    assert_eq!(db.get(b"k").unwrap(), Some(b"base+one".to_vec()));
    assert_eq!(
        operator.full.lock().unwrap().last().unwrap(),
        &(Some(b"base".to_vec()), vec![b"+one".to_vec()])
    );

    // Two operands with no base, in two tables a compaction merges. The
    // database keeps them apart, each with its time, and the operator folds
    // them when the key is read: it is never asked to fold two operands
    // ahead of a read, since they may expire at different times.
    merge(&db, b"m", b"x");
    db.inner().flush().unwrap();
    merge(&db, b"m", b"y");
    db.compact_range(None, None).wait().unwrap();
    assert_eq!(db.get(b"m").unwrap(), Some(b"xy".to_vec()));
    assert!(operator.partial.lock().unwrap().is_empty());
    for (base, operands) in operator.full.lock().unwrap().iter() {
        for value in base.iter().chain(operands) {
            assert!(
                !value.windows(4).any(|w| w == b"RTTL"),
                "a stamp reached the operator: {value:?}"
            );
        }
    }

    // Each write expires by its own time: an hour and a quarter on, the put
    // and the first merge have expired and the second merge has not.
    env.advance_micros(HOUR / 2);
    merge(&db, b"k", b"+two");
    env.advance_micros(HOUR * 3 / 4);
    assert_eq!(db.get(b"k").unwrap(), Some(b"+two".to_vec()));
    assert_eq!(
        operator.full.lock().unwrap().last().unwrap(),
        &(None, vec![b"+two".to_vec()])
    );
    env.advance_micros(HOUR / 2);
    assert_eq!(db.get(b"k").unwrap(), None);
}

/// A merge does not renew what was written before it: a base that expired
/// is not folded into a merge, written before or after the expiry.
#[test]
fn an_expired_base_is_left_out_of_a_merge() {
    let env = Arc::new(MemEnv::new());
    let operator = Arc::new(Concat::default());
    let options = Options::memory()
        .env(env.clone())
        .merge_operator(Some(operator.clone()));
    let db = DbWithTtl::open("ttl-merge-expired", options, 3_600).unwrap();
    db.put(b"k", b"old").unwrap();
    env.advance_micros(2 * HOUR);
    assert_eq!(db.get(b"k").unwrap(), None);
    merge(&db, b"k", b"new");
    assert_eq!(db.get(b"k").unwrap(), Some(b"new".to_vec()));
    assert_eq!(
        operator.full.lock().unwrap().last().unwrap(),
        &(None, vec![b"new".to_vec()])
    );
}
