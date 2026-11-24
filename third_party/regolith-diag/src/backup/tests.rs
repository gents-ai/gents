use std::fs;

use super::*;
use crate::Options;
use crate::env::walk_fault::WalkFault;
use tempfile::TempDir;

pub(super) fn tiny_flush_opts() -> Options {
    Options::default().write_buffer_size(4 * 1024)
}

pub(super) fn populate(db: &Db, prefix: &str, n: usize) {
    let filler = vec![0u8; 256];
    for i in 0..n {
        let k = format!("{}_{:05}", prefix, i);
        let mut v = filler.clone();
        v.extend_from_slice(k.as_bytes());
        crate::through_stalls(|| db.put(k.as_bytes(), &v)).unwrap();
    }
}

pub(super) fn assert_has(db: &Db, prefix: &str, n: usize) {
    let filler = vec![0u8; 256];
    for i in 0..n {
        let k = format!("{}_{:05}", prefix, i);
        let mut expected = filler.clone();
        expected.extend_from_slice(k.as_bytes());
        assert_eq!(db.get(k.as_bytes()).unwrap(), Some(expected));
    }
}

fn shared_bytes(dir: &Path) -> u64 {
    let shared = dir.join("shared");
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(&shared) {
        for entry in entries.flatten() {
            if let Ok(md) = entry.metadata() {
                total += md.len();
            }
        }
    }
    total
}

fn shared_count(dir: &Path) -> usize {
    let shared = dir.join("shared");
    fs::read_dir(&shared)
        .map(|it| it.flatten().count())
        .unwrap_or(0)
}

pub(super) fn shared_file_paths(dir: &Path) -> Vec<PathBuf> {
    let shared = dir.join("shared");
    let mut paths: Vec<_> = fs::read_dir(&shared)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    paths
}

/// Every backup the engine lists, in order, each one asserted readable.
pub(super) fn readable(engine: &BackupEngine) -> Vec<BackupInfo> {
    engine
        .list_backups()
        .map(|entry| entry.unwrap().unwrap())
        .collect()
}

fn corrupt_file_same_size(path: &Path) {
    let mut bytes = fs::read(path).unwrap();
    assert!(!bytes.is_empty());
    bytes[0] ^= 0xFF;
    fs::write(path, bytes).unwrap();
}

#[test]
fn backup_and_restore_roundtrip() {
    let src_dir = TempDir::new().unwrap();
    let bkp_dir = TempDir::new().unwrap();
    let tgt_dir = TempDir::new().unwrap();

    let db = Db::open(src_dir.path(), tiny_flush_opts()).unwrap();
    populate(&db, "k", 300);

    let mut engine = BackupEngine::open(bkp_dir.path()).unwrap();
    let id = engine.create_backup(&db).unwrap();
    let infos = readable(&engine);
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].id, id);
    assert!(infos[0].file_count >= 1);

    engine.restore(id, tgt_dir.path(), None).unwrap();
    drop(db);

    let reopened = Db::open(tgt_dir.path(), Options::default()).unwrap();
    assert_has(&reopened, "k", 300);
}

#[test]
fn an_ingested_table_restores_at_its_recorded_sequence() {
    let src_dir = TempDir::new().unwrap();
    let bkp_dir = TempDir::new().unwrap();
    let tgt_dir = TempDir::new().unwrap();
    let db = Db::open(src_dir.path(), Options::default()).unwrap();
    db.put(b"k", b"old").unwrap();
    db.compact_range(None, None).wait().unwrap();
    let source = src_dir.path().join("source.sst");
    let mut writer = crate::SstFileWriter::create(&source, &Options::default()).unwrap();
    writer.put(b"k", b"new").unwrap();
    writer.finish().unwrap();
    // Lands above the old table, and stays uncompacted, so the restore
    // reads it through the sequence its manifest record carries.
    db.ingest_external_files(&[source], crate::IngestOptions::default())
        .wait()
        .unwrap();

    let mut engine = BackupEngine::open(bkp_dir.path()).unwrap();
    let id = engine.create_backup(&db).unwrap();
    engine.restore(id, tgt_dir.path(), None).unwrap();
    let restored = Db::open(tgt_dir.path(), Options::default()).unwrap();
    assert_eq!(restored.get(b"k").unwrap().as_deref(), Some(&b"new"[..]));
}

#[test]
fn incremental_backup_dedupes() {
    let src_dir = TempDir::new().unwrap();
    let bkp_dir = TempDir::new().unwrap();
    let db = Db::open(src_dir.path(), tiny_flush_opts()).unwrap();
    populate(&db, "x", 400);
    db.compact_range(None, None).wait().unwrap();

    let mut engine = BackupEngine::open(bkp_dir.path()).unwrap();
    let _id1 = engine.create_backup(&db).unwrap();
    let shared_bytes_1 = shared_bytes(bkp_dir.path());
    let shared_count_1 = shared_count(bkp_dir.path());

    // No writes between backups - second backup must add no
    // shared files.
    let _id2 = engine.create_backup(&db).unwrap();
    let shared_bytes_2 = shared_bytes(bkp_dir.path());
    let shared_count_2 = shared_count(bkp_dir.path());

    assert_eq!(shared_bytes_1, shared_bytes_2);
    assert_eq!(shared_count_1, shared_count_2);
    assert_eq!(readable(&engine).len(), 2);
}

#[test]
fn create_backup_replaces_corrupt_existing_shared_object() {
    let src_dir = TempDir::new().unwrap();
    let bkp_dir = TempDir::new().unwrap();
    let tgt_dir = TempDir::new().unwrap();
    let db = Db::open(src_dir.path(), tiny_flush_opts()).unwrap();
    populate(&db, "r", 400);
    db.compact_range(None, None).wait().unwrap();

    let mut engine = BackupEngine::open(bkp_dir.path()).unwrap();
    let _id1 = engine.create_backup(&db).unwrap();
    let shared_files = shared_file_paths(bkp_dir.path());
    assert!(!shared_files.is_empty());
    let original_sizes: Vec<u64> = shared_files
        .iter()
        .map(|path| fs::metadata(path).unwrap().len())
        .collect();
    for path in &shared_files {
        corrupt_file_same_size(path);
    }

    let id2 = engine.create_backup(&db).unwrap();

    for (path, original_size) in shared_files.iter().zip(original_sizes) {
        assert_eq!(fs::metadata(path).unwrap().len(), original_size);
    }
    engine.restore(id2, tgt_dir.path(), None).unwrap();
    drop(db);
    let restored = Db::open(tgt_dir.path(), Options::default()).unwrap();
    assert_has(&restored, "r", 400);
}

#[test]
fn restore_rejects_corrupt_shared_object() {
    let src_dir = TempDir::new().unwrap();
    let bkp_dir = TempDir::new().unwrap();
    let tgt_dir = TempDir::new().unwrap();
    let db = Db::open(src_dir.path(), tiny_flush_opts()).unwrap();
    populate(&db, "bad", 300);
    db.compact_range(None, None).wait().unwrap();

    let mut engine = BackupEngine::open(bkp_dir.path()).unwrap();
    let id = engine.create_backup(&db).unwrap();
    let shared_files = shared_file_paths(bkp_dir.path());
    assert!(!shared_files.is_empty());
    corrupt_file_same_size(&shared_files[0]);

    let kind = match engine.restore(id, tgt_dir.path(), None) {
        Err(Error::Corruption(e)) => e.kind(),
        Err(e) => panic!("expected corruption error, got {e:?}"),
        Ok(()) => panic!("expected restore to reject corrupt shared object"),
    };
    assert_eq!(kind, io::ErrorKind::InvalidData);
}

#[test]
fn delete_backup_gcs_unreferenced_files() {
    let src_dir = TempDir::new().unwrap();
    let bkp_dir = TempDir::new().unwrap();
    let db = Db::open(src_dir.path(), tiny_flush_opts()).unwrap();
    populate(&db, "a", 200);
    db.compact_range(None, None).wait().unwrap();

    let mut engine = BackupEngine::open(bkp_dir.path()).unwrap();
    let id1 = engine.create_backup(&db).unwrap();
    let shared_after_1 = shared_count(bkp_dir.path());
    assert!(shared_after_1 >= 1);

    // New data that doesn't overlap prior SSTs.
    populate(&db, "z", 200);
    db.compact_range(None, None).wait().unwrap();
    let id2 = engine.create_backup(&db).unwrap();
    let shared_after_2 = shared_count(bkp_dir.path());

    // Remove the first backup - any file it held that backup 2
    // does not also reference should be gone.
    engine.delete_backup(id1).unwrap();
    let shared_after_delete = shared_count(bkp_dir.path());
    assert!(shared_after_delete <= shared_after_2);
    assert_eq!(readable(&engine).len(), 1);

    // backup 2 still restores cleanly.
    let tgt_dir = TempDir::new().unwrap();
    engine.restore(id2, tgt_dir.path(), None).unwrap();
    drop(db);
    let reopened = Db::open(tgt_dir.path(), Options::default()).unwrap();
    assert_has(&reopened, "z", 200);
}

#[test]
fn delete_only_backup_removes_all_shared() {
    let src_dir = TempDir::new().unwrap();
    let bkp_dir = TempDir::new().unwrap();
    let db = Db::open(src_dir.path(), tiny_flush_opts()).unwrap();
    populate(&db, "q", 300);
    db.compact_range(None, None).wait().unwrap();

    let mut engine = BackupEngine::open(bkp_dir.path()).unwrap();
    let id = engine.create_backup(&db).unwrap();
    assert!(shared_count(bkp_dir.path()) > 0);

    engine.delete_backup(id).unwrap();
    assert_eq!(shared_count(bkp_dir.path()), 0);
    assert_eq!(engine.list_backups().count(), 0);
}

#[test]
fn purge_keeps_newest() {
    let src_dir = TempDir::new().unwrap();
    let bkp_dir = TempDir::new().unwrap();
    let db = Db::open(src_dir.path(), tiny_flush_opts()).unwrap();

    populate(&db, "g1", 100);
    db.compact_range(None, None).wait().unwrap();
    let mut engine = BackupEngine::open(bkp_dir.path()).unwrap();
    let _b1 = engine.create_backup(&db).unwrap();

    populate(&db, "g2", 100);
    db.compact_range(None, None).wait().unwrap();
    let _b2 = engine.create_backup(&db).unwrap();

    populate(&db, "g3", 100);
    db.compact_range(None, None).wait().unwrap();
    let b3 = engine.create_backup(&db).unwrap();

    engine.purge_old_backups(1).unwrap();
    let infos = readable(&engine);
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].id, b3);
}

#[test]
fn restore_independent_of_source() {
    let src_dir = TempDir::new().unwrap();
    let bkp_dir = TempDir::new().unwrap();
    let tgt_dir = TempDir::new().unwrap();

    let db = Db::open(src_dir.path(), tiny_flush_opts()).unwrap();
    populate(&db, "ind", 250);

    let mut engine = BackupEngine::open(bkp_dir.path()).unwrap();
    let id = engine.create_backup(&db).unwrap();

    db.close().unwrap();
    drop(db);
    fs::remove_dir_all(src_dir.path()).unwrap();

    engine.restore(id, tgt_dir.path(), None).unwrap();
    let reopened = Db::open(tgt_dir.path(), Options::default()).unwrap();
    assert_has(&reopened, "ind", 250);
}

/// A database and a backup repository on one [`WalkFault`] env: backup 1
/// holds the tables of the first writes, backup 2 those of more writes,
/// and backups 3 to `copies + 2` are copies of backup 1's plaintext
/// metadata (which binds no id), so `meta/` has many entries cheaply and
/// every table backup 1 lists is listed again by each copy.
fn walk_fault_repository(copies: u64) -> (Arc<WalkFault>, BackupEngine) {
    let env = Arc::new(WalkFault::default());
    let db = Db::open("/db", fault_options(&env)).unwrap();
    populate(&db, "a", 200);
    db.flush().unwrap();
    let mut engine = BackupEngine::open_with_env("/bk", env.clone()).unwrap();
    assert_eq!(engine.create_backup(&db).unwrap(), BackupId(1));
    populate(&db, "z", 200);
    db.flush().unwrap();
    assert_eq!(engine.create_backup(&db).unwrap(), BackupId(2));
    let first = env.read(Path::new("/bk/meta/000001.backup")).unwrap();
    for id in 3..copies + 3 {
        env.write(
            &Path::new("/bk/meta").join(backup_filename(BackupId(id))),
            &first,
        )
        .unwrap();
    }
    (env, engine)
}

fn fault_options(env: &Arc<WalkFault>) -> Options {
    tiny_flush_opts()
        .env(env.clone())
        .max_background_compactions(0)
}

/// Every shared file in the repository on `env`, sorted.
fn shared_on(env: &WalkFault) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = env
        .read_dir(Path::new("/bk/shared"))
        .unwrap()
        .map(|entry| entry.unwrap().path)
        .collect();
    paths.sort();
    paths
}

/// The collection must see every remaining listing before it removes a
/// file. A walk of `meta/` that breaks part way fails the delete, and no
/// shared file goes: a collection that took the break at the first entry
/// for the end of the walk would remove every table backup 1's copies
/// still list.
#[test]
fn a_collection_whose_walk_of_meta_breaks_removes_nothing() {
    let (env, mut engine) = walk_fault_repository(20);
    let before = shared_on(&env);
    assert!(!before.is_empty());
    let first = env.read(Path::new("/bk/meta/000001.backup")).unwrap();
    for after in [0, 1, 7] {
        env.arm(Path::new("/bk/meta"), after);
        let err = engine.delete_backup(BackupId(1)).unwrap_err();
        assert!(err.to_string().contains("the walk broke"), "{err}");
        assert_eq!(shared_on(&env), before, "a file went after {after} entries");
        // The metadata went before the collection; put it back for the
        // next cut.
        env.write(Path::new("/bk/meta/000001.backup"), &first)
            .unwrap();
    }
    env.arm(Path::new("/nowhere"), 0);
    for id in 1..23 {
        let target = PathBuf::from(format!("/restored-{id}"));
        engine.restore(BackupId(id), &target, None).unwrap();
    }
    for id in [1, 2, 3, 22] {
        let restored = Db::open(format!("/restored-{id}"), fault_options(&env)).unwrap();
        assert_has(&restored, "a", 200);
        if id == 2 {
            assert_has(&restored, "z", 200);
        }
    }
    // Whole again, the walk lets the delete finish, and the copies keep
    // every table backup 1 listed.
    engine.delete_backup(BackupId(1)).unwrap();
    assert_eq!(shared_on(&env), before);
}

/// The purge keeps the newest `keep` whatever `keep` is, through the window
/// that deletes a backup once `keep` newer ones followed it.
#[test]
fn purge_keeps_exactly_the_newest_for_every_window() {
    for (keep, left) in [
        (100, (1..=6).collect::<Vec<u64>>()),
        (6, (1..=6).collect()),
        (5, (2..=6).collect()),
        (2, vec![5, 6]),
        (0, vec![]),
    ] {
        let (_env, mut engine) = walk_fault_repository(4);
        engine.purge_old_backups(keep).unwrap();
        let ids: Vec<u64> = readable(&engine).iter().map(|b| b.id.0).collect();
        assert_eq!(ids, left, "keep {keep}");
    }
}

/// A listing whose walk breaks part way is reported, never a short list,
/// and a purge over it deletes nothing.
#[test]
fn a_listing_whose_walk_breaks_says_so() {
    let (env, mut engine) = walk_fault_repository(4);
    env.arm(Path::new("/bk/meta"), 3);
    let items: Vec<_> = engine.list_backups().collect();
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(items[0].is_err());
    assert!(engine.purge_old_backups(0).is_err());
    env.arm(Path::new("/nowhere"), 0);
    assert_eq!(readable(&engine).len(), 6);
}

/// The sweep hands `shared/` out in batches: more unlisted tables than two
/// batches hold, mixed in the walk with the tables backups 1 and 2 list,
/// all go, and every listed one stays, wherever a batch boundary falls.
#[test]
fn a_sweep_across_batches_removes_every_unlisted_table_and_no_listed_one() {
    let (env, mut engine) = walk_fault_repository(0);
    let listed = shared_on(&env);
    assert!(!listed.is_empty());
    for k in 0..(collect::SWEEP as u128 * 2 + 1) {
        env.write(
            &Path::new("/bk/shared").join(shared_filename(0xF000_0000 + k)),
            b"x",
        )
        .unwrap();
    }
    engine.delete_backup(BackupId(99)).unwrap();
    assert_eq!(shared_on(&env), listed);
    for id in [1, 2] {
        let target = PathBuf::from(format!("/restored-{id}"));
        engine.restore(BackupId(id), &target, None).unwrap();
        let restored = Db::open(&target, fault_options(&env)).unwrap();
        assert_has(&restored, "a", 200);
    }
}
