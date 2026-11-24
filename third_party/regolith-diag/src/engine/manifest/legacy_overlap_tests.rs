//! Recovery of legacy levels widened by an ingested range tombstone.

use super::*;
use crate::column_family::{DEFAULT_CF_ID, prefix_key};
use crate::engine::internal_key::{VALUE_TYPE_VALUE, encode_internal_key};
use crate::engine::sstable::SsTableWriter;
use crate::options::CompressionType;
use crate::{Db, Options};
use tempfile::TempDir;

fn table(
    dir: &Path,
    id: u64,
    seq: u64,
    points: &[(&[u8], &[u8])],
    tombstone: Option<(&[u8], &[u8])>,
) -> SsTableMeta {
    let path = dir.join(sst_filename(id));
    let mut writer =
        SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096).unwrap();
    for (key, value) in points {
        writer
            .add(
                &encode_internal_key(&prefix_key(DEFAULT_CF_ID, key), seq, VALUE_TYPE_VALUE),
                value,
            )
            .unwrap();
    }
    if let Some((start, end)) = tombstone {
        writer.add_range_tombstone(
            &prefix_key(DEFAULT_CF_ID, start),
            &prefix_key(DEFAULT_CF_ID, end),
            seq,
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

/// Original single-edit frames, not the batch encoding used for the repair.
pub(super) fn manifest(dir: &Path, records: &[ManifestRecord]) -> Vec<u8> {
    let mut bytes = VersionSet::encode_stamp().to_vec();
    for record in records {
        let mut payload = Vec::new();
        record.encode(&mut payload);
        let len = payload.len() as u32;
        bytes.extend_from_slice(&len.to_le_bytes());
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&checksum::manifest_record(len, &payload).to_le_bytes());
    }
    std::fs::write(dir.join("MANIFEST"), &bytes).unwrap();
    bytes
}

pub(super) fn fixture() -> (TempDir, Vec<u8>) {
    fixture_with_ingest_seq(20)
}

pub(super) fn fixture_with_ingest_seq(ingest_seq: u64) -> (TempDir, Vec<u8>) {
    let dir = TempDir::new().unwrap();
    let sst_dir = dir.path().join("sst");
    std::fs::create_dir_all(&sst_dir).unwrap();
    std::fs::create_dir_all(dir.path().join("wal")).unwrap();
    std::fs::write(dir.path().join("LOCK"), []).unwrap();
    let old = table(&sst_dir, 8, 10, &[(b"k", b"old"), (b"x", b"x")], None);
    // Its point is disjoint from the old table, but the range tombstone
    // widens its recorded range to b..z: the legacy ingest placement bug.
    let ingest = table(
        &sst_dir,
        3,
        ingest_seq,
        &[(b"z", b"ingested")],
        Some((b"b", b"t")),
    );
    let upper = table(&sst_dir, 10, 30, &[(b"k", b"new")], None);
    let l0 = table(&sst_dir, 90, 50, &[(b"e", b"l0")], None);
    let deep = table(&sst_dir, 77, 5, &[(b"r", b"deleted")], None);
    let bytes = manifest(
        dir.path(),
        &[
            ManifestRecord::AddFile {
                level: 2,
                meta: old,
            },
            ManifestRecord::AddFile {
                level: 2,
                meta: ingest,
            },
            ManifestRecord::AddFile {
                level: 1,
                meta: upper,
            },
            ManifestRecord::AddFile { level: 0, meta: l0 },
            ManifestRecord::AddFile {
                level: 3,
                meta: deep,
            },
            ManifestRecord::SetNextFileId(101),
            ManifestRecord::SetLastSeq(50),
            ManifestRecord::SetMinWalId(5),
        ],
    );
    (dir, bytes)
}

pub(super) fn ids(version: &Version, level: usize) -> Vec<u64> {
    version.levels[level]
        .iter()
        .map(|f| f.meta.file_id)
        .collect()
}

/// How many tables the open that returned `vs` moved, with the move
/// recorded in the manifest the way the engine's first edit records it.
pub(super) fn record_move(vs: &mut VersionSet) -> u64 {
    let tables = vs.take_demotion().map_or(0, |demotion| demotion.tables);
    vs.apply(&[]).unwrap();
    tables
}

pub(super) fn assert_layout(version: &Version) {
    assert_eq!(ids(version, 0), [8, 3, 10, 90]);
    assert!(version.levels[1].is_empty());
    assert!(version.levels[2].is_empty());
    assert_eq!(ids(version, 3), [77]);
    assert!(version.levels_are_sorted_runs());
    assert_eq!(version.last_seq, 50);
    assert_eq!(version.next_file_id, 101);
    assert_eq!(version.min_wal_id, 5);
}

#[test]
fn legacy_overlap_demotes_the_whole_prefix_and_preserves_table_age() {
    let (dir, _) = fixture();
    let sst_dir = dir.path().join("sst");
    let mut vs = VersionSet::open(dir.path(), &sst_dir).expect("legacy overlap must be repairable");
    assert_layout(&vs.current());
    assert_eq!(record_move(&mut vs), 3);
    drop(vs);

    let repaired = std::fs::read(dir.path().join("MANIFEST")).unwrap();
    let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
    assert_layout(&vs.current());
    assert_eq!(record_move(&mut vs), 0);
    assert_eq!(std::fs::read(vs.manifest_path()).unwrap(), repaired);
    vs.compact_manifest().unwrap();
    drop(vs);
    assert_layout(&VersionSet::open(dir.path(), &sst_dir).unwrap().current());
}

#[test]
fn read_only_open_repairs_the_view_without_modifying_the_manifest() {
    let (dir, bytes) = fixture();
    let mut vs = VersionSet::open_read_only(
        &crate::env::std_env(),
        dir.path(),
        &dir.path().join("sst"),
        MetadataPolicy::Pinned,
        None,
    )
    .unwrap();
    assert_layout(&vs.current());
    assert_eq!(vs.take_demotion().map(|demotion| demotion.tables), Some(3));
    assert_eq!(std::fs::read(vs.manifest_path()).unwrap(), bytes);
    drop(vs);
    let db = Db::open_read_only(dir.path(), options()).unwrap();
    assert_eq!(
        db.get_int_property("regolith.manifest.tables_demoted"),
        Some(3)
    );
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(b"new".as_slice()));
    assert_eq!(db.get(b"r").unwrap(), None);
    assert_eq!(std::fs::read(dir.path().join("MANIFEST")).unwrap(), bytes);
}

pub(super) fn options() -> Options {
    Options {
        max_background_compactions: 0,
        ..Options::default()
    }
}

#[test]
fn a_newer_demoted_range_tombstone_hides_an_older_shallow_point() {
    let (dir, _) = fixture_with_ingest_seq(40);
    // A legacy ingest could advance the manifest sequence while an older
    // point was still unflushed. The tombstone must also mask its WAL replay.
    let mut wal = crate::engine::wal::Wal::create(
        &dir.path()
            .join("wal")
            .join(crate::engine::wal::wal_filename(5)),
    )
    .unwrap();
    wal.append_put(&prefix_key(DEFAULT_CF_ID, b"k"), b"unflushed", 35)
        .unwrap();
    wal.sync_data().unwrap();
    drop(wal);

    let db = Db::open_read_only(dir.path(), options()).unwrap();
    assert_eq!(db.get(b"k").unwrap(), None);
    assert_eq!(db.multi_get(&[b"k"]).unwrap(), [None]);
    assert_eq!(
        db.engine()
            .get_slice_at(&prefix_key(DEFAULT_CF_ID, b"k"), 30)
            .unwrap()
            .as_deref(),
        Some(b"new".as_slice())
    );
    drop(db);

    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(db.get(b"k").unwrap(), None);
    assert_eq!(db.multi_get(&[b"k"]).unwrap(), [None]);
    assert!(!db.scan(None, None).unwrap().iter().any(|(k, _)| k == b"k"));
    assert_eq!(db.get(b"e").unwrap().as_deref(), Some(b"l0".as_slice()));
    let snapshot = db.snapshot();
    db.put(b"k", b"latest").unwrap();
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(b"latest".as_slice()));
    assert_eq!(snapshot.get(b"k").unwrap(), None);
    db.flush().unwrap();
    db.compact_range(None, None).wait().unwrap();
    assert_eq!(snapshot.get(b"k").unwrap(), None);
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(b"latest".as_slice()));
    drop(snapshot);
    db.close().unwrap();
    drop(db);
    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(b"latest".as_slice()));
}

#[test]
fn repaired_reads_snapshots_and_compaction_keep_the_newest_values() {
    let (dir, _) = fixture();
    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(
        db.get_int_property("regolith.manifest.tables_demoted"),
        Some(3)
    );
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(b"new".as_slice()));
    assert_eq!(db.get(b"e").unwrap().as_deref(), Some(b"l0".as_slice()));
    assert_eq!(
        db.get(b"z").unwrap().as_deref(),
        Some(b"ingested".as_slice())
    );
    assert_eq!(
        db.get(b"r").unwrap(),
        None,
        "the range tombstone still applies"
    );
    let snapshot = db.snapshot();
    db.put(b"k", b"latest").unwrap();
    db.delete(b"z").unwrap();
    db.flush().unwrap();
    db.compact_range(None, None).wait().unwrap();
    assert_eq!(
        snapshot.get(b"k").unwrap().as_deref(),
        Some(b"new".as_slice())
    );
    assert_eq!(
        snapshot.get(b"z").unwrap().as_deref(),
        Some(b"ingested".as_slice())
    );
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(b"latest".as_slice()));
    assert_eq!(db.get(b"z").unwrap(), None);
    drop(snapshot);
    db.close().unwrap();
    drop(db);
    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(
        db.get_int_property("regolith.manifest.tables_demoted"),
        Some(0)
    );
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(b"latest".as_slice()));
    assert_eq!(db.get(b"z").unwrap(), None);
    assert_eq!(db.get(b"r").unwrap(), None);
}

#[test]
fn a_torn_repair_batch_is_replayed_whole_or_repaired_again() {
    let (dir, legacy) = fixture();
    let sst_dir = dir.path().join("sst");
    record_move(&mut VersionSet::open(dir.path(), &sst_dir).unwrap());
    let repaired = std::fs::read(dir.path().join("MANIFEST")).unwrap();
    assert!(repaired.starts_with(&legacy));
    assert!(repaired.len() > legacy.len());
    for cut in legacy.len()..=repaired.len() {
        std::fs::write(dir.path().join("MANIFEST"), &repaired[..cut]).unwrap();
        let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        assert_layout(&vs.current());
        assert_eq!(
            record_move(&mut vs),
            if cut == repaired.len() { 0 } else { 3 }
        );
        assert_eq!(
            std::fs::read(vs.manifest_path()).unwrap(),
            repaired,
            "cut {cut}"
        );
    }
}

#[test]
fn all_levels_through_the_deepest_overlap_are_demoted() {
    let (dir, _) = fixture();
    let sst_dir = dir.path().join("sst");
    let old = table(&sst_dir, 130, 3, &[(b"v", b"v")], None);
    let new = table(&sst_dir, 131, 4, &[(b"z", b"z")], Some((b"u", b"w")));
    let untouched = table(&sst_dir, 140, 1, &[(b"a", b"a")], None);
    let mut bytes = std::fs::read(dir.path().join("MANIFEST")).unwrap();
    for record in [
        ManifestRecord::AddFile {
            level: 4,
            meta: old,
        },
        ManifestRecord::AddFile {
            level: 4,
            meta: new,
        },
        ManifestRecord::AddFile {
            level: 5,
            meta: untouched,
        },
        ManifestRecord::SetNextFileId(141),
    ] {
        let mut payload = Vec::new();
        record.encode(&mut payload);
        let len = payload.len() as u32;
        bytes.extend_from_slice(&len.to_le_bytes());
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&checksum::manifest_record(len, &payload).to_le_bytes());
    }
    std::fs::write(dir.path().join("MANIFEST"), bytes).unwrap();
    let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
    assert_eq!(ids(&vs.current(), 0), [130, 131, 77, 8, 3, 10, 90]);
    assert!(vs.current().levels[1..=4].iter().all(Vec::is_empty));
    assert_eq!(ids(&vs.current(), 5), [140]);
    assert_eq!(record_move(&mut vs), 6);
    assert_eq!(vs.current().next_file_id, 141);
}

#[test]
fn checkpoint_and_backup_preserve_the_repaired_layout() {
    let (dir, _) = fixture();
    let db = Db::open(dir.path(), options()).unwrap();
    let checkpoint = TempDir::new().unwrap();
    crate::Checkpoint::new(&db)
        .unwrap()
        .create(checkpoint.path())
        .wait()
        .unwrap();
    let checked = Db::open(checkpoint.path(), options()).unwrap();
    assert_eq!(
        checked.get(b"k").unwrap().as_deref(),
        Some(b"new".as_slice())
    );
    assert_eq!(checked.get(b"r").unwrap(), None);
    assert_eq!(
        checked.get_int_property("regolith.manifest.tables_demoted"),
        Some(0)
    );
    let backup_dir = TempDir::new().unwrap();
    let mut backups = crate::BackupEngine::open(backup_dir.path()).unwrap();
    let backup = backups.create_backup(&db).unwrap();
    let restore_dir = TempDir::new().unwrap();
    backups.restore(backup, restore_dir.path(), None).unwrap();
    let restored = Db::open(restore_dir.path(), options()).unwrap();
    assert_eq!(
        restored.get(b"k").unwrap().as_deref(),
        Some(b"new".as_slice())
    );
    assert_eq!(
        restored.get(b"z").unwrap().as_deref(),
        Some(b"ingested".as_slice())
    );
    assert_eq!(restored.get(b"r").unwrap(), None);
    assert_eq!(
        restored.get_int_property("regolith.manifest.tables_demoted"),
        Some(0)
    );
}
