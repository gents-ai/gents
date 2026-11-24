//! Where an ingested table lands. A table is placed by the range it will be
//! recorded with, tombstones included, so a level below L0 never ends up
//! holding two tables that overlap.

use std::path::Path;

use tempfile::TempDir;

use crate::column_family::{DEFAULT_CF_ID, prefix_key};
use crate::engine::internal_key::{VALUE_TYPE_VALUE, encode_internal_key};
use crate::engine::manifest::MAX_LEVELS;
use crate::engine::sstable::SsTableWriter;
use crate::options::CompressionType;
use crate::sst_file_writer::IngestOptions;
use crate::{Db, Options};

/// Write a source table holding `points` and one range tombstone over
/// `[start, end)`, the way a table copied out of another database can.
fn source_with_tombstone(path: &Path, points: &[&[u8]], start: &[u8], end: &[u8]) {
    let mut writer =
        SsTableWriter::new(path, 4096, 10, CompressionType::None, None, false, 4096).unwrap();
    for key in points {
        let internal = encode_internal_key(&prefix_key(DEFAULT_CF_ID, key), 1, VALUE_TYPE_VALUE);
        writer.add(&internal, b"ingested").unwrap();
    }
    writer.add_range_tombstone(
        &prefix_key(DEFAULT_CF_ID, start),
        &prefix_key(DEFAULT_CF_ID, end),
        1,
    );
    writer.finish().unwrap().unwrap();
}

fn files_at(db: &Db, level: usize) -> u64 {
    db.get_int_property(&format!("regolith.num-files-at-level{level}"))
        .unwrap()
}

#[test]
fn a_source_whose_tombstone_reaches_existing_data_lands_above_it_not_beside_it() {
    let dir = TempDir::new().unwrap();
    let options = Options::default().max_background_compactions(0);
    let db = Db::open(dir.path(), options).unwrap();
    for key in [b"c", b"d", b"e", b"f"] {
        db.put(key, b"old").unwrap();
    }
    db.flush().unwrap();
    db.compact_range(None, None).wait().unwrap();
    let bottom = MAX_LEVELS - 1;
    assert_eq!(
        files_at(&db, bottom),
        1,
        "the old data is in the bottom level"
    );

    // The points, m through p, are disjoint from the old table. The tombstone
    // over [a, g) is not, and the table is recorded with that wider range.
    let source = dir.path().join("source.sst");
    source_with_tombstone(&source, &[b"m", b"n", b"p"], b"a", b"g");
    db.ingest_external_files(&[source], IngestOptions::default())
        .wait()
        .unwrap();

    // The deepest level whose tables, and every level's above it, the
    // recorded range does not meet (LsmOrder.tla's placement).
    assert_eq!(
        files_at(&db, bottom - 1),
        1,
        "the ingested table is in the level above the old one"
    );
    assert_eq!(files_at(&db, 0), 0);
    assert_eq!(
        files_at(&db, bottom),
        1,
        "not beside the old table, in its level"
    );
    assert_eq!(
        db.get(b"d").unwrap(),
        None,
        "the tombstone deleted the old data"
    );
    assert_eq!(
        db.get(b"n").unwrap().as_deref(),
        Some(b"ingested".as_slice())
    );
}

#[test]
fn a_source_whose_tombstone_stays_clear_of_existing_data_still_goes_to_the_bottom() {
    let dir = TempDir::new().unwrap();
    let options = Options::default().max_background_compactions(0);
    let db = Db::open(dir.path(), options).unwrap();
    for key in [b"c", b"d", b"e", b"f"] {
        db.put(key, b"old").unwrap();
    }
    db.flush().unwrap();
    db.compact_range(None, None).wait().unwrap();
    let bottom = MAX_LEVELS - 1;

    let source = dir.path().join("source.sst");
    source_with_tombstone(&source, &[b"m", b"n", b"p"], b"q", b"s");
    db.ingest_external_files(&[source], IngestOptions::default())
        .wait()
        .unwrap();

    assert_eq!(files_at(&db, 0), 0);
    assert_eq!(files_at(&db, bottom), 2);
    assert_eq!(db.get(b"d").unwrap().as_deref(), Some(b"old".as_slice()));
    assert_eq!(
        db.get(b"n").unwrap().as_deref(),
        Some(b"ingested".as_slice())
    );
}
