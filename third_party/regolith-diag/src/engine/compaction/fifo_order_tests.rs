//! FIFO compaction drops its tables oldest first by their place in L0, which
//! is the order a read follows, and not in the order of their file ids
//! (E38).

use std::sync::Arc;

use tempfile::TempDir;

use crate::engine::ingest::while_next_ingest_stages;
use crate::{
    CompactionOutcome, CompactionStyle, Db, FifoCompactionOptions, IngestOptions, Options,
    SstFileWriter,
};

/// An ingest draws its file id when it stages its file and installs it
/// later, at the newest place in L0. A table flushed in between has the
/// larger id and the older place. Dropped by id, the ingested table would
/// go first, and the key it rewrote would read as the older value the
/// flushed table holds.
#[test]
fn a_table_ingested_over_a_flush_it_staged_before_is_dropped_after_that_flush() {
    let dir = TempDir::new().unwrap();
    // Room for one of the two small tables, not both.
    let options = || {
        Options::default()
            .compaction_style(CompactionStyle::Fifo)
            .fifo_compaction_options(FifoCompactionOptions {
                max_table_files_size: 300,
            })
            .max_background_compactions(0)
    };
    let db = Arc::new(Db::open(dir.path().join("db"), options()).unwrap());
    let source = dir.path().join("source.sst");
    let mut writer = SstFileWriter::create(&source, &options()).unwrap();
    writer.put(b"k", b"ingested").unwrap();
    writer.finish().unwrap();

    // While the ingest stages its file, its id already drawn: `k = flushed`
    // is flushed to a table, which takes the next id and is installed first.
    let held = Arc::clone(&db);
    while_next_ingest_stages(move || {
        held.put(b"k", b"flushed").unwrap();
        held.flush().unwrap();
    });
    db.ingest_external_files(&[source], IngestOptions::default())
        .wait()
        .unwrap();

    let layout = |db: &Db| -> Vec<(u64, u64)> {
        db.engine().published_version().levels[0]
            .iter()
            .map(|table| (table.meta.file_id, table.meta.file_size))
            .collect()
    };
    let tables = layout(&db);
    // Oldest first: the flushed table, then the ingested one.
    let [flushed, ingested] = tables[..] else {
        panic!("two tables in L0: {tables:?}");
    };
    assert!(
        ingested.0 < flushed.0,
        "the newer table has the smaller id, which is what this test is about: {tables:?}"
    );
    assert!(
        flushed.1 <= 300 && ingested.1 <= 300 && flushed.1 + ingested.1 > 300,
        "one table fits under the cap and two do not: {tables:?}"
    );
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"ingested"[..]));

    assert_eq!(db.compact_step().unwrap(), CompactionOutcome::DidWork);
    assert_eq!(
        db.get(b"k").unwrap().as_deref(),
        Some(&b"ingested"[..]),
        "the pass dropped the newer table and left the older one. L0 before it, oldest first, \
         as (file id, bytes): {tables:?}; after it: {:?}",
        layout(&db)
    );
    assert_eq!(layout(&db), [ingested]);
}
