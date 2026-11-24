//! Handle lifecycle: open, close, reopen, and every misuse in between.
//!
//! The on-disk format is tested elsewhere. What is tested here is the
//! handle contract: a directory is a durable accumulator across handle
//! lifetimes; reopening it under different [`Options`] is an upgrade
//! path and not a data-loss event; exactly one read-write handle may
//! hold it; and every misuse (use after `close`, close twice, drop with
//! readers alive, a hostile path, a file from an unknown format
//! version) yields a clear `Err`, never a panic and never a wrong
//! answer.
//!
//! Hostile-path cases already covered by `tests/open_and_corruption.rs`
//! (a regular file, an unwritable parent, an unsearchable parent, a
//! required subdirectory replaced by a file) are not repeated here.
//!
//! The upgrade-path half - reopening a populated database under changed
//! [`Options`] - lives in the [`options`] submodule.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::thread;

use regolith::{
    Db, DurabilityMode, Error, IngestOptions, Options, Range, SstFileWriter, WriteBatch,
    WriteOptions,
};
use tempfile::TempDir;

use common::fault::{file_len, find_ssts, first_sst, overwrite_range};

/// Reopening under changed [`Options`] lives in its own module. The
/// `#[path]` attribute is what keeps `tests/lifecycle/` from being
/// picked up as a second test binary.
#[path = "lifecycle/options.rs"]
mod options;

#[test]
fn crash_child() {
    common::fault::child_entrypoint(common::fault::builtin_workload);
}

// ---- fixtures ----

/// A 4 KiB write buffer so a few hundred keys really do flush, and the
/// reopen paths exercise files rather than one memtable.
fn opts() -> Options {
    Options::default().write_buffer_size(4 * 1024)
}

fn key(i: usize) -> Vec<u8> {
    format!("key_{i:06}").into_bytes()
}

fn value(i: usize) -> Vec<u8> {
    format!("val_{i:06}").into_bytes()
}

fn write_range(db: &Db, from: usize, to: usize) {
    for i in from..to {
        regolith::through_stalls(|| db.put(&key(i), &value(i))).unwrap();
    }
}

fn assert_range_present(db: &Db, from: usize, to: usize, ctx: &str) {
    for i in from..to {
        assert_eq!(
            db.get(&key(i)).unwrap(),
            Some(value(i)),
            "{ctx}: key {i} is missing or wrong"
        );
    }
    assert_eq!(
        db.scan(None, None).unwrap().len(),
        to - from,
        "{ctx}: the database holds keys it was never given, or lost some"
    );
}

fn assert_closed<T: std::fmt::Debug>(what: &str, result: regolith::Result<T>) {
    match result {
        Err(Error::Closed) => {}
        other => panic!("{what}: expected Error::Closed after close, got {other:?}"),
    }
}

fn assert_read_only<T: std::fmt::Debug>(what: &str, result: regolith::Result<T>) {
    match result {
        Err(Error::ReadOnly) => {}
        other => panic!("{what}: expected Error::ReadOnly, got {other:?}"),
    }
}

/// Call every mutating entry point on `db` and hand each result to
/// `expect`. One list, so the read-only sweep and the closed sweep
/// cannot drift apart as the write surface grows.
fn sweep_every_mutation(db: &Db, scratch: &Path, expect: fn(&str, regolith::Result<()>)) {
    let cf = db.default_cf();
    let wo = WriteOptions::new();
    // Every batch here carries an op: an *empty* batch takes a
    // different path, which is what the empty-batch test below is for.
    let batch = || {
        let mut b = WriteBatch::new();
        b.put(b"k", b"v");
        b
    };
    expect("put", db.put(b"k", b"v"));
    expect("put_opt", db.put_opt(&WriteOptions::sync(), b"k", b"v"));
    expect("delete", db.delete(&key(0)));
    expect("delete_opt", db.delete_opt(&wo, &key(0)));
    expect("merge", db.merge(b"k", b"op"));
    expect("merge_opt", db.merge_opt(&wo, b"k", b"op"));
    expect("delete_range", db.delete_range(&key(0), &key(9)));
    expect("write", db.write(batch()));
    expect("write_opt", db.write_opt(&wo, batch()));
    let sync = DurabilityMode::Immediate;
    expect(
        "write_with_durability",
        db.write_with_durability(batch(), sync),
    );
    expect("compact_range", db.compact_range(None, None).wait());
    expect("drop_all", db.drop_all());
    expect("checkpoint", db.checkpoint(scratch.join("cp")).wait());
    expect(
        "create_column_family",
        db.create_column_family("late").map(drop),
    );
    expect("drop_column_family", db.drop_column_family(cf.clone()));
    expect("put_cf", db.put_cf(&cf, b"k", b"v"));
    expect("delete_cf", db.delete_cf(&cf, b"k"));
    expect("merge_cf", db.merge_cf(&cf, b"k", b"op"));
    expect("delete_range_cf", db.delete_range_cf(&cf, &key(0), &key(9)));
    let ingest = db.ingest_external_files(&[], IngestOptions::default());
    expect("ingest_external_files", ingest.wait());
}

// ---- open, close, reopen ----

/// Property: a database directory is a durable accumulator. After the
/// nth open-write-shutdown cycle the key count is exactly
/// `n * PER_CYCLE` - nothing lost, resurrected, or duplicated. Half the
/// cycles end in `close()` and half just drop the handle, so both
/// shutdown paths carry the same guarantee. Catches a recovery path
/// that replays only the newest WAL, a manifest rewrite that forgets
/// older levels, a `close` that drops the memtable tail, and a `Drop`
/// that skips the flush `close` would have done.
#[test]
fn a_hundred_open_close_reopen_cycles_accumulate_every_write() {
    const CYCLES: usize = 100;
    const PER_CYCLE: usize = 10;

    let dir = TempDir::new().unwrap();
    for cycle in 0..CYCLES {
        let db = Db::open(dir.path(), opts()).unwrap();
        let already = cycle * PER_CYCLE;
        assert_eq!(
            db.scan(None, None).unwrap().len(),
            already,
            "cycle {cycle}: reopen did not recover exactly the {already} keys written so far"
        );
        write_range(&db, already, already + PER_CYCLE);
        if cycle % 2 == 0 {
            db.close().unwrap();
        }
        drop(db);
    }

    let db = Db::open(dir.path(), opts()).unwrap();
    assert_range_present(&db, 0, CYCLES * PER_CYCLE, "final reopen");
}

/// Property: `close` is idempotent and dropping a handle without
/// closing it is not a data-loss event. Catches a `close` that assumes
/// it runs once (double free, double manifest rewrite) and a `Drop` that
/// abandons unflushed state.
#[test]
fn close_is_idempotent_and_a_dropped_handle_persists_the_same_data() {
    let closed = TempDir::new().unwrap();
    let db = Db::open(closed.path(), opts()).unwrap();
    write_range(&db, 0, 300);
    db.close().unwrap();
    db.close().unwrap();
    db.close().unwrap();
    drop(db);

    let dropped = TempDir::new().unwrap();
    let db = Db::open(dropped.path(), opts()).unwrap();
    write_range(&db, 0, 300);
    drop(db);

    for (label, dir) in [("closed", &closed), ("dropped", &dropped)] {
        let db = Db::open(dir.path(), opts()).unwrap();
        assert_range_present(&db, 0, 300, label);
        db.close().unwrap();
    }
}

// ---- one writer at a time ----

/// Property: a directory admits exactly one read-write handle. The
/// second open fails saying the directory is locked, the live handle is
/// undisturbed, and once the holder is gone it opens cleanly. Catches a
/// missing lock, which would let two engines write one manifest.
#[test]
fn a_second_read_write_open_of_the_same_directory_is_refused() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts()).unwrap();
    write_range(&db, 0, 50);

    let err = Db::open(dir.path(), opts()).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("locked"),
        "a second read-write open must name the lock conflict, got: {msg}"
    );

    write_range(&db, 50, 100);
    assert_range_present(&db, 0, 100, "holder after a refused second open");
    db.close().unwrap();
    drop(db);

    let db = Db::open(dir.path(), opts()).unwrap();
    assert_range_present(&db, 0, 100, "after the holder released the lock");
}

/// Property: the refusal holds under a race. Eight threads open the same
/// held directory at once; every one of them must be refused, and the
/// directory must be byte-for-byte usable afterwards. Catches a
/// check-then-create lock that two racing opens can both pass.
#[test]
fn eight_concurrent_opens_of_a_held_directory_are_all_refused() {
    let dir = TempDir::new().unwrap();
    let holder = Db::open(dir.path(), opts()).unwrap();
    write_range(&holder, 0, 100);

    let path: Arc<Path> = Arc::from(dir.path());
    let racers: Vec<_> = (0..8)
        .map(|_| {
            let path = Arc::clone(&path);
            thread::spawn(move || Db::open(&*path, opts()).map(|_| ()))
        })
        .collect();
    for (i, racer) in racers.into_iter().enumerate() {
        let result = racer.join().expect("racing opener panicked");
        assert!(
            result.is_err(),
            "racer {i} opened a directory that was already held read-write"
        );
    }

    assert_range_present(&holder, 0, 100, "holder after eight refused opens");
    holder.close().unwrap();
    drop(holder);

    let db = Db::open(dir.path(), opts()).unwrap();
    assert_range_present(&db, 0, 100, "after the race");
}

/// Property: read-only and read-write access to one directory exclude
/// each other in both directions with a clear lock error, while two
/// read-only handles coexist and agree. Catches a read-only path that
/// skips locking and reads a manifest a writer is mid-append on.
#[test]
fn read_only_and_read_write_handles_exclude_each_other_but_readers_share() {
    let dir = TempDir::new().unwrap();
    let writer = Db::open(dir.path(), opts()).unwrap();
    write_range(&writer, 0, 200);
    writer.close().unwrap();
    drop(writer);

    // A writer locks readers out.
    let writer = Db::open(dir.path(), opts()).unwrap();
    let err = Db::open_read_only(dir.path(), opts()).unwrap_err();
    assert!(
        err.to_string().contains("lock"),
        "a read-only open under a live writer must name the lock conflict, got: {err}"
    );
    writer.close().unwrap();
    drop(writer);

    // Two readers share the directory and agree on what they see.
    let reader_a = Db::open_read_only(dir.path(), opts()).unwrap();
    let reader_b = Db::open_read_only(dir.path(), opts()).unwrap();
    assert_range_present(&reader_a, 0, 200, "first read-only handle");
    assert_range_present(&reader_b, 0, 200, "second read-only handle");

    // And a reader locks the writer out.
    let err = Db::open(dir.path(), opts()).unwrap_err();
    assert!(
        err.to_string().contains("lock"),
        "a read-write open under a live reader must name the lock conflict, got: {err}"
    );

    drop(reader_a);
    drop(reader_b);
    let db = Db::open(dir.path(), opts()).unwrap();
    assert_range_present(&db, 0, 200, "after every reader went away");
}

/// Property: a read-only handle refuses every mutating entry point with
/// `Error::ReadOnly` while still serving reads. Catches a mutation that
/// slipped past the read-only guard and would write into a directory
/// another process believes it owns exclusively.
#[test]
fn a_read_only_handle_refuses_every_mutation_and_still_reads() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts()).unwrap();
    write_range(&db, 0, 100);
    db.close().unwrap();
    drop(db);

    let ro = Db::open_read_only(dir.path(), opts()).unwrap();
    sweep_every_mutation(&ro, dir.path(), assert_read_only);
    assert_range_present(&ro, 0, 100, "read-only handle after refusing every write");
}

/// Property: a write that happens to carry no work is still a write, so
/// a read-only handle must refuse it with `Error::ReadOnly` exactly as
/// it refuses a loaded one. `Db::open_read_only` documents "Mutating
/// APIs return [`Error::ReadOnly`]" without an exemption, and regolith
/// already takes the strict line for the sibling case: on a *closed*
/// handle these same three calls return `Error::Closed` rather than
/// `Ok`. Catches a guard-ordering slip that lets a caller probe a
/// read-only handle with an empty batch, get `Ok(())`, and conclude the
/// handle is writable.
///
/// Regression gate for the mutating-surface sweep. `write_opt`, `delete_range_opt` and
/// `delete_range_cf` each used to check `ensure_open` before their
/// no-op short-circuit and `ensure_writable` after it, so all three
/// answered `Ok(())` on a read-only handle. They now check
/// `ensure_writable` first, which subsumes `ensure_open`, so the
/// `Error::Closed` precedence on a closed handle is unchanged.
#[test]
fn a_read_only_handle_refuses_a_write_that_carries_no_work() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts()).unwrap();
    write_range(&db, 0, 10);
    db.close().unwrap();
    drop(db);

    let ro = Db::open_read_only(dir.path(), opts()).unwrap();
    let cf = ro.default_cf();
    assert_read_only("write(empty batch)", ro.write(WriteBatch::new()));
    assert_read_only(
        "write_opt(empty batch)",
        ro.write_opt(&WriteOptions::new(), WriteBatch::new()),
    );
    assert_read_only(
        "write_with_durability(empty batch)",
        ro.write_with_durability(WriteBatch::new(), DurabilityMode::Eventual),
    );
    assert_read_only("delete_range(empty range)", ro.delete_range(b"z", b"a"));
    assert_read_only(
        "delete_range_cf(empty range)",
        ro.delete_range_cf(&cf, b"z", b"a"),
    );
}

// ---- using a handle after close ----

/// Property: after `close`, every fallible entry point on `Db`, on a
/// `Snapshot` taken before the close, and on the iterators returns
/// `Error::Closed`; the infallible accessors answer rather than panic.
/// Catches the use-after-close bug where the handle keeps serving from
/// a memtable whose backing files were released, and the sloppier one
/// where it panics on an unwrapped `None`.
#[test]
fn every_fallible_method_returns_closed_after_close() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts()).unwrap();
    write_range(&db, 0, 50);
    let cf = db.default_cf();
    let snap = db.snapshot();

    db.close().unwrap();

    sweep_every_mutation(&db, dir.path(), assert_closed);
    assert_closed("get", db.get(&key(0)));
    assert_closed("multi_get", db.multi_get(&[&key(0), &key(1)]));
    assert_closed("scan", db.scan(None, None));
    assert_closed("scan_page", db.scan_page(None, None, 4));
    assert_closed("get_cf", db.get_cf(&cf, &key(0)));
    assert_closed("multi_get_cf", db.multi_get_cf(&cf, &[&key(0)]));
    assert_closed("scan_cf", db.scan_cf(&cf, None, None));
    assert_closed("scan_page_cf", db.scan_page_cf(&cf, None, None, 4));

    // Iterators created after the close report the failure through
    // `status`, which is the only channel they have.
    let mut it = db.iter();
    it.seek_to_first();
    assert_closed("iter().status()", it.status());
    assert!(!it.valid(), "a closed iterator must not claim a position");
    let mut it = db.iter_cf(&cf);
    it.seek_to_first();
    assert_closed("iter_cf().status()", it.status());
    let mut tail = db.iter_tailing();
    tail.seek_to_first();
    assert_closed("iter_tailing().status()", tail.status());

    // A snapshot taken before the close is not a way around it.
    assert_closed("snapshot.get", snap.get(&key(0)));
    assert_closed("snapshot.multi_get", snap.multi_get(&[&key(0)]));
    assert_closed("snapshot.scan", snap.scan(None, None));
    assert_closed("snapshot.scan_page", snap.scan_page(None, None, 4));
    assert_closed("snapshot.get_cf", snap.get_cf(&cf, &key(0)));
    let mut snap_it = snap.iter();
    snap_it.seek_to_first();
    assert_closed("snapshot.iter().status()", snap_it.status());

    // Infallible accessors have no error channel; the contract is that
    // they answer rather than panic.
    let _ = db.get_property("regolith.stats");
    let _ = db.get_property("regolith.sstables");
    let _ = db.get_property("regolith.levelstats");
    let _ = db.get_property("regolith.options");
    let _ = db.get_int_property("regolith.estimate-num-keys");
    let _ = db.get_approximate_sizes(&[Range::new(&key(0), &key(50))]);
    let _ = db.get_approximate_memtable_stats(Range::new(&key(0), &key(50)));
    let _ = db.list_column_families();
    let _ = db.column_family("default");
    let _ = db.snapshot();
    assert!(!format!("{db:?}").is_empty());
}

// ---- outliving the handle ----

/// Property: a `Snapshot` and an owned iterator keep the engine alive
/// after their `Db` is dropped, still serve their pinned view, and
/// release the directory lock once they too are gone. A borrowing
/// `CfIter` cannot outlive its `Db` at all, so there is no runtime case
/// for that one. Catches a use-after-free where the engine is torn down
/// on `Db::drop` while readers hold references, and a leak where the
/// lock survives the last reader.
#[test]
fn a_snapshot_and_an_owned_iterator_outlive_the_handle_that_made_them() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts()).unwrap();
    write_range(&db, 0, 300);

    let snap = db.snapshot();
    let mut owned = db.snapshot().into_owned_iter();
    let mut tail = db.iter_tailing();
    drop(db);

    assert_eq!(snap.get(&key(7)).unwrap(), Some(value(7)));
    assert_eq!(snap.scan(None, None).unwrap().len(), 300);

    owned.seek_to_first();
    let mut walked = 0usize;
    while owned.valid() {
        assert_eq!(owned.key(), Some(key(walked).as_slice()));
        walked += 1;
        owned.next();
    }
    owned.status().unwrap();
    assert_eq!(
        walked, 300,
        "the owned iterator lost entries after Db::drop"
    );

    tail.seek_to_first();
    let mut tailed = 0usize;
    while tail.valid() {
        tailed += 1;
        tail.next();
    }
    tail.status().unwrap();
    assert_eq!(
        tailed, 300,
        "the tailing iterator lost entries after Db::drop"
    );

    // The directory is still locked while a reader is alive, and free
    // again the moment the last one goes.
    assert!(
        Db::open(dir.path(), opts()).is_err(),
        "the lock must survive as long as a snapshot pins the engine"
    );
    drop(snap);
    drop(owned);
    drop(tail);

    let db = Db::open(dir.path(), opts()).unwrap();
    assert_range_present(&db, 0, 300, "after every reader went away");
}

// ---- drop_all ----

/// Property: `drop_all` empties the database, leaves the handle usable,
/// and the emptiness survives a reopen - no dropped key may come back
/// out of a stale WAL or manifest tail. Catches a reset that only
/// clears memory, and one that forgets to fence the WALs it orphaned.
#[test]
fn drop_all_empties_the_database_and_the_emptiness_survives_a_reopen() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts()).unwrap();
    write_range(&db, 0, 500);
    db.compact_range(None, None).wait().unwrap();

    db.drop_all().unwrap();
    assert!(
        db.scan(None, None).unwrap().is_empty(),
        "drop_all left data behind"
    );
    assert_eq!(db.get(&key(0)).unwrap(), None);

    // The handle is still a working database.
    write_range(&db, 1000, 1100);
    for i in 1000..1100 {
        assert_eq!(db.get(&key(i)).unwrap(), Some(value(i)));
    }
    db.close().unwrap();
    drop(db);

    let db = Db::open(dir.path(), opts()).unwrap();
    assert_range_present(&db, 1000, 1100, "reopen after drop_all");
    for i in 0..500 {
        assert_eq!(
            db.get(&key(i)).unwrap(),
            None,
            "key {i} came back from the dead after drop_all + reopen"
        );
    }
}

/// Property: `drop_all` unlinks what it orphans, every time. Ten
/// fill-compact-drop rounds each end with zero SSTables and exactly one
/// WAL, the fresh one the reset installed; removal is synchronous, so
/// these are exact counts rather than a bound. Catches a reset that
/// installs a new WAL and file-id space without unlinking the previous
/// generation, growing the directory without bound.
#[test]
fn repeated_drop_all_cycles_leave_no_sstables_and_exactly_one_wal() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts()).unwrap();

    for round in 0..10 {
        write_range(&db, 0, 500);
        db.compact_range(None, None).wait().unwrap();
        assert!(
            common::count_sst_files(dir.path()) > 0,
            "round {round}: nothing was written, so drop_all would have nothing to reclaim"
        );

        db.drop_all().unwrap();
        assert_eq!(
            common::count_sst_files(dir.path()),
            0,
            "round {round}: drop_all left SSTables on disk"
        );
        assert_eq!(
            common::count_wal_files(dir.path()),
            1,
            "round {round}: drop_all left more than the one live WAL on disk"
        );
    }
}

// ---- hostile and unusual paths ----

/// Property: `Db::open` creates the whole missing parent chain, so a
/// caller can point it at a path several directories deep that does not
/// exist yet. Catches an open that creates only the leaf and fails with
/// a bare `NotFound` on the parent.
#[test]
fn open_creates_a_missing_parent_chain() {
    let dir = TempDir::new().unwrap();
    let deep = dir.path().join("a").join("b").join("c").join("db");
    let db = Db::open(&deep, opts()).unwrap();
    write_range(&db, 0, 20);
    db.close().unwrap();
    drop(db);

    assert!(deep.is_dir(), "the leaf directory was not created");
    let db = Db::open(&deep, opts()).unwrap();
    assert_range_present(&db, 0, 20, "reopen of a deep path");
}

/// Property: a symlink to a directory is an alias, not a second
/// database. The link and the target reach the same bytes, and the lock
/// treats the two paths as one: opening the target while the link is
/// held is refused with the same lock error a duplicate open gets.
/// Catches a lock keyed on the path string rather than on the file it
/// names, which would let one process open a database twice under two
/// names and corrupt its own manifest.
#[cfg(unix)]
#[test]
fn a_symlink_to_a_directory_is_an_alias_for_the_database_behind_it() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().unwrap();
    let target = dir.path().join("real");
    fs::create_dir(&target).unwrap();
    let link = dir.path().join("link");
    symlink(&target, &link).unwrap();

    let db = Db::open(&link, opts()).unwrap();
    write_range(&db, 0, 100);

    let err = Db::open(&target, opts()).unwrap_err();
    assert!(
        err.to_string().contains("locked"),
        "opening the target while the symlink is held must hit the same lock, got: {err}"
    );

    db.close().unwrap();
    drop(db);

    let db = Db::open(&target, opts()).unwrap();
    assert_range_present(&db, 0, 100, "opened through the target after the link");
    db.close().unwrap();
    drop(db);

    // Reopening through the link again sees the writes made through the
    // target: one database, two names.
    let db = Db::open(&link, opts()).unwrap();
    assert_range_present(&db, 0, 100, "opened through the link again");
}

/// Property: a symlink that does not resolve to a directory is refused
/// cleanly. Neither a dangling link nor a link to a regular file may
/// open, panic, or leave anything behind at the link's target path.
/// Catches an open that follows a link, finds no directory, and creates
/// a database somewhere the caller never named.
#[cfg(unix)]
#[test]
fn a_symlink_that_is_not_a_directory_is_refused_and_creates_nothing() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().unwrap();

    let missing = dir.path().join("does-not-exist");
    let dangling = dir.path().join("dangling");
    symlink(&missing, &dangling).unwrap();
    let err = Db::open(&dangling, opts()).unwrap_err();
    assert!(
        matches!(err, Error::Io(_)),
        "a dangling symlink must fail as an I/O error, got {err:?}"
    );
    assert!(
        !missing.exists(),
        "the refused open materialized the symlink's target at {}",
        missing.display()
    );

    let a_file = dir.path().join("a_file");
    fs::write(&a_file, b"not a database").unwrap();
    let to_file = dir.path().join("to_file");
    symlink(&a_file, &to_file).unwrap();
    let err = Db::open(&to_file, opts()).unwrap_err();
    assert!(
        matches!(err, Error::Io(_)),
        "a symlink to a regular file must fail as an I/O error, got {err:?}"
    );
    assert_eq!(
        fs::read(&a_file).unwrap(),
        b"not a database".to_vec(),
        "the refused open rewrote the file the symlink pointed at"
    );
    assert!(
        !dir.path().join("a_file").join("sst").exists(),
        "the refused open created database subdirectories"
    );
}

/// Property: regolith owns the files it creates and nothing else. Unrelated
/// files in the database directory, in `sst/`, and in `wal/` survive an
/// open, a compaction, a `drop_all`, and a reopen with their contents
/// unchanged. Catches an obsolete-file sweep that unlinks by directory
/// listing instead of by the names the manifest recorded.
#[test]
fn opening_a_directory_with_unrelated_files_leaves_them_untouched() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("sst")).unwrap();
    fs::create_dir_all(dir.path().join("wal")).unwrap();

    let strangers = [
        (dir.path().join("README.txt"), "top level"),
        (dir.path().join("sst").join("notes.txt"), "inside sst/"),
        (dir.path().join("wal").join("notes.txt"), "inside wal/"),
    ];
    for (path, body) in &strangers {
        fs::write(path, body.as_bytes()).unwrap();
    }
    fs::create_dir(dir.path().join("subdir")).unwrap();

    let db = Db::open(dir.path(), opts()).unwrap();
    write_range(&db, 0, 500);
    db.compact_range(None, None).wait().unwrap();
    db.drop_all().unwrap();
    write_range(&db, 0, 100);
    db.close().unwrap();
    drop(db);

    let db = Db::open(dir.path(), opts()).unwrap();
    assert_range_present(&db, 0, 100, "reopen alongside unrelated files");
    db.close().unwrap();

    for (path, body) in &strangers {
        assert_eq!(
            fs::read_to_string(path).unwrap_or_default(),
            *body,
            "regolith disturbed an unrelated file at {}",
            path.display()
        );
    }
    assert!(
        dir.path().join("subdir").is_dir(),
        "regolith removed an unrelated subdirectory"
    );
}

// ---- an unknown on-disk format version ----

/// Offset of the SSTable footer's version byte. The trailing 8-byte
/// little-endian magic is `"REGOSST" << 8 | version`, so the version is
/// the first byte of the last eight.
fn sst_version_byte_offset(path: &Path) -> u64 {
    file_len(path) - 8
}

fn read_byte(path: &Path, offset: u64) -> u8 {
    let bytes = fs::read(path).unwrap();
    bytes[offset as usize]
}

/// The distinct SSTable format versions under `<db>/sst/`, sorted, to
/// prove a directory really holds a mix of layouts.
fn sst_format_versions(db_dir: &Path) -> Vec<u8> {
    let mut versions: Vec<u8> = find_ssts(db_dir)
        .iter()
        .map(|sst| read_byte(sst, sst_version_byte_offset(sst)))
        .collect();
    versions.sort_unstable();
    versions.dedup();
    versions
}

/// Property: the footer carries a format version, and one regolith does not
/// know is rejected with a corruption error naming the magic - never
/// parsed as if it were a known version. Catches the forward-compat
/// failure where a newer writer's file is silently misread and serves
/// wrong bytes, and the one where the rejection is a panic.
#[test]
fn an_sstable_from_an_unknown_format_version_is_rejected_with_a_clear_error() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts()).unwrap();
    write_range(&db, 0, 500);
    db.compact_range(None, None).wait().unwrap();
    db.close().unwrap();
    drop(db);

    let sst = first_sst(dir.path());
    let offset = sst_version_byte_offset(&sst);
    let known = read_byte(&sst, offset);
    assert!(
        known == 5 || known == 6,
        "expected a version regolith writes today (5 = flat index, 6 = partitioned, \
         both under the REGOSST magic), found {known} at offset {offset} of {} - \
         this test targets the wrong byte",
        sst.display()
    );

    // 0x05 to 0x08 are real versions now (0x07 and 0x08 are the sealed
    // layouts of an encrypted database), so probing them here would assert
    // that regolith refuses a table it wrote itself.
    for future_version in [0x09u8, 0x7F, 0xFF] {
        overwrite_range(&sst, offset, &[future_version]);
        match Db::open(dir.path(), opts()) {
            Ok(_) => panic!(
                "regolith opened a database whose SSTable claims format version \
                 {future_version}, which it does not implement"
            ),
            Err(Error::Corruption(source)) => {
                let msg = source.to_string();
                assert!(
                    msg.contains("magic"),
                    "the rejection must name the magic/version it did not recognize, got: {msg}"
                );
            }
            Err(other) => panic!(
                "expected a corruption error for format version {future_version}, got {other:?}"
            ),
        }
    }

    // Restoring the real version byte makes the same directory open and
    // read correctly again, which proves the rejection was about the
    // version and not about collateral damage.
    overwrite_range(&sst, offset, &[known]);
    let db = Db::open(dir.path(), opts()).unwrap();
    assert_range_present(&db, 0, 500, "after restoring the format version byte");
}

/// Property: the same version check guards the bulk-ingest door and
/// fires before anything is mutated - a refused ingest leaves the data
/// and the SSTable count exactly as they were. The control comes first:
/// the identical file ingests cleanly until its version byte is bumped,
/// so the refusal is attributable to the version and not to a broken
/// ingest path. Catches an ingest that copies a file in and only then
/// finds it cannot read it, wiring an unreadable file into the manifest.
#[test]
fn ingesting_an_sstable_from_an_unknown_format_version_changes_nothing() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts()).unwrap();
    write_range(&db, 0, 200);
    db.compact_range(None, None).wait().unwrap();

    let build = |path: &Path| {
        let mut writer = SstFileWriter::create(path, &Options::default()).unwrap();
        for i in 5000..5100 {
            writer.put(&key(i), &value(i)).unwrap();
        }
        writer.finish().unwrap();
    };

    // Control: untampered, the same file ingests and its keys show up.
    let clean = dir.path().join("clean.sst");
    build(&clean);
    db.ingest_external_files(&[clean], IngestOptions::default())
        .wait()
        .unwrap();
    for i in 5000..5100 {
        assert_eq!(db.get(&key(i)).unwrap(), Some(value(i)), "ingested key {i}");
    }

    // The same bytes with a bumped version byte are refused, and nothing
    // about the database moves.
    let future = dir.path().join("future.sst");
    build(&future);
    overwrite_range(&future, sst_version_byte_offset(&future), &[0x42]);
    let before_files = find_ssts(dir.path()).len();
    let before_state = db.scan(None, None).unwrap();

    let err = db
        .ingest_external_files(&[future], IngestOptions::default())
        .wait()
        .unwrap_err();
    assert!(
        err.to_string().contains("magic"),
        "a refused ingest must name the format it did not recognize, got: {err}"
    );
    assert_eq!(
        find_ssts(dir.path()).len(),
        before_files,
        "a refused ingest left an SSTable behind"
    );
    assert_eq!(
        db.scan(None, None).unwrap(),
        before_state,
        "a refused ingest changed the visible state"
    );
}
