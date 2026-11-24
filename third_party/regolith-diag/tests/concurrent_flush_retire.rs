//! A memtable must never be retired before the SSTable holding its
//! contents is in the published version.
//!
//! Two paths reach a flush without excluding each other: a writer's
//! rotation, which runs under the commit pipeline's mutex, and
//! `drain_memtables`, which a checkpoint drives and which releases that
//! mutex before it flushes so a whole SSTable write does not block
//! writers. If both are inside the flush at once they take the same
//! victim, `frozen[0]`, and both retire "index 0" on the way out. The
//! second retirement drops a memtable nothing has flushed, so every
//! write it held disappears: a key that was only ever overwritten reads
//! as an older version, or as absent.
//!
//! This is the end-to-end half. It runs the two paths against each
//! other for real: many rounds of checkpoint-driven drain against
//! writers rotating a 16 KiB memtable constantly. Every key is written
//! once with a value naming its own index and never deleted, so any
//! disagreement at the end is a lost write and nothing else, both
//! before and after a reopen.
//!
//! It is a soak, not the regression gate. The interleaving is narrow
//! enough that this workload does not reliably reproduce it, which is
//! why the gate is the deterministic pair in
//! `src/engine/read_view.rs`: they construct the frozen list a racing
//! retirement leaves behind and check the retirement takes the right
//! memtable out of it.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;

use regolith::{Db, DurabilityMode, Options};
use tempfile::TempDir;

const WRITERS: usize = 4;
/// Keys every writer writes, whatever the drain side does.
const KEYS_PER_WRITER: u64 = 900;
/// Checkpoints that must have run under live writes before a writer stops.
const RACED_DRAINS: u64 = 3;
/// Keys a writer writes at most while it waits for them.
const MAX_KEYS_PER_WRITER: u64 = 200_000;

fn key_of(writer: usize, i: u64) -> Vec<u8> {
    format!("w{writer}k{i:06}").into_bytes()
}

fn value_of(writer: usize, i: u64) -> Vec<u8> {
    format!("v{writer}-{i}").into_bytes()
}

fn opts() -> Options {
    Options::default()
        // Small enough that the writers rotate constantly, which is what
        // puts a flush in flight for the drain to collide with.
        .write_buffer_size(16 * 1024)
        .max_write_buffer_number(4)
        .durability(DurabilityMode::Eventual)
}

#[test]
fn a_checkpoint_drain_racing_a_rotation_never_loses_an_acknowledged_write() {
    let dir = TempDir::new().expect("tempdir");
    // Checkpoints land outside the database directory: a target inside
    // it would be walked by the engine's own file scans.
    let cp_root = TempDir::new().expect("tempdir");
    let db = Arc::new(Db::open(dir.path(), opts()).expect("open"));

    let stop = Arc::new(AtomicBool::new(false));
    let drains = Arc::new(AtomicU64::new(0));
    let raced = Arc::new(AtomicU64::new(0));
    let writes = Arc::new(AtomicU64::new(0));

    let writers: Vec<_> = (0..WRITERS)
        .map(|w| {
            let (db, raced, writes) = (Arc::clone(&db), Arc::clone(&raced), Arc::clone(&writes));
            thread::spawn(move || {
                // Past its fixed keys a writer keeps the rotations coming
                // until enough checkpoints have drained under them. Writers
                // that only wrote the fixed keys can be done before the
                // drain side is first scheduled, which leaves it nothing to
                // race.
                let mut i = 0;
                while i < KEYS_PER_WRITER
                    || (i < MAX_KEYS_PER_WRITER && raced.load(Ordering::Relaxed) < RACED_DRAINS)
                {
                    regolith::through_stalls(|| db.put(&key_of(w, i), &value_of(w, i)))
                        .expect("put");
                    writes.fetch_add(1, Ordering::Relaxed);
                    i += 1;
                    if i > KEYS_PER_WRITER {
                        // Where threads outnumber cores, the drain side
                        // gets the core between the extra puts, which keeps
                        // them few.
                        thread::yield_now();
                    }
                }
                i
            })
        })
        .collect();

    // The drain side. `checkpoint` calls `drain_memtables(Always)`,
    // which is the path that flushes without the pipeline mutex.
    let drainer = {
        let (db, stop, drains) = (Arc::clone(&db), Arc::clone(&stop), Arc::clone(&drains));
        let (raced, writes) = (Arc::clone(&raced), Arc::clone(&writes));
        let target = cp_root.path().to_path_buf();
        thread::spawn(move || {
            let mut round = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let into = target.join(format!("cp{round}"));
                let acknowledged = writes.load(Ordering::Relaxed);
                // A checkpoint can legitimately refuse while the
                // engine is busy; the race is in the drain it ran
                // before refusing, so a refusal is not a failure here.
                if db.checkpoint(&into).wait().is_ok() {
                    drains.fetch_add(1, Ordering::Relaxed);
                    // It raced the writers only if a write was acknowledged
                    // while it ran.
                    if writes.load(Ordering::Relaxed) != acknowledged {
                        raced.fetch_add(1, Ordering::Relaxed);
                    }
                    let _ = std::fs::remove_dir_all(&into);
                }
                round += 1;
            }
        })
    };

    let written: Vec<u64> = writers
        .into_iter()
        .map(|h| h.join().expect("writer"))
        .collect();
    stop.store(true, Ordering::Relaxed);
    drainer.join().expect("drain");

    let (drains, raced) = (
        drains.load(Ordering::Relaxed),
        raced.load(Ordering::Relaxed),
    );
    println!(
        "drains completed: {drains}, {raced} of them under writes; keys per writer: {written:?}"
    );
    assert!(
        raced >= RACED_DRAINS,
        "only {raced} of {drains} checkpoints ran while writes were acknowledged, in \
         {MAX_KEYS_PER_WRITER} keys per writer, so the drain path raced too little to prove anything",
    );

    let mut lost = Vec::new();
    for (w, &keys) in written.iter().enumerate() {
        for i in 0..keys {
            let k = key_of(w, i);
            match db.get(&k).expect("get") {
                Some(v) if v == value_of(w, i) => {}
                other => lost.push(format!(
                    "{} -> {:?}, expected {:?}",
                    String::from_utf8_lossy(&k),
                    other.as_deref().map(String::from_utf8_lossy),
                    String::from_utf8_lossy(&value_of(w, i)),
                )),
            }
        }
    }
    assert!(
        lost.is_empty(),
        "{} acknowledged writes did not survive a concurrent flush and drain; first few:\n  {}",
        lost.len(),
        lost.iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n  "),
    );

    // The same must hold across a reopen: a memtable retired without its
    // SSTable also loses the WAL that backed it.
    db.close().expect("close");
    drop(db);
    let reopened = Db::open(dir.path(), opts()).expect("reopen");
    for (w, &keys) in written.iter().enumerate() {
        for i in 0..keys {
            assert_eq!(
                reopened.get(&key_of(w, i)).expect("get").as_deref(),
                Some(&value_of(w, i)[..]),
                "w{w}k{i:06} did not survive the reopen",
            );
        }
    }
}
