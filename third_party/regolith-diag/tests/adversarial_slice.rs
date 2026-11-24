//! Adversarial probes on [`regolith::DbSlice`]'s pinning contract and on
//! the arena memtable's edge shapes.
//!
//! Every test here tries to make a borrowed view outlive what owns it,
//! or to hand the arena a shape it was not sized for. Run these under a
//! sanitizer to get the full value: a use-after-free here is silent
//! without one.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

use regolith::{ArenaProfile, Db, DbSlice, Options, WriteBatch};
use tempfile::TempDir;

fn embedded_opts() -> Options {
    Options::default()
        .write_buffer_size(32 * 1024)
        .arena_profile(ArenaProfile::EMBEDDED)
        .block_cache_size(64 * 1024)
        .block_cache_num_shard_bits(0)
}

/// A slice taken from the memtable must survive the memtable being
/// frozen, flushed to an SSTable, compacted away and the whole `Db`
/// dropped. The arena refcount is the only thing keeping it alive.
#[test]
fn a_memtable_slice_outlives_the_database_that_produced_it() {
    let dir = TempDir::new().unwrap();
    let value: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();

    let held: Vec<DbSlice> = {
        let db = Db::open(dir.path(), embedded_opts()).unwrap();
        let mut held = Vec::new();
        for i in 0..24 {
            regolith::through_stalls(|| db.put(format!("live_{i:03}").as_bytes(), &value)).unwrap();
            held.push(
                db.get_slice(format!("live_{i:03}").as_bytes())
                    .unwrap()
                    .unwrap(),
            );
        }
        // Push every held slice's memtable out from under it.
        for i in 0..4000 {
            regolith::through_stalls(|| db.put(format!("churn_{i:05}").as_bytes(), &[b'z'; 256]))
                .unwrap();
        }
        db.compact_range(None, None).wait().unwrap();
        // Chunks really did go back to the recycling pool and get taken
        // out again, so a violation of the pinning contract would have
        // handed one of these slices' chunks to a later memtable.
        let parked = db
            .get_int_property("regolith.arena-pool-bytes")
            .expect("regolith.arena-pool-bytes is a known property");
        assert!(
            parked > 0,
            "no chunks were recycled, so this probe proves nothing"
        );
        for i in 0..4000 {
            regolith::through_stalls(|| db.delete(format!("churn_{i:05}").as_bytes())).unwrap();
        }
        db.compact_range(None, None).wait().unwrap();
        db.close().unwrap();
        drop(db);
        held
    };

    for (i, slice) in held.iter().enumerate() {
        assert_eq!(
            slice.as_slice(),
            value.as_slice(),
            "memtable slice {i} was corrupted after its database was dropped"
        );
    }
}

/// The same contract for an SSTable-backed slice: it pins the decoded
/// block, and a compaction that unlinks the file it came from must not
/// disturb its bytes.
#[test]
fn an_sstable_slice_survives_the_compaction_that_unlinks_its_file() {
    let dir = TempDir::new().unwrap();
    let value: Vec<u8> = (0..2048u32).map(|i| (i % 199) as u8).collect();

    let db = Db::open(dir.path(), embedded_opts()).unwrap();
    for i in 0..600 {
        regolith::through_stalls(|| db.put(format!("k_{i:05}").as_bytes(), &value)).unwrap();
    }
    db.compact_range(None, None).wait().unwrap();

    // Warm the read so the slice is definitely block-backed.
    let held: Vec<DbSlice> = (0..64)
        .map(|i| {
            db.get_slice(format!("k_{i:05}").as_bytes())
                .unwrap()
                .expect("key present")
        })
        .collect();

    // Rewrite every key so compaction produces new files and drops the
    // old ones, then churn the block cache far past its capacity.
    for round in 0..4 {
        for i in 0..600 {
            regolith::through_stalls(|| {
                db.put(format!("k_{i:05}").as_bytes(), &[round as u8; 2048])
            })
            .unwrap();
        }
        db.compact_range(None, None).wait().unwrap();
    }
    db.close().unwrap();
    drop(db);

    for (i, slice) in held.iter().enumerate() {
        assert_eq!(
            slice.as_slice(),
            value.as_slice(),
            "sstable slice {i} was corrupted after its file was unlinked"
        );
    }
}

/// Two live views over the same block, one dropped while the other is
/// still read. Dropping one must not free the block under the other.
#[test]
fn two_slices_into_one_block_are_independent() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), embedded_opts()).unwrap();
    // Small values so many keys land in the same data block.
    for i in 0..512 {
        regolith::through_stalls(|| {
            db.put(
                format!("b_{i:04}").as_bytes(),
                format!("v{i:04}").as_bytes(),
            )
        })
        .unwrap();
    }
    db.compact_range(None, None).wait().unwrap();

    let a = db.get_slice(b"b_0000").unwrap().unwrap();
    let b = db.get_slice(b"b_0001").unwrap().unwrap();
    let a_sub = a.try_subslice(1..4).expect("in-range subslice");
    drop(a);
    assert_eq!(a_sub.as_slice(), b"000");
    assert_eq!(b.as_slice(), b"v0001");
    drop(b);
    assert_eq!(a_sub.as_slice(), b"000");
    drop(db);
    assert_eq!(a_sub.as_slice(), b"000");
}

/// Zero-length values and single-byte keys, through every read surface.
#[test]
fn empty_values_and_minimal_keys_round_trip() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), embedded_opts()).unwrap();

    regolith::through_stalls(|| db.put(b"e", b"")).unwrap();
    regolith::through_stalls(|| db.put(b"", b"")).unwrap();
    regolith::through_stalls(|| db.put(b"", b"nonempty")).unwrap();
    let mut batch = WriteBatch::new();
    batch.put(b"be", b"");
    batch.put(b"", b"");
    regolith::through_stalls(|| db.write(batch.clone())).unwrap();

    for source in ["memtable", "sstable"] {
        assert_eq!(db.get(b"e").unwrap(), Some(Vec::new()), "{source}");
        assert!(db.has(b"e").unwrap(), "{source}");
        assert_eq!(db.get_size(b"e").unwrap(), Some(0), "{source}");
        let slice = db.get_slice(b"e").unwrap().expect("empty value present");
        assert!(slice.is_empty(), "{source}");
        assert_eq!(slice.as_slice(), b"", "{source}");
        assert_eq!(db.get(b"").unwrap(), Some(Vec::new()), "{source}");
        assert_eq!(db.get(b"be").unwrap(), Some(Vec::new()), "{source}");
        if source == "memtable" {
            db.compact_range(None, None).wait().unwrap();
        }
    }

    // A deleted key and an empty value must stay distinguishable.
    regolith::through_stalls(|| db.delete(b"e")).unwrap();
    assert_eq!(db.get(b"e").unwrap(), None);
    assert!(!db.has(b"e").unwrap());
    assert_eq!(db.get_size(b"e").unwrap(), None);
}

/// A value larger than `ArenaProfile::max_chunk_size` takes a dedicated
/// chunk. Interleave those with small values so the arena has to switch
/// between the class ladder and the dedicated path repeatedly.
#[test]
fn values_far_larger_than_a_chunk_round_trip() {
    let dir = TempDir::new().unwrap();
    let opts = Options::default()
        .write_buffer_size(32 * 1024)
        .arena_profile(ArenaProfile::EMBEDDED);
    let db = Db::open(dir.path(), opts).unwrap();

    // EMBEDDED caps ordinary chunks at 64 KiB.
    let sizes = [
        1usize,
        63 * 1024,
        64 * 1024,
        64 * 1024 + 1,
        192 * 1024,
        7,
        1024 * 1024,
        0,
    ];
    let mut expected = Vec::new();
    for (i, size) in sizes.iter().copied().enumerate() {
        let value: Vec<u8> = (0..size).map(|b| ((b + i) % 256) as u8).collect();
        regolith::through_stalls(|| db.put(format!("big_{i}").as_bytes(), &value)).unwrap();
        expected.push(value);
    }

    for (i, value) in expected.iter().enumerate() {
        let key = format!("big_{i}");
        assert_eq!(
            db.get(key.as_bytes()).unwrap().as_ref(),
            Some(value),
            "memtable {i}"
        );
        assert_eq!(
            db.get_slice(key.as_bytes()).unwrap().unwrap().as_slice(),
            value.as_slice(),
            "memtable slice {i}"
        );
        assert_eq!(db.get_size(key.as_bytes()).unwrap(), Some(value.len()));
    }

    db.compact_range(None, None).wait().unwrap();
    for (i, value) in expected.iter().enumerate() {
        let key = format!("big_{i}");
        assert_eq!(
            db.get(key.as_bytes()).unwrap().as_ref(),
            Some(value),
            "sstable {i}"
        );
    }
}

/// Sizes chosen to land an allocation exactly at, one byte before and
/// one byte after a chunk boundary, repeatedly, so no entry can be
/// silently split across two chunks.
#[test]
fn allocations_that_straddle_a_chunk_boundary_stay_intact() {
    let dir = TempDir::new().unwrap();
    let opts = Options::default()
        .write_buffer_size(1024 * 1024)
        .arena_profile(ArenaProfile {
            initial_chunk_size: 4096,
            max_chunk_size: 4096,
        });
    let db = Db::open(dir.path(), opts).unwrap();

    let mut expected: Vec<(String, Vec<u8>)> = Vec::new();
    // Walk value sizes across the whole 4 KiB chunk so every possible
    // remainder is exercised.
    for (n, size) in (0..4200).step_by(7).enumerate() {
        let key = format!("s_{n:05}");
        let value: Vec<u8> = (0..size).map(|b| ((b ^ n) % 256) as u8).collect();
        regolith::through_stalls(|| db.put(key.as_bytes(), &value)).unwrap();
        expected.push((key, value));
    }

    for (key, value) in &expected {
        assert_eq!(
            db.get_slice(key.as_bytes()).unwrap().unwrap().as_slice(),
            value.as_slice(),
            "chunk-boundary value for {key} came back wrong"
        );
    }
    db.compact_range(None, None).wait().unwrap();
    for (key, value) in &expected {
        assert_eq!(db.get(key.as_bytes()).unwrap().as_ref(), Some(value));
    }
}

/// Range tombstones interleaved with point writes and read concurrently.
/// The tombstone set lives outside the skip list, so its interaction
/// with concurrent inserts gets its own probe.
#[test]
fn range_tombstones_stay_consistent_under_concurrent_inserts() {
    let dir = TempDir::new().unwrap();
    let db = Arc::new(Db::open(dir.path(), embedded_opts()).unwrap());

    // Keys "r_00000".."r_00999" exist up front.
    for i in 0..1000 {
        regolith::through_stalls(|| db.put(format!("r_{i:05}").as_bytes(), b"base")).unwrap();
    }

    let stop = Arc::new(AtomicBool::new(false));
    let violations = Arc::new(AtomicUsize::new(0));

    let writer = {
        let db = Arc::clone(&db);
        thread::spawn(move || {
            for round in 0..200 {
                let mut batch = WriteBatch::new();
                // Delete the whole range, then rewrite half of it in the
                // same atomic batch: order inside a batch is what decides
                // the outcome.
                batch.delete_range(b"r_", b"r_~");
                for i in (0..1000).step_by(2) {
                    batch.put(
                        format!("r_{i:05}").as_bytes(),
                        format!("v{round}").as_bytes(),
                    );
                }
                regolith::through_stalls(|| db.write(batch.clone())).unwrap();
            }
        })
    };

    let mut readers = Vec::new();
    for _ in 0..3 {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        let violations = Arc::clone(&violations);
        readers.push(thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let snap = db.snapshot();
                // Odd keys are covered by every tombstone and never
                // rewritten after one, so they must be absent once the
                // first batch lands, and present-with-"base" before it.
                for i in (1..1000).step_by(2) {
                    let k = format!("r_{i:05}");
                    let got = snap.get(k.as_bytes()).unwrap();
                    if let Some(v) = &got
                        && v != b"base"
                    {
                        violations.fetch_add(1, Ordering::Relaxed);
                    }
                    if snap.has(k.as_bytes()).unwrap() != got.is_some() {
                        violations.fetch_add(1, Ordering::Relaxed);
                    }
                }
                // Even keys are rewritten by every batch, so they must
                // never be missing once the first batch has landed.
                let mut present = 0usize;
                for i in (0..1000).step_by(2) {
                    if snap.has(format!("r_{i:05}").as_bytes()).unwrap() {
                        present += 1;
                    }
                }
                if present != 0 && present != 500 {
                    violations.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    writer.join().unwrap();
    stop.store(true, Ordering::Relaxed);
    for r in readers {
        r.join().unwrap();
    }
    assert_eq!(
        violations.load(Ordering::Relaxed),
        0,
        "range tombstone view was inconsistent"
    );

    for i in (1..1000).step_by(2) {
        assert_eq!(db.get(format!("r_{i:05}").as_bytes()).unwrap(), None);
    }
    for i in (0..1000).step_by(2) {
        assert_eq!(
            db.get(format!("r_{i:05}").as_bytes()).unwrap(),
            Some(b"v199".to_vec())
        );
    }
}

/// Slices taken by many reader threads while a writer keeps rotating
/// the memtable underneath them. Every reader keeps its slice alive
/// past several flushes before reading it.
#[test]
fn slices_taken_during_flush_stay_readable_after_it() {
    let dir = TempDir::new().unwrap();
    let db = Arc::new(Db::open(dir.path(), embedded_opts()).unwrap());
    let value: Vec<u8> = (0..1024u32).map(|i| (i % 241) as u8).collect();
    for i in 0..64 {
        regolith::through_stalls(|| db.put(format!("pin_{i:03}").as_bytes(), &value)).unwrap();
    }

    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let mut i = 0u64;
            while !stop.load(Ordering::Relaxed) {
                regolith::through_stalls(|| {
                    db.put(format!("churn_{i:08}").as_bytes(), &[b'q'; 512])
                })
                .unwrap();
                i += 1;
            }
            i
        })
    };

    let mismatches = Arc::new(AtomicUsize::new(0));
    let mut readers = Vec::new();
    for _ in 0..4 {
        let db = Arc::clone(&db);
        let value = value.clone();
        let mismatches = Arc::clone(&mismatches);
        readers.push(thread::spawn(move || {
            for round in 0..2000 {
                let mut held = Vec::with_capacity(8);
                for i in 0..8 {
                    if let Some(s) = db
                        .get_slice(format!("pin_{:03}", (round + i) % 64).as_bytes())
                        .unwrap()
                    {
                        held.push(s);
                    }
                }
                thread::yield_now();
                for s in &held {
                    if s.as_slice() != value.as_slice() {
                        mismatches.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }));
    }
    for r in readers {
        r.join().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    let churned = writer.join().unwrap();
    assert_eq!(
        mismatches.load(Ordering::Relaxed),
        0,
        "a pinned slice changed under a flush ({churned} churn writes)"
    );
}
