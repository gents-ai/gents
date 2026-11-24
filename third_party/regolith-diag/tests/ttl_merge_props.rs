//! A TTL database with a merge operator reads the same before and after a
//! flush, a compaction and a reopen, whatever was written when, and reads
//! what its rule says: each write expires by its own time, and a write with
//! no time (made past the wrapper, as release 0.1.9 made every merge) is
//! folded as stored.
//!
//! The steps are puts, merges and deletes on a few keys, puts and merges
//! through the inner database, the clock moved on at random, flushes, one
//! compaction pass at a time, full compactions and reopens. Level sizes are
//! tiny and a single level-0 table is due for compaction, so a pass often
//! rewrites a value while a newer merge of its key sits in a table or a
//! memtable the pass does not read.

// Native-only. wasm-pack builds every test target for wasm32, and the browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::sync::Arc;

use proptest::prelude::*;
use regolith::{DbWithTtl, MemEnv, MergeOperator, Options, WriteBatch};

/// Seconds a write lives.
const TTL: u64 = 10;

/// The keys the steps write.
const KEYS: u8 = 3;

/// The stamp a TTL database puts behind a value: magic, version 1, and the
/// second it was written at.
const STAMP_LEN: usize = 13;

/// Applies the operands oldest first: one that starts with `<` goes in
/// front of the value, any other at the end, so a fold of stored writes
/// keeps the put's stamp at the end, or does not.
struct Mix;

fn mix<'a>(base: Option<Vec<u8>>, operands: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut out = base.unwrap_or_default();
    for operand in operands {
        if operand.first() == Some(&b'<') {
            out.splice(0..0, operand.iter().copied());
        } else {
            out.extend_from_slice(operand);
        }
    }
    out
}

impl MergeOperator for Mix {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        Some(mix(base.map(<[u8]>::to_vec), operands.iter().copied()))
    }

    fn name(&self) -> &'static str {
        "mix"
    }
}

#[derive(Clone, Debug)]
enum Op {
    Put(u8, u8),
    Merge(u8, u8),
    Delete(u8),
    /// A put through the inner database: no stamp.
    RawPut(u8, u8),
    /// A merge through the inner database: no stamp.
    RawMerge(u8, u8),
    /// Move the wall clock on by this many seconds.
    Advance(u8),
    Flush,
    /// One compaction pass, which may leave newer tables out.
    CompactStep,
    /// A compaction of the whole database.
    CompactAll,
    Reopen,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0..KEYS, any::<u8>()).prop_map(|(k, v)| Op::Put(k, v)),
        4 => (0..KEYS, any::<u8>()).prop_map(|(k, v)| Op::Merge(k, v)),
        1 => (0..KEYS).prop_map(Op::Delete),
        1 => (0..KEYS, any::<u8>()).prop_map(|(k, v)| Op::RawPut(k, v)),
        2 => (0..KEYS, any::<u8>()).prop_map(|(k, v)| Op::RawMerge(k, v)),
        3 => (0u8..=12).prop_map(Op::Advance),
        2 => Just(Op::Flush),
        2 => Just(Op::CompactStep),
        1 => Just(Op::CompactAll),
        1 => Just(Op::Reopen),
    ]
}

fn key(k: u8) -> Vec<u8> {
    vec![b'k', k]
}

/// A merge operand: odd bytes go in front, even ones at the end.
fn operand(tag: u8, v: u8) -> Vec<u8> {
    vec![if v % 2 == 1 { b'<' } else { tag }, v]
}

fn open(env: &Arc<MemEnv>) -> DbWithTtl {
    let options = Options::memory()
        .env(env.clone())
        .merge_operator(Some(Arc::new(Mix)))
        // Compaction runs only when a step asks for it.
        .inline_compaction(false)
        .l0_compaction_trigger(1)
        .level_base_bytes(1);
    DbWithTtl::open("ttl-props", options, TTL).unwrap()
}

/// What every key reads, by a point read, and what a scan of the whole
/// database returns.
type Reads = (Vec<Option<Vec<u8>>>, Vec<(Vec<u8>, Vec<u8>)>);

fn reads(db: &DbWithTtl) -> Reads {
    let points = (0..KEYS).map(|k| db.get(&key(k)).unwrap()).collect();
    (points, db.scan(None, None).unwrap())
}

/// One write: what was written, and the second it was written at, or
/// `None` for a write through the inner database.
type Write = (Vec<u8>, Option<u64>);

/// One key's writes: the put the merges apply to and the operands merged
/// since.
type Writes = (Option<Write>, Vec<Write>);

/// A write as the database stores it: stamped, when it has a time.
fn as_stored((value, at): &Write) -> Vec<u8> {
    let mut out = value.clone();
    if let Some(at) = at {
        out.extend_from_slice(b"RTTL");
        out.push(1);
        out.extend_from_slice(&at.to_be_bytes());
    }
    out
}

/// What `stored` holds in front of its stamp, if it ends in one that is
/// live under `live`.
fn stamped_value(mut stored: Vec<u8>, live: impl Fn(u64) -> bool) -> Option<Vec<u8>> {
    let start = stored.len().checked_sub(STAMP_LEN)?;
    let suffix = &stored[start..];
    if &suffix[..4] != b"RTTL" || suffix[4] != 1 {
        return None;
    }
    let at = u64::from_be_bytes(suffix[5..].try_into().unwrap());
    stored.truncate(start);
    live(at).then_some(stored)
}

/// The rule, written out: per key, the base a put left and the operands
/// merged since, each with the second it was written at, if it has one.
#[derive(Default)]
struct Model {
    keys: BTreeMap<u8, Writes>,
}

impl Model {
    fn read(&self, k: u8, now: u64) -> Option<Vec<u8>> {
        let (base, operands) = self.keys.get(&k)?;
        let horizon = now.saturating_sub(TTL);
        let live = |at: u64| horizon == 0 || at >= horizon;
        // A write that has expired is left out of every fold.
        let base = base.as_ref().filter(|(_, at)| at.is_none_or(live));
        let operands: Vec<&Write> = operands
            .iter()
            .filter(|(_, at)| at.is_none_or(live))
            .collect();
        let (base, rest) = match operands.iter().rposition(|(_, at)| at.is_none()) {
            // Up to the last merge with no time, the writes fold as stored,
            // and what comes out reads as a stamped value.
            Some(last) => {
                let stored: Vec<Vec<u8>> = operands[..=last].iter().map(|w| as_stored(w)).collect();
                let folded = mix(base.map(as_stored), stored.iter().map(Vec::as_slice));
                (stamped_value(folded, live), &operands[last + 1..])
            }
            // A put with no time is not seen.
            None => (
                base.filter(|(_, at)| at.is_some()).map(|(v, _)| v.clone()),
                &operands[..],
            ),
        };
        if rest.is_empty() {
            return base;
        }
        Some(mix(base, rest.iter().map(|(v, _)| v.as_slice())))
    }

    fn reads(&self, now: u64) -> Reads {
        let points: Vec<Option<Vec<u8>>> = (0..KEYS).map(|k| self.read(k, now)).collect();
        let scan = (0..KEYS)
            .filter_map(|k| points[usize::from(k)].clone().map(|v| (key(k), v)))
            .collect();
        (points, scan)
    }
}

/// Runs `step` on the database, the model and the clock, and hands back
/// the database, reopened when the step says so.
fn apply(
    step: &Op,
    env: &Arc<MemEnv>,
    db: DbWithTtl,
    model: &mut Model,
    now: &mut u64,
) -> DbWithTtl {
    match step {
        Op::Put(k, v) => {
            db.put(&key(*k), &[b'p', *v]).unwrap();
            model
                .keys
                .insert(*k, (Some((vec![b'p', *v], Some(*now))), Vec::new()));
        }
        Op::Merge(k, v) => {
            let mut batch = WriteBatch::new();
            batch.merge(&key(*k), &operand(b'm', *v));
            db.write(batch).unwrap();
            let writes = &mut model.keys.entry(*k).or_default().1;
            writes.push((operand(b'm', *v), Some(*now)));
        }
        Op::Delete(k) => {
            db.delete(&key(*k)).unwrap();
            model.keys.remove(k);
        }
        Op::RawPut(k, v) => {
            db.inner().put(&key(*k), &[b'q', *v]).unwrap();
            model
                .keys
                .insert(*k, (Some((vec![b'q', *v], None)), Vec::new()));
        }
        Op::RawMerge(k, v) => {
            db.inner().merge(&key(*k), &operand(b'r', *v)).unwrap();
            let writes = &mut model.keys.entry(*k).or_default().1;
            writes.push((operand(b'r', *v), None));
        }
        Op::Advance(secs) => {
            env.advance_micros(u64::from(*secs) * 1_000_000);
            *now += u64::from(*secs);
        }
        Op::Flush => db.inner().flush().unwrap(),
        Op::CompactStep => {
            db.inner().compact_step().unwrap();
        }
        Op::CompactAll => db.compact_range(None, None).wait().unwrap(),
        Op::Reopen => {
            db.close().unwrap();
            drop(db);
            return open(env);
        }
    }
    db
}

/// A flush, a compaction pass, a full compaction or a reopen never changes
/// what a read returns.
fn check_steps_keep_reads(steps: &[Op]) -> Result<(), TestCaseError> {
    let env = Arc::new(MemEnv::new());
    let mut db = open(&env);
    let (mut model, mut now) = (Model::default(), 0);
    for step in steps {
        let before = reads(&db);
        db = apply(step, &env, db, &mut model, &mut now);
        if matches!(
            step,
            Op::Flush | Op::CompactStep | Op::CompactAll | Op::Reopen
        ) {
            prop_assert_eq!(&reads(&db), &before, "{:?} changed a read", step);
        }
    }
    Ok(())
}

/// What a read returns is what the rule says.
fn check_reads_follow_the_rule(steps: &[Op]) -> Result<(), TestCaseError> {
    let env = Arc::new(MemEnv::new());
    let mut db = open(&env);
    let (mut model, mut now) = (Model::default(), 0);
    for step in steps {
        db = apply(step, &env, db, &mut model, &mut now);
        prop_assert_eq!(&reads(&db), &model.reads(now), "after {:?}", step);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn no_flush_compaction_or_reopen_changes_a_read(steps in prop::collection::vec(op(), 1..24)) {
        check_steps_keep_reads(&steps)?;
    }

    #[test]
    fn a_read_folds_the_writes_that_have_not_expired(steps in prop::collection::vec(op(), 1..24)) {
        check_reads_follow_the_rule(&steps)?;
    }
}

/// Cases worth keeping by name: the two shortest a merge that renewed what
/// it was folded onto failed (a compaction that met only part of the key
/// changed a read), and one where a fold of writes with no time must not
/// take in a timed put that expires later.
#[test]
fn the_cases_kept_by_name() {
    let cases = [
        vec![
            Op::Merge(0, 0),
            Op::Advance(8),
            Op::Merge(0, 0),
            Op::Advance(3),
        ],
        vec![
            Op::Put(0, 0),
            Op::Advance(1),
            Op::Merge(0, 0),
            Op::Advance(10),
            Op::Flush,
            Op::CompactStep,
        ],
        vec![
            Op::Put(0, 2),
            Op::RawMerge(0, 2),
            Op::Flush,
            Op::CompactAll,
            Op::Advance(5),
            Op::Merge(0, 2),
            Op::RawMerge(0, 1),
            Op::Advance(6),
            Op::CompactAll,
        ],
    ];
    for steps in cases {
        check_steps_keep_reads(&steps).unwrap();
        check_reads_follow_the_rule(&steps).unwrap();
    }
}
