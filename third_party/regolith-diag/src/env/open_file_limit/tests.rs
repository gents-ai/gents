//! The open-file table: its bound, its reads, removals and renames, under
//! one thread, many threads, and a sequential model.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as StdOrdering};

use proptest::prelude::*;

use super::*;
use crate::env::{MemEnv, std_env};

fn contents(i: usize) -> Vec<u8> {
    format!("table {i:04}").repeat(4).into_bytes()
}

fn write_tables(env: &dyn Env, dir: &Path, n: usize) -> Vec<PathBuf> {
    env.create_dir_all(dir).unwrap();
    (0..n)
        .map(|i| {
            let path = dir.join(format!("{i:06}.sst"));
            env.write(&path, &contents(i)).unwrap();
            path
        })
        .collect()
}

fn read_all(file: &dyn ReadFile) -> Vec<u8> {
    let mut buf = vec![0u8; file.len().unwrap() as usize];
    file.read_exact_at(0, &mut buf).unwrap();
    buf
}

/// What the device sees: descriptors open now and at most, reads in flight,
/// and whether a descriptor was ever closed under a read.
#[derive(Default)]
pub(super) struct Device {
    pub(super) open: AtomicUsize,
    pub(super) most_open: AtomicUsize,
    pub(super) closed_in_use: AtomicBool,
}

/// An `Env` over [`MemEnv`] whose table descriptors report to a [`Device`].
#[derive(Clone)]
pub(super) struct Counted {
    inner: MemEnv,
    pub(super) device: Arc<Device>,
}

struct CountedFile {
    inner: Box<dyn ReadFile>,
    device: Arc<Device>,
    in_use: AtomicUsize,
}

impl ReadFile for CountedFile {
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.in_use.fetch_add(1, StdOrdering::SeqCst);
        // Give a close racing this read time to land, if one could.
        std::thread::yield_now();
        let read = self.inner.read_exact_at(offset, buf);
        self.in_use.fetch_sub(1, StdOrdering::SeqCst);
        read
    }

    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }
}

impl Drop for CountedFile {
    fn drop(&mut self) {
        if self.in_use.load(StdOrdering::SeqCst) != 0 {
            self.device.closed_in_use.store(true, StdOrdering::SeqCst);
        }
        self.device.open.fetch_sub(1, StdOrdering::SeqCst);
    }
}

impl std::fmt::Debug for Counted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Counted").finish_non_exhaustive()
    }
}

impl Counted {
    pub(super) fn new() -> Self {
        Self {
            inner: MemEnv::new(),
            device: Arc::new(Device::default()),
        }
    }
}

impl Env for Counted {
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        let inner = self.inner.open_read(path)?;
        let now = self.device.open.fetch_add(1, StdOrdering::SeqCst) + 1;
        self.device.most_open.fetch_max(now, StdOrdering::SeqCst);
        Ok(Box::new(CountedFile {
            inner,
            device: Arc::clone(&self.device),
            in_use: AtomicUsize::new(0),
        }))
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

/// Every name in `dir`, sorted.
fn names(env: &dyn Env, dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = env
        .read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    names
}

#[test]
fn never_holds_more_than_capacity_and_reopens_on_read() {
    let dir = tempfile::tempdir().unwrap();
    let paths = write_tables(&*std_env(), dir.path(), 10);
    let env = OpenFileLimit::new(std_env(), 3);
    let files: Vec<_> = paths.iter().map(|p| env.open_read(p).unwrap()).collect();
    assert!(env.open_count() <= 3, "{} open", env.open_count());

    for _ in 0..3 {
        for (i, file) in files.iter().enumerate() {
            assert_eq!(read_all(&**file), contents(i));
            assert!(env.open_count() <= 3, "{} open", env.open_count());
        }
    }
    drop(files);
    assert_eq!(env.open_count(), 0);
}

#[test]
fn a_dropped_handle_closes_its_descriptor_at_once() {
    let counted = Counted::new();
    let paths = write_tables(&counted, Path::new("/db"), 1);
    let env = OpenFileLimit::new(Arc::new(counted.clone()), 1_000);
    for _ in 0..1_000 {
        let file = env.open_read(&paths[0]).unwrap();
        assert_eq!(read_all(&*file), contents(0));
        drop(file);
        assert_eq!(counted.device.open.load(StdOrdering::SeqCst), 0);
    }
    assert_eq!(env.open_count(), 0);
    assert!(env.shared.entries.is_empty(), "a dead entry stayed mapped");
}

#[test]
fn files_that_are_not_tables_pass_through_unmanaged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("MANIFEST");
    std::fs::write(&path, b"m").unwrap();
    let env = OpenFileLimit::new(std_env(), 1);
    let _file = env.open_read(&path).unwrap();
    assert_eq!(env.open_count(), 0);
}

#[test]
fn a_table_removed_while_a_handle_lives_stays_readable() {
    let counted = Counted::new();
    let dir = Path::new("/db");
    let paths = write_tables(&counted, dir, 3);
    let env = OpenFileLimit::new(Arc::new(counted.clone()), 1);
    let first = env.open_read(&paths[0]).unwrap();
    // Opening the others evicts the first table's descriptor.
    let rest: Vec<_> = paths[1..]
        .iter()
        .map(|p| env.open_read(p).unwrap())
        .collect();

    env.remove_file(&paths[0]).unwrap();
    assert!(!counted.exists(&paths[0]));
    assert_eq!(read_all(&*first), contents(0));

    // More opens and reads close and reopen it, still within the bound.
    for (i, file) in rest.iter().enumerate() {
        assert_eq!(read_all(&**file), contents(i + 1));
        assert_eq!(read_all(&*first), contents(0));
    }
    assert!(counted.device.most_open.load(StdOrdering::SeqCst) <= 1);
    assert_eq!(
        names(&counted, dir).len(),
        3,
        "the removed table waits aside"
    );

    // The last handle unlinks it.
    drop(first);
    assert_eq!(
        names(&counted, dir),
        vec!["000001.sst".to_string(), "000002.sst".to_string()]
    );
}

#[test]
fn a_table_with_no_handle_is_removed_at_once() {
    let counted = Counted::new();
    let paths = write_tables(&counted, Path::new("/db"), 1);
    let env = OpenFileLimit::new(Arc::new(counted.clone()), 1);
    drop(env.open_read(&paths[0]).unwrap());
    env.remove_file(&paths[0]).unwrap();
    assert!(names(&counted, Path::new("/db")).is_empty());
    assert_eq!(
        env.remove_file(&paths[0]).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn a_renamed_table_stays_readable_and_a_new_file_at_its_old_name_is_new() {
    let counted = Counted::new();
    let dir = Path::new("/db");
    let paths = write_tables(&counted, dir, 2);
    let env = OpenFileLimit::new(Arc::new(counted.clone()), 1);
    let file = env.open_read(&paths[0]).unwrap();
    let moved = dir.join("000009.sst");
    env.rename(&paths[0], &moved).unwrap();
    // Evict, then read: the reopen finds the file under its new name.
    let other = env.open_read(&paths[1]).unwrap();
    assert_eq!(read_all(&*other), contents(1));
    assert_eq!(read_all(&*file), contents(0));

    // A new file at the old name is a different table.
    counted.write(&paths[0], b"new").unwrap();
    let fresh = env.open_read(&paths[0]).unwrap();
    assert_eq!(read_all(&*fresh), b"new");
    assert_eq!(read_all(&*file), contents(0));

    // Renaming over a table that is still read keeps its bytes for its
    // handle.
    env.rename(&paths[0], &paths[1]).unwrap();
    assert_eq!(read_all(&*other), contents(1));
    drop((file, fresh, other));
    let left = names(&counted, dir);
    assert_eq!(
        left,
        vec!["000001.sst".to_string(), "000009.sst".to_string()]
    );
    assert_eq!(counted.read(&paths[1]).unwrap(), b"new");
}

#[test]
fn removed_names_are_recognized_and_nothing_else_is() {
    let removed = removed_name(Path::new("/db/sst/000042.sst"));
    assert!(is_removed_table(&removed), "{}", removed.display());
    for other in [
        "/db/sst/000042.sst",
        "/db/sst/000042.sst.removed-",
        "/db/sst/000042.sst.removed-x1",
        "/db/MANIFEST.removed-3",
        "/db/sst/000042.log.removed-3",
    ] {
        assert!(!is_removed_table(Path::new(other)), "{other}");
    }
}

/// Many threads read many tables through a three-slot table while others
/// open, drop and remove handles. Every read returns its own table's bytes,
/// the device never holds more than three descriptors, and none is closed
/// while a read uses it.
#[test]
fn concurrent_readers_stay_within_the_bound_and_never_lose_a_read() {
    const TABLES: usize = 12;
    const CAPACITY: usize = 3;
    let counted = Counted::new();
    let dir = Path::new("/db");
    let paths = write_tables(&counted, dir, TABLES);
    let env = Arc::new(OpenFileLimit::new(Arc::new(counted.clone()), CAPACITY));
    let handles: Arc<Vec<Box<dyn ReadFile>>> =
        Arc::new(paths.iter().map(|p| env.open_read(p).unwrap()).collect());
    let readers: Vec<_> = (0..8)
        .map(|t| {
            let (handles, env, paths) = (Arc::clone(&handles), Arc::clone(&env), paths.clone());
            std::thread::spawn(move || {
                for round in 0..400 {
                    let i = (t * 7 + round * 5) % TABLES;
                    assert_eq!(read_all(&*handles[i]), contents(i));
                    if round % 16 == t {
                        // A short-lived second handle on the same table.
                        let again = env.open_read(&paths[i]).unwrap();
                        assert_eq!(read_all(&*again), contents(i));
                    }
                }
            })
        })
        .collect();
    for reader in readers {
        reader.join().unwrap();
    }
    assert!(!counted.device.closed_in_use.load(StdOrdering::SeqCst));
    let most = counted.device.most_open.load(StdOrdering::SeqCst);
    assert!(most <= CAPACITY, "{most} descriptors open at once");
    drop(handles);
    assert_eq!(counted.device.open.load(StdOrdering::SeqCst), 0);
}

/// Readers keep reading tables that other threads remove under them.
#[test]
fn concurrent_removals_never_cut_a_reader_off() {
    const TABLES: usize = 8;
    let counted = Counted::new();
    let dir = Path::new("/db");
    let paths = write_tables(&counted, dir, TABLES);
    let env = Arc::new(OpenFileLimit::new(Arc::new(counted.clone()), 2));
    let handles: Arc<Vec<Box<dyn ReadFile>>> =
        Arc::new(paths.iter().map(|p| env.open_read(p).unwrap()).collect());
    let remover = {
        let (env, paths) = (Arc::clone(&env), paths.clone());
        std::thread::spawn(move || {
            for path in &paths {
                env.remove_file(path).unwrap();
            }
        })
    };
    let readers: Vec<_> = (0..4)
        .map(|t| {
            let handles = Arc::clone(&handles);
            std::thread::spawn(move || {
                for round in 0..300 {
                    let i = (t + round * 3) % TABLES;
                    assert_eq!(read_all(&*handles[i]), contents(i));
                }
            })
        })
        .collect();
    remover.join().unwrap();
    for reader in readers {
        reader.join().unwrap();
    }
    assert!(counted.device.most_open.load(StdOrdering::SeqCst) <= 2);
    assert!(!counted.device.closed_in_use.load(StdOrdering::SeqCst));
    drop(handles);
    assert!(
        names(&counted, dir).is_empty(),
        "{:?}",
        names(&counted, dir)
    );
}

/// One step of the sequential model.
#[derive(Clone, Debug)]
enum Op {
    Open(usize),
    Read(usize),
    Close(usize),
    Remove(usize),
    Rename(usize, usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0usize..5).prop_map(Op::Open),
        (0usize..8).prop_map(Op::Read),
        (0usize..8).prop_map(Op::Close),
        (0usize..5).prop_map(Op::Remove),
        (0usize..5, 0usize..5).prop_map(|(a, b)| Op::Rename(a, b)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Against a model of a filesystem whose open descriptors outlive an
    /// unlink or a rename (POSIX): every read returns the bytes its handle
    /// opened, the descriptors never pass the capacity, and once every
    /// handle is gone the directory holds exactly the model's files.
    #[test]
    fn the_table_follows_a_posix_model(
        capacity in 1usize..4,
        ops in prop::collection::vec(op(), 1..80),
    ) {
        let counted = Counted::new();
        let dir = Path::new("/db");
        counted.create_dir_all(dir).unwrap();
        let env = OpenFileLimit::new(Arc::new(counted.clone()), capacity);
        let path = |i: usize| dir.join(format!("{i:06}.sst"));
        // Model: name -> bytes on disk; open handles with the bytes they see.
        let mut disk: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
        for i in 0..5 {
            counted.write(&path(i), &contents(i)).unwrap();
            disk.insert(i, contents(i));
        }
        let mut open: Vec<(Box<dyn ReadFile>, Vec<u8>)> = Vec::new();
        for op in ops {
            match op {
                Op::Open(i) => match (env.open_read(&path(i)), disk.get(&i)) {
                    (Ok(file), Some(bytes)) => open.push((file, bytes.clone())),
                    (Err(e), None) => prop_assert_eq!(e.kind(), io::ErrorKind::NotFound),
                    (got, want) => prop_assert!(false, "open {i}: {:?} vs {:?}", got.map(|_| ()), want),
                },
                Op::Read(h) => {
                    if let Some((file, bytes)) = open.get(h) {
                        prop_assert_eq!(&read_all(&**file), bytes);
                    }
                }
                Op::Close(h) => {
                    if h < open.len() {
                        open.remove(h);
                    }
                }
                Op::Remove(i) => {
                    let got = env.remove_file(&path(i));
                    prop_assert_eq!(got.is_ok(), disk.remove(&i).is_some());
                }
                Op::Rename(a, b) => {
                    let got = env.rename(&path(a), &path(b));
                    match disk.remove(&a) {
                        Some(bytes) => {
                            prop_assert!(got.is_ok());
                            disk.insert(b, bytes);
                        }
                        None => prop_assert!(got.is_err()),
                    }
                }
            }
            prop_assert!(env.open_count() <= capacity);
            prop_assert!(counted.device.most_open.load(StdOrdering::SeqCst) <= capacity);
        }
        for (file, bytes) in &open {
            prop_assert_eq!(&read_all(&**file), bytes);
        }
        drop(open);
        let want: Vec<String> = disk.keys().map(|i| format!("{i:06}.sst")).collect();
        prop_assert_eq!(names(&counted, dir), want);
        for (i, bytes) in &disk {
            prop_assert_eq!(&counted.read(&path(*i)).unwrap(), bytes);
        }
        prop_assert_eq!(counted.device.open.load(StdOrdering::SeqCst), 0);
    }
}
