//! A table moved while a handle reopens it: the reopen finds the file at
//! its new name, however the move and the reopen interleave.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering as StdOrdering};

use super::*;
use crate::env::MemEnv;

/// What [`Hooked`] runs, once.
type Hook = Box<dyn Fn() + Send + Sync>;

/// An `Env` over [`MemEnv`] that, once armed, runs a hook just before it
/// opens one path: the hook lands between a reopen's load of the names and
/// its open, the widest gap a move can fall into.
struct Hooked {
    inner: MemEnv,
    at: PathBuf,
    armed: AtomicBool,
    hook: OnceLock<Hook>,
}

impl std::fmt::Debug for Hooked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooked").finish_non_exhaustive()
    }
}

impl Env for Hooked {
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        if path == self.at
            && self.armed.swap(false, StdOrdering::SeqCst)
            && let Some(hook) = self.hook.get()
        {
            hook();
        }
        self.inner.open_read(path)
    }
    fn open_log(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn crate::env::LogFile>> {
        self.inner.open_log(path, mode)
    }
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        self.inner.open_write(path, mode)
    }
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir_all(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.inner.read_dir(path)
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

fn read_all(file: &dyn ReadFile) -> Vec<u8> {
    let mut buf = vec![0u8; file.len().unwrap() as usize];
    file.read_exact_at(0, &mut buf).unwrap();
    buf
}

/// The reopen loads the table's names (only its old name: no move has
/// begun), and before it opens that name the whole move runs: new names
/// stored, the file renamed, the names settled. The open of the old name
/// finds nothing; the reopen loads the names again, sees the move, and
/// opens the file at its new name.
#[test]
fn a_reopen_that_loaded_the_names_before_a_whole_move_finds_the_file_after_it() {
    let mem = MemEnv::new();
    let dir = Path::new("/db");
    let (table, moved, other) = (
        dir.join("000001.sst"),
        dir.join("000009.sst"),
        dir.join("000002.sst"),
    );
    mem.create_dir_all(dir).unwrap();
    mem.write(&table, b"table one").unwrap();
    mem.write(&other, b"table two").unwrap();
    let hooked = Arc::new(Hooked {
        inner: mem.clone(),
        at: table.clone(),
        armed: AtomicBool::new(false),
        hook: OnceLock::new(),
    });
    let env = Arc::new(OpenFileLimit::new(hooked.clone(), 1));
    let file = env.open_read(&table).unwrap();
    // One slot: opening the other table closes this one's descriptor, so
    // the next read must reopen it.
    let other_file = env.open_read(&other).unwrap();
    let weak = Arc::downgrade(&env);
    let (from, to) = (table.clone(), moved.clone());
    let _ = hooked.hook.set(Box::new(move || {
        if let Some(env) = weak.upgrade() {
            env.rename(&from, &to).unwrap();
        }
    }));
    hooked.armed.store(true, StdOrdering::SeqCst);

    assert_eq!(read_all(&*file), b"table one");
    assert!(
        !hooked.armed.load(StdOrdering::SeqCst),
        "the move ran inside the reopen"
    );
    assert!(mem.exists(&moved) && !mem.exists(&table));
    assert_eq!(read_all(&*other_file), b"table two");
    assert_eq!(read_all(&*file), b"table one");
}
