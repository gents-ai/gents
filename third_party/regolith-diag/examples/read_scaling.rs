//! Point-read throughput at 1, 2, 4 and 8 threads.
//!
//! The read path is the one place where the [`regolith::Env`] indirection
//! could plausibly cost something, so it gets a harness rather than an
//! assurance. Cached blocks never reach `Env` at all (the block cache
//! answers first), and an uncached block now takes a positional
//! `read_exact_at` instead of a seek behind a mutex, so the expected
//! result is "no measurable change".
//!
//! ```sh
//! cargo run --release --example read_scaling -- /tmp/regolith-scale 200000 3
//! ```
//!
//! Arguments: database directory, key count, repeat count. Each thread
//! count is measured `repeat` times and the best run is reported, because
//! competing load on the machine can only ever make a run slower.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Instant;

use regolith::{Db, Options};

const THREAD_COUNTS: [usize; 4] = [1, 2, 4, 8];
const READS_PER_THREAD: u64 = 200_000;

fn key(i: u64) -> Vec<u8> {
    format!("key{i:013}").into_bytes()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = args
        .first()
        .cloned()
        .unwrap_or_else(|| "/tmp/regolith-read-scaling".to_string());
    let num: u64 = args.get(1).map_or(Ok(200_000), |s| s.parse())?;
    let repeats: usize = args.get(2).map_or(Ok(3), |s| s.parse())?;

    let _ = std::fs::remove_dir_all(&dir);
    let db = Arc::new(Db::open(&dir, Options::default())?);

    // Materialised once. Formatting a key inside the timed loop would
    // cost an allocation per read and swamp the thing being measured.
    let keys: Arc<Vec<Vec<u8>>> = Arc::new((0..num).map(key).collect());

    let value = vec![0xA5u8; 100];
    for k in keys.iter() {
        db.put(k, &value)?;
    }
    // Reads must come off SSTables through the block cache, not out of
    // a memtable, or this measures the skip list instead of the read
    // path the Env sits behind.
    db.flush()?;
    db.compact_range(None, None).wait()?;
    for k in keys.iter() {
        assert!(db.get(k)?.is_some(), "seed key missing");
    }

    println!("regolith point-read scaling");
    println!("  keys           {num}");
    println!("  reads/thread   {READS_PER_THREAD}");
    println!("  repeats        {repeats} (best reported)");
    println!();
    println!("  {:>7}  {:>14}  {:>8}", "threads", "ops/s", "scaling");

    let mut single = 0f64;
    for threads in THREAD_COUNTS {
        let mut best = 0f64;
        for _ in 0..repeats {
            let ops = measure(&db, &keys, threads);
            if ops > best {
                best = ops;
            }
        }
        if threads == 1 {
            single = best;
        }
        println!(
            "  {:>7}  {:>14.0}  {:>7.2}x",
            threads,
            best,
            if single > 0.0 { best / single } else { 0.0 }
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

fn measure(db: &Arc<Db>, keys: &Arc<Vec<Vec<u8>>>, threads: usize) -> f64 {
    let num = keys.len() as u64;
    let barrier = Arc::new(Barrier::new(threads + 1));
    let found = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::with_capacity(threads);
    for t in 0..threads {
        let db = Arc::clone(db);
        let keys = Arc::clone(keys);
        let barrier = Arc::clone(&barrier);
        let found = Arc::clone(&found);
        handles.push(std::thread::spawn(move || {
            // A cheap deterministic walk with a large odd stride, so
            // every thread touches a different part of the key space
            // and no run depends on an RNG seed.
            let mut i = (t as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) % num;
            let mut hits = 0u64;
            barrier.wait();
            for _ in 0..READS_PER_THREAD {
                if db.get(&keys[i as usize]).expect("read").is_some() {
                    hits += 1;
                }
                i = (i + 0x9E37_79B9) % num;
            }
            found.fetch_add(hits, Ordering::Relaxed);
        }));
    }
    barrier.wait();
    let start = Instant::now();
    for h in handles {
        h.join().expect("reader thread");
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert_eq!(
        found.load(Ordering::Relaxed),
        READS_PER_THREAD * threads as u64,
        "every read must hit"
    );
    (READS_PER_THREAD * threads as u64) as f64 / elapsed
}
