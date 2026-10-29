use serde::{Deserialize, Serialize};

/// Root-scoped, canonical path components supplied by the tool owner.
/// This must include aliases as well as descendants; canonical paths alone
/// do not establish that hard links are disjoint. Unknown or incomplete
/// footprints use `Barrier`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Footprint {
    Read { paths: Vec<Vec<String>> },
    Write { paths: Vec<Vec<String>> },
    Barrier,
}

/// Only a hook-authorized read may currently be launched speculatively.
pub fn speculative_footprint(hook_allows: bool, footprint: Footprint) -> Footprint {
    match (hook_allows, footprint) {
        (true, read @ Footprint::Read { .. }) => read,
        _ => Footprint::Barrier,
    }
}

fn intersects(left: &[Vec<String>], right: &[Vec<String>]) -> bool {
    left.iter().any(|a| {
        right
            .iter()
            .any(|b| a.starts_with(b.as_slice()) || b.starts_with(a.as_slice()))
    })
}

pub fn conflicts(left: &Footprint, right: &Footprint) -> bool {
    use Footprint::{Barrier, Read, Write};
    match (left, right) {
        (Barrier, _) | (_, Barrier) => true,
        (Read { .. }, Read { .. }) => false,
        (Read { paths: a }, Write { paths: b })
        | (Write { paths: a }, Read { paths: b })
        | (Write { paths: a }, Write { paths: b }) => intersects(a, b),
    }
}

/// Length of the next consecutive compatible wave. A conflicting call ends
/// the wave, so later calls cannot overtake it.
pub fn next_wave_len(calls: &[Footprint]) -> usize {
    let Some(first) = calls.first() else { return 0 };
    if matches!(first, Footprint::Barrier) {
        return 1;
    }
    let mut end = 1;
    while end < calls.len()
        && calls[..end]
            .iter()
            .all(|prior| !conflicts(prior, &calls[end]))
    {
        end += 1;
    }
    end
}

/// A repeated call must wait for the preceding outcome so the existing
/// failure-streak owner can decide whether to suppress it. The invalid-call
/// budget also bounds speculative dispatch.
pub fn executable_wave_len(
    calls: &[((String, String), Footprint)],
    invalid_remaining: usize,
) -> usize {
    let footprints: Vec<_> = calls
        .iter()
        .map(|(_, footprint)| footprint.clone())
        .collect();
    let count = next_wave_len(&footprints).min(invalid_remaining);
    let mut seen = std::collections::HashSet::new();
    calls[..count]
        .iter()
        .take_while(|(key, _)| seen.insert(key))
        .count()
}
