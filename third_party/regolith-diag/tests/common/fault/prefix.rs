//! The valid-prefix validator.
//!
//! "No data loss" is the wrong bar for a crash test. Under
//! `DurabilityMode::Eventual` recent writes are *allowed* to disappear.
//! What is never allowed is a torn state.
//!
//! The property this module checks is: there exists some `k` such that the
//! recovered database is exactly the state produced by applying the first
//! `k` writes of the intended history, in order, and nothing else. No
//! gaps, no write from beyond `k`, no half-applied `WriteBatch`, and an
//! intact iteration order. `k` is returned so a test can report how much
//! was lost rather than merely that it recovered.
//!
//! The search for `k` is a single forward fold that keeps a running count
//! of keys where the folded state disagrees with the recovered state, so
//! it is O(history + keys) rather than O(history * keys), and it finds
//! *every* valid `k` rather than guessing one.

use std::collections::HashMap;
use std::fmt;

use regolith::Db;

/// A materialised database state: every live key with its value, in
/// ascending key order.
pub type KeyValues = Vec<(Vec<u8>, Vec<u8>)>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpValue {
    Put(Vec<u8>),
    Delete,
}

/// One intended write. `batch` groups writes that were submitted as a
/// single atomic `WriteBatch`; a standalone `put` is its own batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteOp {
    pub batch: u64,
    pub key: Vec<u8>,
    pub value: OpValue,
}

/// The ordered write history a workload intended to apply.
#[derive(Clone, Debug, Default)]
pub struct History {
    ops: Vec<WriteOp>,
    next_batch: u64,
}

impl History {
    pub fn new() -> History {
        History::default()
    }

    pub fn put(&mut self, key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> &mut History {
        let batch = self.take_batch_id();
        self.ops.push(WriteOp {
            batch,
            key: key.into(),
            value: OpValue::Put(value.into()),
        });
        self
    }

    pub fn delete(&mut self, key: impl Into<Vec<u8>>) -> &mut History {
        let batch = self.take_batch_id();
        self.ops.push(WriteOp {
            batch,
            key: key.into(),
            value: OpValue::Delete,
        });
        self
    }

    /// Record a group of writes that were submitted atomically. A
    /// recovered state that contains some but not all of them is a bug in
    /// every durability mode.
    pub fn batch(&mut self, ops: impl IntoIterator<Item = (Vec<u8>, OpValue)>) -> &mut History {
        let batch = self.take_batch_id();
        for (key, value) in ops {
            self.ops.push(WriteOp { batch, key, value });
        }
        self
    }

    fn take_batch_id(&mut self) -> u64 {
        let id = self.next_batch;
        self.next_batch += 1;
        id
    }

    pub fn ops(&self) -> &[WriteOp] {
        &self.ops
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Prefix lengths that land on a batch boundary, i.e. the only `k`
    /// values a crash is permitted to stop at.
    pub fn batch_boundaries(&self) -> Vec<usize> {
        let mut out = vec![0usize];
        for i in 1..self.ops.len() {
            if self.ops[i].batch != self.ops[i - 1].batch {
                out.push(i);
            }
        }
        if !self.ops.is_empty() {
            out.push(self.ops.len());
        }
        out
    }

    /// The state the database would hold after the first `k` writes.
    pub fn state_after(&self, k: usize) -> HashMap<Vec<u8>, Vec<u8>> {
        let mut m = HashMap::new();
        for op in self.ops.iter().take(k) {
            match &op.value {
                OpValue::Put(v) => {
                    m.insert(op.key.clone(), v.clone());
                }
                OpValue::Delete => {
                    m.remove(&op.key);
                }
            }
        }
        m
    }
}

/// What the validator found. Every field is measured from the recovered
/// database, never assumed.
#[derive(Clone, Debug)]
pub struct PrefixReport {
    /// Canonical prefix length: the largest valid `k` that lands on a
    /// batch boundary.
    pub k: usize,
    /// Every `k` whose folded state matches the recovered state exactly.
    pub valid_ks: Vec<usize>,
    /// Intended writes that did not survive.
    pub lost: usize,
    /// Live keys in the recovered database.
    pub live_keys: usize,
    /// Keys walked by the forward scan.
    pub scanned: usize,
}

impl PrefixReport {
    /// Every write index in `acked` must be inside the surviving prefix.
    /// This is the `DurabilityMode::Immediate` contract: a write that
    /// returned `Ok` survives a power cut.
    pub fn covers_acked(&self, acked: &[usize]) -> Result<(), String> {
        match acked.iter().copied().max() {
            None => Ok(()),
            Some(hi) if hi < self.k => Ok(()),
            Some(hi) => Err(format!(
                "write {hi} returned Ok but the recovered prefix is only {} writes long: \
                 {} acknowledged write(s) were lost",
                self.k,
                acked.iter().filter(|i| **i >= self.k).count(),
            )),
        }
    }

    pub fn summary(&self) -> String {
        format!(
            "valid prefix k={} (lost {}, live keys {}, valid k candidates {:?})",
            self.k, self.lost, self.live_keys, self.valid_ks,
        )
    }
}

/// Every way the recovered state can fail to be a valid prefix.
#[derive(Clone, Debug)]
pub enum PrefixViolation {
    /// No prefix of the history produces the recovered state. Either a
    /// write survived out of order, or one from the middle vanished.
    NotAPrefix {
        detail: String,
    },
    /// A prefix matches, but only one that stops inside a `WriteBatch`.
    /// A batch is atomic, so this is a bug in every durability mode.
    HalfAppliedBatch {
        batch: u64,
        valid_ks: Vec<usize>,
        detail: String,
    },
    /// The database holds a key the workload never wrote.
    ForeignKeys {
        keys: Vec<String>,
    },
    /// The forward scan did not return keys in ascending order, or the
    /// reverse scan disagreed with it.
    OrderBroken {
        detail: String,
    },
    /// A point lookup disagreed with the scan.
    PointScanDisagree {
        detail: String,
    },
    Engine(String),
}

impl fmt::Display for PrefixViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PrefixViolation::NotAPrefix { detail } => {
                write!(
                    f,
                    "recovered state is not a prefix of the write history: {detail}"
                )
            }
            PrefixViolation::HalfAppliedBatch {
                batch,
                valid_ks,
                detail,
            } => write!(
                f,
                "WriteBatch {batch} was half applied: the only matching prefix lengths {valid_ks:?} \
                 all stop inside it. A batch is atomic, so this is a bug regardless of durability \
                 mode. {detail}"
            ),
            PrefixViolation::ForeignKeys { keys } => write!(
                f,
                "recovered database holds {} key(s) the workload never wrote: {:?}",
                keys.len(),
                keys,
            ),
            PrefixViolation::OrderBroken { detail } => {
                write!(f, "iteration order broken: {detail}")
            }
            PrefixViolation::PointScanDisagree { detail } => {
                write!(f, "point lookup disagreed with the scan: {detail}")
            }
            PrefixViolation::Engine(e) => {
                write!(f, "engine error while reading recovered state: {e}")
            }
        }
    }
}

impl std::error::Error for PrefixViolation {}

/// Read the whole database with a forward scan, checking on the way that
/// keys ascend strictly, that a reverse scan yields the mirror image, and
/// that a point lookup agrees with the scan.
pub fn recovered_state(db: &Db) -> Result<KeyValues, PrefixViolation> {
    let mut forward: KeyValues = Vec::new();
    let mut it = db.iter();
    it.seek_to_first();
    while it.valid() {
        let k = it.key().expect("valid iterator has a key").to_vec();
        let v = it.value().expect("valid iterator has a value").to_vec();
        if let Some((prev, _)) = forward.last()
            && prev >= &k
        {
            return Err(PrefixViolation::OrderBroken {
                detail: format!(
                    "forward scan returned {:?} after {:?}",
                    String::from_utf8_lossy(&k),
                    String::from_utf8_lossy(prev),
                ),
            });
        }
        forward.push((k, v));
        it.next();
    }

    let mut backward: KeyValues = Vec::new();
    let mut rit = db.iter();
    rit.seek_to_last();
    while rit.valid() {
        let k = rit.key().expect("valid iterator has a key").to_vec();
        let v = rit.value().expect("valid iterator has a value").to_vec();
        backward.push((k, v));
        rit.prev();
    }
    backward.reverse();
    if backward != forward {
        return Err(PrefixViolation::OrderBroken {
            detail: format!(
                "reverse scan yielded {} entries, forward yielded {}; \
                 first divergence at index {:?}",
                backward.len(),
                forward.len(),
                forward
                    .iter()
                    .zip(backward.iter())
                    .position(|(a, b)| a != b),
            ),
        });
    }

    for (k, v) in &forward {
        match db.get(k) {
            Ok(Some(got)) if &got == v => {}
            Ok(other) => {
                return Err(PrefixViolation::PointScanDisagree {
                    detail: format!(
                        "scan says {:?} -> {} bytes, get says {:?}",
                        String::from_utf8_lossy(k),
                        v.len(),
                        other.map(|b| b.len()),
                    ),
                });
            }
            Err(e) => return Err(PrefixViolation::Engine(e.to_string())),
        }
    }
    Ok(forward)
}

/// Check that `db` holds a valid prefix of `history` and return how long
/// that prefix is.
pub fn validate_prefix(db: &Db, history: &History) -> Result<PrefixReport, PrefixViolation> {
    let scanned = recovered_state(db)?;
    validate_prefix_of_state(&scanned, history)
}

/// The same check against an already-materialised state. Split out so the
/// validator itself is testable without a database.
pub fn validate_prefix_of_state(
    scanned: &[(Vec<u8>, Vec<u8>)],
    history: &History,
) -> Result<PrefixReport, PrefixViolation> {
    let recovered: HashMap<&[u8], &[u8]> = scanned
        .iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect();

    let written: std::collections::HashSet<&[u8]> =
        history.ops().iter().map(|o| o.key.as_slice()).collect();
    let foreign: Vec<String> = recovered
        .keys()
        .filter(|k| !written.contains(*k))
        .map(|k| String::from_utf8_lossy(k).into_owned())
        .collect();
    if !foreign.is_empty() {
        let mut keys = foreign;
        keys.sort();
        keys.truncate(16);
        return Err(PrefixViolation::ForeignKeys { keys });
    }

    // Fold the history forward, keeping a running count of keys where the
    // folded state and the recovered state disagree. Every k with a count
    // of zero is a prefix that reproduces the recovered state exactly.
    let mut state: HashMap<&[u8], &[u8]> = HashMap::new();
    let mut mismatches = recovered.len();
    let mut valid_ks: Vec<usize> = Vec::new();
    if mismatches == 0 {
        valid_ks.push(0);
    }
    for (i, op) in history.ops().iter().enumerate() {
        let key = op.key.as_slice();
        let want = recovered.get(key).copied();
        let before = state.get(key).copied();
        let after = match &op.value {
            OpValue::Put(v) => Some(v.as_slice()),
            OpValue::Delete => None,
        };
        let was_match = before == want;
        let now_match = after == want;
        match after {
            Some(v) => {
                state.insert(key, v);
            }
            None => {
                state.remove(key);
            }
        }
        if was_match && !now_match {
            mismatches += 1;
        } else if !was_match && now_match {
            mismatches -= 1;
        }
        if mismatches == 0 {
            valid_ks.push(i + 1);
        }
    }

    if valid_ks.is_empty() {
        return Err(PrefixViolation::NotAPrefix {
            detail: not_a_prefix_detail(&recovered, history),
        });
    }

    let boundaries = history.batch_boundaries();
    let aligned: Vec<usize> = valid_ks
        .iter()
        .copied()
        .filter(|k| boundaries.contains(k))
        .collect();
    if aligned.is_empty() {
        let k = valid_ks[0];
        let batch = history.ops()[k.min(history.len() - 1)].batch;
        return Err(PrefixViolation::HalfAppliedBatch {
            batch,
            valid_ks,
            detail: format!("batch boundaries are {boundaries:?}"),
        });
    }

    let k = *aligned.last().expect("aligned is non-empty");
    Ok(PrefixReport {
        k,
        valid_ks,
        lost: history.len() - k,
        live_keys: recovered.len(),
        scanned: scanned.len(),
    })
}

fn not_a_prefix_detail(recovered: &HashMap<&[u8], &[u8]>, history: &History) -> String {
    // Report against the prefix that explains the most recovered keys, so
    // the message points at the real divergence rather than at k = 0.
    let mut best = (0usize, usize::MAX, String::new());
    for k in history.batch_boundaries() {
        let expect = history.state_after(k);
        let mut diffs = 0usize;
        let mut first = String::new();
        for (key, want) in &expect {
            if recovered.get(key.as_slice()) != Some(&want.as_slice()) {
                diffs += 1;
                if first.is_empty() {
                    first = format!(
                        "at k={k}, key {:?} should be present with {} bytes but is {:?}",
                        String::from_utf8_lossy(key),
                        want.len(),
                        recovered.get(key.as_slice()).map(|v| v.len()),
                    );
                }
            }
        }
        for key in recovered.keys() {
            if !expect.contains_key(*key) {
                diffs += 1;
                if first.is_empty() {
                    first = format!(
                        "at k={k}, key {:?} is present but should not be",
                        String::from_utf8_lossy(key),
                    );
                }
            }
        }
        if diffs < best.1 {
            best = (k, diffs, first);
        }
    }
    format!(
        "closest prefix is k={} with {} differing key(s); {}",
        best.0, best.1, best.2,
    )
}

/// Assert the recovered database is a valid prefix, panicking with the
/// full diagnosis when it is not.
pub fn assert_valid_prefix(db: &Db, history: &History) -> PrefixReport {
    match validate_prefix(db, history) {
        Ok(r) => r,
        Err(e) => panic!("{e}"),
    }
}

/// Assert that every acknowledged write survived. This is the
/// `DurabilityMode::Immediate` contract and must not be used for
/// `Eventual`, where losing recent writes is allowed.
pub fn assert_acked_survived(report: &PrefixReport, acked: &[usize]) {
    if let Err(e) = report.covers_acked(acked) {
        panic!("{e}\n{}", report.summary());
    }
}

/// Outcome of reopening a database after a fault.
#[derive(Debug)]
pub enum Recovery {
    /// The database opened and holds a valid prefix of the history.
    Recovered(PrefixReport),
    /// The database refused to open and said why. This is a legitimate
    /// outcome for a torn tail: regolith reports corruption and keeps the
    /// damaged file rather than guessing. What is never legitimate is
    /// opening and serving a torn state.
    RefusedToOpen(String),
}

impl Recovery {
    pub fn report(&self) -> Option<&PrefixReport> {
        match self {
            Recovery::Recovered(r) => Some(r),
            Recovery::RefusedToOpen(_) => None,
        }
    }

    /// The surviving prefix length, or 0 when the database refused to open.
    pub fn k(&self) -> usize {
        self.report().map(|r| r.k).unwrap_or(0)
    }
}

/// Reopen a database directory after a fault and check the invariant that
/// holds in every durability mode: the engine either recovers a valid
/// prefix of the intended history, or it refuses to open and says so. It
/// must never open and serve a state that is neither.
///
/// Panics when it opens with a state that is not a valid prefix, quoting
/// the exact divergence.
pub fn recover_and_validate(
    db_dir: &std::path::Path,
    opts: regolith::Options,
    history: &History,
) -> Recovery {
    match Db::open(db_dir, opts) {
        Err(e) => Recovery::RefusedToOpen(e.to_string()),
        Ok(db) => {
            let report = assert_valid_prefix(&db, history);
            let _ = db.close();
            Recovery::Recovered(report)
        }
    }
}
