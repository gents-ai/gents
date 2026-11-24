//! Adversarial differential probe for reverse iteration (reverse iteration).
//!
//! Every iteration surface is compared against a `BTreeMap` oracle over
//! adversarial key sets: all-`0xff` keys at every length around the
//! eight-byte boundary the old probe assumed, keys that are prefixes of
//! each other, the empty key, keys carrying embedded `0x00`, and random
//! binary keys of length 0..24. Each set is checked in three storage
//! states (memtable only, flushed to L0, fully compacted), through the
//! default CF, a named CF, and a snapshot.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;

use regolith::{Db, Options};
use tempfile::TempDir;

/// Deterministic 64-bit stream; no dependency, no wall clock.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn key_sets() -> Vec<(&'static str, Vec<Vec<u8>>)> {
    let mut sets: Vec<(&'static str, Vec<Vec<u8>>)> = Vec::new();

    // All-0xff keys of length 0..=20, the shape the old 8-byte probe
    // could not reach.
    sets.push((
        "all_ff_0_to_20",
        (0..=20usize).map(|n| vec![0xffu8; n]).collect(),
    ));

    // Eight 0xff bytes followed by every possible single suffix byte,
    // which is exactly the family the old probe swallowed.
    let mut ff8 = vec![vec![0xffu8; 8]];
    for b in 0u16..=255 {
        let mut k = vec![0xffu8; 8];
        k.push(b as u8);
        ff8.push(k);
    }
    sets.push(("ff8_plus_one_byte", ff8));

    // Prefixes of each other, straddling the boundary.
    let mut pre = Vec::new();
    for n in 0..=16usize {
        pre.push(vec![0x61u8; n]);
        let mut k = vec![0xffu8; n];
        k.push(0x00);
        pre.push(k);
    }
    sets.push(("prefix_chains", pre));

    // Embedded NULs and a mix of extreme bytes.
    let mut nul = vec![
        Vec::new(),
        vec![0x00],
        vec![0x00, 0x00],
        vec![0x00, 0xff],
        vec![0xff, 0x00],
        vec![0xff; 7],
        vec![0xff; 8],
        vec![0xff; 9],
    ];
    for n in 1..=12usize {
        let mut k = vec![0xffu8; n];
        k[n / 2] = 0x00;
        nul.push(k);
    }
    sets.push(("embedded_nul", nul));

    // Seeded random binary keys of length 0..24.
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut rand_keys = Vec::new();
    for _ in 0..400 {
        let n = rng.below(25);
        let mut k = Vec::with_capacity(n);
        for _ in 0..n {
            // Bias hard towards 0x00 and 0xff, the interesting bytes.
            k.push(match rng.below(4) {
                0 => 0x00,
                1 => 0xff,
                _ => (rng.next() & 0xff) as u8,
            });
        }
        rand_keys.push(k);
    }
    sets.push(("random_binary", rand_keys));

    sets
}

#[derive(Clone, Copy, Debug)]
enum State {
    Memtable,
    Flushed,
    Compacted,
}

#[derive(Clone, Copy, Debug)]
enum Surface {
    DefaultCf,
    NamedCf,
    Snapshot,
    OwnedSnapshot,
}

fn oracle(keys: &[Vec<u8>]) -> BTreeMap<Vec<u8>, Vec<u8>> {
    keys.iter()
        .map(|k| (k.clone(), format!("len{}", k.len()).into_bytes()))
        .collect()
}

/// Walk every surface backwards and forwards and compare to the oracle.
/// Returns the list of mismatches.
fn check(set_name: &str, keys: &[Vec<u8>], state: State, surface: Surface) -> Vec<String> {
    let want = oracle(keys);
    let dir = TempDir::new().expect("tempdir");
    let opts = Options::default().write_buffer_size(match state {
        State::Memtable => 64 * 1024 * 1024,
        _ => 4 * 1024,
    });
    let db = Db::open(dir.path(), opts).expect("open");
    let cf = match surface {
        Surface::NamedCf => Some(db.create_column_family("attack").expect("create cf")),
        _ => None,
    };
    for (k, v) in &want {
        match &cf {
            Some(h) => regolith::through_stalls(|| db.put_cf(h, k, v)).expect("put_cf"),
            None => regolith::through_stalls(|| db.put(k, v)).expect("put"),
        }
    }
    match state {
        State::Memtable => {}
        State::Flushed => {
            // Rotate without compacting: a big put forces a flush.
            regolith::through_stalls(|| db.put(b"\x7f_flush_trigger", &vec![0u8; 8 * 1024]))
                .expect("put");
            regolith::through_stalls(|| db.delete(b"\x7f_flush_trigger")).expect("delete");
        }
        State::Compacted => db.compact_range(None, None).wait().expect("compact"),
    }

    let snap = db.snapshot();
    let mut got_back: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut got_fwd: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut status_err = None;

    macro_rules! walk {
        ($it:expr) => {{
            let mut it = $it;
            it.seek_to_last();
            while it.valid() {
                got_back.push((
                    it.key().expect("key").to_vec(),
                    it.value().expect("value").to_vec(),
                ));
                it.prev();
            }
            if let Err(e) = it.status() {
                status_err = Some(e.to_string());
            }
            it.seek_to_first();
            while it.valid() {
                got_fwd.push((
                    it.key().expect("key").to_vec(),
                    it.value().expect("value").to_vec(),
                ));
                it.next();
            }
            if let Err(e) = it.status() {
                status_err = Some(e.to_string());
            }
        }};
    }

    match surface {
        Surface::DefaultCf => walk!(db.iter()),
        Surface::NamedCf => walk!(db.iter_cf(cf.as_ref().expect("cf"))),
        Surface::Snapshot => walk!(snap.iter()),
        Surface::OwnedSnapshot => walk!(db.snapshot().owned_iter()),
    }

    got_back.reverse();
    let want_vec: Vec<(Vec<u8>, Vec<u8>)> = want
        .iter()
        .filter(|(k, _)| !k.starts_with(b"\x7f_flush_trigger"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let mut bad = Vec::new();
    let label = format!("{set_name}/{state:?}/{surface:?}");
    if let Some(e) = status_err {
        bad.push(format!("{label}: iterator reported an error: {e}"));
    }
    if got_fwd != want_vec {
        bad.push(format!(
            "{label}: forward walk gave {} entries, oracle has {}; first divergence {:?}",
            got_fwd.len(),
            want_vec.len(),
            first_divergence(&got_fwd, &want_vec),
        ));
    }
    if got_back != want_vec {
        bad.push(format!(
            "{label}: REVERSE walk gave {} entries, oracle has {}; first divergence {:?}",
            got_back.len(),
            want_vec.len(),
            first_divergence(&got_back, &want_vec),
        ));
    }
    if got_back != got_fwd {
        bad.push(format!(
            "{label}: forward and reverse walks disagree ({} vs {} entries)",
            got_fwd.len(),
            got_back.len(),
        ));
    }
    bad
}

fn first_divergence(
    got: &[(Vec<u8>, Vec<u8>)],
    want: &[(Vec<u8>, Vec<u8>)],
) -> Option<(String, String)> {
    for i in 0..got.len().max(want.len()) {
        let g = got.get(i).map(|(k, _)| hex(k));
        let w = want.get(i).map(|(k, _)| hex(k));
        if g != w {
            return Some((
                g.unwrap_or_else(|| "<missing>".into()),
                w.unwrap_or_else(|| "<missing>".into()),
            ));
        }
    }
    None
}

fn hex(b: &[u8]) -> String {
    if b.is_empty() {
        return "<empty>".to_string();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn reverse_iteration_matches_a_btreemap_on_every_adversarial_key_set() {
    let mut bad = Vec::new();
    let mut runs = 0usize;
    for (name, keys) in key_sets() {
        for state in [State::Memtable, State::Flushed, State::Compacted] {
            for surface in [
                Surface::DefaultCf,
                Surface::NamedCf,
                Surface::Snapshot,
                Surface::OwnedSnapshot,
            ] {
                runs += 1;
                bad.extend(check(name, &keys, state, surface));
            }
        }
    }
    println!(
        "reverse differential: {runs} (set, state, surface) runs, {} violations",
        bad.len()
    );
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

/// `seek_for_prev` at every key and just above and below it must land on
/// the oracle's answer, for the same adversarial key sets.
#[test]
fn seek_for_prev_matches_the_oracle_at_every_adversarial_probe() {
    let mut bad = Vec::new();
    let mut probes = 0usize;
    for (name, keys) in key_sets() {
        let want = oracle(&keys);
        let dir = TempDir::new().expect("tempdir");
        let db =
            Db::open(dir.path(), Options::default().write_buffer_size(4 * 1024)).expect("open");
        for (k, v) in &want {
            regolith::through_stalls(|| db.put(k, v)).expect("put");
        }
        db.compact_range(None, None).wait().expect("compact");

        let mut targets: Vec<Vec<u8>> = want.keys().cloned().collect();
        for k in want.keys() {
            let mut lo = k.clone();
            if let Some(last) = lo.last_mut() {
                *last = last.wrapping_sub(1);
                targets.push(lo);
            }
            let mut hi = k.clone();
            hi.push(0x00);
            targets.push(hi);
            let mut hi2 = k.clone();
            hi2.push(0xff);
            targets.push(hi2);
        }
        targets.push(vec![0xff; 32]);
        targets.push(Vec::new());
        targets.sort();
        targets.dedup();

        for t in &targets {
            probes += 1;
            let mut it = db.iter();
            it.seek_for_prev(t);
            let got = if it.valid() {
                Some((
                    it.key().expect("key").to_vec(),
                    it.value().expect("value").to_vec(),
                ))
            } else {
                None
            };
            it.status().expect("seek_for_prev status");
            let expect = want
                .range::<[u8], _>((
                    std::ops::Bound::Unbounded,
                    std::ops::Bound::Included(t.as_slice()),
                ))
                .next_back()
                .map(|(k, v)| (k.clone(), v.clone()));
            if got != expect {
                bad.push(format!(
                    "{name}: seek_for_prev({}) gave {:?}, oracle says {:?}",
                    hex(t),
                    got.as_ref().map(|(k, _)| hex(k)),
                    expect.as_ref().map(|(k, _)| hex(k)),
                ));
            }
        }
    }
    println!(
        "seek_for_prev differential: {probes} probes, {} violations",
        bad.len()
    );
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}
