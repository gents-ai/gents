//! A delete or a purge whose sync of `meta/` failed, run again. The first
//! call removed the metadata and stopped before collecting, as it must; the
//! call run again finds nothing more to remove. It must still make the
//! earlier removal durable before it removes any shared table: otherwise a
//! power cut can bring the backup's metadata back while its tables stay
//! gone. The rule is `ListedRestores` in `proofs/tla/BackupSeal.tla`, with
//! the sync a step of its own (RED `SyncOnlyRemoved`).

use std::sync::Mutex;

use super::tests::{populate, tiny_flush_opts};
use super::*;
use crate::env::{
    Capabilities, FileLock, FileMeta, JoinHandle, LogFile, MemEnv, ReadDir, ReadFile, WriteFile,
};

/// What [`SyncFault`] saw, in order, once its journal began.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    /// A directory sync that succeeded.
    Synced(PathBuf),
    /// A file removed.
    Removed(PathBuf),
}

/// A [`MemEnv`] that fails the next sync of one directory and journals the
/// syncs and removals after that.
#[derive(Debug, Default)]
struct SyncFault {
    inner: MemEnv,
    /// The directory whose next sync fails.
    fail_next: Mutex<Option<PathBuf>>,
    /// Set once the failure happened: from then on, every successful sync
    /// and every removal is journaled.
    journal: Mutex<Option<Vec<Op>>>,
}

impl SyncFault {
    fn fail_next_sync(&self, dir: &Path) {
        *self.fail_next.lock().unwrap() = Some(dir.to_path_buf());
    }

    fn journal(&self) -> Vec<Op> {
        self.journal.lock().unwrap().clone().unwrap_or_default()
    }

    fn note(&self, op: Op) {
        if let Some(journal) = self.journal.lock().unwrap().as_mut() {
            journal.push(op);
        }
    }
}

impl Env for SyncFault {
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        let mut armed = self.fail_next.lock().unwrap();
        if armed.as_deref() == Some(path) {
            *armed = None;
            *self.journal.lock().unwrap() = Some(Vec::new());
            return Err(io::Error::other("the directory sync failed"));
        }
        drop(armed);
        self.inner.sync_dir(path)?;
        self.note(Op::Synced(path.to_path_buf()));
        Ok(())
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_file(path)?;
        self.note(Op::Removed(path.to_path_buf()));
        Ok(())
    }
    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.inner.read_dir(path)
    }
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir_all(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.inner.open_read(path)
    }
    fn open_log(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn LogFile>> {
        self.inner.open_log(path, mode)
    }
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        self.inner.open_write(path, mode)
    }
    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        self.inner.metadata(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename(from, to)
    }
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        self.inner.hard_link(src, dst)
    }
    fn lock_file(&self, path: &Path, exclusive: bool) -> io::Result<Box<dyn FileLock>> {
        self.inner.lock_file(path, exclusive)
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    fn now_micros(&self) -> Option<u64> {
        self.inner.now_micros()
    }
    fn unix_secs(&self) -> Option<u64> {
        self.inner.unix_secs()
    }
    fn spawn(
        &self,
        name: &str,
        body: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<Box<dyn JoinHandle>> {
        self.inner.spawn(name, body)
    }
}

const META: &str = "/bk/meta";
const SHARED: &str = "/bk/shared";

/// A repository on a [`SyncFault`] env holding `n` backups of one database,
/// each after more writes, so backup 1 holds tables no later one lists.
fn repository(n: usize) -> (Arc<SyncFault>, BackupEngine) {
    let env = Arc::new(SyncFault::default());
    let options = tiny_flush_opts()
        .env(env.clone())
        .max_background_compactions(0);
    let db = Db::open("/db", options).unwrap();
    let mut engine = BackupEngine::open_with_env("/bk", env.clone()).unwrap();
    for i in 0..n {
        populate(&db, &format!("g{i}"), 200);
        crate::through_stalls(|| db.put(b"zz", &i.to_le_bytes())).unwrap();
        db.compact_range(None, None).wait().unwrap();
        engine.create_backup(&db).unwrap();
    }
    (env, engine)
}

fn shared_count(env: &SyncFault) -> usize {
    env.read_dir(Path::new(SHARED)).unwrap().count()
}

/// The journal of the call run again: it removed shared tables, and a
/// successful sync of `meta/` came before the first of them.
fn assert_synced_before_collecting(env: &SyncFault) {
    let journal = env.journal();
    let first_removal = journal
        .iter()
        .position(|op| matches!(op, Op::Removed(path) if path.starts_with(SHARED)))
        .expect("the call run again removed no shared table");
    let synced = journal
        .iter()
        .position(|op| *op == Op::Synced(PathBuf::from(META)));
    assert!(
        synced.is_some_and(|synced| synced < first_removal),
        "a shared table was removed before meta/ was synced: {journal:?}"
    );
}

#[test]
fn a_delete_run_again_after_a_failed_sync_syncs_meta_before_it_collects() {
    let (env, mut engine) = repository(2);
    let before = shared_count(&env);
    env.fail_next_sync(Path::new(META));
    assert!(engine.delete_backup(BackupId(1)).is_err());
    assert_eq!(shared_count(&env), before, "the failed delete collected");
    assert!(!env.exists(&Path::new(META).join(backup_filename(BackupId(1)))));

    engine.delete_backup(BackupId(1)).unwrap();
    assert!(shared_count(&env) < before);
    assert_synced_before_collecting(&env);
}

#[test]
fn a_purge_run_again_after_a_failed_sync_syncs_meta_before_it_collects() {
    let (env, mut engine) = repository(2);
    let before = shared_count(&env);
    env.fail_next_sync(Path::new(META));
    assert!(engine.purge_old_backups(1).is_err());
    assert_eq!(shared_count(&env), before, "the failed purge collected");

    engine.purge_old_backups(1).unwrap();
    assert!(shared_count(&env) < before);
    assert_synced_before_collecting(&env);
}
