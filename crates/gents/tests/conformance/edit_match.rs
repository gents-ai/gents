use gents::toolset::edit_match::{
    decide, EditOutcome, EditRequest, MatchMode, Operation, Strategy,
};

fn ladder_req<'a>(old: &'a str, new: &'a str, replace_all: bool) -> EditRequest<'a> {
    EditRequest {
        old_text: old,
        new_text: new,
        replace_all,
        operation: Operation::Replace,
        match_mode: MatchMode::Ladder,
    }
}

pub(super) fn exact_priority_is_never_shadowed() {
    let content = "target: 1\n  target: 1  \nend\n";
    match decide(content, &ladder_req("target: 1\n", "hit: 2\n", false)) {
        EditOutcome::Applied {
            strategy, result, ..
        } => {
            assert_eq!(strategy, Strategy::Exact);
            assert_eq!(result, "hit: 2\n  target: 1  \nend\n");
        }
        other => panic!("E1 violated: {other:?}"),
    }
}

pub(super) fn ladder_fires_at_the_strictest_matching_rung() {
    let cases = [
        ("let x = 1;\n", "let x = 1;", Strategy::Exact),
        ("let x = 1;\n", "let x = 1;   ", Strategy::TrailingWs),
        ("  let x = 1;\n", "        let x = 1;", Strategy::Trim),
        ("say \u{201C}hi\u{201D}\n", "say \"hi\"", Strategy::Unicode),
    ];
    for (content, pattern, expected) in cases {
        match decide(content, &ladder_req(pattern, "REPLACED", false)) {
            EditOutcome::Applied { strategy, .. } => {
                assert_eq!(
                    strategy, expected,
                    "content {content:?} pattern {pattern:?}"
                )
            }
            other => panic!("expected applied for {pattern:?}, got {other:?}"),
        }
    }
}

pub(super) fn ambiguity_gate_requires_unique_or_replace_all() {
    let content = "dup\nmiddle\ndup\n";
    match decide(content, &ladder_req("dup", "x", false)) {
        EditOutcome::Ambiguous { count, .. } => assert_eq!(count, 2),
        other => panic!("E4 violated: {other:?}"),
    }
    match decide(content, &ladder_req("dup", "x", true)) {
        EditOutcome::Applied {
            replacements,
            result,
            ..
        } => {
            assert_eq!(replacements, 2);
            assert_eq!(result, "x\nmiddle\nx\n");
        }
        other => panic!("replace_all should apply: {other:?}"),
    }
}

pub(super) fn decision_is_pure_and_deterministic() {
    let content = "a\nvalue = 1\nz\n";
    let req = ladder_req("value = 1", "value = 2", false);
    let first = decide(content, &req);
    let second = decide(content, &req);
    assert_eq!(first, second, "decide must be deterministic");
    match first {
        EditOutcome::Applied { result, .. } => {
            assert_eq!(result, "a\nvalue = 2\nz\n");
        }
        other => panic!("expected applied, got {other:?}"),
    }
}

pub(super) fn noop_is_reported_not_applied() {
    let content = "same\n";
    match decide(content, &ladder_req("same", "same", false)) {
        EditOutcome::Noop { strategy } => assert_eq!(strategy, Strategy::Exact),
        other => panic!("E8 violated: {other:?}"),
    }
}

pub(super) fn operations_desugar_onto_the_single_matcher() {
    let content = "anchor\nrest\n";
    let mut req = ladder_req("anchor", "\nadded", false);
    req.operation = Operation::InsertAfter;
    match decide(content, &req) {
        EditOutcome::Applied { result, .. } => assert_eq!(result, "anchor\nadded\nrest\n"),
        other => panic!("insert_after: {other:?}"),
    }
    let mut req = ladder_req("anchor\n", "", false);
    req.operation = Operation::Delete;
    match decide(content, &req) {
        EditOutcome::Applied { result, .. } => assert_eq!(result, "rest\n"),
        other => panic!("delete: {other:?}"),
    }
}

/// E9 — overlapping relaxed windows: the applied selection is pairwise
/// disjoint (greedy), so replace_all splicing can never corrupt or panic.
pub(super) fn overlapping_windows_apply_disjoint_selection() {
    let content = "a \na \na ";
    let mut req = ladder_req("a\na", "", true);
    req.operation = Operation::Delete;
    match decide(content, &req) {
        EditOutcome::Applied {
            result,
            replacements,
            ..
        } => {
            assert_eq!(replacements, 1);
            assert_eq!(result, "\na ");
        }
        other => panic!("E9 violated: {other:?}"),
    }
}

pub(super) fn near_miss_is_diagnosed_never_applied() {
    let content = "max_turns: 20\n";
    match decide(
        content,
        &ladder_req("max_turns: 21", "max_turns: 250", false),
    ) {
        EditOutcome::NotFound { closest: Some(c) } => {
            assert_eq!(c.line, 1);
            assert!(c.similarity_pct < 100);
        }
        other => panic!("near-miss must be NotFound with diagnostics: {other:?}"),
    }
}
