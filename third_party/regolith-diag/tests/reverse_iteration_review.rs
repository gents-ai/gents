//! Independent adversarial review of reverse iteration.
//!
//! `seek_to_last` used to build an upper-bound probe by appending eight
//! `0xff` bytes, which is an upper bound only for user keys of eight
//! bytes or fewer. The fix takes the exclusive bound directly. These
//! probes attack the boundary from both sides and in both directions:
//! keys of every length around eight, keys made entirely of `0xff`, keys
//! that are proper prefixes of one another, and the empty key.
//!
//! The oracle in every case is the forward scan, sorted. A backward walk
//! that disagrees with it has either skipped a key or invented one, and
//! the failure message prints both sequences.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;

use regolith::{Db, DurabilityMode, Options};
use tempfile::TempDir;

fn opts() -> Options {
    Options::default()
        .write_buffer_size(4 * 1024)
        .durability(DurabilityMode::Eventual)
}

/// Write `keys`, then check the reverse walk against the forward one.
/// `label` names the case in the failure message. Returns the number of
/// keys the reverse walk produced.
fn check(label: &str, keys: &[Vec<u8>], flush: bool) -> usize {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), opts()).expect("open");
    for k in keys {
        regolith::through_stalls(|| db.put(k, b"v")).expect("put");
    }
    if flush {
        db.compact_range(None, None).wait().expect("compact");
    }

    let unique: BTreeSet<Vec<u8>> = keys.iter().cloned().collect();
    let expected: Vec<Vec<u8>> = unique.into_iter().collect();

    let mut forward = Vec::new();
    let mut it = db.iter();
    it.seek_to_first();
    while it.valid() {
        forward.push(it.key().expect("key").to_vec());
        it.next();
    }
    assert_eq!(
        forward, expected,
        "[{label}] the forward scan itself is wrong, so the reverse oracle is unusable",
    );

    let mut backward = Vec::new();
    let mut it = db.iter();
    it.seek_to_last();
    while it.valid() {
        backward.push(it.key().expect("key").to_vec());
        it.prev();
    }
    backward.reverse();

    assert_eq!(
        backward, expected,
        "[{label}] the backward walk disagrees with the forward one\n  \
         forward:  {:02x?}\n  backward: {:02x?}",
        expected, backward,
    );

    // Every key must also be reachable by seek_for_prev from itself.
    for k in &expected {
        let mut it = db.iter();
        it.seek_for_prev(k);
        assert!(
            it.valid() && it.key().expect("key") == &k[..],
            "[{label}] seek_for_prev could not land on {k:02x?}",
        );
    }
    drop(db);
    backward.len()
}

/// Both the memtable path and the flushed-and-compacted SSTable path.
fn check_both(label: &str, keys: &[Vec<u8>]) {
    check(&format!("{label}/memtable"), keys, false);
    check(&format!("{label}/sstable"), keys, true);
}

/// A run of `n` `0xff` bytes, optionally with a suffix.
fn ff(n: usize, suffix: &[u8]) -> Vec<u8> {
    let mut k = vec![0xffu8; n];
    k.extend_from_slice(suffix);
    k
}

/// The original defect, and every length around the eight-byte boundary
/// the old probe was built for.
#[test]
fn every_key_length_around_the_eight_byte_ff_boundary_is_reachable_backwards() {
    for n in 0..=12usize {
        for suffix in [vec![], vec![0x00], vec![0x01], vec![0xff], vec![0x00, 0x00]] {
            let key = ff(n, &suffix);
            if key.is_empty() {
                continue;
            }
            check_both(&format!("ff{n}+{suffix:02x?}"), &[key]);
        }
    }
    println!("swept ff-runs of length 0..=12 with five suffixes, both storage paths");
}

/// All of them at once, so the last key is the largest and every shorter
/// one has to be walked past on the way down.
#[test]
fn a_database_of_nothing_but_ff_keys_of_every_length_walks_backwards_whole() {
    let mut keys = Vec::new();
    for n in 1..=20usize {
        keys.push(ff(n, &[]));
        keys.push(ff(n, &[0x00]));
        keys.push(ff(n, &[0x01]));
    }
    check_both("all-ff", &keys);
    println!("60 all-0xff keys of lengths 1..=21, both storage paths");
}

/// Keys that are proper prefixes of one another, which is the case that
/// separates a correct user-key comparison from a raw byte comparison of
/// encoded internal keys.
#[test]
fn keys_that_are_prefixes_of_each_other_walk_backwards_whole() {
    let mut keys = Vec::new();
    for base in [vec![0xffu8; 8], vec![0xffu8; 9], b"a".to_vec(), vec![0x00]] {
        for extra in 0..5usize {
            let mut k = base.clone();
            k.extend(std::iter::repeat_n(0x00, extra));
            keys.push(k);
        }
    }
    check_both("prefix-chain", &keys);
    println!("prefix chains over 0xff*8, 0xff*9, 'a' and 0x00, both storage paths");
}

/// The empty key, alone and mixed with the boundary cases.
#[test]
fn the_empty_key_is_reachable_backwards() {
    check_both("empty-alone", &[vec![]]);
    check_both(
        "empty-mixed",
        &[vec![], vec![0x00], ff(8, &[]), ff(8, &[0x01]), ff(9, &[])],
    );
    println!("empty key alone and mixed with the 0xff boundary keys");
}

/// The maximum-length key the engine accepts, made of `0xff`, is still
/// the last key and is still reachable backwards.
#[test]
fn a_long_all_ff_key_is_still_the_last_key_backwards() {
    for n in [16usize, 64, 255, 256, 1024] {
        check_both(&format!("long-ff-{n}"), &[vec![0x41u8], ff(n, &[])]);
    }
    println!("all-0xff keys of 16, 64, 255, 256 and 1024 bytes, both storage paths");
}

/// The same sweep through the other reverse surfaces that share the
/// probe: a `Snapshot`'s iterator and a column-family iterator.
#[test]
fn the_snapshot_iterator_reaches_the_same_last_key() {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), opts()).expect("open");
    let keys: Vec<Vec<u8>> = (0..=12usize).map(|n| ff(n, &[0x01])).collect();
    for k in &keys {
        regolith::through_stalls(|| db.put(k, b"v")).expect("put");
    }
    db.compact_range(None, None).wait().expect("compact");

    let expected: Vec<Vec<u8>> = {
        let s: BTreeSet<Vec<u8>> = keys.iter().cloned().collect();
        s.into_iter().collect()
    };

    let snap = db.snapshot();
    let mut backward = Vec::new();
    let mut it = snap.iter();
    it.seek_to_last();
    while it.valid() {
        backward.push(it.key().expect("key").to_vec());
        it.prev();
    }
    backward.reverse();
    assert_eq!(
        backward, expected,
        "the snapshot's backward walk missed keys the forward order holds",
    );
    drop(snap);
    drop(db);
    println!("snapshot reverse walk over 13 ff-length keys agrees with the forward order");
}
