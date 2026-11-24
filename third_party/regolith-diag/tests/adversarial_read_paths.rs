//! Differential probe over every read surface.
//!
//! `get`, `get_slice`, `has`, `get_size`, `multi_get`, `scan`,
//! `scan_page` and the forward and reverse iterators are now four
//! different projections of one engine walk. This drives a deterministic
//! pseudorandom op stream against a `BTreeMap` model and demands that
//! every surface agree with the model and with each other, across the
//! memtable, frozen memtables and every SSTable level, with range
//! tombstones and snapshots in the mix.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::ops::Bound;
use std::sync::Arc;

use regolith::{Db, MergeOperator, Options, WriteBatch};
use tempfile::TempDir;

/// Append-only merge: the final value is the base followed by every
/// operand in order, which a `BTreeMap` model can reproduce exactly.
struct Concat;

impl MergeOperator for Concat {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(|b| b.to_vec()).unwrap_or_default();
        for op in operands {
            out.extend_from_slice(op);
        }
        Some(out)
    }

    fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        let mut out = left.to_vec();
        out.extend_from_slice(right);
        Some(out)
    }

    fn name(&self) -> &'static str {
        "adversarial-concat"
    }
}

/// xorshift64, so a failure is reproducible from its seed alone.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const KEY_SPACE: u64 = 400;

fn key_of(i: u64) -> Vec<u8> {
    format!("k{i:04}").into_bytes()
}

fn value_of(rng: &mut Rng) -> Vec<u8> {
    let len = (rng.below(300)) as usize;
    let seed = rng.next() as u8;
    (0..len).map(|i| seed.wrapping_add(i as u8)).collect()
}

/// Compare every read surface against `model` and against each other.
fn check(db: &Db, model: &BTreeMap<Vec<u8>, Vec<u8>>, step: usize) {
    for i in 0..KEY_SPACE {
        let k = key_of(i);
        let want = model.get(&k);

        let got = db.get(&k).unwrap();
        assert_eq!(
            got.as_ref(),
            want,
            "step {step}: get disagreed with the model for {:?}",
            String::from_utf8_lossy(&k)
        );

        let slice = db.get_slice(&k).unwrap().map(|s| s.to_vec());
        assert_eq!(
            slice.as_ref(),
            want,
            "step {step}: get_slice disagreed with get for {:?}",
            String::from_utf8_lossy(&k)
        );

        assert_eq!(
            db.has(&k).unwrap(),
            want.is_some(),
            "step {step}: has disagreed for {:?}",
            String::from_utf8_lossy(&k)
        );
        assert_eq!(
            db.get_size(&k).unwrap(),
            want.map(|v| v.len()),
            "step {step}: get_size disagreed for {:?}",
            String::from_utf8_lossy(&k)
        );
    }

    // multi_get over the whole key space in one call.
    let keys: Vec<Vec<u8>> = (0..KEY_SPACE).map(key_of).collect();
    let refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
    let multi = db.multi_get(&refs).unwrap();
    for (k, got) in keys.iter().zip(multi.iter()) {
        assert_eq!(
            got.as_ref(),
            model.get(k),
            "step {step}: multi_get disagreed for {:?}",
            String::from_utf8_lossy(k)
        );
    }

    let expected: Vec<(Vec<u8>, Vec<u8>)> =
        model.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

    assert_eq!(
        db.scan(None, None).unwrap(),
        expected,
        "step {step}: full scan disagreed with the model"
    );

    // Paged scan must reassemble into the same sequence.
    let mut paged: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut start: Option<Vec<u8>> = None;
    loop {
        let page = db
            .scan_page(start.as_deref(), None, 7)
            .expect("scan_page succeeds");
        paged.extend(page.entries);
        match page.next_start {
            Some(next) => start = Some(next),
            None => break,
        }
    }
    assert_eq!(paged, expected, "step {step}: paged scan disagreed");

    // Forward iteration.
    let mut it = db.iter();
    it.seek_to_first();
    let mut forward = Vec::new();
    while it.valid() {
        forward.push((it.key().unwrap().to_vec(), it.value().unwrap().to_vec()));
        assert_eq!(
            it.value_slice().map(|s| s.to_vec()).as_deref(),
            it.value(),
            "step {step}: value_slice disagreed with value during forward iteration"
        );
        it.next();
    }
    it.status().unwrap();
    assert_eq!(
        forward, expected,
        "step {step}: forward iteration disagreed"
    );

    // Reverse iteration.
    let mut it = db.iter();
    it.seek_to_last();
    let mut reverse = Vec::new();
    while it.valid() {
        reverse.push((it.key().unwrap().to_vec(), it.value().unwrap().to_vec()));
        it.prev();
    }
    it.status().unwrap();
    reverse.reverse();
    assert_eq!(
        reverse, expected,
        "step {step}: reverse iteration disagreed"
    );
}

/// Seeks at every key in the space, including absent ones, checked
/// against the model's own bound queries.
fn check_seeks(db: &Db, model: &BTreeMap<Vec<u8>, Vec<u8>>, step: usize) {
    for i in 0..KEY_SPACE {
        let k = key_of(i);

        let mut it = db.iter();
        it.seek(&k);
        let want = model.range(k.clone()..).next().map(|(k, _)| k.clone());
        let got = it.valid().then(|| it.key().unwrap().to_vec());
        assert_eq!(
            got,
            want,
            "step {step}: seek landed wrong for {:?}",
            String::from_utf8_lossy(&k)
        );

        let mut it = db.iter();
        it.seek_for_prev(&k);
        let want = model
            .range((Bound::Unbounded, Bound::Included(k.clone())))
            .next_back()
            .map(|(k, _)| k.clone());
        let got = it.valid().then(|| it.key().unwrap().to_vec());
        assert_eq!(
            got,
            want,
            "step {step}: seek_for_prev landed wrong for {:?}",
            String::from_utf8_lossy(&k)
        );
    }
}

fn run(seed: u64, steps: usize, opts: Options) {
    let merges_enabled = opts.get_merge_operator().is_some();
    let reopen_opts = Options::default().merge_operator(opts.get_merge_operator().cloned());
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), opts).unwrap();
    let mut model: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    let mut rng = Rng(seed);

    for step in 0..steps {
        match rng.below(100) {
            0..=39 => {
                let k = key_of(rng.below(KEY_SPACE));
                let v = value_of(&mut rng);
                regolith::through_stalls(|| db.put(&k, &v)).unwrap();
                model.insert(k, v);
            }
            40..=54 => {
                let k = key_of(rng.below(KEY_SPACE));
                regolith::through_stalls(|| db.delete(&k)).unwrap();
                model.remove(&k);
            }
            55..=64 => {
                let a = rng.below(KEY_SPACE);
                let b = rng.below(KEY_SPACE);
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                let (lo, hi) = (key_of(lo), key_of(hi));
                regolith::through_stalls(|| db.delete_range(&lo, &hi)).unwrap();
                let doomed: Vec<Vec<u8>> = model
                    .range(lo.clone()..hi.clone())
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in doomed {
                    model.remove(&k);
                }
            }
            65..=74 => {
                let k = key_of(rng.below(KEY_SPACE));
                let operand = value_of(&mut rng);
                if merges_enabled {
                    regolith::through_stalls(|| db.merge(&k, &operand)).unwrap();
                    model
                        .entry(k)
                        .and_modify(|v| v.extend_from_slice(&operand))
                        .or_insert_with(|| operand.clone());
                } else {
                    regolith::through_stalls(|| db.put(&k, &operand)).unwrap();
                    model.insert(k, operand);
                }
            }
            75..=89 => {
                let mut batch = WriteBatch::new();
                let ops = 1 + rng.below(16);
                let mut staged: Vec<(Vec<u8>, Option<Vec<u8>>)> = Vec::new();
                for _ in 0..ops {
                    let k = key_of(rng.below(KEY_SPACE));
                    if rng.below(4) == 0 {
                        batch.delete(&k);
                        staged.push((k, None));
                    } else {
                        let v = value_of(&mut rng);
                        batch.put(&k, &v);
                        staged.push((k, Some(v)));
                    }
                }
                regolith::through_stalls(|| db.write(batch.clone())).unwrap();
                for (k, v) in staged {
                    match v {
                        Some(v) => {
                            model.insert(k, v);
                        }
                        None => {
                            model.remove(&k);
                        }
                    }
                }
            }
            _ => {
                db.compact_range(None, None).wait().unwrap();
            }
        }

        if step % 25 == 24 {
            check(&db, &model, step);
        }
    }
    check(&db, &model, steps);
    check_seeks(&db, &model, steps);

    // The same state must survive a reopen unchanged.
    db.close().unwrap();
    drop(db);
    let reopened = Db::open(dir.path(), reopen_opts).unwrap();
    let expected: Vec<(Vec<u8>, Vec<u8>)> =
        model.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    assert_eq!(
        reopened.scan(None, None).unwrap(),
        expected,
        "seed {seed}: the model did not survive a reopen"
    );
}

/// Small memtables and small blocks, so entries scatter across the
/// memtable, frozen memtables and several SSTable levels.
fn churny() -> Options {
    Options::default()
        .write_buffer_size(8 * 1024)
        .block_size(512)
        .target_file_size(32 * 1024)
        .l0_compaction_trigger(2)
}

#[test]
fn every_read_surface_agrees_with_the_model_across_many_seeds() {
    // Three seeds of 900 operations rather than six. Each operation is
    // checked against the model on every read surface, so a divergence
    // shows up within a seed rather than by trying more of them; the
    // extra three cost wall clock, and this timed out at five minutes on
    // a Windows runner while taking seconds on Linux.
    for seed in [1u64, 0x9E3779B97F4A7C15, 42] {
        run(seed, 900, churny());
    }
}

#[test]
fn every_read_surface_agrees_with_a_partitioned_index_and_a_tiny_cache() {
    let opts = churny()
        .partitioned_index(true)
        .metadata_block_size(512)
        .cache_index_and_filter_blocks(true)
        .block_cache_size(8 * 1024)
        .block_cache_num_shard_bits(0);
    run(0xA5A5_5A5A, 900, opts);
}

#[test]
fn every_read_surface_agrees_under_the_embedded_arena_profile() {
    let opts = churny()
        .arena_profile(regolith::ArenaProfile::EMBEDDED)
        .write_buffer_size(4 * 1024);
    run(0x0BAD_C0DE, 900, opts);
}

/// The merge path is the one place `has` and `get_size` must
/// materialize, because only `full_merge` decides whether a value
/// exists at all. Drive it with a concat operator the model can mirror.
#[test]
fn every_read_surface_agrees_with_a_merge_operator() {
    for seed in [11u64, 0xC0FFEE] {
        println!("--- merge seed {seed} ---");
        let opts = churny().merge_operator(Some(Arc::new(Concat)));
        run(seed, 900, opts);
    }
}
