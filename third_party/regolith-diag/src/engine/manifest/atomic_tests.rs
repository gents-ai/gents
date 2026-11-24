use super::*;
use crate::env::{
    Capabilities, FileLock, FileMeta, JoinHandle, MemEnv, ReadDir, ReadFile, WriteFile,
};
use std::sync::atomic::{AtomicBool, Ordering};

fn legacy_manifest() -> Vec<u8> {
    // Spell out the original format so the fixture cannot change with
    // the batch encoder under test.
    let mut data = b"REGOMAN\x01".to_vec();
    let crc = checksum::manifest_record(0, &data);
    data.extend_from_slice(&crc.to_le_bytes());
    for (tag, value) in [(4u8, 81u64), (3, 45), (5, 7)] {
        let mut payload = vec![tag];
        payload.extend_from_slice(&value.to_le_bytes());
        let len = payload.len() as u32;
        let crc = checksum::manifest_record(len, &payload);
        data.extend_from_slice(&len.to_le_bytes());
        data.extend_from_slice(&payload);
        data.extend_from_slice(&crc.to_le_bytes());
    }
    data
}

fn fixture() -> (Arc<dyn Env>, PathBuf, PathBuf) {
    let env: Arc<dyn Env> = Arc::new(MemEnv::new());
    let db_dir = PathBuf::from("manifest-tests");
    let sst_dir = db_dir.join("sst");
    env.create_dir_all(&sst_dir).unwrap();
    env.write(&db_dir.join("MANIFEST"), &legacy_manifest())
        .unwrap();
    (env, db_dir, sst_dir)
}

fn open(env: &Arc<dyn Env>, db_dir: &Path, sst_dir: &Path) -> VersionSet {
    VersionSet::open_with_policy(env, db_dir, sst_dir, MetadataPolicy::Pinned, None).unwrap()
}

fn assert_version(vs: &VersionSet, next_file_id: u64, last_seq: u64, min_wal_id: u64) {
    let version = vs.current();
    assert_eq!(version.next_file_id, next_file_id);
    assert_eq!(version.last_seq, last_seq);
    assert_eq!(version.min_wal_id, min_wal_id);
    assert!(version.levels.iter().all(Vec::is_empty));
}

#[test]
fn legacy_single_edit_frames_replay() {
    let (env, db_dir, sst_dir) = fixture();
    let vs = open(&env, &db_dir, &sst_dir);
    assert_version(&vs, 81, 45, 7);
    assert_eq!(env.read(vs.manifest_path()).unwrap(), legacy_manifest());
}

#[test]
fn legacy_manifest_accepts_batches_and_rewrite() {
    let (env, db_dir, sst_dir) = fixture();
    let mut vs = open(&env, &db_dir, &sst_dir);
    vs.apply(&[VersionEdit::SetLastSeq(67), VersionEdit::SetNextFileId(92)])
        .unwrap();
    let mixed = env.read(vs.manifest_path()).unwrap();
    assert!(mixed.starts_with(&legacy_manifest()));
    assert!(mixed.len() > legacy_manifest().len());
    drop(vs);

    let mut vs = open(&env, &db_dir, &sst_dir);
    assert_version(&vs, 92, 67, 7);
    vs.compact_manifest().unwrap();
    drop(vs);

    let mut vs = open(&env, &db_dir, &sst_dir);
    assert_version(&vs, 92, 67, 7);
    vs.apply(&[
        VersionEdit::Reset {
            next_file_id: 109,
            min_wal_id: 108,
        },
        VersionEdit::SetLastSeq(90),
    ])
    .unwrap();
    drop(vs);

    let vs = open(&env, &db_dir, &sst_dir);
    assert_version(&vs, 109, 90, 108);
}

#[test]
fn torn_batch_after_legacy_frames_is_discarded_whole() {
    let (env, db_dir, sst_dir) = fixture();
    let mut vs = open(&env, &db_dir, &sst_dir);
    vs.apply(&[VersionEdit::SetLastSeq(67), VersionEdit::SetNextFileId(92)])
        .unwrap();
    let manifest = env.read(vs.manifest_path()).unwrap();
    drop(vs);

    for cut in legacy_manifest().len()..manifest.len() {
        let (env, db_dir, sst_dir) = fixture();
        let manifest_path = db_dir.join("MANIFEST");
        env.write(&manifest_path, &manifest[..cut]).unwrap();
        let mut vs = open(&env, &db_dir, &sst_dir);
        assert_version(&vs, 81, 45, 7);
        assert_eq!(
            env.read(&manifest_path).unwrap(),
            legacy_manifest(),
            "repair left a partial batch at byte {cut}"
        );
        // A repaired tail must also be safe to append to.
        vs.apply(&[
            VersionEdit::SetLastSeq(100),
            VersionEdit::SetNextFileId(110),
        ])
        .unwrap();
        drop(vs);
        let vs = open(&env, &db_dir, &sst_dir);
        assert_version(&vs, 110, 100, 7);
    }
}

#[derive(Clone, Copy)]
enum Failure {
    PartialWrite,
    Sync,
}

struct FailingWriter {
    inner: Box<dyn WriteFile>,
    failure: Failure,
}

impl WriteFile for FailingWriter {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        if matches!(self.failure, Failure::PartialWrite) {
            self.inner.write_all(&buf[..buf.len() / 2])?;
            return Err(io::Error::other("injected MANIFEST append failure"));
        }
        self.inner.write_all(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    fn sync_all(&mut self) -> io::Result<()> {
        if matches!(self.failure, Failure::Sync) {
            return Err(io::Error::other("injected MANIFEST sync failure"));
        }
        self.inner.sync_all()
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }

    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }
}

fn assert_failure_requires_reopen(failure: Failure) {
    let (env, db_dir, sst_dir) = fixture();
    let mut vs = open(&env, &db_dir, &sst_dir);
    // Replace only the append handle, after the initial manifest is
    // established, so the fault cannot be consumed during creation.
    let inner = env
        .open_write(vs.manifest_path(), WriteMode::Append)
        .unwrap();
    vs.manifest_writer = Some(BufferedWriter::new(Box::new(FailingWriter {
        inner,
        failure,
    })));

    let error = vs
        .apply(&[VersionEdit::SetLastSeq(67), VersionEdit::SetNextFileId(92)])
        .unwrap_err();
    assert!(error.to_string().contains("injected MANIFEST"));
    assert_version(&vs, 81, 45, 7);
    let failed_bytes = env.read(vs.manifest_path()).unwrap();

    let error = vs.apply(&[VersionEdit::SetLastSeq(100)]).unwrap_err();
    assert!(error.to_string().contains("reopen"));
    let error = vs.compact_manifest().unwrap_err();
    assert!(error.to_string().contains("reopen"));
    assert_version(&vs, 81, 45, 7);
    assert_eq!(env.read(vs.manifest_path()).unwrap(), failed_bytes);
    assert!(!env.exists(&db_dir.join("MANIFEST.tmp")));
    drop(vs);

    let mut vs = open(&env, &db_dir, &sst_dir);
    match failure {
        Failure::PartialWrite => assert_version(&vs, 81, 45, 7),
        // MemEnv retains every written byte. A sync error can leave a
        // complete batch on disk, which recovery must apply whole.
        Failure::Sync => assert_version(&vs, 92, 67, 7),
    }
    vs.apply(&[
        VersionEdit::SetLastSeq(100),
        VersionEdit::SetNextFileId(110),
    ])
    .unwrap();
    vs.compact_manifest().unwrap();
    drop(vs);
    let vs = open(&env, &db_dir, &sst_dir);
    assert_version(&vs, 110, 100, 7);
}

#[test]
fn failed_append_requires_reopen() {
    assert_failure_requires_reopen(Failure::PartialWrite);
}

#[test]
fn failed_sync_requires_reopen() {
    assert_failure_requires_reopen(Failure::Sync);
}

#[derive(Debug, Default)]
struct RewriteFailureEnv {
    inner: MemEnv,
    fail_rename: AtomicBool,
    fail_append_open: AtomicBool,
}

impl Env for RewriteFailureEnv {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir_all(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.inner.read_dir(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.inner.open_read(path)
    }

    fn open_log(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn crate::env::LogFile>> {
        self.inner.open_log(path, mode)
    }
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        if matches!(mode, WriteMode::Append) && self.fail_append_open.swap(false, Ordering::SeqCst)
        {
            return Err(io::Error::other("injected MANIFEST append-open failure"));
        }
        self.inner.open_write(path, mode)
    }

    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        self.inner.metadata(path)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_file(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        if self.fail_rename.swap(false, Ordering::SeqCst) {
            return Err(io::Error::other("injected MANIFEST rename failure"));
        }
        self.inner.rename(from, to)
    }

    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        self.inner.hard_link(src, dst)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.sync_dir(path)
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

fn assert_rewrite_failure_requires_reopen(fail_rename: bool) {
    let failing = Arc::new(RewriteFailureEnv::default());
    let env: Arc<dyn Env> = failing.clone();
    let db_dir = PathBuf::from("manifest-tests");
    let sst_dir = db_dir.join("sst");
    env.create_dir_all(&sst_dir).unwrap();
    let mut vs = open(&env, &db_dir, &sst_dir);
    vs.apply(&[VersionEdit::SetLastSeq(67), VersionEdit::SetNextFileId(92)])
        .unwrap();

    if fail_rename {
        failing.fail_rename.store(true, Ordering::SeqCst);
    } else {
        failing.fail_append_open.store(true, Ordering::SeqCst);
    }
    let error = vs.compact_manifest().unwrap_err();
    assert!(error.to_string().contains("injected MANIFEST"));
    assert_version(&vs, 92, 67, 0);
    let failed_bytes = env.read(vs.manifest_path()).unwrap();

    let error = vs.apply(&[VersionEdit::SetLastSeq(100)]).unwrap_err();
    assert!(error.to_string().contains("reopen"));
    let error = vs.compact_manifest().unwrap_err();
    assert!(error.to_string().contains("reopen"));
    assert_eq!(env.read(vs.manifest_path()).unwrap(), failed_bytes);
    assert_version(&vs, 92, 67, 0);
    drop(vs);

    let mut vs = open(&env, &db_dir, &sst_dir);
    assert_version(&vs, 92, 67, 0);
    vs.apply(&[VersionEdit::SetLastSeq(100)]).unwrap();
    vs.compact_manifest().unwrap();
    drop(vs);
    let vs = open(&env, &db_dir, &sst_dir);
    assert_version(&vs, 92, 100, 0);
}

#[test]
fn failed_rewrite_rename_requires_reopen() {
    assert_rewrite_failure_requires_reopen(true);
}

#[test]
fn failed_rewrite_append_open_requires_reopen() {
    assert_rewrite_failure_requires_reopen(false);
}
