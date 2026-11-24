//! Harder adversarial probes on commits that share a sync and on the arena
//! memtable.
//!
//! These go past `tests/adversarial_mvcc.rs` in three directions the
//! existing probes do not cover: snapshots that are *held* across
//! thousands of commits rather than taken and dropped, every read surface
//! cross-checked against every other on the same snapshot, and borrowed
//! [`regolith::DbSlice`] views carried through `drop_all`, which is the one
//! engine operation that discards every memtable and resets the read
//! horizon underneath a live reader.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

use regolith::{ArenaProfile, Db, DbSlice, Options, WriteBatch};
use tempfile::TempDir;

const BATCH_WIDTH: usize = 24;
/// Floor the read side must clear before it stops, so the assertion
/// at the end states what the loops enforce.
const MIN_READS: usize = 200_000;

fn opts() -> Options {
    Options::default().write_buffer_size(8 * 1024)
}

fn batch_key(writer: usize, round: usize, k: usize) -> Vec<u8> {
    format!("b{writer:02}/{round:06}/{k:03}").into_bytes()
}

/// A snapshot taken once and read for the whole run. Every read surface
/// must agree with every other on it, and none may ever see a prefix of
/// a batch, no matter how many commits, flushes and compactions land
/// after the snapshot was captured.
#[test]
fn one_long_lived_snapshot_agrees_with_itself_across_every_read_surface() {
    let dir = TempDir::new().unwrap();
    let db = Arc::new(Db::open(dir.path(), opts()).unwrap());

    let writers = 16usize;
    let rounds = 250usize;
    let seeded = 40usize;

    // Seed a keyspace the snapshot will pin, then capture it.
    for round in 0..seeded {
        let mut batch = WriteBatch::new();
        for k in 0..BATCH_WIDTH {
            batch.put(&batch_key(99, round, k), format!("seed{round}").as_bytes());
        }
        regolith::through_stalls(|| db.write(batch.clone())).unwrap();
    }
    let snap = db.snapshot();
    let pinned: Vec<(Vec<u8>, Vec<u8>)> = (0..seeded)
        .flat_map(|round| {
            (0..BATCH_WIDTH)
                .map(move |k| (batch_key(99, round, k), format!("seed{round}").into_bytes()))
        })
        .collect();

    let stop = Arc::new(AtomicBool::new(false));
    let frontier: Arc<Vec<AtomicUsize>> =
        Arc::new((0..writers).map(|_| AtomicUsize::new(0)).collect());

    let mut handles = Vec::new();
    for w in 0..writers {
        let db = Arc::clone(&db);
        let frontier = Arc::clone(&frontier);
        handles.push(thread::spawn(move || {
            for round in 0..rounds {
                frontier[w].store(round, Ordering::Release);
                let mut batch = WriteBatch::new();
                for k in 0..BATCH_WIDTH {
                    batch.put(&batch_key(w, round, k), b"v");
                }
                // Overwrite the snapshot's pinned keys as well, so the
                // snapshot has to hold a genuinely older version rather
                // than the only version.
                batch.put(&batch_key(99, round % seeded, w), b"clobbered");
                regolith::through_stalls(|| db.write(batch.clone())).unwrap();
            }
            frontier[w].store(rounds, Ordering::Release);
        }));
    }

    let torn = Arc::new(AtomicUsize::new(0));
    let drifted = Arc::new(AtomicUsize::new(0));
    let disagreed = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(AtomicUsize::new(0));

    let mut readers = Vec::new();
    for _ in 0..4 {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        let frontier = Arc::clone(&frontier);
        let (torn, reads) = (Arc::clone(&torn), Arc::clone(&reads));
        let _ = &db;
        let pinned = pinned.clone();
        readers.push(thread::spawn(move || {
            // Each reader also takes its own short-lived snapshots, so
            // both the fresh-snapshot and the long-held-snapshot paths
            // run against the same commit stream.
            // Stop only once the writers are done AND the read floor is
            // cleared. The writers commit a quota, so `reads` is
            // otherwise the ratio of the two sides' throughput on
            // whatever host is running, and the floor asserted at the
            // end becomes a speed test rather than a coverage claim.
            while !stop.load(Ordering::Relaxed) || reads.load(Ordering::Relaxed) < MIN_READS {
                let fresh = db.snapshot();
                for w in 0..writers {
                    let at = frontier[w].load(Ordering::Acquire);
                    for round in [at.saturating_sub(1), at, at + 1] {
                        if round >= rounds {
                            continue;
                        }
                        let present = (0..BATCH_WIDTH)
                            .filter(|k| fresh.get(&batch_key(w, round, *k)).unwrap().is_some())
                            .count();
                        reads.fetch_add(BATCH_WIDTH, Ordering::Relaxed);
                        if present != 0 && present != BATCH_WIDTH {
                            torn.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                // The fresh snapshot must also see whole batches only.
                for (key, _) in pinned.iter().step_by(7) {
                    reads.fetch_add(1, Ordering::Relaxed);
                    let _ = fresh.get(key).unwrap();
                }
            }
        }));
    }

    // One thread does nothing but interrogate the long-held snapshot
    // through every surface at once.
    let auditor = {
        let stop = Arc::clone(&stop);
        let (drifted, disagreed, reads) = (
            Arc::clone(&drifted),
            Arc::clone(&disagreed),
            Arc::clone(&reads),
        );
        let pinned = pinned.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) || reads.load(Ordering::Relaxed) < MIN_READS {
                for (key, value) in &pinned {
                    let got = snap.get(key).unwrap();
                    let has = snap.has(key).unwrap();
                    let size = snap.get_size(key).unwrap();
                    let slice = snap.get_slice(key).unwrap();
                    reads.fetch_add(4, Ordering::Relaxed);

                    if got.as_deref() != Some(value.as_slice()) {
                        drifted.fetch_add(1, Ordering::Relaxed);
                    }
                    if has != got.is_some()
                        || size != got.as_ref().map(|v| v.len())
                        || slice.as_ref().map(|s| s.as_slice().to_vec()) != got
                    {
                        disagreed.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            snap
        })
    };

    let compactor = {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            // At least one pass, so "a compaction ran underneath the
            // probe" is enforced rather than hoped for.
            let mut n = 0usize;
            while n == 0 || !stop.load(Ordering::Relaxed) {
                db.compact_range(None, None).wait().unwrap();
                n += 1;
            }
            n
        })
    };

    for h in handles {
        h.join().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    for r in readers {
        r.join().unwrap();
    }
    let snap = auditor.join().unwrap();
    let compactions = compactor.join().unwrap();

    let (torn, drifted, disagreed, reads) = (
        torn.load(Ordering::Relaxed),
        drifted.load(Ordering::Relaxed),
        disagreed.load(Ordering::Relaxed),
        reads.load(Ordering::Relaxed),
    );
    println!(
        "torn={torn} drifted={drifted} surface_disagreements={disagreed} \
         reads={reads} compactions={compactions}"
    );
    assert_eq!(torn, 0, "{torn} torn batches across {reads} snapshot reads");
    assert_eq!(
        drifted, 0,
        "{drifted} reads through a held snapshot returned a value the snapshot could not see"
    );
    assert_eq!(
        disagreed, 0,
        "{disagreed} disagreements between get / has / get_size / get_slice on one snapshot"
    );
    assert!(
        reads >= MIN_READS,
        "probe was too weak: only {reads} reads against a floor of {MIN_READS}"
    );
    assert!(compactions > 0, "no compaction ran underneath the probe");

    // And the snapshot is still intact after everything above.
    for (key, value) in &pinned {
        assert_eq!(
            snap.get(key).unwrap().as_deref(),
            Some(value.as_slice()),
            "the held snapshot lost a key after the run finished"
        );
    }
}

/// `drop_all` discards every memtable, unlinks every SSTable and resets
/// the read horizon. Slices taken before it must still address their own
/// bytes: the arena refcount and the pinned block are the entire contract.
#[test]
fn slices_survive_drop_all_and_the_reuse_that_follows_it() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(
        dir.path(),
        Options::default()
            .write_buffer_size(32 * 1024)
            .arena_profile(ArenaProfile::EMBEDDED),
    )
    .unwrap();

    let value: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
    let mut held: Vec<DbSlice> = Vec::new();
    for i in 0..32 {
        regolith::through_stalls(|| db.put(format!("pinned_{i:03}").as_bytes(), &value)).unwrap();
        held.push(
            db.get_slice(format!("pinned_{i:03}").as_bytes())
                .unwrap()
                .unwrap(),
        );
    }
    // Some of the held slices come from an SSTable rather than the arena.
    db.compact_range(None, None).wait().unwrap();
    for i in 0..32 {
        held.push(
            db.get_slice(format!("pinned_{i:03}").as_bytes())
                .unwrap()
                .unwrap(),
        );
    }

    db.drop_all().unwrap();
    assert_eq!(db.get(b"pinned_000").unwrap(), None, "drop_all left data");

    // Refill hard, so every recycled chunk and every evicted block frame
    // is handed to somebody else.
    for i in 0..6000 {
        regolith::through_stalls(|| db.put(format!("after_{i:05}").as_bytes(), &[b'z'; 200]))
            .unwrap();
    }
    db.compact_range(None, None).wait().unwrap();

    for (i, slice) in held.iter().enumerate() {
        assert_eq!(
            slice.as_slice(),
            value.as_slice(),
            "slice {i} was corrupted by drop_all and the reuse after it"
        );
    }
}

/// External-file ingest reads its source through the engine's shared
/// block cache under a hard-coded `file_id` of `0`
/// (`src/engine/mod.rs`, `SsTableReader::open(path, 0)`), so a second
/// ingest is served the first source's cached data blocks and silently
/// writes that file's contents instead of its own.
///
/// This is a minimal deterministic reproducer: two ingests, no threads,
/// no concurrency. It reproduces byte for byte at the base commit
/// `d1ec2e7`, so the defect is pre-existing rather than introduced here.
#[test]
fn a_second_ingest_must_not_be_served_the_first_sources_cached_blocks() {
    use regolith::{IngestOptions, SstFileWriter};

    let dir = TempDir::new().unwrap();
    let staging = TempDir::new().unwrap();
    let options = Options::default().write_buffer_size(32 * 1024);
    let db = Db::open(dir.path(), options.clone()).unwrap();

    for batch in 0..2usize {
        let path = staging.path().join(format!("src_{batch}.sst"));
        let mut writer = SstFileWriter::create(&path, &options).unwrap();
        for i in 0..64 {
            writer
                .put(
                    format!("ing_{batch:04}_{i:03}").as_bytes(),
                    format!("v{batch}").as_bytes(),
                )
                .unwrap();
        }
        writer.finish().unwrap();

        // A plain write between the ingests is what forces the second
        // source's blocks to be looked up rather than served from the
        // reader that is still open.
        regolith::through_stalls(|| db.put(format!("plain{batch}").as_bytes(), b"p")).unwrap();
        db.ingest_external_files(
            std::slice::from_ref(&path),
            IngestOptions {
                snapshot_consistency: false,
                ..IngestOptions::default()
            },
        )
        .wait()
        .unwrap();
    }

    let mut missing = BTreeSet::new();
    for batch in 0..2usize {
        for i in 0..64 {
            let key = format!("ing_{batch:04}_{i:03}");
            if db.get(key.as_bytes()).unwrap() != Some(format!("v{batch}").into_bytes()) {
                missing.insert(key);
            }
        }
    }
    assert!(
        missing.is_empty(),
        "ingest_external_files returned Ok but {} keys it was given are unreadable; \
         the engine wrote a different source file's contents instead. Missing: {:?}",
        missing.len(),
        missing.iter().take(3).collect::<Vec<_>>()
    );
}
