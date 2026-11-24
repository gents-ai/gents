//! The threads a database starts (plan 3.13 and 4.5): as many compaction
//! workers as `Options::max_background_compactions` asks for, started through
//! its `Env` when it opens, and no other thread at any time, unless the caller
//! asks for the log writeback lane (`Options::log_writeback`), which is one
//! more on Linux and Apple with a worker. A caller that runs a pinned worker
//! pool sizes it around that number, so the number is a promise.
//!
//! Each configuration is opened, put through writes, a flush, a compaction,
//! reads, a snapshot, transactions with a blocking and a ticketed commit, an
//! allocation and a column family, and closed. Three things are counted:
//!
//! - the threads the database asked its `Env` for, by name;
//! - the threads that reached the `Env` at all, which must be the caller's
//!   own and those workers, so a thread started any other way shows up as
//!   soon as it touches a file;
//! - on Linux, the threads the process has, read from the kernel: the workers
//!   more than before the open, at every point of the life, and none more
//!   once the database is closed.
//!
//! The one thing this cannot see is a thread started around the `Env` that
//! touches no file and is gone before the next count.
//!
//! All of it is one test, so no sibling in this binary starts a thread
//! between two counts.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads and the filesystem.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::thread;

use common::keys::Keys;
use common::watched_env::{Watch, WatchedEnv};
use regolith::env::Env;
use regolith::prelude::*;
use regolith::{DurabilityMode, IoBudget, MemEnv, TransactionDb};

/// The threads the process has now, where the host can say.
fn process_threads() -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        Some(std::fs::read_dir("/proc/self/task").ok()?.count())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Which handle opens the database.
#[derive(Clone, Copy, Debug)]
enum Handle {
    Plain,
    Optimistic,
    Pessimistic,
}

/// An open database of any kind.
enum Opened {
    Plain(Db),
    Optimistic(OptimisticTransactionDb),
    Pessimistic(TransactionDb),
}

impl Opened {
    fn open(handle: Handle, path: &Path, options: Options) -> Self {
        match handle {
            Handle::Plain => Self::Plain(Db::open(path, options).unwrap()),
            Handle::Optimistic => {
                Self::Optimistic(OptimisticTransactionDb::open(path, options).unwrap())
            }
            Handle::Pessimistic => Self::Pessimistic(TransactionDb::open(path, options).unwrap()),
        }
    }

    fn db(&self) -> &Db {
        match self {
            Self::Plain(db) => db,
            Self::Optimistic(db) => db.db(),
            Self::Pessimistic(db) => db.db(),
        }
    }

    fn begin(&self, options: &TxnOptions) -> Option<Transaction> {
        match self {
            Self::Plain(_) => None,
            Self::Optimistic(db) => Some(db.begin(options)),
            Self::Pessimistic(db) => Some(db.begin(options)),
        }
    }
}

/// Keys the life of a database writes, twice over.
const KEYS: u32 = 64;

/// Everything a caller does to a database in its life, on the calling thread.
fn use_it(opened: &Opened) {
    let db = opened.db();
    // Few writes, since one row syncs every one of them; enough bytes to
    // overflow the smallest write buffer of the rows.
    let value = vec![7u8; 256];
    for i in 0..KEYS {
        regolith::through_stalls(|| db.put(format!("key/{i:05}").as_bytes(), &value)).unwrap();
    }
    db.flush().unwrap();
    for i in 0..KEYS {
        regolith::through_stalls(|| db.put(format!("key/{i:05}").as_bytes(), b"again")).unwrap();
    }
    db.flush().unwrap();
    db.compact_range(None, None).wait().unwrap();

    let snapshot = db.snapshot();
    assert_eq!(
        snapshot.get(b"key/00007").unwrap().as_deref(),
        Some(&b"again"[..])
    );
    assert_eq!(
        db.scan(Some(b"key/"), Some(b"key0")).unwrap().len(),
        KEYS as usize
    );
    drop(snapshot);

    assert_eq!(db.allocate(b"ids", 3).unwrap(), 1..4);
    let family = db.create_column_family("other").unwrap();
    db.put_cf(&family, b"k", b"v").unwrap();

    if let Some(txn) = opened.begin(&TxnOptions::new()) {
        assert!(txn.get(b"key/00001").unwrap().is_some());
        txn.put(b"txn/blocking", b"v").unwrap();
        txn.commit().unwrap();
    }
    let mut queue = db.io_queue();
    if let Some(txn) = opened.begin(&TxnOptions::new().io_queue(queue.id())) {
        txn.put(b"txn/ticketed", b"v").unwrap();
        let ticket = txn.commit_nowait();
        queue.poll(IoBudget::ALL);
        queue.block_on(ticket).unwrap();
    }
}

/// One configuration: its name, the handle that opens it, its options over a
/// given `Env`, and the threads the plan says it starts.
struct Row {
    name: &'static str,
    handle: Handle,
    options: fn(Arc<dyn Env>) -> Options,
    /// Whether it runs on an in-memory env, which starts no thread at all,
    /// instead of the host's files.
    in_memory: bool,
    workers: usize,
    /// Whether it asks for the log writeback lane.
    lane: bool,
}

const fn on_files(
    name: &'static str,
    handle: Handle,
    options: fn(Arc<dyn Env>) -> Options,
    workers: usize,
) -> Row {
    Row {
        name,
        handle,
        options,
        in_memory: false,
        workers,
        lane: false,
    }
}

/// The transaction layers add no thread, so the default options run through
/// every handle and each other configuration through one.
const ROWS: [Row; 14] = [
    on_files(
        "the default options",
        Handle::Plain,
        |env| Options::default().env(env),
        1,
    ),
    on_files(
        "the default options",
        Handle::Optimistic,
        |env| Options::default().env(env),
        1,
    ),
    on_files(
        "the default options",
        Handle::Pessimistic,
        |env| Options::default().env(env),
        1,
    ),
    on_files(
        "no background compaction",
        Handle::Plain,
        |env| Options::default().env(env).max_background_compactions(0),
        0,
    ),
    on_files(
        "no background compaction, compaction inline",
        Handle::Optimistic,
        |env| {
            Options::default()
                .env(env)
                .max_background_compactions(0)
                .inline_compaction(true)
        },
        0,
    ),
    on_files(
        "three compaction workers",
        Handle::Plain,
        |env| Options::default().env(env).max_background_compactions(3),
        3,
    ),
    on_files(
        "the embedded profile",
        Handle::Plain,
        |env| Options::embedded().env(env),
        0,
    ),
    on_files(
        "the wasm profile",
        Handle::Optimistic,
        |env| Options::wasm().env(env),
        0,
    ),
    Row {
        name: "the memory profile",
        handle: Handle::Plain,
        options: |env| Options::memory().env(env),
        in_memory: true,
        workers: 0,
        lane: false,
    },
    on_files(
        "a sync on every commit",
        Handle::Pessimistic,
        |env| {
            Options::default()
                .env(env)
                .durability(DurabilityMode::Immediate)
        },
        1,
    ),
    on_files(
        "encryption at rest",
        Handle::Plain,
        |env| Options::default().env(env).key_provider(Keys::new(&[1])),
        1,
    ),
    on_files(
        "a small write buffer, so memtables rotate by themselves",
        Handle::Optimistic,
        |env| {
            Options::default()
                .env(env)
                .write_buffer_size(8 * 1024)
                .l0_compaction_trigger(2)
        },
        1,
    ),
    Row {
        name: "the log writeback lane, asked for",
        handle: Handle::Plain,
        options: |env| Options::default().env(env).log_writeback(true),
        in_memory: false,
        workers: 1,
        lane: true,
    },
    // With no worker the lane is not started, asked for or not.
    Row {
        name: "the log writeback lane, asked for with no worker",
        handle: Handle::Plain,
        options: |env| {
            Options::default()
                .env(env)
                .max_background_compactions(0)
                .log_writeback(true)
        },
        in_memory: false,
        workers: 0,
        lane: true,
    },
];

/// Whether `row` starts the log writeback lane: asked for, with a worker,
/// on Linux or Apple, the hosts with a way to start the writeback.
fn lane_runs(row: &Row) -> bool {
    row.lane && row.workers > 0 && cfg!(any(target_os = "linux", target_vendor = "apple"))
}

/// The names of the threads `row` starts: its workers, then the lane.
fn thread_names(row: &Row) -> Vec<String> {
    let mut names: Vec<String> = (0..row.workers)
        .map(|index| format!("regolith-compaction-{index}"))
        .collect();
    if lane_runs(row) {
        names.push("regolith-log-writeback".to_string());
    }
    names
}

fn watched(row: &Row) -> (Arc<dyn Env>, Arc<Watch>) {
    if row.in_memory {
        WatchedEnv::over(Arc::new(MemEnv::new()))
    } else {
        WatchedEnv::on_host()
    }
}

#[test]
fn a_database_starts_the_compaction_workers_it_was_asked_for_and_no_other_thread() {
    let me = [thread::current().id()];
    for row in &ROWS {
        let case = format!("{} through {:?}", row.name, row.handle);
        let dir = tempfile::tempdir().unwrap();
        let (env, watch) = watched(row);
        let before = process_threads();
        let expected = before.map(|before| before + thread_names(row).len());

        let opened = Opened::open(row.handle, dir.path(), (row.options)(Arc::clone(&env)));
        assert_eq!(
            watch.started(),
            thread_names(row),
            "{case}: threads asked of the env at open"
        );
        assert_eq!(process_threads(), expected, "{case}: threads after open");

        use_it(&opened);
        assert_eq!(
            watch.started(),
            thread_names(row),
            "{case}: threads asked of the env in the database's whole life"
        );
        assert_eq!(process_threads(), expected, "{case}: threads after use");
        assert_eq!(
            watch.strangers(&me),
            [],
            "{case}: a thread that is neither the caller's nor a worker reached the files"
        );

        opened.db().close().unwrap();
        drop(opened);
        // A joined thread leaves the kernel's list a moment after the join
        // returns.
        common::eventually(&format!("{case}: every worker is gone after close"), || {
            process_threads() == before
        });
        assert_eq!(watch.started(), thread_names(row), "{case}");
        assert_eq!(watch.strangers(&me), [], "{case}: after close");

        // A read-only handle on what was written starts no thread, whatever
        // its options ask for.
        if !row.in_memory {
            let (env, watch) = watched(row);
            let reader = Db::open_read_only(dir.path(), (row.options)(env)).unwrap();
            assert!(reader.get(b"key/00007").unwrap().is_some());
            assert_eq!(watch.started(), Vec::<String>::new(), "{case}: read-only");
            assert_eq!(process_threads(), before, "{case}: read-only");
            assert_eq!(watch.strangers(&me), [], "{case}: read-only");
        }
    }
}
