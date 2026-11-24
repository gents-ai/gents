use super::*;

#[test]
fn selections_and_related_pages_follow_the_model() {
    gents_lean_vocab::families::contracts_query_selection::replay::<Decision, Kind>(|input| {
        let segments: Vec<Segment> = input
            .segments
            .iter()
            .map(|segment| Segment {
                kind: segment.kind,
                granted: segment.granted,
            })
            .collect();
        let (decision, segment) = decide(&segments);
        let mut related = vec![Value::Null; input.observed_related as usize];
        let truncated = related_page(&mut related);
        gents_lean_vocab::families::contracts_query_selection::QuerySelectionOutput {
            decision,
            segment: segment as u64,
            shown: related.len() as u64,
            truncated,
        }
    });
}

#[test]
fn search_ranking_follows_the_model() {
    gents_lean_vocab::families::tool_policy_query_selection_ranking::replay::<Ranking>(|input| {
        ranking(input.keywords, input.vector)
    });
}
