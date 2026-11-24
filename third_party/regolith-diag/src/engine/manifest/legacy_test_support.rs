//! Helpers the tests of an open that repairs what an earlier release left
//! share: tables and manifests written directly, and the reads every such
//! test checks.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::TempDir;

use super::legacy_overlap_tests::options;
use super::*;
use crate::column_family::{DEFAULT_CF_ID, prefix_key};
use crate::engine::internal_key::encode_internal_key;
use crate::engine::sstable::SsTableWriter;
use crate::options::CompressionType;
use crate::{Db, MergeOperator, Options};

/// One entry of a table: key, sequence, kind, value.
pub(super) type Entry<'a> = (&'a [u8], u64, u8, &'a [u8]);

/// A range tombstone of a table: start, end, sequence.
pub(super) type Tombstone<'a> = (&'a [u8], &'a [u8], u64);

/// Write table `id` holding `entries`, given in key order, and
/// `tombstones`, all in the default column family.
pub(super) fn table_of(
    dir: &Path,
    id: u64,
    entries: &[Entry<'_>],
    tombstones: &[Tombstone<'_>],
) -> SsTableMeta {
    let path = dir.join(sst_filename(id));
    let mut writer =
        SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096).unwrap();
    for (key, seq, kind, value) in entries {
        writer
            .add(
                &encode_internal_key(&prefix_key(DEFAULT_CF_ID, key), *seq, *kind),
                value,
            )
            .unwrap();
    }
    for (start, end, seq) in tombstones {
        writer.add_range_tombstone(
            &prefix_key(DEFAULT_CF_ID, start),
            &prefix_key(DEFAULT_CF_ID, end),
            *seq,
        );
    }
    let summary = writer.finish().unwrap().unwrap();
    SsTableMeta {
        file_id: id,
        smallest_key: summary.smallest_user_key,
        largest_key: summary.largest_user_key,
        file_size: std::fs::metadata(path).unwrap().len(),
        num_entries: summary.num_entries,
        global_seq: None,
        // A 0.1.x table: recorded without deletion counts.
        deletions: None,
    }
}

/// An empty database directory, and its table directory.
pub(super) fn empty_db() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let sst_dir = dir.path().join("sst");
    std::fs::create_dir_all(&sst_dir).unwrap();
    std::fs::create_dir_all(dir.path().join("wal")).unwrap();
    std::fs::write(dir.path().join("LOCK"), []).unwrap();
    (dir, sst_dir)
}

pub(super) fn add(level: usize, meta: SsTableMeta) -> ManifestRecord {
    ManifestRecord::AddFile { level, meta }
}

pub(super) struct Concat;

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

pub(super) fn merging() -> Options {
    options().merge_operator(Some(Arc::new(Concat)))
}

pub(super) fn get(db: &Db, key: &[u8]) -> Option<Vec<u8>> {
    db.get(key).unwrap().map(|value| value.to_vec())
}

/// `key` as a read pinned at `seq` sees it.
pub(super) fn get_at(db: &Db, key: &[u8], seq: u64) -> Option<Vec<u8>> {
    db.engine()
        .get_slice_at(&prefix_key(DEFAULT_CF_ID, key), seq)
        .unwrap()
        .map(|value| value.to_vec())
}

pub(super) fn files_per_level(db: &Db) -> Vec<u64> {
    (0..MAX_LEVELS)
        .map(|level| {
            db.get_int_property(&format!("regolith.num-files-at-level{level}"))
                .unwrap()
        })
        .collect()
}

/// A point read, a batch read and a scan of `db` all agree with
/// `expected`, absent keys included.
pub(super) fn assert_reads(db: &Db, when: &str, expected: &[(&[u8], Option<&[u8]>)]) {
    for (key, value) in expected {
        assert_eq!(
            get(db, key).as_deref(),
            *value,
            "{when}: get({})",
            String::from_utf8_lossy(key)
        );
    }
    let keys: Vec<&[u8]> = expected.iter().map(|(key, _)| *key).collect();
    let batch = db.multi_get(&keys).unwrap();
    for ((key, value), got) in expected.iter().zip(&batch) {
        assert_eq!(
            got.as_deref(),
            *value,
            "{when}: multi_get({})",
            String::from_utf8_lossy(key)
        );
    }
    let live: Vec<(Vec<u8>, Vec<u8>)> = expected
        .iter()
        .filter_map(|(key, value)| value.map(|value| (key.to_vec(), value.to_vec())))
        .collect();
    assert_eq!(db.scan(None, None).unwrap(), live, "{when}: scan");
}

/// A copy of the database directory `from`, with a lock file of its own.
pub(super) fn copy_of(from: &Path) -> TempDir {
    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap().flatten() {
            let target = to.join(entry.file_name());
            if entry.path().is_dir() {
                copy_tree(&entry.path(), &target);
            } else if entry.file_name() != "LOCK" {
                std::fs::copy(entry.path(), &target).unwrap();
            }
        }
    }
    let copy = TempDir::new().unwrap();
    copy_tree(from, copy.path());
    std::fs::write(copy.path().join("LOCK"), []).unwrap();
    copy
}
