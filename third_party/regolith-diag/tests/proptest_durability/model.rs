//! The reference model, the shared workload generator and the failure
//! reporting helpers behind `tests/proptest_durability.rs`.
//!
//! Two things live here rather than in the test file. The first is the
//! `BTreeMap` model every assertion in that file is measured against,
//! together with the one function that applies an [`Op`] to a real
//! database, so regolith and the model can never be driven by two different
//! readings of the same generated sequence. The second is the seeded
//! generator: the crash test regenerates its operation sequence inside a
//! child process from the seed alone, which `proptest`'s RNG cannot
//! cross a process boundary to do.
//!
//! Compiled as a submodule of the `proptest_durability` test crate
//! (`tests/proptest_durability/model.rs`); cargo does not pick the
//! directory up as a test target of its own because it holds no
//! `main.rs`.

use std::collections::BTreeMap;
use std::sync::Arc;

use regolith::{Db, DurabilityMode, MergeOperator, Options, WriteBatch, through_stalls};

// ── the model ──────────────────────────────────────────────────

/// One write. Every variant maps to exactly one regolith call, or to one
/// operation inside a [`WriteBatch`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unit {
    Put(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
    DeleteRange(Vec<u8>, Vec<u8>),
    Merge(Vec<u8>, Vec<u8>),
}

/// One atomic step of a generated sequence. `Batch` is submitted as a
/// single `WriteBatch`, so recovery may keep all of it or none of it and
/// never part of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Single(Unit),
    Batch(Vec<Unit>),
    Compact,
}

pub(crate) type Model = BTreeMap<Vec<u8>, Vec<u8>>;

pub(crate) fn apply_to_db(db: &Db, op: &Op) -> regolith::Result<()> {
    match op {
        Op::Single(Unit::Put(k, v)) => through_stalls(|| db.put(k, v)),
        Op::Single(Unit::Delete(k)) => through_stalls(|| db.delete(k)),
        Op::Single(Unit::DeleteRange(s, e)) => through_stalls(|| db.delete_range(s, e)),
        Op::Single(Unit::Merge(k, o)) => through_stalls(|| db.merge(k, o)),
        Op::Batch(units) => {
            let mut batch = WriteBatch::new();
            for unit in units {
                match unit {
                    Unit::Put(k, v) => batch.put(k, v),
                    Unit::Delete(k) => batch.delete(k),
                    Unit::DeleteRange(s, e) => batch.delete_range(s, e),
                    Unit::Merge(k, o) => batch.merge(k, o),
                }
            }
            through_stalls(|| db.write(batch.clone()))
        }
        Op::Compact => db.compact_range(None, None).wait(),
    }
}

/// The reference semantics, written out in full because every assertion
/// in this file is measured against them.
///
/// * a put replaces the value,
/// * a delete removes the key,
/// * a range delete removes `[start, end)` and is a no-op when
///   `start >= end`, which is what both `Db::delete_range` and
///   `WriteBatch::delete_range` do,
/// * a merge appends its operand to the current value (or to nothing),
///   which is exactly [`Append`], the operator configured below.
pub(crate) fn apply_to_model(model: &mut Model, op: &Op) {
    for unit in op_units(op) {
        match unit {
            Unit::Put(k, v) => {
                model.insert(k.clone(), v.clone());
            }
            Unit::Delete(k) => {
                model.remove(k);
            }
            Unit::DeleteRange(s, e) => {
                if s < e {
                    model.retain(|k, _| k < s || k >= e);
                }
            }
            Unit::Merge(k, o) => {
                model.entry(k.clone()).or_default().extend_from_slice(o);
            }
        }
    }
}

fn op_units(op: &Op) -> &[Unit] {
    match op {
        Op::Single(unit) => std::slice::from_ref(unit),
        Op::Batch(units) => units,
        Op::Compact => &[],
    }
}

pub(crate) fn model_state(model: &Model) -> Vec<(Vec<u8>, Vec<u8>)> {
    model.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// The merge operator used everywhere in this file: append the operand
/// to the base. It is associative, so folding operands one at a time
/// (what the model does) and folding a whole chain at once (what regolith
/// does at read time) must give the same bytes. `partial_merge` is
/// implemented too, so compaction-time operand collapsing is exercised
/// rather than skipped.
struct Append;

impl MergeOperator for Append {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        for operand in operands {
            out.extend_from_slice(operand);
        }
        Some(out)
    }

    fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        let mut out = left.to_vec();
        out.extend_from_slice(right);
        Some(out)
    }

    fn name(&self) -> &'static str {
        "append"
    }
}

pub(crate) fn opts(write_buffer_size: usize, durability: DurabilityMode) -> Options {
    Options::default()
        .write_buffer_size(write_buffer_size)
        .durability(durability)
        .merge_operator(Some(Arc::new(Append)))
}

/// Small enough that a generated sequence flushes several memtables and
/// compacts, so the tests reach the SSTable and recovery paths rather
/// than only the memtable.
pub(crate) const WRITE_BUFFER: usize = 4 * 1024;

/// The key pool. Deliberately full of prefix relations, embedded zero
/// bytes and a `0xff` tail: those are the shapes the internal-key
/// comparator has to get right, since an internal key is
/// `user_key || !seq || value_type` and a naive byte comparison of two
/// encoded keys is wrong exactly when one user key is a prefix of
/// another.
pub(crate) const KEYS: &[&[u8]] = &[
    b"",
    b"a",
    b"a\x00",
    b"ab",
    b"abc",
    b"b",
    b"b\xff",
    b"m",
    b"m\x00\x00",
    b"z",
];

/// The nine bytes an internal key carries after the user key: `!seq` for
/// sequence number 0 followed by the `VALUE` type tag. A user key ending
/// in this pattern is the adversarial case for any code that splits an
/// internal key by scanning rather than by length.
pub(crate) const SUFFIX_LOOKALIKE: [u8; 9] = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01];

// ── reporting helpers ──────────────────────────────────────────

pub(crate) fn show(bytes: &[u8]) -> String {
    let head: String = bytes
        .iter()
        .take(24)
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join("");
    if bytes.len() > 24 {
        format!("{head}..({} bytes)", bytes.len())
    } else {
        format!("{head}({} bytes)", bytes.len())
    }
}

/// A compact description of the first place two states differ, so a
/// failure names one key instead of dumping two key-value dumps.
pub(crate) fn first_difference(
    got: &[(Vec<u8>, Vec<u8>)],
    want: &[(Vec<u8>, Vec<u8>)],
) -> Option<String> {
    for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
        if g.0 != w.0 {
            return Some(format!(
                "entry {i}: regolith has key {}, model has key {}",
                show(&g.0),
                show(&w.0),
            ));
        }
        if g.1 != w.1 {
            return Some(format!(
                "key {}: regolith has value {}, model has value {}",
                show(&g.0),
                show(&g.1),
                show(&w.1),
            ));
        }
    }
    match got.len().cmp(&want.len()) {
        std::cmp::Ordering::Greater => Some(format!(
            "regolith has {} extra entrie(s), first is key {}",
            got.len() - want.len(),
            show(&got[want.len()].0),
        )),
        std::cmp::Ordering::Less => Some(format!(
            "regolith is missing {} entrie(s), first is key {}",
            want.len() - got.len(),
            show(&want[got.len()].0),
        )),
        std::cmp::Ordering::Equal => None,
    }
}

// ── the seeded sequence, regenerated in parent and child ──────

/// Phase name for the workload below. Any other phase falls through to
/// the harness's built-in workload, so this entry point stays usable by
/// the rest of the substrate.
pub(crate) const CRASH_PHASE: &str = "model_ops";
pub(crate) const CRASH_VALUE_LEN: usize = 160;

/// xorshift64. The operation sequence has to be regenerated bit for bit
/// in a different process from the seed alone, so it cannot come from
/// `proptest`'s RNG.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// The same sequence in the parent and in the child, from the seed, the
/// operation count and the value length: every one of those crosses the
/// process boundary in the `ChildSpec` environment, so the two can never
/// drift.
pub(crate) fn ops_from_seed(seed: u64, count: usize, value_len: usize) -> Vec<Op> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        if rng.below(4) == 0 {
            let n = 2 + rng.below(3);
            let mut units = Vec::with_capacity(n);
            for j in 0..n {
                units.push(unit_from_seed(&mut rng, i * 8 + j, value_len));
            }
            out.push(Op::Batch(units));
        } else {
            out.push(Op::Single(unit_from_seed(&mut rng, i * 8, value_len)));
        }
    }
    out
}

fn unit_from_seed(rng: &mut Rng, tag: usize, value_len: usize) -> Unit {
    let key = KEYS[rng.below(KEYS.len())].to_vec();
    match rng.below(10) {
        0..=5 => Unit::Put(key, value_from_seed(rng, tag, value_len)),
        6..=7 => Unit::Delete(key),
        8 => Unit::Merge(key, format!("+{tag:04}").into_bytes()),
        _ => {
            let other = KEYS[rng.below(KEYS.len())].to_vec();
            let (s, e) = if key <= other {
                (key, other)
            } else {
                (other, key)
            };
            Unit::DeleteRange(s, e)
        }
    }
}

/// The value carries the operation tag, so a mismatch names the write
/// that produced it instead of dumping bytes.
fn value_from_seed(rng: &mut Rng, tag: usize, value_len: usize) -> Vec<u8> {
    let mut v = format!("v{tag:06}#").into_bytes();
    while v.len() < value_len {
        v.push(b'a' + (rng.next_u64() % 26) as u8);
    }
    v
}

/// Which operation the child dies after. Derived from the seed so the
/// parent knows it exactly, and clamped into `1..=count` so the crash
/// always fires: a crash test whose crash never fired is a false green.
pub(crate) fn kill_after(seed: u64, count: usize) -> usize {
    let lo = (count / 4).max(1);
    let span = (count - lo + 1) as u64;
    lo + (seed.rotate_left(17) % span) as usize
}

// ── prefix checking ────────────────────────────────────────────

/// Every prefix length whose model state equals `state`. More than one
/// can match, because an operation can leave the state unchanged.
///
/// The substrate's own `validate_prefix` is not used here: its `History`
/// type models point puts and deletes only, and the whole point of this
/// file is to crash a workload that also issues range deletes and merge
/// operands. The ordering, reverse-scan and point-lookup checks in
/// `fault::recovered_state` are reused as they are.
pub(crate) fn matching_prefixes(state: &[(Vec<u8>, Vec<u8>)], sequence: &[Op]) -> Vec<usize> {
    let mut model = Model::new();
    let mut out = Vec::new();
    if model_state(&model) == state {
        out.push(0);
    }
    for (i, op) in sequence.iter().enumerate() {
        apply_to_model(&mut model, op);
        if model_state(&model) == state {
            out.push(i + 1);
        }
    }
    out
}

/// How many keys two states disagree on, counting a key present in one
/// and absent from the other.
fn differing_keys(a: &[(Vec<u8>, Vec<u8>)], b: &[(Vec<u8>, Vec<u8>)]) -> usize {
    let am: BTreeMap<&[u8], &[u8]> = a
        .iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect();
    let bm: BTreeMap<&[u8], &[u8]> = b
        .iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect();
    am.iter().filter(|(k, v)| bm.get(*k) != Some(v)).count()
        + bm.keys().filter(|k| !am.contains_key(*k)).count()
}

/// The prefix that explains the most of a recovered state, for the
/// failure message: a report against `k = 0` would point at the wrong
/// place when recovery went wrong near the end.
pub(crate) fn closest_prefix(state: &[(Vec<u8>, Vec<u8>)], sequence: &[Op]) -> String {
    let mut model = Model::new();
    let mut best = (0usize, usize::MAX, String::new());
    for k in 0..=sequence.len() {
        if k > 0 {
            apply_to_model(&mut model, &sequence[k - 1]);
        }
        let want = model_state(&model);
        let diffs = differing_keys(state, &want);
        if diffs < best.1 {
            best = (k, diffs, first_difference(state, &want).unwrap_or_default());
        }
    }
    format!(
        "The closest prefix is k={} with {} differing key(s): {}",
        best.0, best.1, best.2,
    )
}
