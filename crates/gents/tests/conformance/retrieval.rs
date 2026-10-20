use std::collections::BTreeSet;

use gents::retrieval::plan::{pending, select_hits, step, Candidate, IndexMessage, SessionIndex};

use crate::lean_vocab_test::{
    lean_retrieval_hit_cases, lean_retrieval_index_cases, LeanSessionIndex,
};

fn state(lean: &LeanSessionIndex) -> SessionIndex {
    SessionIndex {
        indexed: lean.indexed.iter().copied().collect(),
        watermark: lean.watermark,
    }
}

pub(crate) fn generated_retrieval_index_cases_drive_the_indexer_plan() {
    let cases = lean_retrieval_index_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let messages: Vec<IndexMessage> = case
            .messages
            .iter()
            .map(|m| IndexMessage {
                sequence: m.sequence,
                settled: m.settled,
            })
            .collect();
        let result = step(&messages, &state(&case.state));
        assert_eq!(result.next, state(&case.next), "{}: next state", case.name);
        assert_eq!(
            result.index.iter().collect::<BTreeSet<_>>(),
            case.index.iter().collect::<BTreeSet<_>>(),
            "{}: index",
            case.name
        );
        assert_eq!(
            result.delete.iter().collect::<BTreeSet<_>>(),
            case.delete.iter().collect::<BTreeSet<_>>(),
            "{}: delete",
            case.name
        );
        assert_eq!(
            pending(&messages, &result.next),
            case.pending_after,
            "{}: pending",
            case.name
        );
    }
}

pub(crate) fn generated_retrieval_hit_cases_drive_hit_selection() {
    let cases = lean_retrieval_hit_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let candidates = case
            .candidates
            .iter()
            .map(|c| Candidate {
                reference: c.reference,
                score: c.score as f64,
            })
            .collect();
        let hits = select_hits(
            candidates,
            |reference| case.authorized.contains(reference),
            case.limit,
        );
        let expected: Vec<(u32, f64)> = case
            .hits
            .iter()
            .map(|c| (c.reference, c.score as f64))
            .collect();
        let actual: Vec<(u32, f64)> = hits.iter().map(|c| (c.reference, c.score)).collect();
        assert_eq!(actual, expected, "{}", case.name);
    }
}
