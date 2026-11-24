//! The check an open runs once for range deletes an earlier release left
//! behind older keys they delete (E37), on databases built by writing
//! tables and manifest records directly: the layouts here (a range delete
//! in the bottom level below a key of level 1, one in an older level 0
//! table below a newer table's older key, one carried to the bottom
//! legitimately) need chosen sequences on both sides of the range delete.
//! `tests/upgrade_buried_range_delete.rs` opens a database the 0.1.9
//! release wrote.

use tempfile::TempDir;

use super::legacy_overlap_tests::{fixture, manifest, options};
use super::legacy_test_support::*;
use super::*;
use crate::Db;
use crate::engine::buried_range_deletes::buried_range_deletes;
use crate::engine::internal_key::{VALUE_TYPE_MERGE, VALUE_TYPE_VALUE};
use crate::engine::memtable::{MemTable, MemTableConfig};
use crate::options::ArenaProfile;

fn copied(db: &Db) -> Option<u64> {
    db.get_int_property("regolith.manifest.range_deletes_copied")
}

/// Range tombstones the tables of `db`'s current version hold in `levels`.
fn range_deletes_in(db: &Db, levels: std::ops::Range<usize>) -> usize {
    db.engine().view.load().version.levels[levels]
        .iter()
        .flatten()
        .map(|file| file.reader.range_tombstones().len())
        .sum()
}

/// What a 0.1.x ingest left: level 1 holds `k` at sequence 10, level 5 `u`
/// at 12, and the bottom level the ingested table, `z` and a range delete
/// over `[b, t)` at 20, recorded as `b..z`, which meets no other table of
/// its level. A read finds `k` in level 1 before it reaches the range
/// delete that deletes it.
fn buried_below_level_1() -> TempDir {
    let (dir, sst_dir) = empty_db();
    let above = table_of(&sst_dir, 8, &[(b"k", 10, VALUE_TYPE_VALUE, b"old")], &[]);
    let beside = table_of(&sst_dir, 9, &[(b"u", 12, VALUE_TYPE_VALUE, b"u")], &[]);
    let ingest = table_of(
        &sst_dir,
        3,
        &[(b"z", 20, VALUE_TYPE_VALUE, b"ingested")],
        &[(b"b", b"t", 20)],
    );
    manifest(
        dir.path(),
        &[
            add(1, above),
            add(5, beside),
            add(6, ingest),
            ManifestRecord::SetNextFileId(101),
            ManifestRecord::SetLastSeq(20),
            ManifestRecord::SetMinWalId(5),
        ],
    );
    dir
}

const BURIED: [(&[u8], Option<&[u8]>); 3] =
    [(b"k", None), (b"u", Some(b"u")), (b"z", Some(b"ingested"))];

/// The defect on this release: a compaction writing the bottom level
/// retires the range delete, which has nothing below it, while `k`, which
/// it deletes, stays in level 1. Without the copy, `k` reads back for good.
#[test]
fn a_bottom_compaction_does_not_bring_back_a_key_a_buried_range_delete_deletes() {
    let dir = buried_below_level_1();
    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(copied(&db), Some(1));
    assert_reads(&db, "after the open", &BURIED);

    // `u`'s table and the ingested one, written to the bottom level: the
    // pass retires the range delete there.
    db.compact_range(Some(b"u"), Some(b"v")).wait().unwrap();
    assert_eq!(
        range_deletes_in(&db, 6..MAX_LEVELS),
        0,
        "the bottom pass retired the range delete of the ingested table"
    );
    assert_eq!(
        range_deletes_in(&db, 0..1),
        1,
        "the copy, flushed by the compaction, is in level 0"
    );
    assert_reads(&db, "after the bottom pass", &BURIED);
    db.flush().unwrap();
    assert_reads(&db, "after a flush", &BURIED);
    db.compact_range(None, None).wait().unwrap();
    assert_reads(&db, "after compacting everything", &BURIED);
    db.close().unwrap();
    drop(db);
    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(copied(&db), Some(0));
    assert_reads(&db, "after a reopen", &BURIED);
}

/// What a 0.1.x ingest left when the older key had not left level 0 yet: a
/// flushed table holds `k` at 10 in level 0, and the ingested table, `z` and
/// a range delete over `[b, t)` at 20, went to the bottom level, which its
/// point keys met nothing in. Level 0 is read before the bottom level, so a
/// read found `k` first.
#[test]
fn a_range_delete_in_the_bottom_level_deletes_an_older_key_of_level_0() {
    let (dir, sst_dir) = empty_db();
    let key = table_of(&sst_dir, 8, &[(b"k", 10, VALUE_TYPE_VALUE, b"old")], &[]);
    let ingest = table_of(
        &sst_dir,
        3,
        &[(b"z", 20, VALUE_TYPE_VALUE, b"ingested")],
        &[(b"b", b"t", 20)],
    );
    manifest(
        dir.path(),
        &[
            add(0, key),
            add(6, ingest),
            ManifestRecord::SetNextFileId(101),
            ManifestRecord::SetLastSeq(20),
            ManifestRecord::SetMinWalId(5),
        ],
    );
    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(copied(&db), Some(1));
    let expected: [(&[u8], Option<&[u8]>); 2] = [(b"k", None), (b"z", Some(b"ingested"))];
    assert_reads(&db, "after the open", &expected);
    assert_eq!(get_at(&db, b"k", 19).as_deref(), Some(&b"old"[..]));
    db.compact_range(None, None).wait().unwrap();
    assert_reads(&db, "after compacting everything", &expected);
}

/// The order a release that rotated its memtable into an ingest without
/// flushing it could leave inside level 0: the range delete in an older
/// table, an older key it deletes in a newer one. Read newest first, the
/// key comes first.
#[test]
fn a_range_delete_in_an_older_level_0_table_deletes_an_older_key_in_a_newer_one() {
    let (dir, sst_dir) = empty_db();
    let delete = table_of(
        &sst_dir,
        3,
        &[(b"y", 20, VALUE_TYPE_VALUE, b"ingested")],
        &[(b"a", b"n", 20)],
    );
    let key = table_of(
        &sst_dir,
        8,
        &[
            (b"c", 10, VALUE_TYPE_VALUE, b"old"),
            (b"mrg", 11, VALUE_TYPE_MERGE, b"stale"),
        ],
        &[],
    );
    manifest(
        dir.path(),
        &[
            add(0, delete),
            add(0, key),
            ManifestRecord::SetNextFileId(101),
            ManifestRecord::SetLastSeq(20),
            ManifestRecord::SetMinWalId(5),
        ],
    );
    let db = Db::open(dir.path(), merging()).unwrap();
    assert_eq!(copied(&db), Some(1));
    let expected: [(&[u8], Option<&[u8]>); 3] =
        [(b"c", None), (b"mrg", None), (b"y", Some(b"ingested"))];
    assert_reads(&db, "after the open", &expected);
    assert_eq!(get_at(&db, b"c", 19).as_deref(), Some(&b"old"[..]));
    db.compact_range(None, None).wait().unwrap();
    assert_reads(&db, "after compacting everything", &expected);
}

/// The order every read relies on: a range delete in a newer level 0 table
/// than an older key it deletes, or above it, sits in front of it. Nothing
/// there meets a source read before it, so nothing is copied.
#[test]
fn a_range_delete_already_in_front_of_what_it_deletes_is_not_copied() {
    let (dir, sst_dir) = empty_db();
    let key = table_of(&sst_dir, 8, &[(b"c", 10, VALUE_TYPE_VALUE, b"old")], &[]);
    let delete = table_of(
        &sst_dir,
        9,
        &[(b"y", 20, VALUE_TYPE_VALUE, b"y")],
        &[(b"a", b"m", 20)],
    );
    let bottom = table_of(&sst_dir, 10, &[(b"d", 5, VALUE_TYPE_VALUE, b"older")], &[]);
    manifest(
        dir.path(),
        &[
            add(0, key),
            add(0, delete),
            add(6, bottom),
            ManifestRecord::SetNextFileId(101),
            ManifestRecord::SetLastSeq(20),
            ManifestRecord::SetMinWalId(5),
        ],
    );
    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(copied(&db), Some(0));
    assert_reads(
        &db,
        "after the open",
        &[(b"c", None), (b"d", None), (b"y", Some(b"y"))],
    );
}

/// One range delete whose pieces two levels hold, both below older keys
/// they delete, is copied, and counted, once.
#[test]
fn the_pieces_of_one_range_delete_are_copied_as_one() {
    let (dir, sst_dir) = empty_db();
    let keys = table_of(
        &sst_dir,
        8,
        &[
            (b"c", 10, VALUE_TYPE_VALUE, b"old"),
            (b"m", 10, VALUE_TYPE_VALUE, b"old"),
        ],
        &[],
    );
    let first = table_of(
        &sst_dir,
        3,
        &[(b"a", 20, VALUE_TYPE_VALUE, b"a")],
        &[(b"b", b"k", 20)],
    );
    let second = table_of(
        &sst_dir,
        4,
        &[(b"z", 20, VALUE_TYPE_VALUE, b"z")],
        &[(b"k", b"t", 20)],
    );
    manifest(
        dir.path(),
        &[
            add(1, keys),
            add(5, first),
            add(6, second),
            ManifestRecord::SetNextFileId(101),
            ManifestRecord::SetLastSeq(20),
            ManifestRecord::SetMinWalId(5),
        ],
    );
    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(copied(&db), Some(1));
    assert_reads(
        &db,
        "after the open",
        &[
            (b"a", Some(b"a")),
            (b"c", None),
            (b"m", None),
            (b"z", Some(b"z")),
        ],
    );
}

/// A table of a shallower level that starts where a bottom range delete
/// ends holds none of its range, so it does not make the range delete a
/// candidate.
#[test]
fn a_table_starting_where_a_range_delete_ends_does_not_make_it_a_candidate() {
    let (dir, sst_dir) = empty_db();
    let above = table_of(&sst_dir, 8, &[(b"t", 10, VALUE_TYPE_VALUE, b"t")], &[]);
    let ingest = table_of(
        &sst_dir,
        3,
        &[(b"z", 20, VALUE_TYPE_VALUE, b"ingested")],
        &[(b"b", b"t", 20)],
    );
    manifest(
        dir.path(),
        &[
            add(1, above),
            add(6, ingest),
            ManifestRecord::SetNextFileId(101),
            ManifestRecord::SetLastSeq(20),
            ManifestRecord::SetMinWalId(5),
        ],
    );
    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(copied(&db), Some(0));
    assert_reads(
        &db,
        "after the open",
        &[(b"t", Some(b"t")), (b"z", Some(b"ingested"))],
    );
}

/// A range delete this release carried to the bottom level legitimately,
/// below newer keys of a shallower level, meets them, so the check copies
/// it. The copy deletes only what the range delete deletes: the newer key
/// still reads, the older one stays deleted, and a read at a sequence
/// before the delete still sees the older one.
#[test]
fn a_range_delete_carried_legitimately_to_the_bottom_is_copied_and_changes_no_read() {
    let (dir, sst_dir) = empty_db();
    let bottom = table_of(
        &sst_dir,
        3,
        &[(b"j", 3, VALUE_TYPE_VALUE, b"older")],
        &[(b"a", b"z", 5)],
    );
    let newer = table_of(&sst_dir, 8, &[(b"k", 10, VALUE_TYPE_VALUE, b"newer")], &[]);
    manifest(
        dir.path(),
        &[
            add(6, bottom),
            add(1, newer),
            ManifestRecord::SetNextFileId(101),
            ManifestRecord::SetLastSeq(10),
            ManifestRecord::SetMinWalId(5),
        ],
    );
    let expected: [(&[u8], Option<&[u8]>); 2] = [(b"j", None), (b"k", Some(b"newer"))];
    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(copied(&db), Some(1), "a false positive of the check");
    assert_reads(&db, "after the open", &expected);
    assert_eq!(get_at(&db, b"j", 4).as_deref(), Some(&b"older"[..]));
    assert_eq!(get_at(&db, b"k", 9), None);
    db.flush().unwrap();
    db.compact_range(None, None).wait().unwrap();
    assert_reads(&db, "after compacting everything", &expected);
    db.close().unwrap();
    drop(db);
    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(copied(&db), Some(0));
    assert_reads(&db, "after a reopen", &expected);
}

/// The check runs once. The open records it with its first edit; the next
/// open finds the record and checks nothing; a rewrite of the manifest
/// keeps the record.
#[test]
fn the_check_runs_once_and_a_manifest_rewrite_keeps_its_record() {
    let dir = buried_below_level_1();
    let sst_dir = dir.path().join("sst");
    let vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
    assert!(!vs.range_deletes_checked());
    drop(vs);

    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(copied(&db), Some(1));
    // Copied while it is open, before anything is flushed: what the open
    // alone made durable.
    let copy = copy_of(dir.path());
    drop(db);

    let mut vs = VersionSet::open(copy.path(), &copy.path().join("sst")).unwrap();
    assert!(vs.range_deletes_checked(), "the open recorded the check");
    vs.compact_manifest().unwrap();
    drop(vs);
    let vs = VersionSet::open(copy.path(), &copy.path().join("sst")).unwrap();
    assert!(vs.range_deletes_checked(), "the rewrite dropped the record");
    drop(vs);

    let db = Db::open(copy.path(), options()).unwrap();
    assert_eq!(copied(&db), Some(0));
    assert!(
        db.engine().view.load().active.has_range_tombstones(),
        "the log the first open wrote holds the copy"
    );
    assert_reads(&db, "after the rewrite", &BURIED);
}

/// A database this release creates records the check with its first edit,
/// before it can hold anything to check, so it is never checked: a range
/// delete it keeps at the bottom for a snapshot, below a newer key, is not
/// copied at a later open.
#[test]
fn a_new_database_records_the_check_and_is_never_checked() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(copied(&db), Some(0));
    db.put(b"c", b"old").unwrap();
    let snapshot = db.snapshot();
    db.delete_range(b"a", b"m").unwrap();
    db.flush().unwrap();
    db.compact_range(None, None).wait().unwrap();
    assert!(
        range_deletes_in(&db, 6..MAX_LEVELS) > 0,
        "the snapshot keeps the range delete in the bottom level"
    );
    db.put(b"d", b"new").unwrap();
    db.flush().unwrap();
    drop(snapshot);
    db.close().unwrap();
    drop(db);

    let vs = VersionSet::open(dir.path(), &dir.path().join("sst")).unwrap();
    assert!(vs.range_deletes_checked());
    drop(vs);
    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(copied(&db), Some(0));
    assert!(!db.engine().view.load().active.has_range_tombstones());
    assert_reads(&db, "after a reopen", &[(b"c", None), (b"d", Some(b"new"))]);
}

/// A manifest that records the check is not checked: the open trusts the
/// record and copies nothing, whatever its tables hold.
#[test]
fn an_open_that_finds_the_record_copies_nothing() {
    let (dir, sst_dir) = empty_db();
    let above = table_of(&sst_dir, 8, &[(b"k", 10, VALUE_TYPE_VALUE, b"old")], &[]);
    let ingest = table_of(
        &sst_dir,
        3,
        &[(b"z", 20, VALUE_TYPE_VALUE, b"ingested")],
        &[(b"b", b"t", 20)],
    );
    let image = encode_manifest_image(101, 20, 5, true, [(1, above), (6, ingest)], None).unwrap();
    std::fs::write(dir.path().join("MANIFEST"), image).unwrap();
    let vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
    assert!(vs.range_deletes_checked());
    drop(vs);
    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(copied(&db), Some(0));
    assert!(!db.engine().view.load().active.has_range_tombstones());
}

/// A read-only open copies in its own view and writes nothing; until a
/// read-write open records the check, each read-only open checks again.
#[test]
fn a_read_only_open_copies_in_its_view_and_writes_nothing() {
    let dir = buried_below_level_1();
    let before = std::fs::read(dir.path().join("MANIFEST")).unwrap();
    for _ in 0..2 {
        let db = Db::open_read_only(dir.path(), options()).unwrap();
        assert_eq!(copied(&db), Some(1));
        assert_reads(&db, "a read-only open", &BURIED);
        drop(db);
        assert_eq!(std::fs::read(dir.path().join("MANIFEST")).unwrap(), before);
    }
    drop(Db::open(dir.path(), options()).unwrap());
    let db = Db::open_read_only(dir.path(), options()).unwrap();
    assert_eq!(copied(&db), Some(0));
    assert_reads(&db, "read-only, once the check is recorded", &BURIED);
}

/// The tables an open moves out of overlapping levels have all their range
/// deletes copied by the move; the check leaves them out, so nothing is
/// counted twice. The E14 fixture's other tables hold no range delete.
#[test]
fn the_check_leaves_the_tables_the_move_copies_to_the_move() {
    let (dir, _) = fixture();
    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(
        db.get_int_property("regolith.manifest.tables_demoted"),
        Some(3)
    );
    assert_eq!(copied(&db), Some(0));

    // The same through the check itself: with no table left out it finds
    // the moved table's range delete, which meets the level 0 table above
    // it; with the moved tables left out it finds nothing.
    let vs = VersionSet::open(dir.path(), &dir.path().join("sst")).unwrap();
    let memtable = MemTable::new(&MemTableConfig::new(ArenaProfile::SERVER, 1 << 20, 2)).unwrap();
    let version = vs.current();
    assert_eq!(buried_range_deletes(&version, &memtable, 0).len(), 1);
    assert!(buried_range_deletes(&version, &memtable, 3).is_empty());
}

/// What the check reads beyond the tables: a key of the recovered memtable
/// read before a table makes that table's range delete a candidate.
#[test]
fn a_key_of_the_recovered_memtable_makes_a_range_delete_below_it_a_candidate() {
    let (dir, sst_dir) = empty_db();
    let ingest = table_of(
        &sst_dir,
        3,
        &[(b"z", 20, VALUE_TYPE_VALUE, b"ingested")],
        &[(b"b", b"t", 20)],
    );
    manifest(
        dir.path(),
        &[
            add(6, ingest),
            ManifestRecord::SetNextFileId(101),
            ManifestRecord::SetLastSeq(20),
            ManifestRecord::SetMinWalId(5),
        ],
    );
    let vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
    let version = vs.current();
    let memtable = MemTable::new(&MemTableConfig::new(ArenaProfile::SERVER, 1 << 20, 2)).unwrap();
    assert!(buried_range_deletes(&version, &memtable, 0).is_empty());
    memtable.put(
        &crate::column_family::prefix_key(crate::column_family::DEFAULT_CF_ID, b"k"),
        b"replayed",
        15,
    );
    let found = buried_range_deletes(&version, &memtable, 0);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].seq, 20);
}
