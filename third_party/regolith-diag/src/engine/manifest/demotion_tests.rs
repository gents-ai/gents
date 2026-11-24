//! What an open that moves overlapping levels owes beyond the move: the
//! newest version from every level it moved, the moved tables' range
//! tombstones in front of the keys they cover, the move in the manifest
//! with its first edit and not before, and the sequence of an ingested
//! table kept.
//!
//! The databases here are built by writing tables and manifest records
//! directly: 0.1.x only ever left an overlap in its bottom level, and these
//! tests need one in level 1 and in level 3, with chosen sequences on both
//! sides of the range tombstone. `tests/upgrade_overlapping_level.rs` opens
//! a database the 0.1.9 release wrote.

use std::sync::Arc;

use tempfile::TempDir;

use super::legacy_overlap_tests::{ids, manifest, options, record_move};
use super::legacy_test_support::*;
use super::*;
use crate::Db;
use crate::column_family::{DEFAULT_CF_ID, prefix_key};
use crate::engine::internal_key::{VALUE_TYPE_MERGE, VALUE_TYPE_VALUE};
use crate::engine::memtable::range_tombstone_work;
use crate::engine::seal::test_keys::{TestKeys, keyring};

/// Level 1 itself overlaps: the old table and, beside it, a table with one
/// key and a range tombstone that widens its recorded range over the old
/// one's. Level 0 holds a newer version, level 2 an older key the
/// tombstone covers.
fn overlapping_level_1() -> TempDir {
    let (dir, sst_dir) = empty_db();
    let old = table_of(
        &sst_dir,
        8,
        &[
            (b"k", 10, VALUE_TYPE_VALUE, b"k-old"),
            (b"x", 10, VALUE_TYPE_VALUE, b"x"),
        ],
        &[],
    );
    let ingest = table_of(
        &sst_dir,
        3,
        &[(b"z", 20, VALUE_TYPE_VALUE, b"ingested")],
        &[(b"b", b"t", 20)],
    );
    let newer = table_of(&sst_dir, 90, &[(b"k", 30, VALUE_TYPE_VALUE, b"k-new")], &[]);
    let deep = table_of(&sst_dir, 77, &[(b"r", 5, VALUE_TYPE_VALUE, b"r-old")], &[]);
    manifest(
        dir.path(),
        &[
            add(1, old),
            add(1, ingest),
            add(0, newer),
            add(2, deep),
            ManifestRecord::SetNextFileId(101),
            ManifestRecord::SetLastSeq(30),
            ManifestRecord::SetMinWalId(5),
        ],
    );
    dir
}

#[test]
fn an_overlapping_level_1_opens_with_its_tables_in_level_0_and_reads_the_newest_versions() {
    let dir = overlapping_level_1();
    let db = Db::open(dir.path(), options()).expect("an overlapping level 1 opens");
    assert_eq!(files_per_level(&db), [3, 0, 1, 0, 0, 0, 0]);
    assert_eq!(
        db.get_int_property("regolith.manifest.tables_demoted"),
        Some(2)
    );
    let expected: [(&[u8], Option<&[u8]>); 4] = [
        (b"k", Some(b"k-new")),
        (b"r", None),
        (b"x", Some(b"x")),
        (b"z", Some(b"ingested")),
    ];
    assert_reads(&db, "after the open", &expected);
    // As each sequence saw it: the old value until the tombstone, nothing
    // until the newer put.
    assert_eq!(get_at(&db, b"k", 19).as_deref(), Some(&b"k-old"[..]));
    assert_eq!(get_at(&db, b"k", 20), None);
    assert_eq!(get_at(&db, b"k", 30).as_deref(), Some(&b"k-new"[..]));
    assert_eq!(get_at(&db, b"r", 19).as_deref(), Some(&b"r-old"[..]));
    db.close().unwrap();
    drop(db);

    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(
        db.get_int_property("regolith.manifest.tables_demoted"),
        Some(0)
    );
    assert_reads(&db, "after a reopen", &expected);
}

/// Level 3 overlaps, and levels 1 and 2 hold newer versions of a key it
/// holds and newer operands of a key it merges. Level 4, below, holds an
/// older key the range tombstone covers. Level 1, above, holds an operand of
/// `h` older than that tombstone, and level 0 one newer.
fn overlapping_level_3() -> TempDir {
    let (dir, sst_dir) = empty_db();
    let old = table_of(
        &sst_dir,
        8,
        &[
            (b"k", 10, VALUE_TYPE_VALUE, b"v3"),
            (b"mrg", 11, VALUE_TYPE_MERGE, b"a"),
            (b"x", 10, VALUE_TYPE_VALUE, b"x"),
        ],
        &[],
    );
    let ingest = table_of(
        &sst_dir,
        3,
        &[(b"z", 20, VALUE_TYPE_VALUE, b"ingested")],
        &[(b"b", b"j", 20)],
    );
    let level_2 = table_of(
        &sst_dir,
        10,
        &[
            (b"k", 30, VALUE_TYPE_VALUE, b"v2"),
            (b"mrg", 31, VALUE_TYPE_MERGE, b"b"),
        ],
        &[],
    );
    let level_1 = table_of(
        &sst_dir,
        11,
        &[
            (b"h", 12, VALUE_TYPE_MERGE, b"stale"),
            (b"k", 40, VALUE_TYPE_VALUE, b"v1"),
            (b"mrg", 41, VALUE_TYPE_MERGE, b"c"),
        ],
        &[],
    );
    let level_0 = table_of(
        &sst_dir,
        90,
        &[
            (b"e", 50, VALUE_TYPE_VALUE, b"l0"),
            (b"h", 45, VALUE_TYPE_MERGE, b"fresh"),
        ],
        &[],
    );
    let level_4 = table_of(&sst_dir, 77, &[(b"c", 5, VALUE_TYPE_VALUE, b"deep")], &[]);
    manifest(
        dir.path(),
        &[
            add(3, old),
            add(3, ingest),
            add(2, level_2),
            add(1, level_1),
            add(0, level_0),
            add(4, level_4),
            ManifestRecord::SetNextFileId(101),
            ManifestRecord::SetLastSeq(50),
            ManifestRecord::SetMinWalId(5),
        ],
    );
    dir
}

/// The reads that tell a move of levels 1 to 3 from a move of level 3
/// alone: the second puts level 3's `v3` and operand `a` in front of the
/// newer ones levels 1 and 2 hold. Nothing here looks at where a table is.
#[test]
fn an_overlapping_level_3_reads_the_newest_versions_which_levels_1_and_2_hold() {
    let dir = overlapping_level_3();
    let db = Db::open(dir.path(), merging()).expect("an overlapping level 3 opens");
    let expected: [(&[u8], Option<&[u8]>); 7] = [
        (b"c", None),
        (b"e", Some(b"l0")),
        // The operand older than the range tombstone is deleted by it,
        // though its table is read first; the newer one is not.
        (b"h", Some(b"fresh")),
        (b"k", Some(b"v1")),
        (b"mrg", Some(b"abc")),
        (b"x", Some(b"x")),
        (b"z", Some(b"ingested")),
    ];
    assert_reads(&db, "after the open", &expected);
    // Each level's version at the sequence it was the newest.
    assert_eq!(get_at(&db, b"k", 15).as_deref(), Some(&b"v3"[..]));
    assert_eq!(get_at(&db, b"k", 35).as_deref(), Some(&b"v2"[..]));
    assert_eq!(get_at(&db, b"c", 19).as_deref(), Some(&b"deep"[..]));
    assert_eq!(get_at(&db, b"c", 20), None);

    // A snapshot keeps them through later writes and a compaction of
    // everything.
    let snapshot = db.snapshot();
    db.put(b"k", b"v0").unwrap();
    db.merge(b"mrg", b"d").unwrap();
    db.flush().unwrap();
    db.compact_range(None, None).wait().unwrap();
    assert_eq!(snapshot.get(b"k").unwrap().as_deref(), Some(&b"v1"[..]));
    assert_eq!(snapshot.get(b"mrg").unwrap().as_deref(), Some(&b"abc"[..]));
    assert_eq!(snapshot.get(b"c").unwrap(), None);
    assert_eq!(snapshot.get(b"h").unwrap().as_deref(), Some(&b"fresh"[..]));
    assert_eq!(get(&db, b"k").as_deref(), Some(&b"v0"[..]));
    assert_eq!(get(&db, b"mrg").as_deref(), Some(&b"abcd"[..]));
    assert_eq!(get(&db, b"h").as_deref(), Some(&b"fresh"[..]));
    drop(snapshot);
    db.close().unwrap();
    drop(db);

    let db = Db::open(dir.path(), merging()).unwrap();
    assert_eq!(get(&db, b"k").as_deref(), Some(&b"v0"[..]));
    assert_eq!(get(&db, b"mrg").as_deref(), Some(&b"abcd"[..]));
    assert_eq!(get(&db, b"c"), None);
}

/// Where the tables of that database end up: levels 1 to 3 in level 0,
/// oldest level first and in front of the table level 0 held; level 4,
/// below the overlap, where it was.
#[test]
fn an_overlapping_level_3_moves_levels_1_to_3_and_leaves_level_4() {
    let dir = overlapping_level_3();
    let mut vs = VersionSet::open(dir.path(), &dir.path().join("sst")).unwrap();
    assert_eq!(ids(&vs.current(), 0), [8, 3, 10, 11, 90]);
    assert!(vs.current().levels[1..=3].iter().all(Vec::is_empty));
    assert_eq!(ids(&vs.current(), 4), [77]);
    let demotion = vs.take_demotion().unwrap();
    assert_eq!((demotion.tables, demotion.deepest_level), (4, 3));
    drop(vs);

    let db = Db::open(dir.path(), merging()).unwrap();
    assert_eq!(files_per_level(&db), [5, 0, 0, 0, 1, 0, 0]);
    assert_eq!(
        db.get_int_property("regolith.manifest.tables_demoted"),
        Some(4)
    );
}

/// The tombstone of the moved table is newer than `k` in the level above
/// it. Compacting only `[z, ..)` takes that table and the older one beside
/// it down to level 1 and leaves the newer `k` in level 0: the tombstone
/// must still be read first.
#[test]
fn a_compaction_that_takes_a_moved_table_down_leaves_its_range_tombstone_in_force() {
    let (dir, _) = super::legacy_overlap_tests::fixture_with_ingest_seq(40);
    let db = Db::open(dir.path(), options()).unwrap();
    let expected: [(&[u8], Option<&[u8]>); 5] = [
        (b"e", Some(b"l0")),
        (b"k", None),
        (b"r", None),
        (b"x", Some(b"x")),
        (b"z", Some(b"ingested")),
    ];
    assert_reads(&db, "before any compaction", &expected);

    db.compact_range(Some(b"z"), None).wait().unwrap();
    let levels = files_per_level(&db);
    assert_eq!(
        (levels[1], levels[3]),
        (1, 1),
        "the two tables of the overlapping level were compacted to level 1"
    );
    assert!(
        levels[0] >= 2,
        "the tables holding `k` and `e` stayed: {levels:?}"
    );
    assert_reads(&db, "after compacting [z, ..)", &expected);

    db.flush().unwrap();
    assert_reads(&db, "after a flush", &expected);
    db.close().unwrap();
    drop(db);
    let db = Db::open(dir.path(), options()).unwrap();
    assert_reads(&db, "after a reopen", &expected);
    db.compact_range(None, None).wait().unwrap();
    assert_reads(&db, "after compacting everything", &expected);
}

/// The open hands the tombstones over and leaves the manifest alone; the
/// edit that follows records the move. In between, a second open finds the
/// overlap again.
#[test]
fn the_open_does_not_record_the_move_its_first_edit_does() {
    let (dir, legacy) = super::legacy_overlap_tests::fixture();
    let sst_dir = dir.path().join("sst");
    let on_disk = || std::fs::read(dir.path().join("MANIFEST")).unwrap();

    let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
    let demotion = vs.take_demotion().expect("the open moved tables");
    assert_eq!((demotion.tables, demotion.deepest_level), (3, 2));
    let tombstones: Vec<(Vec<u8>, Vec<u8>, u64)> = demotion
        .range_deletes
        .iter()
        .map(|t| (t.start.clone(), t.end.clone(), t.seq))
        .collect();
    assert_eq!(
        tombstones,
        [(
            prefix_key(DEFAULT_CF_ID, b"b"),
            prefix_key(DEFAULT_CF_ID, b"t"),
            20
        )]
    );
    assert!(vs.take_demotion().is_none(), "handed over once");
    assert_eq!(on_disk(), legacy, "the open alone wrote the move");
    drop(vs);

    let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
    assert_eq!(ids(&vs.current(), 0), [8, 3, 10, 90]);
    assert!(
        vs.take_demotion().is_some(),
        "an unrecorded move is made again"
    );
    vs.apply(&[VersionEdit::SetNextFileId(102)]).unwrap();
    let recorded = on_disk();
    assert!(recorded.starts_with(&legacy) && recorded.len() > legacy.len());
    // One batch: the move and the edit it rode with.
    let batch = tail::batch_at(&recorded, legacy.len()).expect("a whole batch");
    assert_eq!(batch.end, recorded.len());
    vs.apply(&[VersionEdit::SetNextFileId(103)]).unwrap();
    drop(vs);

    let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
    assert!(
        vs.take_demotion().is_none(),
        "a recorded move is not made again"
    );
    assert_eq!(ids(&vs.current(), 0), [8, 3, 10, 90]);
    assert_eq!(vs.current().next_file_id, 103);
}

/// `Db::open` records the move before it returns: the log holding the
/// tombstones first, then the manifest. The directory is copied while the
/// database is open, before anything is flushed, and opened from the copy:
/// what the open alone made durable.
#[test]
fn a_database_open_records_the_move_and_logs_the_tombstones_before_it_returns() {
    let (dir, legacy) = super::legacy_overlap_tests::fixture();
    let db = Db::open(dir.path(), options()).unwrap();
    let copy = copy_of(dir.path());
    drop(db);

    let manifest_now = std::fs::read(copy.path().join("MANIFEST")).unwrap();
    assert!(manifest_now.starts_with(&legacy) && manifest_now.len() > legacy.len());
    let mut vs = VersionSet::open(copy.path(), &copy.path().join("sst")).unwrap();
    assert!(vs.take_demotion().is_none(), "the open recorded the move");
    drop(vs);
    let db = Db::open(copy.path(), options()).unwrap();
    assert!(
        db.engine().view.load().active.has_range_tombstones(),
        "the log the first open wrote holds the moved table's tombstone"
    );
    assert_eq!(get(&db, b"r"), None);
}

/// A moved table that was ingested at a sequence of its own (D48) keeps
/// it: in the view, in the tombstone handed over, and in the record of the
/// move.
#[test]
fn a_moved_table_keeps_the_sequence_it_was_ingested_at() {
    let (dir, sst_dir) = empty_db();
    let old = table_of(
        &sst_dir,
        8,
        &[
            (b"k", 10, VALUE_TYPE_VALUE, b"old"),
            (b"x", 10, VALUE_TYPE_VALUE, b"x"),
        ],
        &[],
    );
    // Stored at sequence 1, read at 60.
    let mut ingest = table_of(
        &sst_dir,
        3,
        &[(b"z", 1, VALUE_TYPE_VALUE, b"ingested")],
        &[(b"b", b"t", 1)],
    );
    ingest.global_seq = Some(60);
    manifest(
        dir.path(),
        &[
            add(2, old),
            add(2, ingest),
            ManifestRecord::SetNextFileId(101),
            ManifestRecord::SetLastSeq(60),
            ManifestRecord::SetMinWalId(5),
        ],
    );

    let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
    let demotion = vs.take_demotion().unwrap();
    assert_eq!(demotion.range_deletes.len(), 1);
    assert_eq!(demotion.range_deletes[0].seq, 60);
    drop(vs);

    let reads_at_60 = |db: &Db, open: &str| {
        assert_eq!(get_at(db, b"z", 59), None, "{open}");
        assert_eq!(
            get_at(db, b"z", 60).as_deref(),
            Some(&b"ingested"[..]),
            "{open}"
        );
        assert_eq!(get_at(db, b"k", 59).as_deref(), Some(&b"old"[..]), "{open}");
        assert_eq!(get_at(db, b"k", 60), None, "{open}");
    };
    let db = Db::open(dir.path(), options()).unwrap();
    reads_at_60(&db, "the open that moves");
    // Copied before anything rewrites the table: the manifest's record of
    // the move is what names its sequence from here on.
    let copy = copy_of(dir.path());
    drop(db);

    let vs = VersionSet::open(copy.path(), &copy.path().join("sst")).unwrap();
    let recorded = vs.current().levels[0]
        .iter()
        .find(|file| file.meta.file_id == 3)
        .map(|file| file.meta.global_seq);
    assert_eq!(recorded, Some(Some(60)));
    drop(vs);
    let db = Db::open(copy.path(), options()).unwrap();
    reads_at_60(&db, "the open after it");
}

/// A manifest already sealed: the batch that records the move is sealed
/// like any other, and replays.
#[test]
fn a_sealed_manifest_records_the_move_sealed() {
    let (dir, sst_dir) = empty_db();
    let old = table_of(
        &sst_dir,
        8,
        &[
            (b"k", 10, VALUE_TYPE_VALUE, b"old"),
            (b"x", 10, VALUE_TYPE_VALUE, b"x"),
        ],
        &[],
    );
    let ingest = table_of(
        &sst_dir,
        3,
        &[(b"z", 20, VALUE_TYPE_VALUE, b"ingested")],
        &[(b"b", b"t", 20)],
    );
    let ring = keyring(&[7]);
    let image =
        encode_manifest_image(101, 20, 5, false, [(1, old), (1, ingest)], Some(&ring)).unwrap();
    std::fs::write(dir.path().join("MANIFEST"), &image).unwrap();
    let open = |ring: Option<Arc<Keyring>>| {
        VersionSet::open_with_policy(
            &crate::env::std_env(),
            dir.path(),
            &sst_dir,
            MetadataPolicy::Pinned,
            ring,
        )
    };

    let mut vs = open(Some(ring.clone())).expect("a sealed manifest with an overlap opens");
    assert_eq!(ids(&vs.current(), 0), [8, 3]);
    assert_eq!(record_move(&mut vs), 2);
    assert!(!vs.has_stale_seals());
    drop(vs);

    let recorded = std::fs::read(dir.path().join("MANIFEST")).unwrap();
    assert!(recorded.starts_with(&image) && recorded.len() > image.len());
    let plain_key = prefix_key(DEFAULT_CF_ID, b"z");
    assert!(
        !recorded[image.len()..]
            .windows(plain_key.len())
            .any(|window| window == plain_key),
        "the record of the move names a key in plaintext"
    );
    let mut vs = open(Some(ring)).unwrap();
    assert_eq!(ids(&vs.current(), 0), [8, 3]);
    assert!(vs.current().levels[1].is_empty());
    assert!(vs.take_demotion().is_none());
    assert!(matches!(
        crate::Error::from(open(None).map(|_| ()).unwrap_err()),
        crate::Error::KeyProviderRequired
    ));
}

/// A batch that cannot be sealed writes nothing, so the move it would have
/// carried is still owed: the next batch records it. Losing it there would
/// leave a manifest whose later edits name level 0 tables it still lists
/// below.
#[test]
fn a_batch_that_fails_before_it_is_written_leaves_the_move_to_the_next_one() {
    let (dir, sst_dir) = empty_db();
    let old = table_of(
        &sst_dir,
        8,
        &[
            (b"k", 10, VALUE_TYPE_VALUE, b"old"),
            (b"x", 10, VALUE_TYPE_VALUE, b"x"),
        ],
        &[],
    );
    let ingest = table_of(
        &sst_dir,
        3,
        &[(b"z", 20, VALUE_TYPE_VALUE, b"ingested")],
        &[(b"b", b"t", 20)],
    );
    let keys = TestKeys::new(&[7]);
    let ring = Arc::new(Keyring::new(keys.clone()));
    let image =
        encode_manifest_image(101, 20, 5, false, [(1, old), (1, ingest)], Some(&ring)).unwrap();
    std::fs::write(dir.path().join("MANIFEST"), &image).unwrap();
    let open = || {
        VersionSet::open_with_policy(
            &crate::env::std_env(),
            dir.path(),
            &sst_dir,
            MetadataPolicy::Pinned,
            Some(ring.clone()),
        )
        .unwrap()
    };

    let mut vs = open();
    // A current key the provider does not hold: the batch cannot be sealed.
    keys.set_current(9);
    assert!(vs.apply(&[VersionEdit::SetNextFileId(102)]).is_err());
    assert_eq!(
        std::fs::read(dir.path().join("MANIFEST")).unwrap(),
        image,
        "a batch that failed to encode reached the file"
    );
    keys.set_current(7);
    vs.apply(&[VersionEdit::SetNextFileId(103)]).unwrap();
    drop(vs);

    let mut vs = open();
    assert!(
        vs.take_demotion().is_none(),
        "the move was lost with the failed batch"
    );
    assert_eq!(ids(&vs.current(), 0), [8, 3]);
    assert!(vs.current().levels[1].is_empty());
    assert_eq!(vs.current().next_file_id, 103);
}

/// A manifest first opened with a keyring is rewritten sealed by that
/// open. When the open also moved tables, the rewrite waits for the edit
/// that records the move: made at once, it would record the move itself,
/// before the engine has logged the moved tables' range tombstones.
#[test]
fn a_manifest_first_opened_with_a_keyring_is_sealed_by_the_edit_that_records_the_move() {
    let (dir, legacy) = super::legacy_overlap_tests::fixture();
    let sst_dir = dir.path().join("sst");
    let on_disk = || std::fs::read(dir.path().join("MANIFEST")).unwrap();
    let ring = keyring(&[7]);
    let open = |ring: Option<Arc<Keyring>>| {
        VersionSet::open_with_policy(
            &crate::env::std_env(),
            dir.path(),
            &sst_dir,
            MetadataPolicy::Pinned,
            ring,
        )
    };

    let mut vs = open(Some(ring.clone())).unwrap();
    assert_eq!(on_disk(), legacy, "the open alone rewrote the manifest");
    assert!(vs.has_stale_seals());
    assert_eq!(record_move(&mut vs), 3);
    assert!(
        !vs.has_stale_seals(),
        "the edit that recorded the move left the manifest unsealed"
    );
    drop(vs);

    let sealed = on_disk();
    assert_eq!(sealed[7], MANIFEST_FORMAT_SEALED);
    let mut vs = open(Some(ring)).unwrap();
    assert_eq!(ids(&vs.current(), 0), [8, 3, 10, 90]);
    assert!(vs.take_demotion().is_none());
    assert!(!vs.has_stale_seals());
    assert!(matches!(
        crate::Error::from(open(None).map(|_| ()).unwrap_err()),
        crate::Error::KeyProviderRequired
    ));
}

/// Range tombstones the ingested table of [`many_range_deletes`] holds.
const MANY: usize = 4_096;

/// Level 1 overlaps, and its ingested table carries [`MANY`] range
/// tombstones at its one sequence, 20, no two touching:
/// `[d00000a, d00000m)`, `[d00001a, d00001m)`, and so on. The older table
/// beside it holds `d00007c` and `d04000c`, which two of them cover, and
/// `d00007p`, which none does.
fn many_range_deletes() -> TempDir {
    let (dir, sst_dir) = empty_db();
    let old = table_of(
        &sst_dir,
        8,
        &[
            (b"d00007c", 10, VALUE_TYPE_VALUE, b"old"),
            (b"d00007p", 10, VALUE_TYPE_VALUE, b"kept"),
            (b"d04000c", 10, VALUE_TYPE_VALUE, b"old"),
            (b"x", 10, VALUE_TYPE_VALUE, b"x"),
        ],
        &[],
    );
    let ranges: Vec<(Vec<u8>, Vec<u8>)> = (0..MANY)
        .map(|i| {
            (
                format!("d{i:05}a").into_bytes(),
                format!("d{i:05}m").into_bytes(),
            )
        })
        .collect();
    let tombstones: Vec<Tombstone<'_>> = ranges
        .iter()
        .map(|(start, end)| (start.as_slice(), end.as_slice(), 20))
        .collect();
    let ingest = table_of(
        &sst_dir,
        3,
        &[(b"z", 20, VALUE_TYPE_VALUE, b"ingested")],
        &tombstones,
    );
    manifest(
        dir.path(),
        &[
            add(1, old),
            add(1, ingest),
            ManifestRecord::SetNextFileId(101),
            ManifestRecord::SetLastSeq(20),
            ManifestRecord::SetMinWalId(5),
        ],
    );
    dir
}

/// What every open of [`many_range_deletes`] must read.
fn assert_many_reads(db: &Db, when: &str) {
    assert_eq!(get(db, b"d00007c"), None, "{when}");
    assert_eq!(get(db, b"d04000c"), None, "{when}");
    assert_eq!(get(db, b"d00007p").as_deref(), Some(&b"kept"[..]), "{when}");
    assert_eq!(get(db, b"x").as_deref(), Some(&b"x"[..]), "{when}");
    assert_eq!(
        get_at(db, b"d00007c", 19).as_deref(),
        Some(&b"old"[..]),
        "{when}"
    );
}

/// The copy is one batch, whatever its size: the open that moves the
/// tables builds one index over the range tombstones it copies and walks
/// the memtable for no duplicate, and so does the open after it, which
/// finds them in the log. Counted, not timed: one tombstone at a time,
/// [`MANY`] of them at one sequence build an index every sixteen and walk
/// the whole log for each, about eight million slot reads.
#[test]
fn an_open_copies_thousands_of_range_deletes_as_one_batch_and_the_next_replays_them_as_one() {
    let dir = many_range_deletes();
    range_tombstone_work();
    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(
        range_tombstone_work(),
        (1, 0),
        "(indexes built, slots walked for a duplicate) by the open that copies"
    );
    assert_eq!(
        db.engine()
            .view
            .load()
            .active
            .clone_range_tombstones()
            .len(),
        MANY
    );
    assert_many_reads(&db, "the open that copies");
    // Copied while it is open, before anything is flushed: the log the
    // open wrote holds every one of them.
    let copy = copy_of(dir.path());
    drop(db);

    range_tombstone_work();
    let db = Db::open(copy.path(), options()).unwrap();
    assert_eq!(
        range_tombstone_work(),
        (1, 0),
        "(indexes built, slots walked for a duplicate) by the open that replays"
    );
    assert_eq!(
        db.get_int_property("regolith.manifest.tables_demoted"),
        Some(0)
    );
    assert_eq!(
        db.engine()
            .view
            .load()
            .active
            .clone_range_tombstones()
            .len(),
        MANY
    );
    assert_many_reads(&db, "the open that replays");
}

/// An open cut off before its move was recorded leaves a log that already
/// holds the copy. The next open replays it and copies again: each
/// tombstone is in the memtable once, by one look at what it holds.
#[test]
fn a_copy_the_log_already_brought_is_not_made_twice() {
    let dir = many_range_deletes();
    let sst_dir = dir.path().join("sst");
    let tombstones = |vs: &mut VersionSet| vs.take_demotion().unwrap().range_deletes;
    let mut vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
    let copied = tombstones(&mut vs);
    assert_eq!(copied.len(), MANY);
    drop(vs);

    let memtable =
        crate::engine::memtable::MemTable::new(&crate::engine::memtable::MemTableConfig::new(
            crate::options::ArenaProfile::SERVER,
            1 << 20,
            2,
        ))
        .unwrap();
    // What the replay of that log brings, then the copy of the same open.
    memtable.delete_ranges(copied.clone());
    let once = memtable.approximate_size();
    range_tombstone_work();
    memtable.delete_ranges(copied);
    assert_eq!(range_tombstone_work(), (0, 0));
    assert_eq!(memtable.approximate_size(), once);
    assert_eq!(memtable.clone_range_tombstones().len(), MANY);
}

/// What bounds the copy: its bytes count toward the memtable's size, so
/// a memtable the copy filled past the write buffer is sealed by the first
/// write, and flushed like any other. Every read stays what it was.
#[test]
fn the_copy_counts_toward_the_memtable_and_the_first_write_past_the_buffer_seals_it() {
    let dir = many_range_deletes();
    let small = options().write_buffer_size(64 * 1024);
    let db = Db::open(dir.path(), small).unwrap();
    let copied = db.engine().active_memtable_size();
    assert!(
        copied > 64 * 1024,
        "the copy of {MANY} range tombstones counts {copied} bytes"
    );
    assert!(db.engine().view.load().active.has_range_tombstones());
    assert_many_reads(&db, "before a write");

    crate::through_stalls(|| db.put(b"new", b"write")).unwrap();
    assert!(
        !db.engine().view.load().active.has_range_tombstones(),
        "the first write did not seal the memtable the copy had filled"
    );
    assert!(db.engine().active_memtable_size() < 64 * 1024);
    assert_many_reads(&db, "after the write that sealed it");
    assert_eq!(get(&db, b"new").as_deref(), Some(&b"write"[..]));

    db.flush().unwrap();
    assert!(db.engine().view.load().frozen.is_empty());
    assert_many_reads(&db, "after the flush");
    db.close().unwrap();
    drop(db);
    let db = Db::open(dir.path(), options()).unwrap();
    assert_many_reads(&db, "after a reopen");
    assert!(
        !db.engine().view.load().active.has_range_tombstones(),
        "flushed, so no later open replays the copy"
    );
}

/// Tables level 1 holds in [`a_whole_tree_past_the_stop_trigger`], one key
/// each: with the two of level 2, more than the 36 level 0 tables at which
/// writes stop.
const WIDE: u64 = 41;

/// The overlap in the level below a wide level 1: the open moves both
/// levels, [`WIDE`] + 2 tables, into level 0, past the threshold at which
/// writes stop. Level 2 holds `k` and the ingested table, whose range
/// tombstone over `[b, t)` at sequence 45 is newer than level 1's `c` and
/// older than level 1's `k`.
fn a_whole_tree_past_the_stop_trigger() -> TempDir {
    let (dir, sst_dir) = empty_db();
    let mut records = vec![
        add(
            2,
            table_of(
                &sst_dir,
                8,
                &[
                    (b"k", 2, VALUE_TYPE_VALUE, b"k-old"),
                    (b"x", 2, VALUE_TYPE_VALUE, b"x"),
                ],
                &[],
            ),
        ),
        add(
            2,
            table_of(
                &sst_dir,
                3,
                &[(b"z", 45, VALUE_TYPE_VALUE, b"ingested")],
                &[(b"b", b"t", 45)],
            ),
        ),
    ];
    for i in 0..WIDE - 2 {
        let key = format!("a{i:03}").into_bytes();
        records.push(add(
            1,
            table_of(
                &sst_dir,
                10 + i,
                &[(&key, 3 + i, VALUE_TYPE_VALUE, b"wide")],
                &[],
            ),
        ));
    }
    records.push(add(
        1,
        table_of(&sst_dir, 90, &[(b"c", 10, VALUE_TYPE_VALUE, b"c-old")], &[]),
    ));
    records.push(add(
        1,
        table_of(&sst_dir, 91, &[(b"k", 60, VALUE_TYPE_VALUE, b"k-new")], &[]),
    ));
    records.extend([
        ManifestRecord::SetNextFileId(101),
        ManifestRecord::SetLastSeq(60),
        ManifestRecord::SetMinWalId(5),
    ]);
    manifest(dir.path(), &records);
    dir
}

/// What every read of [`a_whole_tree_past_the_stop_trigger`] must return.
fn assert_wide_reads(db: &Db, when: &str) {
    assert_eq!(
        get(db, b"c"),
        None,
        "{when}: the tombstone at 45 over c at 10"
    );
    assert_eq!(get(db, b"k").as_deref(), Some(&b"k-new"[..]), "{when}");
    assert_eq!(get(db, b"a000").as_deref(), Some(&b"wide"[..]), "{when}");
    assert_eq!(get(db, b"a038").as_deref(), Some(&b"wide"[..]), "{when}");
    assert_eq!(get(db, b"z").as_deref(), Some(&b"ingested"[..]), "{when}");
    let scanned = db.scan(None, None).unwrap();
    assert_eq!(scanned.len() as u64, WIDE - 2 + 3, "{when}: scan");
    assert!(scanned.iter().all(|(key, _)| key != b"c"), "{when}: scan");
}

/// A database the open moved whole into level 0, past the threshold at
/// which writes stop, takes a write with no worker and nothing said about
/// inline compaction: the write relieves the stop on its own thread, by
/// the compaction that takes level 0 down, and returns. Every read is the
/// same before and after.
#[test]
fn a_tree_moved_whole_into_level_0_takes_a_write_that_relieves_the_stop_itself() {
    let dir = a_whole_tree_past_the_stop_trigger();
    let db = Db::open(dir.path(), options()).unwrap();
    assert_eq!(
        db.get_int_property("regolith.manifest.tables_demoted"),
        Some(WIDE + 2)
    );
    assert_eq!(files_per_level(&db)[0], WIDE + 2);
    assert_wide_reads(&db, "before a write");

    // Not through `through_stalls`: with no worker the put itself clears
    // the stop, and returns the write, not a wait.
    db.put(b"w", b"written")
        .expect("the write relieves the stop");
    assert!(
        files_per_level(&db)[0] < 36,
        "the write left level 0 past the stop: {:?}",
        files_per_level(&db)
    );
    assert_eq!(get(&db, b"w").as_deref(), Some(&b"written"[..]));
    db.delete(b"w").unwrap();
    assert_wide_reads(&db, "after the write that relieved the stop");
    db.close().unwrap();
    drop(db);
    let db = Db::open(dir.path(), options()).unwrap();
    assert_wide_reads(&db, "after a reopen");
}

/// The same database with a worker: a plain put may be handed the wait
/// while the worker takes level 0 down, and a transaction's commit waits
/// the stop out itself. Either way the write lands and every read is right.
#[test]
fn a_tree_moved_whole_into_level_0_takes_writes_while_a_worker_relieves_the_stop() {
    let dir = a_whole_tree_past_the_stop_trigger();
    let txn_db =
        crate::OptimisticTransactionDb::open(dir.path(), options().max_background_compactions(1))
            .unwrap();
    let db = txn_db.db();
    assert_eq!(
        db.get_int_property("regolith.manifest.tables_demoted"),
        Some(WIDE + 2)
    );
    crate::through_stalls(|| db.put(b"w", b"written")).unwrap();
    let txn = txn_db.begin(&crate::TxnOptions::new());
    txn.put(b"t", b"committed").unwrap();
    txn.commit().expect("a commit waits a stop out");
    assert_eq!(get(db, b"w").as_deref(), Some(&b"written"[..]));
    assert_eq!(get(db, b"t").as_deref(), Some(&b"committed"[..]));
    crate::through_stalls(|| db.delete(b"w")).unwrap();
    crate::through_stalls(|| db.delete(b"t")).unwrap();
    assert_wide_reads(db, "with a worker");
}
