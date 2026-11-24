//! Column-family writes pay the same write-stall admission as default
//! column-family writes.
//!
//! A write path that skips admission keeps writing while every admitted
//! writer is stopped. In the read-view chaos workload that was a livelock:
//! a column-family churn loop kept adding range tombstones while the
//! stopped writers waited, each compaction had more of them to carry, the
//! stop never cleared, and the loop only ends once those writers finish.
//!
//! FIFO with no worker makes the stop permanent; with inline compaction on
//! and no queue, every admitted write runs the steps itself, finds none
//! relieves the stop and reports `Busy` at once, so the property is checked
//! without any timing.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use regolith::{CompactionStyle, Db, Error, FifoCompactionOptions, MergeOperator, Options};

/// Configured so `merge_cf` reaches the stall instead of being refused for
/// having no operator.
struct Concat;

impl MergeOperator for Concat {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        for op in operands {
            out.extend_from_slice(op);
        }
        Some(out)
    }

    fn name(&self) -> &'static str {
        "cf-write-stall-concat"
    }
}

#[test]
fn every_column_family_write_is_stopped_with_the_default_column_family() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(
        dir.path(),
        Options::default()
            .write_buffer_size(16 * 1024)
            .level0_slowdown_writes_trigger(3)
            .level0_stop_writes_trigger(4)
            .max_background_compactions(0)
            .inline_compaction(true)
            .merge_operator(Some(Arc::new(Concat)))
            .compaction_style(CompactionStyle::Fifo)
            .fifo_compaction_options(FifoCompactionOptions {
                max_table_files_size: 1024 * 1024 * 1024,
            }),
    )
    .expect("open");
    let cf = db.create_column_family("cf").expect("create cf");
    let stopped_at = (0..20_000usize).find(|i| {
        let put = db.put(format!("k{i:08}").as_bytes(), &[7u8; 512]);
        assert!(
            matches!(put, Ok(()) | Err(Error::Busy(_))),
            "put #{i}: {put:?}"
        );
        put.is_err()
    });
    assert!(stopped_at.is_some(), "no write stop within 20000 writes");

    let refused = |what: &str, outcome: regolith::Result<()>| {
        assert!(
            matches!(outcome, Err(Error::Busy(_))),
            "{what} under a write stop: expected Busy, got {outcome:?}"
        );
    };
    refused("put_cf", db.put_cf(&cf, b"a", b"1"));
    refused("delete_cf", db.delete_cf(&cf, b"a"));
    refused("delete_range_cf", db.delete_range_cf(&cf, b"a", b"z"));
    refused("merge_cf", db.merge_cf(&cf, b"a", b"1"));
    refused(
        "create_column_family",
        db.create_column_family("other").map(|_| ()),
    );
    refused("drop_column_family", db.drop_column_family(cf.clone()));

    // A refused write must not have landed.
    assert_eq!(db.get_cf(&cf, b"a").expect("get_cf"), None);
    assert!(db.column_family("other").is_none());
    assert!(db.column_family("cf").is_some());
}
