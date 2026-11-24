//! `RateLimiter::request`, the trait's one method: asked on a compaction
//! worker for the bytes of each table it wrote, at low priority, and never
//! on a caller's thread.

use std::sync::{Arc, Mutex};

use regolith::{
    CompactionOutcome, Db, EventListener, Options, Priority, RateLimiter, TableFileCreationInfo,
};

fn this_thread() -> String {
    std::thread::current().name().unwrap_or("").to_owned()
}

fn is_worker(thread: &str) -> bool {
    thread.starts_with("regolith-compaction-")
}

/// Records every request: the bytes, the priority, and the thread asking.
#[derive(Default)]
struct Recording(Mutex<Vec<(u64, Priority, String)>>);

impl RateLimiter for Recording {
    fn request(&self, bytes: u64, pri: Priority) {
        self.0.lock().unwrap().push((bytes, pri, this_thread()));
    }
}

/// Records every table a flush or a compaction wrote: its size, and the
/// thread that wrote it, which is the thread a listener is told on.
#[derive(Default)]
struct Tables(Mutex<Vec<(u64, String)>>);

impl EventListener for Tables {
    fn on_table_file_created(&self, info: &TableFileCreationInfo) {
        self.0.lock().unwrap().push((info.file_size, this_thread()));
    }
}

fn open(dir: &tempfile::TempDir, options: Options) -> (Db, Arc<Recording>, Arc<Tables>) {
    let (limiter, tables) = (Arc::new(Recording::default()), Arc::new(Tables::default()));
    let options = options
        .rate_limiter(Some(limiter.clone()))
        .add_listener(tables.clone());
    (Db::open(dir.path(), options).unwrap(), limiter, tables)
}

#[test]
fn request_is_asked_on_a_worker_for_the_bytes_of_each_table_it_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let (db, limiter, tables) = open(&dir, Options::default());
    db.put(b"a", b"1").unwrap();
    db.put(b"c", b"1").unwrap();
    db.flush().unwrap();
    // The job runs on the worker: it flushes what the memtable holds, then
    // merges the two overlapping tables.
    db.put(b"b", b"2").unwrap();
    db.compact_range(None, None).wait().unwrap();

    let asked = limiter.0.lock().unwrap().clone();
    for (_, pri, thread) in &asked {
        assert_eq!(*pri, Priority::Low);
        assert!(is_worker(thread), "asked on {thread:?}");
    }
    // One request for each table a worker wrote, for the table's size on
    // disk, and none for a table the caller's own thread wrote.
    let mut bytes_asked: Vec<u64> = asked.iter().map(|(bytes, ..)| *bytes).collect();
    let mut on_workers: Vec<u64> = tables
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, thread)| is_worker(thread))
        .map(|(bytes, _)| *bytes)
        .collect();
    bytes_asked.sort_unstable();
    on_workers.sort_unstable();
    assert_eq!(bytes_asked, on_workers);
    assert!(
        bytes_asked.len() >= 2,
        "the job's flush and its merge: {asked:?}"
    );
}

/// With no worker every flush and every compaction runs on a caller's
/// thread, which is the caller's own I/O: nothing is asked.
#[test]
fn request_is_never_asked_on_a_callers_thread() {
    let dir = tempfile::tempdir().unwrap();
    let options = Options::default()
        .max_background_compactions(0)
        .inline_compaction(false)
        .l0_compaction_trigger(2);
    let (db, limiter, tables) = open(&dir, options);
    db.put(b"a", b"1").unwrap();
    db.put(b"c", b"1").unwrap();
    db.flush().unwrap();
    db.put(b"b", b"2").unwrap();
    db.flush().unwrap();
    assert_eq!(db.compact_step().unwrap(), CompactionOutcome::DidWork);

    assert_eq!(tables.0.lock().unwrap().len(), 3, "two flushes and a merge");
    assert!(limiter.0.lock().unwrap().is_empty());
}
