//! Adversarial probes for the reverse-iteration upper-bound probe.
//!
//! The defect was a probe built as "the CF upper bound with its last
//! byte decremented, followed by eight `0xff` bytes", which is only an
//! upper bound over user keys of eight bytes or fewer. Every longer key
//! beginning with eight `0xff` bytes was unreachable backwards.
//!
//! These tests hammer the shapes that broke it and the shapes next to
//! them: keys of every length across the eight-byte boundary, keys that
//! are all `0xff`, keys that are prefixes of one another, the empty key,
//! and a randomized sweep over an alphabet of `0x00` and `0xff` where
//! every key is a prefix of some other.
//!
//! Every case asserts the same three things: `seek_to_last` lands on the
//! greatest key, a full reverse walk visits exactly the keys a full
//! forward walk visits in exactly the mirrored order, and
//! `seek_for_prev` on every key and on every key extended by one byte
//! lands where a sorted set says it should.
#![cfg(not(target_arch = "wasm32"))]

use regolith::{Db, Options};
use tempfile::TempDir;

fn opts() -> Options {
    Options::default().write_buffer_size(4096).block_size(128)
}

fn forward(db: &Db) -> Vec<Vec<u8>> {
    let mut it = db.iter();
    it.seek_to_first();
    let mut out = Vec::new();
    while it.valid() {
        out.push(it.key().expect("key").to_vec());
        it.next();
    }
    it.status().expect("iterator error");
    out
}

fn reverse(db: &Db) -> Vec<Vec<u8>> {
    let mut it = db.iter();
    it.seek_to_last();
    let mut out = Vec::new();
    while it.valid() {
        out.push(it.key().expect("key").to_vec());
        it.prev();
    }
    it.status().expect("iterator error");
    out
}

fn reverse_snapshot(db: &Db) -> Vec<Vec<u8>> {
    let snap = db.snapshot();
    let mut it = snap.iter();
    it.seek_to_last();
    let mut out = Vec::new();
    while it.valid() {
        out.push(it.key().expect("key").to_vec());
        it.prev();
    }
    it.status().expect("iterator error");
    out
}

/// Run the whole battery over one key set, in memtable and after a
/// flush plus compaction, and on the default CF and a named one.
fn check(label: &str, keys: &[Vec<u8>]) {
    let mut sorted: Vec<Vec<u8>> = keys.to_vec();
    sorted.sort();
    sorted.dedup();

    for (stage, compact) in [("memtable", false), ("compacted", true)] {
        let dir = TempDir::new().expect("tempdir");
        let db = Db::open(dir.path(), opts()).expect("open");
        for k in &sorted {
            regolith::through_stalls(|| db.put(k, b"v")).expect("put");
        }
        if compact {
            db.compact_range(None, None).wait().expect("compact_range");
        }
        let ctx = format!("{label} [{stage}]");

        assert_eq!(forward(&db), sorted, "{ctx}: forward walk");

        let mut want_rev = sorted.clone();
        want_rev.reverse();
        assert_eq!(
            reverse(&db),
            want_rev,
            "{ctx}: reverse walk from seek_to_last"
        );
        assert_eq!(
            reverse_snapshot(&db),
            want_rev,
            "{ctx}: reverse walk through a Snapshot",
        );

        // seek_to_last must land on the greatest key, not merely
        // somewhere reachable.
        let mut it = db.iter();
        it.seek_to_last();
        assert!(it.valid(), "{ctx}: seek_to_last found nothing");
        assert_eq!(
            it.key().expect("key"),
            sorted.last().expect("non-empty").as_slice(),
            "{ctx}: seek_to_last did not land on the greatest key",
        );

        // seek_for_prev on every key, and on every key with one more
        // byte appended, must land where a sorted set says.
        let mut probes: Vec<Vec<u8>> = sorted.clone();
        for k in &sorted {
            for extra in [0x00u8, 0x01, 0x7f, 0xfe, 0xff] {
                let mut p = k.clone();
                p.push(extra);
                probes.push(p);
            }
        }
        probes.push(Vec::new());
        probes.push(vec![0xff; 32]);
        for p in &probes {
            let want = sorted.iter().rev().find(|k| k.as_slice() <= p.as_slice());
            let mut it = db.iter();
            it.seek_for_prev(p);
            let got = if it.valid() {
                Some(it.key().expect("key").to_vec())
            } else {
                None
            };
            it.status().expect("iterator error");
            assert_eq!(
                got.as_deref(),
                want.map(|v| v.as_slice()),
                "{ctx}: seek_for_prev({p:02x?}) landed wrong",
            );
        }
    }

    // The same key set inside a named column family, whose upper bound
    // is a different four-byte prefix.
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), opts()).expect("open");
    let cf = db
        .create_column_family("attack")
        .expect("create_column_family");
    for k in &sorted {
        regolith::through_stalls(|| db.put_cf(&cf, k, b"v")).expect("put_cf");
    }
    // A neighbour CF holding keys that sort after this one's upper
    // bound, so a probe that overshoots would pick one of them up.
    let other = db
        .create_column_family("zzz_neighbour")
        .expect("create_column_family");
    for k in &sorted {
        regolith::through_stalls(|| db.put_cf(&other, k, b"other")).expect("put_cf");
    }
    db.compact_range(None, None).wait().expect("compact_range");

    let mut it = db.iter_cf(&cf);
    it.seek_to_last();
    let mut got = Vec::new();
    while it.valid() {
        got.push(it.key().expect("key").to_vec());
        it.prev();
    }
    it.status().expect("iterator error");
    let mut want_rev = sorted.clone();
    want_rev.reverse();
    assert_eq!(
        got, want_rev,
        "{label} [cf]: reverse walk inside a named CF"
    );
}

/// The exact shape the defect was measured on, plus its neighbours:
/// every key length from 0 to 12 bytes, all `0xff`.
#[test]
fn all_ff_keys_of_every_length_around_the_eight_byte_boundary() {
    let keys: Vec<Vec<u8>> = (0..=12).map(|n| vec![0xffu8; n]).collect();
    check("all-0xff of every length", &keys);
}

/// The single nine-byte key the gap report names, alone in the
/// database: `ffffffffffffffff01`.
#[test]
fn the_single_nine_byte_key_above_the_old_probe_is_reachable_backwards() {
    let mut k = vec![0xffu8; 8];
    k.push(0x01);
    check("ffffffffffffffff01 alone", &[k]);
}

/// Keys that are prefixes of each other across the boundary, mixing
/// `0x00` and `0xff` continuations so ordering is not decided by length
/// alone.
#[test]
fn keys_that_are_prefixes_of_each_other_across_the_boundary() {
    let mut keys = Vec::new();
    for n in 0..=10usize {
        let base = vec![0xffu8; n];
        keys.push(base.clone());
        for tail in [0x00u8, 0x01, 0x80, 0xfe] {
            let mut k = base.clone();
            k.push(tail);
            keys.push(k);
        }
    }
    check("0xff prefixes with mixed tails", &keys);
}

/// The empty key next to the largest keys there are.
#[test]
fn the_empty_key_next_to_the_largest_keys() {
    let keys = vec![
        Vec::new(),
        vec![0x00],
        vec![0x00, 0x00],
        vec![0xff; 8],
        vec![0xff; 9],
        vec![0xff; 64],
        {
            let mut k = vec![0xff; 8];
            k.extend_from_slice(&[0x00; 8]);
            k
        },
    ];
    check("empty key with maximal neighbours", &keys);
}

/// A randomized sweep over the alphabet the defect lives in: every key
/// is a string of `0x00` and `0xff` bytes of length 0 to 16, so many are
/// prefixes of others and many begin with eight `0xff` bytes. Seeded, so
/// a failure reproduces byte for byte.
#[test]
fn a_seeded_sweep_over_binary_keys_of_every_length() {
    let mut state = 0x243F_6A88_85A3_08D3u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for round in 0..24 {
        let mut keys = Vec::new();
        for _ in 0..40 {
            let len = (next() % 17) as usize;
            let bits = next();
            let k: Vec<u8> = (0..len)
                .map(|i| {
                    if (bits >> (i % 64)) & 1 == 1 {
                        0xffu8
                    } else {
                        0x00
                    }
                })
                .collect();
            keys.push(k);
        }
        // Guarantee the boundary shapes are present in every round.
        keys.push(vec![0xff; 8]);
        keys.push({
            let mut k = vec![0xff; 8];
            k.push(0x01);
            k
        });
        check(&format!("seeded round {round}"), &keys);
    }
}
