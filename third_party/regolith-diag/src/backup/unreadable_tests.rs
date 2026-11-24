//! Backups whose metadata cannot be read: the listing reports each one with
//! its reason and never leaves one out, and deleting and purging account
//! for it. The rules are `ListingNamesEvery` and `ListedRestores` in
//! `proofs/tla/BackupSeal.tla`, and `listing_names_every_backup` and
//! `collect_keeps_listed` in `proofs/lean/Regolith/BackupSeal.lean`.

use std::collections::BTreeSet;
use std::fs;

use super::tests::{assert_has, populate, readable, shared_file_paths, tiny_flush_opts};
use super::*;
use crate::Options;
use crate::engine::seal::test_keys::TestKeys;
use tempfile::TempDir;

/// Rows each backup of [`backups`] writes under its own prefix.
const ROWS: usize = 100;

/// `n` backups of one database. Backup `i` is taken after writing prefix
/// `g{i}` and a key past every prefix, then compacting: every table spans
/// that key, so the compaction rewrites them all and each backup holds
/// tables no other backup holds.
fn backups(n: usize) -> (TempDir, BackupEngine, Vec<BackupId>) {
    let src = TempDir::new().unwrap();
    let bkp = TempDir::new().unwrap();
    let db = Db::open(src.path(), tiny_flush_opts()).unwrap();
    let mut engine = BackupEngine::open(bkp.path()).unwrap();
    let ids = (1..=n)
        .map(|i| {
            populate(&db, &format!("g{i}"), ROWS);
            crate::through_stalls(|| db.put(b"zz", &i.to_le_bytes())).unwrap();
            db.compact_range(None, None).wait().unwrap();
            engine.create_backup(&db).unwrap()
        })
        .collect();
    (bkp, engine, ids)
}

fn meta_file(bkp: &Path, id: BackupId) -> PathBuf {
    bkp.join("meta").join(backup_filename(id))
}

/// Each backup the engine lists, by id, and whether its metadata read.
fn listed(engine: &BackupEngine) -> Vec<(u64, bool)> {
    engine
        .list_backups()
        .map(|item| match item.unwrap() {
            Ok(info) => (info.id.0, true),
            Err(bad) => (bad.id.0, false),
        })
        .collect()
}

/// The entry the engine lists for `id`, which must be unreadable.
fn unreadable(engine: &BackupEngine, id: BackupId) -> UnreadableBackup {
    engine
        .list_backups()
        .map(Result::unwrap)
        .find_map(|entry| entry.err().filter(|bad| bad.id == id))
        .unwrap_or_else(|| panic!("backup {id} is not listed as unreadable"))
}

/// The shared files backups `ids` list, read while they are readable.
fn listed_files(engine: &BackupEngine, ids: &[BackupId]) -> BTreeSet<PathBuf> {
    ids.iter()
        .flat_map(|&id| engine.read_listing(id).unwrap().objects)
        .map(|(hash, _)| engine.shared_dir.join(shared_filename(hash)))
        .collect()
}

fn shared_now(bkp: &Path) -> BTreeSet<PathBuf> {
    shared_file_paths(bkp).into_iter().collect()
}

/// Flip a byte past the head of `path`, so its checksum fails. Returns
/// the bytes it held.
fn damage(path: &Path) -> Vec<u8> {
    let intact = fs::read(path).unwrap();
    let mut damaged = intact.clone();
    damaged[20] ^= 0x01;
    fs::write(path, damaged).unwrap();
    intact
}

/// Rewrite the bytes `path`'s checksum covers through `edit`, then put
/// back a valid checksum, as a newer build or a deliberate edit leaves it.
fn rewrite(path: &Path, edit: impl FnOnce(&mut Vec<u8>)) {
    let mut bytes = fs::read(path).unwrap();
    bytes.truncate(bytes.len() - 8);
    edit(&mut bytes);
    let sum = checksum::backup_manifest(&bytes);
    bytes.extend_from_slice(&sum.to_le_bytes());
    fs::write(path, bytes).unwrap();
}

/// Backup `id` restores, holding every prefix in `prefixes`.
fn restores(engine: &BackupEngine, id: BackupId, prefixes: &[&str]) {
    let target = TempDir::new().unwrap();
    engine.restore(id, target.path(), None).unwrap();
    let db = Db::open(target.path(), Options::default()).unwrap();
    for prefix in prefixes {
        assert_has(&db, prefix, ROWS);
    }
}

#[test]
fn a_damaged_backup_is_listed_with_its_reason_and_never_left_out() {
    let (bkp, engine, ids) = backups(3);
    damage(&meta_file(bkp.path(), ids[1]));

    let entries: Vec<_> = engine.list_backups().map(Result::unwrap).collect();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].as_ref().unwrap().id, ids[0]);
    assert_eq!(entries[2].as_ref().unwrap().id, ids[2]);
    let bad = entries[1].as_ref().unwrap_err();
    assert_eq!(bad.id, ids[1]);
    assert!(matches!(bad.reason, Error::Corruption(_)), "{bad:?}");
    assert_eq!(
        bad.to_string(),
        "backup 2 cannot be read: corruption: backup manifest checksum mismatch"
    );
}

#[test]
fn an_empty_file_and_a_directory_under_a_backups_name_are_listed_as_unreadable() {
    let (bkp, engine, ids) = backups(1);
    let meta = bkp.path().join("meta");
    fs::write(meta_file(bkp.path(), BackupId(2)), b"").unwrap();
    fs::create_dir(meta_file(bkp.path(), BackupId(3))).unwrap();
    // Not backups: the staging file a backup cut short leaves, and names
    // the engine never writes, even holding a whole backup's metadata.
    let whole = fs::read(meta_file(bkp.path(), ids[0])).unwrap();
    fs::write(meta.join("000004.tmp"), &whole).unwrap();
    fs::write(meta.join("5.backup"), &whole).unwrap();
    fs::write(meta.join("+00006.backup"), &whole).unwrap();

    assert_eq!(listed(&engine), [(1, true), (2, false), (3, false)]);
    let empty = unreadable(&engine, BackupId(2));
    assert!(matches!(empty.reason, Error::Corruption(_)), "{empty:?}");
    assert!(empty.to_string().ends_with("short backup"), "{empty}");
}

#[test]
fn a_backup_of_a_version_this_build_does_not_read_is_listed_with_its_reason() {
    let (bkp, engine, ids) = backups(2);
    let at = format::MAGIC.len();
    rewrite(&meta_file(bkp.path(), ids[1]), |bytes| {
        bytes[at..at + 4].copy_from_slice(&99u32.to_le_bytes());
    });

    assert_eq!(listed(&engine), [(1, true), (2, false)]);
    let bad = unreadable(&engine, ids[1]);
    assert!(matches!(bad.reason, Error::Corruption(_)), "{bad:?}");
    assert!(
        bad.to_string()
            .ends_with("unsupported backup manifest version 99"),
        "{bad}"
    );
    // A restore refuses it for the same reason, having written nothing.
    let root = TempDir::new().unwrap();
    let target = root.path().join("target");
    let err = engine.restore(ids[1], &target, None).unwrap_err();
    assert!(err.to_string().contains("version 99"), "{err}");
    assert!(!target.exists());
}

#[test]
fn a_sealed_backup_lists_without_a_key_and_one_whose_seal_is_cut_short_is_reported() {
    let src = TempDir::new().unwrap();
    let bkp = TempDir::new().unwrap();
    let db = Db::open(
        src.path(),
        tiny_flush_opts().key_provider(TestKeys::new(&[1])),
    )
    .unwrap();
    populate(&db, "s", ROWS);
    let mut engine = BackupEngine::open(bkp.path()).unwrap();
    let first = engine.create_backup(&db).unwrap();
    let second = engine.create_backup(&db).unwrap();
    drop(db);

    // The engine holds no key, and both sealed backups list.
    let infos = readable(&engine);
    assert_eq!(
        infos.iter().map(|i| i.id).collect::<Vec<_>>(),
        [first, second]
    );
    assert!(infos.iter().all(|i| i.file_count > 0 && i.bytes > 0));

    // Everything after the key id dropped and the checksum put back: the
    // clear listing still parses, but the seal it promises is not there.
    let listing_end = format::MAGIC.len() + 4 + 8 + 4 + infos[1].file_count * (16 + 8);
    rewrite(&meta_file(bkp.path(), second), |bytes| {
        bytes.truncate(listing_end + 4);
    });
    assert_eq!(listed(&engine), [(1, true), (2, false)]);
    let bad = unreadable(&engine, second);
    assert!(
        bad.to_string()
            .ends_with("backup manifest ends before its seal"),
        "{bad}"
    );
}

#[test]
fn listing_deleting_and_purging_fail_loud_when_the_meta_directory_cannot_be_read() {
    let (bkp, mut engine, ids) = backups(1);
    let shared = shared_now(bkp.path());
    assert!(!shared.is_empty());
    fs::remove_dir_all(bkp.path().join("meta")).unwrap();

    assert!(matches!(
        engine.list_backups().collect::<Result<Vec<_>>>(),
        Err(Error::Io(_))
    ));
    assert!(matches!(engine.purge_old_backups(0), Err(Error::Io(_))));
    assert!(matches!(engine.delete_backup(ids[0]), Err(Error::Io(_))));
    // No backup could be read, so none could be counted: nothing removed.
    assert_eq!(shared_now(bkp.path()), shared);
}

#[test]
fn a_delete_with_an_unreadable_backup_present_deletes_but_removes_no_shared_file() {
    let (bkp, mut engine, ids) = backups(2);
    let second = meta_file(bkp.path(), ids[1]);
    let kept = listed_files(&engine, &ids[1..]);
    let before = shared_now(bkp.path());
    assert!(
        kept.len() < before.len(),
        "backup 1 holds no table of its own"
    );
    let intact = damage(&second);

    let err = engine.delete_backup(ids[0]).unwrap_err();
    assert!(matches!(err, Error::Corruption(_)), "{err:?}");
    assert!(
        err.to_string()
            .contains("backup 2 cannot be read, so no shared file was removed"),
        "{err}"
    );
    assert_eq!(listed(&engine), [(2, false)]);
    assert_eq!(shared_now(bkp.path()), before);

    // Repaired, the next delete, even of a backup no longer there, removes
    // what only the deleted backup held.
    fs::write(&second, intact).unwrap();
    engine.delete_backup(ids[0]).unwrap();
    assert_eq!(shared_now(bkp.path()), kept);
    restores(&engine, ids[1], &["g1", "g2"]);
}

#[test]
fn an_unreadable_backup_is_deleted_with_the_shared_files_only_it_listed() {
    let (bkp, mut engine, ids) = backups(2);
    let kept = listed_files(&engine, &ids[..1]);
    assert!(kept.len() < shared_now(bkp.path()).len());
    damage(&meta_file(bkp.path(), ids[1]));

    engine.delete_backup(ids[1]).unwrap();
    assert_eq!(listed(&engine), [(1, true)]);
    assert_eq!(shared_now(bkp.path()), kept);
    restores(&engine, ids[0], &["g1"]);
}

#[test]
fn purge_counts_every_backup_and_deletes_the_oldest_even_when_it_cannot_be_read() {
    let (bkp, mut engine, ids) = backups(3);
    let kept = listed_files(&engine, &ids[2..]);
    damage(&meta_file(bkp.path(), ids[0]));

    engine.purge_old_backups(1).unwrap();
    assert_eq!(listed(&engine), [(3, true)]);
    assert_eq!(shared_now(bkp.path()), kept);
    restores(&engine, ids[2], &["g1", "g2", "g3"]);
}

#[test]
fn purge_keeping_an_unreadable_backup_deletes_the_rest_but_removes_no_shared_file() {
    let (bkp, mut engine, ids) = backups(3);
    let before = shared_now(bkp.path());
    damage(&meta_file(bkp.path(), ids[2]));

    let err = engine.purge_old_backups(1).unwrap_err();
    assert!(
        err.to_string()
            .contains("backup 3 cannot be read, so no shared file was removed"),
        "{err}"
    );
    assert_eq!(listed(&engine), [(3, false)]);
    assert_eq!(shared_now(bkp.path()), before);
    // With nothing left to delete it still says why nothing is collected.
    assert!(engine.purge_old_backups(5).is_err());
    assert_eq!(shared_now(bkp.path()), before);
}

#[test]
fn a_collection_removes_every_shared_table_no_backup_lists_and_nothing_else() {
    let (bkp, mut engine, ids) = backups(1);
    let shared = bkp.path().join("shared");
    let mut expected = listed_files(&engine, &ids);
    // A table a backup cut short copied and never listed.
    let orphan = shared.join(shared_filename(0xDEAD));
    fs::write(&orphan, b"orphan").unwrap();
    // Names the engine never gives a shared table, and a directory: kept.
    for stray in [
        shared.join(format!("{:032X}.sst", 0xBEEFu128)),
        shared.join(format!("{:032x}.tmp", 0xF00Du128)),
        shared.join("notes.txt"),
    ] {
        fs::write(&stray, b"stray").unwrap();
        expected.insert(stray);
    }
    let directory = shared.join(shared_filename(0xCAFE));
    fs::create_dir(&directory).unwrap();
    expected.insert(directory);

    engine.delete_backup(BackupId(9)).unwrap();
    assert!(!orphan.exists());
    assert_eq!(shared_now(bkp.path()), expected);
    restores(&engine, ids[0], &["g1"]);
}

#[test]
fn only_the_names_the_engine_writes_parse() {
    assert_eq!(parse_backup_id("000007.backup"), Some(BackupId(7)));
    assert_eq!(parse_backup_id("1234567.backup"), Some(BackupId(1_234_567)));
    for name in [
        "7.backup",
        "+00007.backup",
        "0000007.backup",
        "000007.tmp",
        "00000x.backup",
        ".backup",
    ] {
        assert_eq!(parse_backup_id(name), None, "{name}");
    }

    let hash = 0x0123_4567_89AB_CDEFu128;
    assert_eq!(parse_shared_filename(&shared_filename(hash)), Some(hash));
    assert_eq!(
        parse_shared_filename(&shared_filename(u128::MAX)),
        Some(u128::MAX)
    );
    for name in [
        format!("{hash:032X}.sst"),
        format!("+{hash:031x}.sst"),
        format!("{hash:x}.sst"),
        format!("{hash:032x}.tmp"),
    ] {
        assert_eq!(parse_shared_filename(&name), None, "{name}");
    }
}

#[test]
fn a_listing_whose_sizes_overflow_is_reported_not_summed() {
    let bkp = TempDir::new().unwrap();
    let engine = BackupEngine::open(bkp.path()).unwrap();
    let table = |hash: u128| BackupFileEntry {
        level: 1,
        file_id: hash as u64,
        file_size: u64::MAX,
        hash,
        smallest_key: b"a".to_vec(),
        largest_key: b"z".to_vec(),
        num_entries: 1,
        global_seq: None,
        seal_key: None,
    };
    let manifest = BackupManifest {
        created_at_unix: 1,
        files: vec![table(1), table(2)],
        next_file_id: 3,
        last_seq: 1,
        sealed_under: None,
        range_deletes_checked: true,
    };
    let id = BackupId(1);
    fs::write(
        meta_file(bkp.path(), id),
        format::encode(&manifest, id, None).unwrap(),
    )
    .unwrap();

    let bad = unreadable(&engine, id);
    assert!(matches!(bad.reason, Error::Corruption(_)), "{bad:?}");
    assert!(
        bad.to_string()
            .ends_with("backup 1: its tables' sizes overflow"),
        "{bad}"
    );
}

#[test]
fn the_largest_backup_id_refuses_the_next_backup_instead_of_wrapping() {
    let src = TempDir::new().unwrap();
    let bkp = TempDir::new().unwrap();
    let db = Db::open(src.path(), Options::default()).unwrap();
    db.put(b"k", b"v").unwrap();
    let mut engine = BackupEngine::open(bkp.path()).unwrap();
    fs::write(meta_file(bkp.path(), BackupId(u64::MAX)), b"").unwrap();

    let err = engine.create_backup(&db).unwrap_err();
    assert!(matches!(err, Error::Corruption(_)), "{err:?}");
    assert!(shared_now(bkp.path()).is_empty());
    assert_eq!(listed(&engine), [(u64::MAX, false)]);
}
