//! Failed repair I/O must fail the edit that records the repair, which is
//! the open's first, and leave the old inputs recoverable.

use super::legacy_overlap_tests::{assert_layout, fixture, fixture_with_ingest_seq, options};
use super::*;
use crate::env::{Capabilities, FileLock, FileMeta, JoinHandle, ReadDir, ReadFile, WriteFile};

#[derive(Clone, Copy, Debug)]
enum Failure {
    Write,
    Sync,
}

struct FailingWriter {
    inner: Box<dyn WriteFile>,
    failure: Failure,
}

impl WriteFile for FailingWriter {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        if matches!(self.failure, Failure::Write) {
            self.inner.write_all(&bytes[..bytes.len() / 2])?;
            return Err(io::Error::other("injected repair append failure"));
        }
        self.inner.write_all(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    fn sync_all(&mut self) -> io::Result<()> {
        if matches!(self.failure, Failure::Sync) {
            return Err(io::Error::other("injected repair sync failure"));
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

#[derive(Debug)]
struct FailingEnv {
    inner: Arc<dyn Env>,
    failure: Failure,
}

impl Env for FailingEnv {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir_all(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.inner.read_dir(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.inner.open_read(path)
    }

    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        let inner = self.inner.open_write(path, mode)?;
        if matches!(mode, WriteMode::Append) && path.file_name().is_some_and(|n| n == "MANIFEST") {
            return Ok(Box::new(FailingWriter {
                inner,
                failure: self.failure,
            }));
        }
        Ok(inner)
    }

    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        self.inner.metadata(path)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_file(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename(from, to)
    }

    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.hard_link(from, to)
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

#[test]
fn repair_append_and_sync_failures_fail_the_open_and_keep_every_table() {
    for failure in [Failure::Write, Failure::Sync] {
        let (dir, _) = fixture();
        let sst_dir = dir.path().join("sst");
        let env: Arc<dyn Env> = Arc::new(FailingEnv {
            inner: crate::env::std_env(),
            failure,
        });
        let error =
            VersionSet::open_with_policy(&env, dir.path(), &sst_dir, MetadataPolicy::Pinned, None)
                .and_then(|mut vs| vs.apply(&[]))
                .expect_err("repair I/O must fail the open's first edit");
        assert!(error.to_string().contains("injected repair"));
        for id in [8, 3, 10, 90, 77] {
            assert!(
                sst_dir.join(sst_filename(id)).exists(),
                "table {id} after {failure:?}"
            );
        }
        let vs = VersionSet::open(dir.path(), &sst_dir).unwrap();
        assert_layout(&vs.current());
    }
}

/// The whole open through a manifest that cannot take the record of the
/// move: the open fails, and the next one reads what the range tombstone of
/// the moved table says, whether the failed write left half a batch (the
/// tables are moved again) or a whole one (the move is found recorded).
#[test]
fn a_database_open_that_cannot_record_the_move_fails_and_the_next_one_reads_the_same() {
    for (failure, moved_again) in [(Failure::Write, 3), (Failure::Sync, 0)] {
        let (dir, _) = fixture_with_ingest_seq(40);
        let env: Arc<dyn Env> = Arc::new(FailingEnv {
            inner: crate::env::std_env(),
            failure,
        });
        let error = crate::Db::open(dir.path(), options().env(env))
            .expect_err("an open that cannot record the move fails");
        assert!(error.to_string().contains("injected repair"), "{error}");

        let db = crate::Db::open(dir.path(), options()).unwrap();
        assert_eq!(
            db.get_int_property("regolith.manifest.tables_demoted"),
            Some(moved_again),
            "{failure:?}"
        );
        // The moved table's tombstone, at sequence 40, over `k` at 30.
        assert_eq!(db.get(b"k").unwrap(), None, "{failure:?}");
        assert_eq!(
            db.get(b"e").unwrap().as_deref(),
            Some(b"l0".as_slice()),
            "{failure:?}"
        );
        assert_eq!(
            db.get(b"z").unwrap().as_deref(),
            Some(b"ingested".as_slice()),
            "{failure:?}"
        );
    }
}
