use crate::env::{
    Capabilities, Env, FileLock, FileMeta, JoinHandle, LogFile, ReadDir, ReadFile, StdEnv,
    WriteFile, WriteMode,
};
use std::{
    io,
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Debug, Default)]
pub(crate) struct Recording {
    /// Every host write to a log, one entry per call, a vectored write as
    /// one.
    pub writes: Vec<Vec<u8>>,
    pub data_syncs: usize,
    pub fail_on_write: Option<usize>,
    pub partial_failures: usize,
    /// Writes and syncs in the order they happened: `write <len>` and
    /// `sync`.
    pub log: Vec<String>,
    /// Everything `log` holds, plus every rename and directory sync, in
    /// the order they happened: `rename <to>` and `sync dir`; every log
    /// created and removed: `create <name>` and `remove <name>`; and every
    /// writeback started: `writeback <offset> <len>`.
    pub ops: Vec<String>,
    /// The name of every thread spawned, in order.
    pub spawned: Vec<String>,
    /// Every directory listed, in order.
    pub listed: Vec<std::path::PathBuf>,
}

#[derive(Debug, Default)]
pub(crate) struct RecordingEnv {
    inner: StdEnv,
    pub events: Arc<Mutex<Recording>>,
}

impl Recording {
    /// Record one host write of `bytes` to a log. `true` when this is the
    /// write to fail part way.
    fn write(&mut self, bytes: &[u8]) -> bool {
        self.writes.push(bytes.to_vec());
        self.log.push(format!("write {}", bytes.len()));
        self.ops.push(format!("write {}", bytes.len()));
        let fails = self.fail_on_write == Some(self.writes.len());
        if fails {
            self.fail_on_write = None;
            self.partial_failures += 1;
        }
        fails
    }

    fn sync(&mut self) {
        self.data_syncs += 1;
        self.log.push("sync".to_string());
        self.ops.push("sync".to_string());
    }
}

struct RecordingWrite {
    inner: Box<dyn WriteFile>,
    events: Arc<Mutex<Recording>>,
}

impl WriteFile for RecordingWrite {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.events.lock().unwrap().write(bytes) {
            self.inner.write_all(&bytes[..7])?;
            return Err(io::Error::other("injected partial recovery write"));
        }
        self.inner.write_all(bytes)
    }

    /// One recorded write, as the host sees one `writev`.
    fn write_all_vectored(&mut self, slices: &[&[u8]]) -> io::Result<()> {
        self.write_all(&slices.concat())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    fn sync_all(&mut self) -> io::Result<()> {
        self.inner.sync_all()
    }

    fn sync_data(&mut self) -> io::Result<()> {
        self.events.lock().unwrap().sync();
        self.inner.sync_data()
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }

    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }
}

/// The log the wrapped environment hands out, with every write and sync
/// recorded as [`RecordingWrite`] records an append.
struct RecordingLog {
    inner: Box<dyn LogFile>,
    events: Arc<Mutex<Recording>>,
}

impl LogFile for RecordingLog {
    fn write_all_at(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        if self.events.lock().unwrap().write(bytes) {
            self.inner.write_all_at(offset, &bytes[..7])?;
            return Err(io::Error::other("injected partial recovery write"));
        }
        self.inner.write_all_at(offset, bytes)
    }

    /// One recorded write, as the host sees one `pwritev`.
    fn write_all_vectored_at(&self, offset: u64, bufs: &[io::IoSlice<'_>]) -> io::Result<()> {
        let bytes: Vec<u8> = bufs.iter().flat_map(|buf| buf.iter().copied()).collect();
        self.write_all_at(offset, &bytes)
    }

    fn sync_data(&self) -> io::Result<()> {
        self.events.lock().unwrap().sync();
        self.inner.sync_data()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }

    fn start_writeback(&self, offset: u64, len: u64) {
        self.events
            .lock()
            .unwrap()
            .ops
            .push(format!("writeback {offset} {len}"));
        self.inner.start_writeback(offset, len);
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn is_wal(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "wal")
        || path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "wal")
}

impl Env for RecordingEnv {
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        let inner = self.inner.open_write(path, mode)?;
        if !is_wal(path) {
            return Ok(inner);
        }
        Ok(Box::new(RecordingWrite {
            inner,
            events: Arc::clone(&self.events),
        }))
    }
    fn open_log(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn LogFile>> {
        let inner = self.inner.open_log(path, mode)?;
        if !is_wal(path) {
            return Ok(inner);
        }
        if mode == WriteMode::Truncate {
            self.events
                .lock()
                .unwrap()
                .ops
                .push(format!("create {}", file_name(path)));
        }
        Ok(Box::new(RecordingLog {
            inner,
            events: Arc::clone(&self.events),
        }))
    }
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir_all(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.events.lock().unwrap().listed.push(path.to_path_buf());
        self.inner.read_dir(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.inner.open_read(path)
    }
    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        self.inner.metadata(path)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        if is_wal(path) {
            self.events
                .lock()
                .unwrap()
                .ops
                .push(format!("remove {}", file_name(path)));
        }
        self.inner.remove_file(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let name = to.file_name().map(|n| n.to_string_lossy().into_owned());
        self.events
            .lock()
            .unwrap()
            .ops
            .push(format!("rename {}", name.unwrap_or_default()));
        self.inner.rename(from, to)
    }
    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.hard_link(from, to)
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.events.lock().unwrap().ops.push("sync dir".to_string());
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
        self.events.lock().unwrap().spawned.push(name.to_string());
        self.inner.spawn(name, body)
    }
}
