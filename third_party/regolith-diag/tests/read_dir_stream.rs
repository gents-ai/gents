//! Every directory walk regolith makes holds a bounded batch of entries,
//! never the directory (`Env::read_dir`).
//!
//! Each test fills a directory with thousands of entries, far more than one
//! `readdir` buffer or one bucket of the in-memory map, and drives a caller
//! through the public API over [`WalkProbe`], an `Env` that watches every
//! walk. The probe numbers each entry a walk hands out and records, for each
//! call the caller then makes on that entry's path, whether the walk had
//! already handed out a later entry. An entry acted on after that was held
//! across a pull: the caller carried it. A caller that streams acts on each
//! entry before it pulls the next and carries none; one that collected the
//! directory first carries every entry it acts on; one that keeps a bounded
//! subset (the live logs recovery sorts, a page of backup ids) carries at
//! most that subset. The probe counts, it does not time: no benchmark.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use regolith::env::{
    Capabilities, DirEntry, Env, FileLock, FileMeta, JoinHandle, MemEnv, ReadDir, ReadFile, StdEnv,
    WriteFile, WriteMode,
};
use regolith::{BackupEngine, BackupId, Db, Error, Options, SstFileWriter};
use tempfile::TempDir;

/// Entries a test directory holds: several `readdir` buffers' worth, and
/// thousands of buckets of the in-memory map.
const MANY: usize = 6_000;

/// One walk the probe watched.
#[derive(Debug, Default)]
struct Walk {
    dir: PathBuf,
    /// Entries handed out so far.
    pulled: usize,
    /// Entries acted on after a later entry of this walk was handed out.
    carried: HashSet<PathBuf>,
}

#[derive(Debug, Default)]
struct ProbeState {
    walks: Vec<Walk>,
    /// Each entry handed out: the walk and the position it came at.
    handed: HashMap<PathBuf, (usize, usize)>,
}

/// An `Env` over `inner` that watches every walk; see the module docs.
#[derive(Debug, Clone)]
struct WalkProbe {
    inner: Arc<dyn Env>,
    state: Arc<Mutex<ProbeState>>,
}

impl WalkProbe {
    fn new(inner: Arc<dyn Env>) -> Self {
        Self {
            inner,
            state: Arc::default(),
        }
    }

    fn std() -> Self {
        Self::new(Arc::new(StdEnv::new()))
    }

    fn mem() -> Self {
        Self::new(Arc::new(MemEnv::new()))
    }

    fn env(&self) -> Arc<dyn Env> {
        Arc::new(self.clone())
    }

    /// Forget the walks watched so far.
    fn reset(&self) {
        *self.state.lock().unwrap() = ProbeState::default();
    }

    /// The caller acts on `path`.
    fn act(&self, path: &Path) {
        let mut state = self.state.lock().unwrap();
        if let Some(&(walk, at)) = state.handed.get(path)
            && state.walks[walk].pulled > at + 1
        {
            state.walks[walk].carried.insert(path.to_path_buf());
        }
    }

    /// `(entries handed out, most entries one walk carried)` over the walks
    /// of `dir`.
    fn walks_of(&self, dir: &Path) -> (usize, usize) {
        let state = self.state.lock().unwrap();
        let walks = state.walks.iter().filter(|w| w.dir == dir);
        walks.fold((0, 0), |(pulled, carried), w| {
            (pulled + w.pulled, carried.max(w.carried.len()))
        })
    }

    /// How many walks of `dir` were opened.
    fn walk_count(&self, dir: &Path) -> usize {
        let state = self.state.lock().unwrap();
        state.walks.iter().filter(|w| w.dir == dir).count()
    }
}

impl Env for WalkProbe {
    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        let inner = self.inner.read_dir(path)?;
        let walk = {
            let mut state = self.state.lock().unwrap();
            state.walks.push(Walk {
                dir: path.to_path_buf(),
                ..Walk::default()
            });
            state.walks.len() - 1
        };
        let state = Arc::clone(&self.state);
        Ok(Box::new(inner.inspect(move |entry| {
            if let Ok(DirEntry { path, .. }) = entry {
                let mut state = state.lock().unwrap();
                let at = state.walks[walk].pulled;
                state.walks[walk].pulled += 1;
                state.handed.insert(path.clone(), (walk, at));
            }
        })))
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.act(path);
        self.inner.open_read(path)
    }
    fn open_log(
        &self,
        path: &Path,
        mode: WriteMode,
    ) -> io::Result<Box<dyn regolith::env::LogFile>> {
        self.act(path);
        self.inner.open_log(path, mode)
    }
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        self.act(path);
        self.inner.open_write(path, mode)
    }
    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        self.act(path);
        self.inner.metadata(path)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.act(path);
        self.inner.remove_file(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.act(from);
        self.inner.rename(from, to)
    }
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        self.act(src);
        self.inner.hard_link(src, dst)
    }
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir_all(path)
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

fn options(probe: &WalkProbe) -> Options {
    let options = Options::default()
        .write_buffer_size(4 * 1024)
        .env(probe.env());
    // An env with no threads compacts on the caller's thread.
    if probe.inner.capabilities().threads {
        options
    } else {
        options.max_background_compactions(0)
    }
}

fn put_some(db: &Db, n: usize) {
    for i in 0..n {
        regolith::through_stalls(|| db.put(format!("key_{i:06}").as_bytes(), &[7u8; 64])).unwrap();
    }
}

fn holds_some(db: &Db, n: usize) {
    for i in 0..n {
        assert_eq!(
            db.get(format!("key_{i:06}").as_bytes()).unwrap(),
            Some(vec![7u8; 64]),
            "key {i}"
        );
    }
}

fn touch(env: &dyn Env, path: &Path) {
    env.write(path, b"x").unwrap();
}

/// Names of logs older than any open's `min_wal_id`, which starts at the
/// first log, 1: log 0 under every zero padding a file name has room for,
/// as `.log` and as `.wal`. Hundreds of logs no open replays, which a scan
/// that kept every log before it filtered them would hold.
fn retired_log_names() -> impl Iterator<Item = String> {
    (1..=240)
        .flat_map(|zeros| ["log", "wal"].map(move |ext| format!("wal_{}.{ext}", "0".repeat(zeros))))
}

/// Every name under `dir`, from a walk the probe does not watch.
fn names(env: &dyn Env, dir: &Path) -> Vec<String> {
    env.read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect()
}

#[test]
fn the_orphan_sweep_removes_each_table_as_its_walk_reaches_it() {
    let dir = TempDir::new().unwrap();
    let probe = WalkProbe::std();
    let db = Db::open(dir.path(), options(&probe)).unwrap();
    put_some(&db, 200);
    db.flush().unwrap();
    db.close().unwrap();
    drop(db);
    let sst = dir.path().join("sst");
    // Tables an earlier process renamed aside while a handle read them.
    for k in 0..MANY {
        touch(&*probe.inner, &sst.join(format!("000001.sst.removed-{k}")));
    }

    probe.reset();
    // No compaction worker: one that started compacting the live tables as
    // soon as the open returned would remove them after the sweep's walk
    // handed them out, and the probe would count them against the sweep.
    let db = Db::open(dir.path(), options(&probe).max_background_compactions(0)).unwrap();
    holds_some(&db, 200);
    let (pulled, carried) = probe.walks_of(&sst);
    assert!(pulled >= MANY, "the sweep walked {pulled} entries");
    assert_eq!(carried, 0, "the sweep held entries across the walk");
    assert!(
        !names(&*probe.inner, &sst)
            .iter()
            .any(|n| n.contains(".removed-")),
        "a table renamed aside survived the sweep"
    );
}

#[test]
fn recovery_holds_the_live_logs_and_the_sweeps_hold_nothing() {
    for probe in [WalkProbe::std(), WalkProbe::mem()] {
        let dir = TempDir::new().unwrap();
        let root = if probe.inner.capabilities().file_lock {
            dir.path().to_path_buf()
        } else {
            PathBuf::from("/db")
        };
        let db = Db::open(&root, options(&probe)).unwrap();
        put_some(&db, 300);
        drop(db);
        let wal = root.join("wal");
        let logs = names(&*probe.inner, &wal)
            .iter()
            .filter(|n| n.ends_with(".log"))
            .count();
        // Staging files a crash left before a rename, which the open
        // removes; names no log has, which every walk passes over; and
        // retired logs, which recovery passes over and the open removes.
        for k in 0..MANY {
            touch(&*probe.inner, &wal.join(format!("wal_{k:06}.tmp")));
            touch(&*probe.inner, &wal.join(format!("note-{k}")));
        }
        for name in retired_log_names() {
            touch(&*probe.inner, &wal.join(name));
        }

        probe.reset();
        let db = Db::open(&root, options(&probe)).unwrap();
        holds_some(&db, 300);
        let (pulled, carried) = probe.walks_of(&wal);
        assert!(
            probe.walk_count(&wal) >= 2 && pulled >= 2 * MANY,
            "{:?}: the walks of wal/ handed out {pulled} entries",
            probe.inner
        );
        // Recovery sorts the live logs after its walk, and nothing else is
        // held across one: the staging files go as the walk reaches them.
        assert!(
            carried <= logs,
            "{:?}: a walk of wal/ held {carried} entries; {logs} logs exist",
            probe.inner
        );
        let left = names(&*probe.inner, &wal);
        assert!(
            !left.iter().any(|n| n.ends_with(".tmp")),
            "a staging file survived the open"
        );
        let retired: HashSet<String> = retired_log_names().collect();
        assert!(
            !left.iter().any(|n| retired.contains(n)),
            "a retired log survived the open"
        );
        drop(db);
    }
}

#[test]
fn drop_all_removes_each_old_log_as_its_walk_reaches_it() {
    let dir = TempDir::new().unwrap();
    let probe = WalkProbe::std();
    let db = Db::open(dir.path(), options(&probe).max_write_buffer_number(4)).unwrap();
    put_some(&db, 400);
    let wal = dir.path().join("wal");
    for k in 0..MANY {
        touch(&*probe.inner, &wal.join(format!("note-{k}")));
    }
    for name in retired_log_names() {
        touch(&*probe.inner, &wal.join(name));
    }
    probe.reset();
    db.drop_all().unwrap();
    let (pulled, carried) = probe.walks_of(&wal);
    assert!(pulled >= MANY, "drop_all walked {pulled} entries");
    assert_eq!(carried, 0, "drop_all held entries across its walk of wal/");
    let left = names(&*probe.inner, &wal)
        .into_iter()
        .filter(|n| n.ends_with(".log"))
        .count();
    assert_eq!(left, 1, "only the new log stays");
}

#[test]
fn a_checkpoint_stops_at_the_first_entry_of_an_occupied_target() {
    let dir = TempDir::new().unwrap();
    let target = TempDir::new().unwrap();
    let probe = WalkProbe::std();
    let db = Db::open(dir.path(), options(&probe)).unwrap();
    put_some(&db, 10);
    let target_sst = target.path().join("sst");
    probe.inner.create_dir_all(&target_sst).unwrap();
    for k in 0..MANY {
        touch(&*probe.inner, &target_sst.join(format!("{k:06}.sst")));
    }
    probe.reset();
    let refused = db.checkpoint(target.path()).wait();
    assert!(
        matches!(&refused, Err(Error::Io(e)) if e.kind() == io::ErrorKind::AlreadyExists),
        "{refused:?}"
    );
    assert_eq!(probe.walk_count(&target_sst), 1);
    assert_eq!(
        probe.walks_of(&target_sst).0,
        1,
        "one entry is enough to know the target is occupied"
    );
}

#[test]
fn the_discarded_table_guard_names_a_bounded_few_and_counts_the_rest() {
    let dir = TempDir::new().unwrap();
    let probe = WalkProbe::std();
    let db = Db::open(dir.path(), options(&probe)).unwrap();
    put_some(&db, 10);
    db.close().unwrap();
    drop(db);
    // A table that carries data, written through the public writer.
    let fixture = dir.path().join("fixture.sst");
    let mut writer = SstFileWriter::create(&fixture, &Options::default()).unwrap();
    writer.put(b"a", b"b").unwrap();
    writer.finish().unwrap();
    let table = std::fs::read(&fixture).unwrap();
    let sst = dir.path().join("sst");
    const SUSPECTS: usize = 3_000;
    for k in 0..SUSPECTS {
        std::fs::write(sst.join(format!("{:06}.sst", 1_000 + k)), &table).unwrap();
        // Zero-length tables a crash inside a flush leaves: passed over.
        std::fs::write(sst.join(format!("{:06}.sst", 10_000 + k)), b"").unwrap();
    }
    std::fs::write(dir.path().join("MANIFEST"), b"").unwrap();
    // Every table that carries data counts, the database's own included,
    // and the message names the first eight by path.
    let mut carrying: Vec<String> = std::fs::read_dir(&sst)
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.metadata().unwrap().len() > 0)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    carrying.sort();
    assert!(carrying.len() >= SUSPECTS);

    probe.reset();
    let err = match Db::open(dir.path(), options(&probe)) {
        Ok(_) => panic!("opened over tables a headerless manifest cannot account for"),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains(&format!("yet {} table file(s)", carrying.len())),
        "{err}"
    );
    assert!(
        err.contains(&format!("and {} more not named here", carrying.len() - 8)),
        "{err}"
    );
    // The first by path, as a full sort would name them, and no other.
    for (i, name) in carrying.iter().enumerate() {
        assert_eq!(err.contains(name.as_str()), i < 8, "{name}: {err}");
    }
    let (pulled, carried) = probe.walks_of(&sst);
    assert!(pulled >= 2 * SUSPECTS, "the guard walked {pulled} entries");
    assert_eq!(carried, 0, "the guard held tables across its walk");
}

/// A backup repository on `probe` holding `n` backups of one small
/// database, every one a copy of the first's metadata (a plaintext backup
/// binds no id), so a large repository costs one real backup.
fn repository(probe: &WalkProbe, root: &Path, n: u64) -> BackupEngine {
    let db = Db::open(root.join("db"), options(probe)).unwrap();
    put_some(&db, 200);
    db.flush().unwrap();
    let mut engine = BackupEngine::open_with_env(root.join("backups"), probe.env()).unwrap();
    assert_eq!(engine.create_backup(&db).unwrap(), BackupId(1));
    let meta = root.join("backups").join("meta");
    let first = probe.inner.read(&meta.join("000001.backup")).unwrap();
    for id in 2..=n {
        probe
            .inner
            .write(&meta.join(format!("{id:06}.backup")), &first)
            .unwrap();
    }
    engine
}

#[test]
fn listing_and_purging_hold_a_page_of_backups_never_the_repository() {
    const BACKUPS: u64 = 1_100;
    for probe in [WalkProbe::std(), WalkProbe::mem()] {
        let dir = TempDir::new().unwrap();
        let root = if probe.inner.capabilities().file_lock {
            dir.path().to_path_buf()
        } else {
            PathBuf::from("/repo")
        };
        let mut engine = repository(&probe, &root, BACKUPS);
        let meta = root.join("backups").join("meta");

        probe.reset();
        let ids: Vec<u64> = engine
            .list_backups()
            .map(|b| b.unwrap().unwrap().id.0)
            .collect();
        assert_eq!(ids, (1..=BACKUPS).collect::<Vec<_>>());
        let (pulled, carried) = probe.walks_of(&meta);
        assert!(pulled >= 2 * BACKUPS as usize, "{pulled}");
        assert!(
            carried < BACKUPS as usize,
            "{:?}: a listing walk held every backup ({carried})",
            probe.inner
        );

        // A new backup's id comes from one walk that keeps the largest.
        probe.reset();
        let db = Db::open(root.join("db"), options(&probe)).unwrap();
        assert_eq!(engine.create_backup(&db).unwrap(), BackupId(BACKUPS + 1));
        drop(db);

        // Deleting one backup reads every other listing as its walk
        // reaches it.
        probe.reset();
        engine.delete_backup(BackupId(1)).unwrap();
        assert_eq!(probe.walks_of(&meta).1, 0, "the collection held listings");

        // Ids 2 to BACKUPS + 1 are left; the purge keeps all but the five
        // oldest.
        probe.reset();
        engine.purge_old_backups(BACKUPS as usize - 5).unwrap();
        let left: Vec<u64> = engine
            .list_backups()
            .map(|b| b.unwrap().unwrap().id.0)
            .collect();
        assert_eq!(left, (7..=BACKUPS + 1).collect::<Vec<_>>());
        let (_, carried) = probe.walks_of(&meta);
        assert!(
            carried < BACKUPS as usize,
            "{:?}: the purge held every backup ({carried})",
            probe.inner
        );
        // The tables the newest backups list are still there to restore.
        let target = root.join("restored");
        engine
            .restore(BackupId(BACKUPS + 1), &target, None)
            .unwrap();
        holds_some(&Db::open(&target, options(&probe)).unwrap(), 200);
    }
}
