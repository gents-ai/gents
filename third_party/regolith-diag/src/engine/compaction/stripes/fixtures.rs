//! Shared fixtures for the stripe tests: entry builders, merge operators
//! and compaction filters with known behaviour.

use std::sync::Mutex;

use super::*;

pub(super) const KEY: &[u8] = b"k";

/// An entry for [`KEY`] at `seq`.
pub(super) fn entry(seq: u64, value_type: u8, value: &[u8]) -> Entry {
    (encode_internal_key(KEY, seq, value_type), value.to_vec())
}

/// A value.
pub(super) fn put(seq: u64, value: &str) -> Entry {
    entry(seq, VALUE_TYPE_VALUE, value.as_bytes())
}

/// A deletion.
pub(super) fn del(seq: u64) -> Entry {
    entry(seq, VALUE_TYPE_DELETION, b"")
}

/// A merge operand.
pub(super) fn operand(seq: u64, value: &str) -> Entry {
    entry(seq, VALUE_TYPE_MERGE, value.as_bytes())
}

/// One entry per string, `seq:type:value`, with `v` a value, `d` a deletion
/// and `m` an operand.
pub(super) fn show(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .map(|(key, value)| {
            let (_, seq, value_type) = decode_internal_key(key);
            let kind = match value_type {
                VALUE_TYPE_VALUE => 'v',
                VALUE_TYPE_DELETION => 'd',
                _ => 'm',
            };
            format!("{seq}:{kind}:{}", String::from_utf8_lossy(value))
        })
        .collect()
}

/// How far an [`Append`] folds two operands.
#[derive(Clone, Copy)]
pub(super) enum Partial {
    Always,
    Never,
    /// Only while the left operand is a single byte, so a run of three
    /// folds once and is then declined.
    Once,
}

/// Concatenates, oldest first: associative, and not commutative, so a fold
/// in the wrong order shows.
pub(super) struct Append(pub(super) Partial);

impl MergeOperator for Append {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut merged = base.map(<[u8]>::to_vec).unwrap_or_default();
        operands.iter().for_each(|operand| merged.extend(*operand));
        Some(merged)
    }

    fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        match self.0 {
            Partial::Always => Some([left, right].concat()),
            Partial::Never => None,
            Partial::Once => (left.len() == 1).then(|| [left, right].concat()),
        }
    }

    fn name(&self) -> &'static str {
        "append"
    }
}

/// Sums big-endian `i64` deltas: associative, with a partial merge.
pub(super) struct Sum;

impl Sum {
    /// The delta or total `bytes` encode, if they encode one.
    fn decode(bytes: &[u8]) -> Option<i64> {
        Some(i64::from_be_bytes(bytes.try_into().ok()?))
    }
}

impl MergeOperator for Sum {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut total = base.map_or(Some(0), Self::decode)?;
        for operand in operands {
            total = total.wrapping_add(Self::decode(operand)?);
        }
        Some(total.to_be_bytes().to_vec())
    }

    fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        let sum = Self::decode(left)?.wrapping_add(Self::decode(right)?);
        Some(sum.to_be_bytes().to_vec())
    }

    fn name(&self) -> &'static str {
        "sum"
    }
}

/// Declines every merge.
pub(super) struct Refuse;

impl MergeOperator for Refuse {
    fn full_merge(&self, _: &[u8], _: Option<&[u8]>, _: &[&[u8]]) -> Option<Vec<u8>> {
        None
    }

    fn name(&self) -> &'static str {
        "refuse"
    }
}

/// A filter that decides from the value alone and records every value it
/// was shown.
pub(super) struct Judge {
    decide: fn(&[u8]) -> CompactionDecision,
    seen: Mutex<Vec<String>>,
}

impl Judge {
    /// A filter that answers every value with `decide`.
    pub(super) fn new(decide: fn(&[u8]) -> CompactionDecision) -> Self {
        Self {
            decide,
            seen: Mutex::new(Vec::new()),
        }
    }

    /// The values the filter was shown, in order.
    pub(super) fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl CompactionFilter for Judge {
    fn filter(&self, _level: usize, _key: &[u8], value: &[u8]) -> CompactionDecision {
        self.seen
            .lock()
            .unwrap()
            .push(String::from_utf8_lossy(value).into_owned());
        (self.decide)(value)
    }

    fn name(&self) -> &'static str {
        "judge"
    }
}

/// Remove every value.
pub(super) fn remove_all(_: &[u8]) -> CompactionDecision {
    CompactionDecision::Remove
}

/// Remove the value `a` and keep the rest.
pub(super) fn remove_a(value: &[u8]) -> CompactionDecision {
    if value == b"a" {
        CompactionDecision::Remove
    } else {
        CompactionDecision::Keep
    }
}

/// Change every value to upper case.
pub(super) fn shout(value: &[u8]) -> CompactionDecision {
    CompactionDecision::Change(value.to_ascii_uppercase())
}
