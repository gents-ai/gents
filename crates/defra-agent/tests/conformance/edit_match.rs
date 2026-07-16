//! EditMatch conformance home (`proofs/Proofs/EditMatch/`, #738/#724).
//!
//! Fences the runtime matcher (`defra_agent::toolset::edit_match`) against
//! the Lean obligations E1–E8. E6's write-side gate ordering (stale hash
//! rejects before matching, file untouched) is additionally fenced against
//! the real `edit_file` tool by
//! `toolset::tests::edit_file_stale_hash_rejects_before_matching_and_reports_current`
//! — the decision-level pieces live here.

use defra_agent::toolset::edit_match::{
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

/// E1 — an exact hit is never shadowed by relaxation, even when a coarser
/// strategy would also match elsewhere.
pub(super) fn exact_priority_is_never_shadowed() {
    // The pattern (with trailing newline) exact-matches only line 1; the
    // trim rung would ALSO match line 2. Exact must win and edit only its
    // own unique site.
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

/// E3 — ladder ordering: each drift class fires at its own rung, never a
/// coarser one (coarsening means stricter rungs are checked first over the
/// WHOLE document).
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

/// E4 — ambiguity gate: >= 2 occurrences without replace_all is an error
/// carrying the count; with replace_all every occurrence is applied.
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

/// E5 — one pure decision: identical inputs decide identically, so dry-run
/// (decide, no write) and apply (decide, write result) cannot diverge.
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

/// E8 — no-op honesty: an edit producing identical content is reported as
/// noop, never as applied.
pub(super) fn noop_is_reported_not_applied() {
    let content = "same\n";
    match decide(content, &ladder_req("same", "same", false)) {
        EditOutcome::Noop { strategy } => assert_eq!(strategy, Strategy::Exact),
        other => panic!("E8 violated: {other:?}"),
    }
}

/// E7 family — convenience operations desugar onto the one matcher: the
/// matched text survives insert_after/insert_before and disappears on delete.
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
            // No-newline delete empties the window's lines (exact-pass
            // parity); the disjointness property is the single application.
            assert_eq!(result, "\na ");
        }
        other => panic!("E9 violated: {other:?}"),
    }
}

/// Diagnostics discipline: similarity scoring may only ever surface in
/// NotFound diagnostics — a below-ladder near-miss must not be applied.
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
