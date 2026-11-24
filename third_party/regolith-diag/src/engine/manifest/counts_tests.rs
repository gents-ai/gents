//! Tag 8, the deletion counts of a table the manifest lists: written with a
//! new table in one batch, written alone for a table that had none, and
//! never more than knowledge about a table that is there.

use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;

use super::tests::make_live_sst;
use super::*;

fn counts(points: u64, dead: u64, newest_seq: u64) -> Deletions {
    Deletions {
        points,
        dead,
        newest_seq,
    }
}

/// A two-entry table of `dir` with the given counts, as a flush would add it.
fn table(dir: &Path, file_id: u64, deletions: Option<Deletions>) -> Arc<LiveSst> {
    let plain = make_live_sst(dir, file_id, b"a", b"z");
    LiveSst::new(
        SsTableMeta {
            deletions,
            ..plain.meta.clone()
        },
        Arc::clone(&plain.reader),
        plain.sequence_ceiling,
    )
}

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("sst")).unwrap();
        Self { dir }
    }

    fn sst_dir(&self) -> std::path::PathBuf {
        self.dir.path().join("sst")
    }

    fn manifest(&self) -> std::path::PathBuf {
        self.dir.path().join("MANIFEST")
    }

    fn open(&self) -> VersionSet {
        VersionSet::open(self.dir.path(), &self.sst_dir()).unwrap()
    }

    fn table(&self, file_id: u64, deletions: Option<Deletions>) -> Arc<LiveSst> {
        table(&self.sst_dir(), file_id, deletions)
    }
}

/// The counts of every table of level `level`, in order.
fn listed(vs: &VersionSet, level: usize) -> Vec<(u64, Option<Deletions>)> {
    vs.current().levels[level]
        .iter()
        .map(|file| (file.meta.file_id, file.meta.deletions))
        .collect()
}

#[test]
fn a_table_and_its_counts_are_one_batch_and_read_back_together() {
    let f = Fixture::new();
    let mut vs = f.open();
    let before = std::fs::read(f.manifest()).unwrap().len();
    vs.apply(&[VersionEdit::AddFile {
        level: 0,
        file: f.table(1, Some(counts(1, 2, 7))),
    }])
    .unwrap();
    assert_eq!(vs.current().uncounted, 0);

    let data = std::fs::read(f.manifest()).unwrap();
    let batch = tail::batch_at(&data, before).expect("one batch was appended");
    assert_eq!(batch.end, data.len(), "and only one");
    let mut pos = 0;
    let mut tags = Vec::new();
    while pos < batch.records.len() {
        tags.push(batch.records[pos]);
        ManifestRecord::decode(batch.records, &mut pos).unwrap();
    }
    // A new manifest's first batch also records, in front of the edits,
    // that its range deletes were checked: it has none to check.
    assert_eq!(
        tags,
        [TAG_RANGE_DELETES_CHECKED, TAG_ADD_FILE, TAG_TABLE_COUNTS]
    );

    drop(vs);
    let vs = f.open();
    assert_eq!(listed(&vs, 0), [(1, Some(counts(1, 2, 7)))]);
    assert_eq!(vs.current().uncounted, 0);
}

#[test]
fn a_table_recorded_without_counts_is_unknown_until_an_edit_counts_it() {
    let f = Fixture::new();
    let mut vs = f.open();
    vs.apply(&[
        VersionEdit::AddFile {
            level: 1,
            file: f.table(1, None),
        },
        VersionEdit::AddFile {
            level: 2,
            file: f.table(2, Some(counts(0, 0, 0))),
        },
    ])
    .unwrap();
    assert_eq!(vs.current().uncounted, 1);
    let reader = Arc::clone(&vs.current().levels[1][0].reader);

    vs.apply(&[VersionEdit::CountFile {
        level: 1,
        file_id: 1,
        deletions: counts(0, 0, 0),
    }])
    .unwrap();
    let version = vs.current();
    assert_eq!(version.uncounted, 0);
    assert_eq!(
        listed(&vs, 1),
        [(1, Some(counts(0, 0, 0)))],
        "counted in place"
    );
    assert_eq!(listed(&vs, 2), [(2, Some(counts(0, 0, 0)))]);
    assert!(
        Arc::ptr_eq(&version.levels[1][0].reader, &reader),
        "the table keeps its reader and its file"
    );

    drop(version);
    drop(vs);
    let vs = f.open();
    assert_eq!(vs.current().uncounted, 0, "the count was recorded");
    assert_eq!(listed(&vs, 1)[0], (1, Some(counts(0, 0, 0))));
}

#[test]
fn counts_for_a_table_no_longer_listed_are_ignored() {
    let f = Fixture::new();
    let mut vs = f.open();
    vs.apply(&[VersionEdit::AddFile {
        level: 0,
        file: f.table(1, None),
    }])
    .unwrap();
    vs.apply(&[
        VersionEdit::RemoveFile {
            level: 0,
            file_id: 1,
        },
        VersionEdit::AddFile {
            level: 1,
            file: f.table(2, None),
        },
    ])
    .unwrap();
    // The table was compacted away between a count of it and the edit:
    // the edit changes nothing in memory, and the record it leaves names a
    // table the manifest no longer lists. So does one for a table it never
    // listed.
    for file_id in [1, 99] {
        vs.apply(&[VersionEdit::CountFile {
            level: 0,
            file_id,
            deletions: counts(1, 1, 1),
        }])
        .unwrap();
    }
    assert_eq!(listed(&vs, 0), []);
    assert_eq!(listed(&vs, 1), [(2, None)]);
    assert_eq!(vs.current().uncounted, 1);

    drop(vs);
    let vs = f.open();
    assert_eq!(listed(&vs, 0), []);
    assert_eq!(listed(&vs, 1), [(2, None)]);
    assert_eq!(vs.current().uncounted, 1);
}

#[test]
fn counts_that_cannot_be_the_tables_are_not_believed() {
    let f = Fixture::new();
    let mut vs = f.open();
    // More hidden entries than the table has entries, and more point
    // deletions than hidden entries.
    vs.apply(&[
        VersionEdit::AddFile {
            level: 1,
            file: f.table(1, Some(counts(0, 3, 0))),
        },
        VersionEdit::AddFile {
            level: 2,
            file: f.table(2, Some(counts(2, 1, 0))),
        },
    ])
    .unwrap();
    drop(vs);
    let vs = f.open();
    assert_eq!(listed(&vs, 1), [(1, None)]);
    assert_eq!(listed(&vs, 2), [(2, None)]);
    assert_eq!(vs.current().uncounted, 2);
}

#[test]
fn a_rewrite_of_the_manifest_keeps_the_counts() {
    let f = Fixture::new();
    let mut vs = f.open();
    vs.apply(&[
        VersionEdit::AddFile {
            level: 0,
            file: f.table(1, Some(counts(1, 1, 5))),
        },
        VersionEdit::AddFile {
            level: 3,
            file: f.table(2, None),
        },
    ])
    .unwrap();
    vs.compact_manifest().unwrap();
    drop(vs);
    let vs = f.open();
    assert_eq!(listed(&vs, 0), [(1, Some(counts(1, 1, 5)))]);
    assert_eq!(listed(&vs, 3), [(2, None)]);
    assert_eq!(vs.current().uncounted, 1);
}

#[test]
fn a_manifest_written_before_the_counts_opens_with_every_table_unknown() {
    let f = Fixture::new();
    let mut vs = f.open();
    vs.apply(&[
        VersionEdit::AddFile {
            level: 0,
            file: f.table(1, Some(counts(1, 1, 5))),
        },
        VersionEdit::AddFile {
            level: 4,
            file: f.table(2, Some(counts(0, 0, 0))),
        },
    ])
    .unwrap();
    drop(vs);
    forget_counts(f.dir.path());
    let data = std::fs::read(f.manifest()).unwrap();
    let mut offset = MANIFEST_STAMP_LEN;
    while let Some(batch) = tail::batch_at(&data, offset) {
        let mut pos = 0;
        while pos < batch.records.len() {
            assert_ne!(batch.records[pos], TAG_TABLE_COUNTS);
            ManifestRecord::decode(batch.records, &mut pos).unwrap();
        }
        offset = batch.end;
    }

    let vs = f.open();
    assert_eq!(listed(&vs, 0), [(1, None)]);
    assert_eq!(listed(&vs, 4), [(2, None)]);
    assert_eq!(vs.current().uncounted, 2);
}

/// A batch is all or nothing: cut anywhere inside the batch that adds a
/// table with its counts, the manifest reads back without the table, and
/// never with the table and no counts.
#[test]
fn a_cut_inside_a_tables_batch_leaves_neither_the_table_nor_its_counts() {
    let f = Fixture::new();
    let mut vs = f.open();
    vs.apply(&[VersionEdit::SetLastSeq(3)]).unwrap();
    let before = std::fs::read(f.manifest()).unwrap().len();
    vs.apply(&[VersionEdit::AddFile {
        level: 0,
        file: f.table(1, Some(counts(1, 2, 7))),
    }])
    .unwrap();
    drop(vs);
    let whole = std::fs::read(f.manifest()).unwrap();
    assert!(whole.len() > before + 8);

    for len in before + 1..whole.len() {
        std::fs::write(f.manifest(), &whole[..len]).unwrap();
        let vs = f.open();
        assert_eq!(listed(&vs, 0), [], "cut at {len} of {}", whole.len());
        assert_eq!(
            vs.current().last_seq,
            3,
            "cut at {len}: the batch before it stands"
        );
        assert_eq!(
            vs.dropped_tail().map(|tail| tail.offset),
            Some(before as u64),
            "cut at {len}"
        );
    }
    std::fs::write(f.manifest(), &whole).unwrap();
    let vs = f.open();
    assert_eq!(listed(&vs, 0), [(1, Some(counts(1, 2, 7)))]);
}

/// The census's batch holds counts alone and is not synced: a crash may
/// lose it, so a whole one after a torn batch proves nothing about the torn
/// one, and the open drops the tail as it would without it.
#[test]
fn a_batch_of_counts_alone_needs_no_sync() {
    let alone = ManifestRecord::TableCounts {
        file_id: 1,
        deletions: counts(0, 0, 0),
    };
    assert!(!alone.requires_sync());

    let batch =
        |records: &[ManifestRecord]| VersionSet::encode_records(records, 0, None).unwrap().0;
    let synced = batch(&[ManifestRecord::SetLastSeq(1)]);
    let counted = batch(&[alone]);
    let trailing = batch(&[ManifestRecord::SetNextFileId(9)]);
    let mut data = VersionSet::encode_stamp().to_vec();
    data.extend_from_slice(&synced);
    let torn_at = data.len();
    let mut torn = synced.clone();
    torn[4] ^= 0xFF;
    data.extend_from_slice(&torn);
    data.extend_from_slice(&counted);
    data.extend_from_slice(&trailing);

    let tail = tail::judge(&data, torn_at, Path::new("/db/MANIFEST"), None)
        .expect("a crash can have left this tail");
    assert_eq!(tail.offset, torn_at as u64);
    assert_eq!(tail.bytes, (data.len() - torn_at) as u64);
}
