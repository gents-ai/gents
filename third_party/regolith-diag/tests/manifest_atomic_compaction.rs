//! Compaction must preserve every durable key at every byte cut of its
//! MANIFEST append, including cuts between complete remove/add records.

#![cfg(not(target_arch = "wasm32"))]

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use regolith::env::{
    Capabilities, Env, FileLock, FileMeta, JoinHandle, MemEnv, ReadDir, ReadFile, WriteFile,
    WriteMode,
};
use regolith::{CompactionStyle, Db, DurabilityMode, Options, UniversalCompactionOptions};

const DB_PATH: &str = "manifest-atomic-compaction";
const INPUT_FILES: usize = 4;
const KEYS_PER_FILE: usize = 10;

#[derive(Debug)]
struct CrashImage {
    files: Vec<(PathBuf, Vec<u8>)>,
    manifest_path: PathBuf,
    append: Vec<u8>,
}

fn collect_files(env: &MemEnv, dir: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
    for entry in env.read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        if entry.is_dir {
            collect_files(env, &entry.path, files);
        } else {
            files.push((entry.path.clone(), env.read(&entry.path).unwrap()));
        }
    }
}

/// Capture the filesystem before a MANIFEST write can return to the engine.
/// Each recovery gets its own copy, so error handling and destructors in the
/// original database cannot finish the append or remove files from the image.
/// Completed writes are treated as persisted; only the armed append is cut.
#[derive(Debug, Default)]
struct CaptureEnv {
    inner: MemEnv,
    armed: Arc<AtomicBool>,
    captured: Arc<Mutex<Option<CrashImage>>>,
}

struct CaptureFile {
    inner: Box<dyn WriteFile>,
    env: MemEnv,
    path: PathBuf,
    armed: Arc<AtomicBool>,
    captured: Arc<Mutex<Option<CrashImage>>>,
}

impl WriteFile for CaptureFile {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        // Ignore file-id reservations; the replacement starts with RemoveFile.
        if self.armed.load(Ordering::SeqCst) && buf.get(4) == Some(&2) {
            let mut captured = self.captured.lock().unwrap();
            if captured.is_none() {
                let mut files = Vec::new();
                collect_files(&self.env, Path::new(DB_PATH), &mut files);
                *captured = Some(CrashImage {
                    files,
                    manifest_path: self.path.clone(),
                    append: buf.to_vec(),
                });
            }
            return Err(io::Error::other("injected MANIFEST write failure"));
        }
        self.inner.write_all(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    fn sync_all(&mut self) -> io::Result<()> {
        self.inner.sync_all()
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }

    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }
}

impl Env for CaptureEnv {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir_all(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.inner.read_dir(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.inner.open_read(path)
    }

    fn open_log(
        &self,
        path: &Path,
        mode: WriteMode,
    ) -> io::Result<Box<dyn regolith::env::LogFile>> {
        self.inner.open_log(path, mode)
    }
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        let inner = self.inner.open_write(path, mode)?;
        if path.file_name().is_some_and(|name| name == "MANIFEST") {
            Ok(Box::new(CaptureFile {
                inner,
                env: self.inner.clone(),
                path: path.to_path_buf(),
                armed: Arc::clone(&self.armed),
                captured: Arc::clone(&self.captured),
            }))
        } else {
            Ok(inner)
        }
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
        let mut capabilities = self.inner.capabilities();
        capabilities.durable_sync = true;
        capabilities
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

fn options(env: Arc<dyn Env>, style: CompactionStyle) -> Options {
    Options::default()
        .env(env)
        .durability(DurabilityMode::Immediate)
        .max_background_compactions(0)
        .compaction_style(style)
        .write_buffer_size(1024 * 1024)
        .l0_compaction_trigger(1_000_000)
        .level0_slowdown_writes_trigger(0)
        .level0_stop_writes_trigger(0)
        .universal_compaction_options(UniversalCompactionOptions {
            min_merge_width: 100,
            max_merge_width: 100,
            max_size_amplification_percent: u32::MAX,
            ..UniversalCompactionOptions::default()
        })
}

fn capture_compaction(style: CompactionStyle, expected: &[(Vec<u8>, Vec<u8>)]) -> CrashImage {
    let env = Arc::new(CaptureEnv::default());
    {
        let db = Db::open(DB_PATH, options(env.clone(), style)).unwrap();
        for entries in expected.chunks(KEYS_PER_FILE) {
            for (key, value) in entries {
                regolith::through_stalls(|| db.put(key, value)).unwrap();
            }
            db.flush().unwrap();
        }
        assert_eq!(
            db.get_int_property("regolith.num-files-at-level0"),
            Some(INPUT_FILES as u64),
            "the durable input must contain four separate SSTables",
        );
        db.close().unwrap();
    }

    let db = Db::open(DB_PATH, options(env.clone(), style)).unwrap();
    env.armed.store(true, Ordering::SeqCst);
    assert!(db.compact_range(None, None).wait().is_err());
    let image = env.captured.lock().unwrap().take().unwrap();
    assert!(!image.append.is_empty());
    assert_eq!(
        image
            .files
            .iter()
            .filter(|(path, _)| path.extension().is_some_and(|ext| ext == "sst"))
            .count(),
        INPUT_FILES + 1,
        "capture must include the compaction output and all original inputs",
    );
    image
}

fn sweep_compaction_append(style: CompactionStyle) {
    let expected: Vec<_> = (0..INPUT_FILES * KEYS_PER_FILE)
        .map(|i| {
            (
                format!("key-{i:04}").into_bytes(),
                format!("value-{i:04}").into_bytes(),
            )
        })
        .collect();
    let image = capture_compaction(style, &expected);

    // Include no append and a complete append as controls.
    for cut in 0..=image.append.len() {
        let context = format!("{style:?}: {cut}/{} MANIFEST bytes", image.append.len());
        check_recovery(
            &image,
            &image.append[..cut],
            style,
            &expected,
            cut == image.append.len(),
            &context,
        );
    }

    // A complete frame with damaged bytes must not apply a prefix either.
    for byte in 0..image.append.len() {
        let mut damaged = image.append.clone();
        damaged[byte] ^= 1;
        check_recovery(
            &image,
            &damaged,
            style,
            &expected,
            false,
            &format!("{style:?}: damaged MANIFEST byte {byte}"),
        );
    }
}

fn check_recovery(
    image: &CrashImage,
    append: &[u8],
    style: CompactionStyle,
    expected: &[(Vec<u8>, Vec<u8>)],
    installed: bool,
    context: &str,
) {
    let env = Arc::new(CaptureEnv::default());
    for (path, contents) in &image.files {
        env.create_dir_all(path.parent().unwrap()).unwrap();
        env.write(path, contents).unwrap();
    }
    let mut manifest = env
        .open_write(&image.manifest_path, WriteMode::Append)
        .unwrap();
    manifest.write_all(append).unwrap();
    drop(manifest);

    let db = Db::open(DB_PATH, options(env.clone(), style))
        .unwrap_or_else(|error| panic!("{context}: recovery failed: {error}"));
    for (key, value) in expected {
        assert_eq!(
            db.get(key).unwrap().as_ref(),
            Some(value),
            "{context}: lost durable key {}",
            String::from_utf8_lossy(key),
        );
    }
    assert_eq!(db.scan(None, None).unwrap(), expected, "{context}");
    let (l0, l1) = match (installed, style) {
        (false, _) => (INPUT_FILES as u64, 0),
        (true, CompactionStyle::Level) => (0, 1),
        (true, CompactionStyle::Universal) => (1, 0),
        _ => unreachable!(),
    };
    assert_eq!(
        db.get_int_property("regolith.num-files-at-level0"),
        Some(l0),
        "{context}"
    );
    assert_eq!(
        db.get_int_property("regolith.num-files-at-level1"),
        Some(l1),
        "{context}"
    );

    // Recovery must trim the incomplete batch before later metadata appends.
    regolith::through_stalls(|| db.put(b"after-recovery", b"survives")).unwrap();
    db.flush().unwrap();
    db.close().unwrap();
    drop(db);
    let db = Db::open(DB_PATH, options(env, style)).unwrap();
    assert_eq!(
        db.get(b"after-recovery").unwrap().as_deref(),
        Some(b"survives".as_slice()),
        "{context}"
    );
    let mut complete = vec![(b"after-recovery".to_vec(), b"survives".to_vec())];
    complete.extend_from_slice(expected);
    assert_eq!(db.scan(None, None).unwrap(), complete, "{context}");
}

#[test]
fn leveled_compaction_preserves_durable_data_at_every_manifest_byte_cut() {
    sweep_compaction_append(CompactionStyle::Level);
}

#[test]
fn universal_compaction_preserves_durable_data_at_every_manifest_byte_cut() {
    sweep_compaction_append(CompactionStyle::Universal);
}
