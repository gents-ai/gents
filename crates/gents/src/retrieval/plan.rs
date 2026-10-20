//! Executable refinement of `Proofs/Retrieval/Types.lean`: one indexer pass
//! over a session and the selection of semantic hits.

use std::cmp::Ordering;
use std::collections::BTreeSet;

/// First message sequence of every session (`max_sequence + 1` from an empty
/// session).
pub const FIRST_SEQUENCE: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexMessage {
    pub sequence: u32,
    /// The message's request is terminal, or it has no request.
    pub settled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIndex {
    pub indexed: BTreeSet<u32>,
    pub watermark: u32,
}

impl SessionIndex {
    pub fn initial() -> Self {
        Self {
            indexed: BTreeSet::new(),
            watermark: FIRST_SEQUENCE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexStep {
    pub next: SessionIndex,
    pub index: Vec<u32>,
    pub delete: Vec<u32>,
}

pub fn step(messages: &[IndexMessage], state: &SessionIndex) -> IndexStep {
    let present: BTreeSet<u32> = messages.iter().map(|m| m.sequence).collect();
    let index: Vec<u32> = messages
        .iter()
        .filter(|m| {
            m.settled && state.watermark <= m.sequence && !state.indexed.contains(&m.sequence)
        })
        .map(|m| m.sequence)
        .collect();
    let delete = state
        .indexed
        .iter()
        .copied()
        .filter(|s| !present.contains(s))
        .collect();
    let covered: BTreeSet<u32> = state
        .indexed
        .iter()
        .chain(&index)
        .copied()
        .filter(|s| present.contains(s))
        .collect();
    let mut watermark = state.watermark;
    while covered.contains(&watermark) {
        watermark += 1;
    }
    IndexStep {
        next: SessionIndex {
            indexed: covered,
            watermark,
        },
        index,
        delete,
    }
}

/// Messages the session still owes the index.
pub fn pending(messages: &[IndexMessage], state: &SessionIndex) -> usize {
    messages
        .iter()
        .filter(|m| !state.indexed.contains(&m.sequence))
        .count()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate<R> {
    pub reference: R,
    pub score: f64,
}

/// Candidates whose canonical source materialized for the caller, nearest
/// first, ties by reference, cut to `limit`.
pub fn select_hits<R: Ord>(
    candidates: Vec<Candidate<R>>,
    mut authorized: impl FnMut(&R) -> bool,
    limit: usize,
) -> Vec<Candidate<R>> {
    let mut hits: Vec<_> = candidates
        .into_iter()
        .filter(|c| authorized(&c.reference))
        .collect();
    hits.sort_by(|a, b| match b.score.total_cmp(&a.score) {
        Ordering::Equal => a.reference.cmp(&b.reference),
        order => order,
    });
    hits.truncate(limit);
    hits
}
