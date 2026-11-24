//! Real process-kill crash consistency for the group-commit write path.
//!
//! Every other durability test in this tree fails an fsync in-process and
//! reopens the engine in the same address space. That never exercises the
//! thing group commit changed most: a group's WAL bytes, its memtable
//! inserts and its published horizon are now produced by a *different*
//! thread than the one that asked for the write. This target kills the
//! whole process part-way through a concurrent durable write load and
//! then checks the survivors.
//!
//! The invariant under test needs no bookkeeping file. Writer `w` writes
//! `w/0`, `w/1`, `w/2`, ... strictly in order, and each `put` returns only
//! after the engine has declared that write durable. So on recovery the
//! keys present for writer `w` must be a *prefix* of its sequence: a hole
//! means the engine acknowledged `i` as durable, then lost it while
//! keeping the later `i + 1`.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use regolith::{Db, DurabilityMode, Options, WriteBatch, WriteOptions};
use tempfile::TempDir;

const CHILD_MODE: &str = "REGOLITH_ADV_CRASH_MODE";
const CHILD_DIR: &str = "REGOLITH_ADV_CRASH_DIR";
const CHILD_ENTRY: &str = "zzz_child_entry_point";
const WRITERS: usize = 8;
const BATCH_OPS: usize = 4;

fn key(writer: usize, index: usize) -> Vec<u8> {
    format!("w{writer:02}/{index:08}").into_bytes()
}

fn opts(durability: DurabilityMode) -> Options {
    Options::default()
        // Small enough that the run rotates memtables and flushes while
        // the writers are still going, so the kill lands with live frozen
        // memtables and a compaction in flight.
        .write_buffer_size(32 * 1024)
        .durability(durability)
}

/// The child half: write flat out from `WRITERS` threads, then exit
/// without running a single destructor.
fn child(mode: &str, dir: &Path) -> ! {
    let durability = if mode == "eventual" {
        DurabilityMode::Eventual
    } else {
        DurabilityMode::Immediate
    };
    let db = Arc::new(Db::open(dir, opts(durability)).expect("child open"));
    let stop = Arc::new(AtomicBool::new(false));
    let batched = mode == "batch";

    for w in 0..WRITERS {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let wopts = WriteOptions::default();
            let mut i = 0usize;
            while !stop.load(Ordering::Relaxed) {
                if batched {
                    // A batch is one atomic unit, so its keys must survive
                    // or vanish together.
                    let mut batch = WriteBatch::new();
                    for slot in 0..BATCH_OPS {
                        batch.put(&key(w, i * BATCH_OPS + slot), b"v");
                    }
                    if regolith::through_stalls(|| db.write_opt(&wopts, batch.clone())).is_err() {
                        return;
                    }
                } else if regolith::through_stalls(|| db.put_opt(&wopts, &key(w, i), b"v")).is_err()
                {
                    return;
                }
                i += 1;
            }
        });
    }

    thread::sleep(Duration::from_millis(1200));
    // No unwinding, no `Drop`, no flush, and any `write_all` a leader is
    // mid-way through is cut off exactly where the kernel left it: what a
    // power cut does to everything not already made durable.
    std::process::exit(9);
}

/// Run one child to its death and hand back its data directory.
fn crash_a_child(mode: &str) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let exe = std::env::current_exe().expect("current_exe");
    let status = Command::new(exe)
        .args(["--exact", "--test-threads=1", CHILD_ENTRY])
        .env(CHILD_MODE, mode)
        .env(CHILD_DIR, dir.path())
        .status()
        .expect("spawn child");
    assert_eq!(
        status.code(),
        Some(9),
        "the child was supposed to die at its own hand, got {status:?}"
    );
    dir
}

/// Collect, per writer, the sorted indices that survived.
fn survivors(db: &Db) -> BTreeMap<usize, Vec<usize>> {
    let mut found: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut iter = db.iter();
    iter.seek_to_first();
    while iter.valid() {
        let raw = iter.key().expect("a valid cursor has a key");
        let text = std::str::from_utf8(raw).expect("ascii key");
        let (w, i) = text.split_once('/').expect("key shape");
        let w: usize = w.trim_start_matches('w').parse().expect("writer id");
        let i: usize = i.parse().expect("index");
        found.entry(w).or_default().push(i);
        iter.next();
    }
    iter.status().expect("iteration failed after recovery");
    for indices in found.values_mut() {
        indices.sort_unstable();
    }
    found
}

fn assert_each_writer_is_a_prefix(found: &BTreeMap<usize, Vec<usize>>, label: &str) {
    let mut total = 0usize;
    for (w, indices) in found {
        total += indices.len();
        for (position, index) in indices.iter().enumerate() {
            assert_eq!(
                *index,
                position,
                "{label}: writer {w} lost index {position} but kept {index}; an \
                 acknowledged durable write vanished while a later one survived \
                 (recovered {} keys for this writer)",
                indices.len()
            );
        }
    }
    assert!(
        total > 0,
        "{label}: the child died before committing anything, so the probe proved nothing"
    );
}

#[test]
fn a_process_kill_never_leaves_a_hole_in_a_durable_writers_sequence() {
    if std::env::var(CHILD_MODE).is_ok() {
        return;
    }
    let dir = crash_a_child("immediate");
    let db = Db::open(dir.path(), opts(DurabilityMode::Immediate)).expect("reopen after crash");
    let found = survivors(&db);
    assert_each_writer_is_a_prefix(&found, "immediate");
}

#[test]
fn a_process_kill_never_tears_a_write_batch() {
    if std::env::var(CHILD_MODE).is_ok() {
        return;
    }
    let dir = crash_a_child("batch");
    let db = Db::open(dir.path(), opts(DurabilityMode::Immediate)).expect("reopen after crash");
    let found = survivors(&db);
    let mut batches = 0usize;
    for (w, indices) in &found {
        // Every batch contributed indices `BATCH_OPS * n ..`. A surviving
        // partial batch is a torn atomic write.
        assert!(
            indices.len().is_multiple_of(BATCH_OPS),
            "batch writer {w} recovered {} keys, not a whole number of {BATCH_OPS}-key batches",
            indices.len()
        );
        for (position, index) in indices.iter().enumerate() {
            assert_eq!(
                *index, position,
                "batch writer {w}: recovered index {index} at position {position}, so a \
                 committed batch was lost while a later one survived"
            );
        }
        batches += indices.len() / BATCH_OPS;
    }
    assert!(batches > 0, "the child committed no batch at all");
}

#[test]
fn a_process_kill_under_eventual_durability_still_recovers_a_clean_prefix() {
    if std::env::var(CHILD_MODE).is_ok() {
        return;
    }
    let dir = crash_a_child("eventual");
    let db = Db::open(dir.path(), opts(DurabilityMode::Eventual)).expect("reopen after crash");
    let found = survivors(&db);
    assert_each_writer_is_a_prefix(&found, "eventual");
}

/// The child entry point, selected by name from the parent.
#[test]
fn zzz_child_entry_point() {
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        return;
    };
    let dir = std::env::var(CHILD_DIR).expect("child dir");
    child(&mode, Path::new(&dir));
}
