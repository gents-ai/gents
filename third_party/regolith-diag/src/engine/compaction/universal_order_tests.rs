//! Universal compaction takes its runs in the order L0 lists them, which is
//! the order a read follows, and not in the order of their file ids.

use std::sync::Arc;

use tempfile::TempDir;

use crate::engine::snapshot_registry::after_next_live_read;
use crate::{CompactionOutcome, CompactionStyle, CompressionType, Db, Options};

/// A merge's output draws its file id when it starts writing and takes the
/// place of its oldest input. Runs flushed while the merge ran therefore
/// have smaller ids and newer places. A later size-rule merge that went by
/// id would take the merged run and the newest one, leave the run between
/// them out, and put its output below that run: a key the newest run
/// rewrote would then read as the older value the run in between holds.
#[test]
fn a_run_flushed_while_a_merge_ran_is_never_left_out_between_its_neighbours() {
    let dir = TempDir::new().unwrap();
    let db = Arc::new(
        Db::open(
            dir.path(),
            Options::default()
                .compaction_style(CompactionStyle::Universal)
                .compression(CompressionType::None)
                .max_background_compactions(0),
        )
        .unwrap(),
    );
    // Two small runs of one size, which the size rule merges.
    db.put(b"a", &[b'1'; 16]).unwrap();
    db.flush().unwrap();
    db.put(b"b", &[b'1'; 16]).unwrap();
    db.flush().unwrap();

    // While that merge runs, after it fixed its inputs and before it drew
    // its output's id: a large run that holds `k = old`, then a small one
    // that rewrites `k`.
    let held = Arc::clone(&db);
    after_next_live_read(move || {
        held.put(b"k", b"old").unwrap();
        for i in 0..200 {
            held.put(format!("pad{i:04}").as_bytes(), &[b'p'; 512])
                .unwrap();
        }
        held.flush().unwrap();
        held.put(b"k", b"new").unwrap();
        held.put(b"c", b"1").unwrap();
        held.flush().unwrap();
    });
    assert_eq!(db.compact_step().unwrap(), CompactionOutcome::DidWork);
    let runs: Vec<(u64, u64)> = db.engine().published_version().levels[0]
        .iter()
        .map(|run| (run.meta.file_id, run.meta.file_size))
        .collect();
    // Oldest first: the merged run, the large run, the small run.
    let [merged, large, small] = runs[..] else {
        panic!("three runs: {runs:?}");
    };
    assert!(
        merged.0 > large.0 && merged.0 > small.0,
        "the oldest run has the largest id, which is what this test is about: {runs:?}"
    );
    assert!(
        merged.1 >= small.1 && large.1 > 100 * merged.1,
        "by size the merged and the small run go together and the large one does not: {runs:?}"
    );
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"new"[..]));

    while db.compact_step().unwrap() == CompactionOutcome::DidWork {}
    let after: Vec<u64> = db.engine().published_version().levels[0]
        .iter()
        .map(|run| run.meta.file_id)
        .collect();
    assert_eq!(
        db.get(b"k").unwrap().as_deref(),
        Some(&b"new"[..]),
        "the newest run rewrote k, and a merge put it below the run holding the older value. \
         L0 before the merge, oldest first, as (file id, bytes): {runs:?}; file ids after it: \
         {after:?}"
    );
    assert_eq!(db.scan(None, None).unwrap().len(), 204);
}
