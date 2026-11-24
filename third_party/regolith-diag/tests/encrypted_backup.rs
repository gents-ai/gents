//! Backups of a database encrypted at rest (D57): the metadata is sealed
//! through the database's key provider, and a restore takes a provider and
//! refuses without the right one before it writes anything.
//!
//! The format-level rules (the listing bound by the tag, a backup bound to
//! its id, every changed byte refused) are pinned next to the code in
//! `src/backup/format_tests.rs`; power cuts are in
//! `tests/encrypted_backup_power_loss.rs`. The model is
//! `proofs/tla/BackupSeal.tla`.

// Native-only: these use the filesystem.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::keys::Keys;
use regolith::{BackupEngine, BackupId, Db, Error, KeyId, KeyProvider, Options};
use tempfile::TempDir;

/// Bytes no file of an encrypted backup or restore may hold in plaintext.
const MARKER: &[u8] = b"ENCRYPTED-BACKUP-MARKER";

fn key(i: usize) -> Vec<u8> {
    format!("user-key-{i:06}").into_bytes()
}

fn value(i: usize) -> Vec<u8> {
    let mut v = MARKER.to_vec();
    v.extend_from_slice(format!("/{i:06}").as_bytes());
    v
}

/// Small buffers, so a few hundred writes flush into several tables, and no
/// compaction, so which key each table is sealed under is the test's to
/// decide: no worker, and an L0 trigger out of reach, because with no worker
/// the write after a flush runs one compaction step once L0 reaches its
/// trigger (E16).
fn options(keys: &Arc<Keys>) -> Options {
    Options::default()
        .write_buffer_size(16 * 1024)
        .block_size(1024)
        .max_background_compactions(0)
        .l0_compaction_trigger(1000)
        .key_provider(keys.clone())
}

fn provider(keys: &Arc<Keys>) -> Option<Arc<dyn KeyProvider>> {
    Some(keys.clone())
}

fn fill(db: &Db, from: usize, to: usize) {
    for i in from..to {
        regolith::through_stalls(|| db.put(&key(i), &value(i))).unwrap();
    }
}

fn check(db: &Db, from: usize, to: usize) {
    for i in from..to {
        assert_eq!(db.get(&key(i)).unwrap(), Some(value(i)), "key {i}");
    }
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

fn files_holding(dir: &Path, needle: &[u8]) -> Vec<PathBuf> {
    files_under(dir)
        .into_iter()
        .filter(|path| {
            std::fs::read(path)
                .unwrap()
                .windows(needle.len())
                .any(|w| w == needle)
        })
        .collect()
}

fn meta_path(backups: &Path, id: BackupId) -> PathBuf {
    backups.join("meta").join(format!("{:06}.backup", id.0))
}

fn shared_count(backups: &Path) -> usize {
    std::fs::read_dir(backups.join("shared")).unwrap().count()
}

/// Restore `id` into `target` under `keys`, and expect `refused`, with the
/// target never created.
fn assert_refused(
    engine: &BackupEngine,
    id: BackupId,
    target: &Path,
    keys: Option<Arc<dyn KeyProvider>>,
    refused: impl Fn(&Error) -> bool,
) {
    match engine.restore(id, target, keys) {
        Ok(()) => panic!("backup {id} restored"),
        Err(e) => assert!(refused(&e), "backup {id}: refused with {e:?}"),
    }
    assert!(
        !target.exists(),
        "a refused restore of backup {id} wrote {}",
        target.display()
    );
}

/// A populated encrypted database under `keys`, flushed into tables, with
/// a few writes left in the memtable for the backup's own flush.
fn populated(dir: &Path, keys: &Arc<Keys>) -> Db {
    let db = Db::open(dir, options(keys)).unwrap();
    fill(&db, 0, 400);
    db.flush().unwrap();
    fill(&db, 400, 420);
    db
}

#[test]
fn a_restore_needs_the_right_key_and_refuses_before_writing() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    let db = populated(dir.path(), &keys);
    let backups = TempDir::new().unwrap();
    let mut engine = BackupEngine::open(backups.path()).unwrap();
    let id = engine.create_backup(&db).unwrap();
    drop(db);
    assert!(
        files_holding(backups.path(), b"user-key-").is_empty(),
        "a key range is in the backup in plaintext"
    );
    assert!(files_holding(backups.path(), MARKER).is_empty());

    let root = TempDir::new().unwrap();
    let target = root.path().join("restored");
    assert_refused(&engine, id, &target, None, |e| {
        matches!(e, Error::KeyProviderRequired)
    });
    assert_refused(&engine, id, &target, provider(&Keys::new(&[2])), |e| {
        matches!(e, Error::UnknownKey { id: KeyId(1) })
    });
    assert_refused(&engine, id, &target, provider(&Keys::wrong(&[1])), |e| {
        matches!(e, Error::Corruption(_))
            && e.to_string().contains("does not verify under key id 1")
    });
    let unprovided = Keys::new(&[1]);
    unprovided.set_current(9);
    assert_refused(&engine, id, &target, provider(&unprovided), |e| {
        matches!(e, Error::UnknownKey { id: KeyId(9) })
    });

    engine.restore(id, &target, provider(&keys)).unwrap();
    // Sealed as written, before any open: the restored MANIFEST names no
    // key range in plaintext.
    assert!(
        files_holding(&target, b"user-key-").is_empty(),
        "the restored MANIFEST is plaintext"
    );
    assert!(matches!(
        Db::open(&target, Options::default()),
        Err(Error::KeyProviderRequired)
    ));
    let restored = Db::open(&target, options(&keys)).unwrap();
    check(&restored, 0, 420);
    drop(restored);

    // A finished target is a database: a second restore over it refuses
    // before it replaces a table its MANIFEST names.
    let before = files_under(&target)
        .into_iter()
        .map(|p| (std::fs::read(&p).unwrap(), p))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(matches!(
        engine.restore(id, &target, provider(&keys)),
        Err(Error::InvalidArgument(_))
    ));
    let after = files_under(&target)
        .into_iter()
        .map(|p| (std::fs::read(&p).unwrap(), p))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(before == after, "a refused restore wrote into a database");
}

#[test]
fn tampered_backup_metadata_refuses_and_writes_nothing() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    let db = populated(dir.path(), &keys);
    let backups = TempDir::new().unwrap();
    let mut engine = BackupEngine::open(backups.path()).unwrap();
    let first = engine.create_backup(&db).unwrap();
    fill(&db, 420, 800);
    let second = engine.create_backup(&db).unwrap();
    drop(db);
    let root = TempDir::new().unwrap();

    // A changed byte anywhere: the checksum refuses it.
    let path = meta_path(backups.path(), first);
    let whole = std::fs::read(&path).unwrap();
    for at in [
        0,
        12,
        20,
        whole.len() / 2,
        whole.len() - 20,
        whole.len() - 1,
    ] {
        let mut bad = whole.clone();
        bad[at] ^= 0x10;
        std::fs::write(&path, &bad).unwrap();
        assert_refused(
            &engine,
            first,
            &root.path().join(format!("at-{at}")),
            provider(&keys),
            |e| matches!(e, Error::Corruption(_)),
        );
    }
    std::fs::write(&path, &whole).unwrap();

    // A whole, valid file moved to another backup's name: the tag binds the
    // id, so backup 2 never restores as backup 1's tables.
    let second_path = meta_path(backups.path(), second);
    let second_bytes = std::fs::read(&second_path).unwrap();
    std::fs::write(&second_path, &whole).unwrap();
    assert_refused(
        &engine,
        second,
        &root.path().join("moved"),
        provider(&keys),
        |e| matches!(e, Error::Corruption(_)) && e.to_string().contains("backup 2"),
    );
    std::fs::write(&second_path, &second_bytes).unwrap();

    let target = root.path().join("restored");
    engine.restore(second, &target, provider(&keys)).unwrap();
    check(&Db::open(&target, options(&keys)).unwrap(), 0, 800);
}

#[test]
fn a_backup_taken_across_a_key_rotation_restores_under_the_keys_it_names() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1, 2]);
    let db = populated(dir.path(), &keys);
    let backups = TempDir::new().unwrap();
    let mut engine = BackupEngine::open(backups.path()).unwrap();
    // Tables and metadata under key 1.
    let before = engine.create_backup(&db).unwrap();
    keys.set_current(2);
    fill(&db, 420, 800);
    // Metadata under key 2, tables under both.
    let across = engine.create_backup(&db).unwrap();
    db.compact_range(None, None).wait().unwrap();
    // Everything under key 2.
    let after = engine.create_backup(&db).unwrap();
    drop(db);

    let root = TempDir::new().unwrap();
    let only_two = Keys::new(&[2]);
    assert_refused(
        &engine,
        before,
        &root.path().join("before-2"),
        provider(&only_two),
        |e| matches!(e, Error::UnknownKey { id: KeyId(1) }),
    );
    // Its metadata opens under key 2, but tables it lists name key 1.
    assert_refused(
        &engine,
        across,
        &root.path().join("across-2"),
        provider(&only_two),
        |e| matches!(e, Error::UnknownKey { id: KeyId(1) }),
    );

    let both = Keys::new(&[2, 1]);
    for (id, upto) in [(before, 420), (across, 800)] {
        let target = root.path().join(format!("both-{id}"));
        engine.restore(id, &target, provider(&both)).unwrap();
        check(&Db::open(&target, options(&both)).unwrap(), 0, upto);
    }
    let target = root.path().join("after-2");
    engine.restore(after, &target, provider(&only_two)).unwrap();
    let restored = Db::open(&target, options(&only_two)).unwrap();
    check(&restored, 0, 800);
    drop(restored);
    assert!(files_holding(backups.path(), b"user-key-").is_empty());
    assert!(files_holding(root.path(), b"user-key-").is_empty());
}

/// Right bytes for the metadata's key and the current key, wrong bytes for
/// the key older tables are sealed under: the metadata opens and every key
/// is there, but those tables' footers do not verify. The restore refuses
/// before it writes anything, so the target opens nothing broken and takes
/// a restore with the right keys afterwards.
#[test]
fn a_restore_refuses_wrong_bytes_for_an_older_tables_key_before_writing() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1, 2]);
    // Tables under key 1.
    let db = populated(dir.path(), &keys);
    keys.set_current(2);
    fill(&db, 420, 800);
    let backups = TempDir::new().unwrap();
    let mut engine = BackupEngine::open(backups.path()).unwrap();
    // Metadata under key 2, tables under both.
    let across = engine.create_backup(&db).unwrap();
    drop(db);

    let root = TempDir::new().unwrap();
    let target = root.path().join("restored");
    // Key 2 current and right, key 1 present with the wrong bytes.
    let older_wrong = Keys::wrong_for(&[2, 1], &[1]);
    assert_refused(&engine, across, &target, provider(&older_wrong), |e| {
        matches!(e, Error::Corruption(_)) && e.to_string().contains("key id 1")
    });

    let both = Keys::new(&[2, 1]);
    engine.restore(across, &target, provider(&both)).unwrap();
    check(&Db::open(&target, options(&both)).unwrap(), 0, 800);
}

#[test]
fn listing_deleting_and_purging_need_no_key_and_keep_what_sealed_backups_share() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    let db = populated(dir.path(), &keys);
    let backups = TempDir::new().unwrap();
    let mut engine = BackupEngine::open(backups.path()).unwrap();
    let first = engine.create_backup(&db).unwrap();
    fill(&db, 420, 600);
    let second = engine.create_backup(&db).unwrap();
    drop(db);

    // An engine that holds no key, as every engine does: every sealed
    // backup lists, and reads.
    let mut engine = BackupEngine::open(backups.path()).unwrap();
    let infos: Vec<_> = engine
        .list_backups()
        .map(|entry| entry.unwrap().unwrap())
        .collect();
    assert_eq!(
        infos.iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![first, second]
    );
    assert!(infos.iter().all(|i| i.file_count > 0 && i.bytes > 0));
    let shared_before = shared_count(backups.path());

    // The second backup reuses tables of the first: deleting the first must
    // keep every one the sealed second still lists.
    engine.delete_backup(first).unwrap();
    assert!(shared_count(backups.path()) <= shared_before);
    let root = TempDir::new().unwrap();
    let target = root.path().join("restored");
    engine.restore(second, &target, provider(&keys)).unwrap();
    check(&Db::open(&target, options(&keys)).unwrap(), 0, 600);

    engine.purge_old_backups(0).unwrap();
    assert_eq!(engine.list_backups().count(), 0);
    assert_eq!(shared_count(backups.path()), 0);
}

#[test]
fn a_backup_that_cannot_be_read_stops_a_delete_from_removing_shared_files() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    let db = populated(dir.path(), &keys);
    let backups = TempDir::new().unwrap();
    let mut engine = BackupEngine::open(backups.path()).unwrap();
    let first = engine.create_backup(&db).unwrap();
    let second = engine.create_backup(&db).unwrap();
    drop(db);
    let shared_before = shared_count(backups.path());
    assert!(shared_before > 0);

    let path = meta_path(backups.path(), second);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[30] ^= 0x01;
    std::fs::write(&path, &bytes).unwrap();
    let err = engine.delete_backup(first).unwrap_err();
    assert!(err.to_string().contains("backup 2 cannot be read"), "{err}");
    assert_eq!(
        shared_count(backups.path()),
        shared_before,
        "a shared file the unreadable backup lists was removed"
    );
    // The first is gone, and the second is listed with its reason, not
    // left out.
    let listed: Vec<_> = engine
        .list_backups()
        .map(|entry| entry.unwrap().map(|info| info.id).map_err(|bad| bad.id))
        .collect();
    assert_eq!(listed, [Err(second)]);
}

#[test]
fn a_backup_refuses_before_copying_when_the_current_key_is_not_provided() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::new(&[1]);
    let db = populated(dir.path(), &keys);
    keys.set_current(9);
    let backups = TempDir::new().unwrap();
    let mut engine = BackupEngine::open(backups.path()).unwrap();
    assert!(matches!(
        engine.create_backup(&db),
        Err(Error::UnknownKey { id: KeyId(9) })
    ));
    assert!(files_under(backups.path()).is_empty());
    keys.set_current(1);
    let id = engine.create_backup(&db).unwrap();
    drop(db);
    let target = TempDir::new().unwrap();
    engine.restore(id, target.path(), provider(&keys)).unwrap();
    check(&Db::open(target.path(), options(&keys)).unwrap(), 0, 420);
}

#[test]
fn a_backup_of_an_unencrypted_database_stays_plaintext_and_restores_either_way() {
    let dir = TempDir::new().unwrap();
    let plain = Options::default().write_buffer_size(16 * 1024);
    let db = Db::open(dir.path(), plain.clone()).unwrap();
    fill(&db, 0, 400);
    let backups = TempDir::new().unwrap();
    let mut engine = BackupEngine::open(backups.path()).unwrap();
    let id = engine.create_backup(&db).unwrap();
    drop(db);
    // As it always was: version 3, key ranges in plaintext.
    let meta = std::fs::read(meta_path(backups.path(), id)).unwrap();
    assert_eq!(&meta[8..12], &3u32.to_le_bytes());
    assert!(!files_holding(&backups.path().join("meta"), b"user-key-").is_empty());

    let root = TempDir::new().unwrap();
    let without = root.path().join("without");
    engine.restore(id, &without, None).unwrap();
    check(&Db::open(&without, plain.clone()).unwrap(), 0, 400);

    // Given a provider, the restore seals what it writes, as the first open
    // with a provider would.
    let keys = Keys::new(&[1]);
    let with = root.path().join("with");
    engine.restore(id, &with, provider(&keys)).unwrap();
    assert!(matches!(
        Db::open(&with, plain),
        Err(Error::KeyProviderRequired)
    ));
    check(&Db::open(&with, options(&keys)).unwrap(), 0, 400);
}
