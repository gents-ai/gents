//! A level below L0 is searched by binary search over its tables. A bottom
//! level of hundreds of tables, with tombstone-only tables among them, must
//! read exactly as the data says through every read surface, before and after
//! a reopen.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// the filesystem. The browser suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use regolith::{Db, Options};

/// Keys of the correctness run are `key000000` through `key039999`; only the
/// even ones are written, so every odd key is a gap a search can fall into.
const KEYS: usize = 40_000;

fn key(n: usize) -> Vec<u8> {
    format!("key{n:06}").into_bytes()
}

fn value(n: usize, generation: u8) -> Vec<u8> {
    format!("value-{n:06}-{generation}-{}", "x".repeat(24)).into_bytes()
}

/// Compaction runs only when asked, and cuts small tables, so one pass leaves
/// hundreds of them in the bottom level.
fn options(target_file_size: u64) -> Options {
    Options::default()
        .max_background_compactions(0)
        .write_buffer_size(256 * 1024)
        .target_file_size(target_file_size)
        .block_size(1024)
}

/// A small deterministic generator, so a failure reproduces.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

fn tables_at(db: &Db, level: usize) -> u64 {
    db.get_int_property(&format!("regolith.num-files-at-level{level}"))
        .unwrap()
}

/// Write every even key, push everything to the bottom level, then rewrite,
/// delete and range-delete parts of it and push that down as well, so the
/// bottom level holds point tables and tombstone-only tables side by side.
/// Returns the database and what it should read as.
fn build(dir: &Path, target_file_size: u64, keys: usize) -> (Db, BTreeMap<Vec<u8>, Vec<u8>>) {
    let db = Db::open(dir, options(target_file_size)).unwrap();
    let mut model = BTreeMap::new();
    for n in (0..keys).step_by(2) {
        regolith::through_stalls(|| db.put(&key(n), &value(n, 0))).unwrap();
        model.insert(key(n), value(n, 0));
    }
    db.flush().unwrap();
    db.compact_range(None, None).wait().unwrap();

    // Overwrites and point deletes scattered through the range.
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..3_000 {
        let n = rng.below(keys / 2) * 2;
        if rng.below(3) == 0 {
            regolith::through_stalls(|| db.delete(&key(n))).unwrap();
            model.remove(&key(n));
        } else {
            regolith::through_stalls(|| db.put(&key(n), &value(n, 1))).unwrap();
            model.insert(key(n), value(n, 1));
        }
    }
    // Two whole regions deleted: nothing of them survives compaction, so the
    // tombstones stand alone in tables of their own.
    for (lo, hi) in [(6_000, 9_000), (30_000, 31_000)] {
        regolith::through_stalls(|| db.delete_range(&key(lo), &key(hi))).unwrap();
        model.retain(|k, _| !(key(lo)..key(hi)).contains(k));
    }
    // Keys written back into one of those regions after its tombstone.
    for n in [6_100, 6_102, 8_998] {
        regolith::through_stalls(|| db.put(&key(n), &value(n, 2))).unwrap();
        model.insert(key(n), value(n, 2));
    }
    db.flush().unwrap();
    db.compact_range(None, None).wait().unwrap();
    (db, model)
}

/// Every key the model knows, every gap between them, and keys outside the
/// range, through `get`, and random batches of them through `multi_get`.
fn verify(db: &Db, model: &BTreeMap<Vec<u8>, Vec<u8>>, what: &str) {
    let probe = |n: usize| key(n);
    for n in 0..KEYS {
        assert_eq!(
            db.get(&probe(n)).unwrap().as_deref(),
            model.get(&probe(n)).map(Vec::as_slice),
            "{what}: get of {}",
            String::from_utf8_lossy(&probe(n))
        );
    }
    for outside in [b"a".as_slice(), b"key", b"key0399990", b"zzzz"] {
        assert_eq!(db.get(outside).unwrap(), None, "{what}: {outside:?}");
    }

    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for batch in 0..300 {
        let size = 1 + rng.below(64);
        let keys: Vec<Vec<u8>> = (0..size)
            .map(|_| match rng.below(10) {
                0 => b"key".to_vec(),
                1 => key(KEYS + rng.below(10)),
                _ => key(rng.below(KEYS)),
            })
            .collect();
        let refs: Vec<&[u8]> = keys.iter().map(Vec::as_slice).collect();
        let got = db.multi_get(&refs).unwrap();
        for (k, got) in keys.iter().zip(&got) {
            assert_eq!(
                got.as_deref(),
                model.get(k).map(Vec::as_slice),
                "{what}: multi_get batch {batch} of {}",
                String::from_utf8_lossy(k)
            );
        }
    }
}

#[test]
fn a_bottom_level_of_hundreds_of_tables_reads_exactly_as_the_data_says() {
    let dir = tempfile::tempdir().unwrap();
    let (db, model) = build(dir.path(), 8 * 1024, KEYS);
    let bottom = tables_at(&db, 6);
    assert!(
        bottom >= 100,
        "the setup is meant to leave a deep level of many tables, got {bottom}"
    );
    verify(&db, &model, "after compaction");

    // Writes newer than every table, in the memtable and in L0, shadow and
    // delete what the bottom level holds.
    let mut rng = Rng(0xA076_1D64_78BD_642F);
    let mut model = model;
    for _ in 0..500 {
        let n = rng.below(KEYS);
        if rng.below(4) == 0 {
            regolith::through_stalls(|| db.delete(&key(n))).unwrap();
            model.remove(&key(n));
        } else {
            regolith::through_stalls(|| db.put(&key(n), &value(n, 3))).unwrap();
            model.insert(key(n), value(n, 3));
        }
    }
    db.flush().unwrap();
    regolith::through_stalls(|| db.delete_range(&key(20_000), &key(20_500))).unwrap();
    model.retain(|k, _| !(key(20_000)..key(20_500)).contains(k));
    verify(&db, &model, "with newer writes above the bottom level");

    db.close().unwrap();
    drop(db);
    let db = Db::open(dir.path(), options(8 * 1024)).unwrap();
    assert_eq!(tables_at(&db, 6), bottom, "reopen keeps the bottom level");
    verify(&db, &model, "after a reopen");
}

/// Timing for a level of about a thousand tables. It prints and asserts
/// nothing about speed: run it in release with
/// `cargo test --release --test deep_level_search -- --ignored --nocapture`.
#[test]
#[ignore = "prints timings; run in release with --ignored --nocapture"]
fn point_reads_and_batches_over_a_level_of_a_thousand_tables() {
    let dir = tempfile::tempdir().unwrap();
    let (db, model) = build(dir.path(), 4 * 1024, 160_000);
    let tables = tables_at(&db, 6);
    println!("bottom level: {tables} tables, {} live keys", model.len());
    assert!(tables >= 500, "too few tables to mean anything: {tables}");

    let live: Vec<&Vec<u8>> = model.keys().collect();
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);

    const GETS: usize = 400_000;
    let probes: Vec<&Vec<u8>> = (0..GETS).map(|_| live[rng.below(live.len())]).collect();
    // One untimed pass warms the block cache, so the timed pass is the search
    // and the cached read, not the first disk read.
    for k in probes.iter().take(GETS / 4) {
        db.get(k).unwrap();
    }
    let started = Instant::now();
    let mut hits = 0u64;
    for k in &probes {
        hits += u64::from(db.get(k).unwrap().is_some());
    }
    let elapsed = started.elapsed();
    println!(
        "get: {GETS} reads in {:.3}s = {:.0} reads/s ({:.2} us each), {hits} hits",
        elapsed.as_secs_f64(),
        GETS as f64 / elapsed.as_secs_f64(),
        elapsed.as_secs_f64() * 1e6 / GETS as f64
    );

    const BATCHES: usize = 6_000;
    let batches: Vec<Vec<&[u8]>> = (0..BATCHES)
        .map(|_| {
            let mut keys: Vec<&[u8]> = (0..64)
                .map(|_| live[rng.below(live.len())].as_slice())
                .collect();
            keys.sort_unstable();
            keys
        })
        .collect();
    let started = Instant::now();
    let mut found = 0u64;
    for batch in &batches {
        found += db
            .multi_get(batch)
            .unwrap()
            .iter()
            .filter(|v| v.is_some())
            .count() as u64;
    }
    let elapsed = started.elapsed();
    println!(
        "multi_get: {BATCHES} batches of 64 in {:.3}s = {:.0} batches/s ({:.2} us each), {found} found",
        elapsed.as_secs_f64(),
        BATCHES as f64 / elapsed.as_secs_f64(),
        elapsed.as_secs_f64() * 1e6 / BATCHES as f64
    );
}
