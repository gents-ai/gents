//! Adversarial probes on the surfaces that sit above the rewritten write
//! and read paths: column families, checkpoints, backups, the tailing
//! iterator, and the `DbSlice` trait contract.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

use regolith::{BackupEngine, Db, Options, WriteBatch};
use tempfile::TempDir;

/// `DbSlice` is `Eq + Hash`; equal bytes must hash equally no matter
/// which owner variant carries them, or a `HashSet<DbSlice>` silently
/// keeps duplicates.
#[test]
fn dbslice_hash_and_eq_agree_across_every_owner() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), Options::default().write_buffer_size(4 * 1024)).unwrap();

    let payload = vec![b'q'; 300];
    regolith::through_stalls(|| db.put(b"same_a", &payload)).unwrap();
    // Arena-backed, straight out of the memtable.
    let arena_slice = db.get_slice(b"same_a").unwrap().unwrap();

    regolith::through_stalls(|| db.put(b"same_b", &payload)).unwrap();
    db.compact_range(None, None).wait().unwrap();
    // Block-backed, out of an SSTable.
    let block_slice = db.get_slice(b"same_b").unwrap().unwrap();

    assert_eq!(
        arena_slice, block_slice,
        "equal bytes from different owners must compare equal"
    );

    // `DbSlice` is transitively interior-mutable through
    // `SliceOwner::Arena(Arc<Arena>)`, so clippy flags it as a mutable
    // key. `DbSlice::hash` hashes only the immutable bytes. Every
    // library user putting a `DbSlice` in a set hits this same lint.
    #[allow(
        clippy::mutable_key_type,
        reason = "DbSlice hashes only its immutable bytes"
    )]
    let mut set: HashSet<regolith::DbSlice> = HashSet::new();
    assert!(set.insert(arena_slice));
    assert!(
        !set.insert(block_slice),
        "an arena slice and a block slice over equal bytes hashed differently"
    );
    assert_eq!(set.len(), 1);

    // A subslice over the same bytes must behave the same way.
    let whole = db.get_slice(b"same_a").unwrap().unwrap();
    let sub = whole.try_subslice(0..300).unwrap();
    assert_eq!(sub, payload.as_slice());
    assert!(!set.insert(sub), "an equal subslice hashed differently");

    // Ordering must agree with the bytes.
    regolith::through_stalls(|| db.put(b"low", b"aaa")).unwrap();
    regolith::through_stalls(|| db.put(b"high", b"zzz")).unwrap();
    let lo = db.get_slice(b"low").unwrap().unwrap();
    let hi = db.get_slice(b"high").unwrap().unwrap();
    assert!(lo < hi, "DbSlice ordering disagreed with byte ordering");
}

/// A batch that spans several column families is one atomic unit. A
/// snapshot must see all of it or none of it, with many writers in
/// flight so real commit groups form.
#[test]
fn a_cross_column_family_batch_is_atomic_under_group_commit() {
    let dir = TempDir::new().unwrap();
    let db =
        Arc::new(Db::open(dir.path(), Options::default().write_buffer_size(16 * 1024)).unwrap());
    let cfs: Vec<_> = (0..4)
        .map(|i| db.create_column_family(&format!("cf{i}")).unwrap())
        .collect();

    let rounds = 500usize;
    let frontier = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    let mut writers = Vec::new();
    for w in 0..6usize {
        let db = Arc::clone(&db);
        let cfs = cfs.clone();
        let frontier = Arc::clone(&frontier);
        writers.push(thread::spawn(move || {
            for round in 0..rounds {
                let mut batch = WriteBatch::new();
                for cf in &cfs {
                    for k in 0..8 {
                        batch.put_cf(cf, format!("w{w}_{round:05}_{k}").as_bytes(), b"v");
                    }
                }
                regolith::through_stalls(|| db.write(batch.clone())).unwrap();
                frontier.fetch_max(round, Ordering::Release);
            }
        }));
    }

    let torn = Arc::new(AtomicUsize::new(0));
    let reader = {
        let db = Arc::clone(&db);
        let cfs = cfs.clone();
        let frontier = Arc::clone(&frontier);
        let stop = Arc::clone(&stop);
        let torn = Arc::clone(&torn);
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let round = frontier.load(Ordering::Acquire);
                for w in 0..6usize {
                    let snap = db.snapshot();
                    let mut present = 0usize;
                    for cf in &cfs {
                        for k in 0..8 {
                            if snap
                                .get_cf(cf, format!("w{w}_{round:05}_{k}").as_bytes())
                                .unwrap()
                                .is_some()
                            {
                                present += 1;
                            }
                        }
                    }
                    if present != 0 && present != cfs.len() * 8 {
                        torn.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        })
    };

    for w in writers {
        w.join().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    reader.join().unwrap();
    assert_eq!(
        torn.load(Ordering::Relaxed),
        0,
        "a cross-column-family batch was observed torn"
    );

    for w in 0..6usize {
        for cf in &cfs {
            for k in 0..8 {
                assert_eq!(
                    db.get_cf(cf, format!("w{w}_{:05}_{k}", rounds - 1).as_bytes())
                        .unwrap(),
                    Some(b"v".to_vec())
                );
            }
        }
    }
}

/// A checkpoint taken while writers are hammering the pipeline must be a
/// consistent point in time: every key it contains must have the value
/// the source had, and it must never contain a partial batch.
#[test]
fn a_checkpoint_taken_under_load_is_a_consistent_point_in_time() {
    let dir = TempDir::new().unwrap();
    let db =
        Arc::new(Db::open(dir.path(), Options::default().write_buffer_size(32 * 1024)).unwrap());

    let stop = Arc::new(AtomicBool::new(false));
    let mut writers = Vec::new();
    for w in 0..6usize {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        writers.push(thread::spawn(move || {
            let mut round = 0usize;
            while !stop.load(Ordering::Relaxed) {
                let mut batch = WriteBatch::new();
                for k in 0..12 {
                    batch.put(
                        format!("w{w}_{round:06}_{k:02}").as_bytes(),
                        format!("w{w}r{round}").as_bytes(),
                    );
                }
                regolith::through_stalls(|| db.write(batch.clone())).unwrap();
                round += 1;
            }
            round
        }));
    }

    let target = TempDir::new().unwrap();
    let cp_dir = target.path().join("cp");
    thread::sleep(std::time::Duration::from_millis(60));
    db.checkpoint(&cp_dir).wait().unwrap();
    thread::sleep(std::time::Duration::from_millis(20));
    stop.store(true, Ordering::Relaxed);
    for w in writers {
        w.join().unwrap();
    }

    let cp = Db::open(&cp_dir, Options::default()).unwrap();
    let entries = cp.scan(None, None).unwrap();
    assert!(!entries.is_empty(), "the checkpoint captured nothing");

    // Every batch present in the checkpoint must be whole.
    let mut counts: HashMap<(usize, usize), usize> = HashMap::new();
    for (k, v) in &entries {
        let text = String::from_utf8(k.clone()).unwrap();
        let mut parts = text.split('_');
        let w: usize = parts.next().unwrap()[1..].parse().unwrap();
        let round: usize = parts.next().unwrap().parse().unwrap();
        *counts.entry((w, round)).or_default() += 1;
        assert_eq!(
            v,
            format!("w{w}r{round}").as_bytes(),
            "the checkpoint holds a value from a different round"
        );
    }
    for ((w, round), n) in counts {
        assert_eq!(
            n, 12,
            "checkpoint holds a torn batch w{w} round {round}: {n}/12"
        );
    }
}

/// A backup taken under the same load, restored, must round-trip whole
/// batches too.
#[test]
fn a_backup_taken_under_load_restores_whole_batches() {
    let dir = TempDir::new().unwrap();
    let db =
        Arc::new(Db::open(dir.path(), Options::default().write_buffer_size(32 * 1024)).unwrap());

    let stop = Arc::new(AtomicBool::new(false));
    let mut writers = Vec::new();
    for w in 0..4usize {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        writers.push(thread::spawn(move || {
            let mut round = 0usize;
            while !stop.load(Ordering::Relaxed) {
                let mut batch = WriteBatch::new();
                for k in 0..12 {
                    batch.put(
                        format!("w{w}_{round:06}_{k:02}").as_bytes(),
                        format!("w{w}r{round}").as_bytes(),
                    );
                }
                regolith::through_stalls(|| db.write(batch.clone())).unwrap();
                round += 1;
            }
        }));
    }

    let backup_dir = TempDir::new().unwrap();
    thread::sleep(std::time::Duration::from_millis(60));
    let mut engine = BackupEngine::open(backup_dir.path()).unwrap();
    let id = engine.create_backup(&db).unwrap();
    stop.store(true, Ordering::Relaxed);
    for w in writers {
        w.join().unwrap();
    }

    let restore_dir = TempDir::new().unwrap();
    engine.restore(id, restore_dir.path(), None).unwrap();
    let restored = Db::open(restore_dir.path(), Options::default()).unwrap();

    let mut counts: HashMap<(usize, usize), usize> = HashMap::new();
    for (k, v) in restored.scan(None, None).unwrap() {
        let text = String::from_utf8(k).unwrap();
        let mut parts = text.split('_');
        let w: usize = parts.next().unwrap()[1..].parse().unwrap();
        let round: usize = parts.next().unwrap().parse().unwrap();
        *counts.entry((w, round)).or_default() += 1;
        assert_eq!(v, format!("w{w}r{round}").as_bytes());
    }
    assert!(!counts.is_empty(), "the backup captured nothing");
    for ((w, round), n) in counts {
        assert_eq!(
            n, 12,
            "restored backup holds a torn batch w{w} round {round}: {n}/12"
        );
    }
}

/// The tailing iterator must never surface a partial batch either: it
/// reads at the published horizon like everything else.
#[test]
fn a_tailing_iterator_never_surfaces_a_partial_batch() {
    let dir = TempDir::new().unwrap();
    let db =
        Arc::new(Db::open(dir.path(), Options::default().write_buffer_size(16 * 1024)).unwrap());

    let stop = Arc::new(AtomicBool::new(false));
    let rounds = 400usize;
    let mut writers = Vec::new();
    for w in 0..4usize {
        let db = Arc::clone(&db);
        writers.push(thread::spawn(move || {
            for round in 0..rounds {
                let mut batch = WriteBatch::new();
                for k in 0..10 {
                    batch.put(format!("t{w}_{round:05}_{k}").as_bytes(), b"v");
                }
                regolith::through_stalls(|| db.write(batch.clone())).unwrap();
            }
        }));
    }

    let torn = Arc::new(AtomicUsize::new(0));
    let tailer = {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        let torn = Arc::clone(&torn);
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let mut counts: HashMap<(usize, usize), usize> = HashMap::new();
                let mut it = db.iter_tailing();
                it.seek_to_first();
                while it.valid() {
                    let text = String::from_utf8(it.key().unwrap().to_vec()).unwrap();
                    let mut parts = text.split('_');
                    let w: usize = parts.next().unwrap()[1..].parse().unwrap();
                    let round: usize = parts.next().unwrap().parse().unwrap();
                    *counts.entry((w, round)).or_default() += 1;
                    it.next();
                }
                it.status().unwrap();
                // The newest round of each writer may still be arriving,
                // so only rounds strictly below each writer's maximum are
                // required to be whole.
                let mut max_round: HashMap<usize, usize> = HashMap::new();
                for (w, round) in counts.keys() {
                    let e = max_round.entry(*w).or_default();
                    *e = (*e).max(*round);
                }
                for ((w, round), n) in &counts {
                    if *round < max_round[w] && *n != 10 {
                        torn.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        })
    };

    for w in writers {
        w.join().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    tailer.join().unwrap();
    assert_eq!(
        torn.load(Ordering::Relaxed),
        0,
        "the tailing iterator surfaced a partial batch"
    );
}
