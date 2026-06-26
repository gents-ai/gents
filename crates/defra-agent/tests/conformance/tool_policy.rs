//! ToolPolicy conformance home: re-derive the effective surface from the
//! Lean-emitted inputs and assert it equals the Lean-emitted expected output.

use crate::lean_vocab_test::lean_tool_policy_cases;

#[path = "tool_policy_mirror.rs"]
mod tool_policy_mirror;

pub(super) fn generated_tool_policy_cases_match_lean_composition() {
    let cases = lean_tool_policy_cases();
    assert!(!cases.is_empty(), "no tool-policy cases emitted by Lean");

    for case in cases {
        let got = tool_policy_mirror::rederive(&case.behavior, &case.ceiling, &case.runtime);
        assert_eq!(
            got, case.expected,
            "case {}: Rust re-derivation diverged from Lean effective surface",
            case.name
        );
        assert!(
            case.expected.file_rank <= case.ceiling.file_rank,
            "case {}: effective file rank exceeds ceiling",
            case.name
        );
        if case.expected.mcp_permits {
            assert!(
                case.ceiling.mcp_permits,
                "case {}: effective permits an MCP service the ceiling forbids",
                case.name
            );
        }
        if case.name == "disjoint_only_scopes_intersect_to_empty" {
            // The probe key is in the behavior scope but not the ceiling scope.
            // A correct key-INTERSECTION meet drops it; a union bug would keep
            // it. This is the discriminating case the other probes can't reach.
            assert!(
                case.behavior.mcp_permits,
                "disjoint case: probe must be present in the behavior scope"
            );
            assert!(
                !case.ceiling.mcp_permits,
                "disjoint case: probe must be absent from the ceiling scope"
            );
            assert!(
                !case.expected.mcp_permits,
                "disjoint case: only ∩ only must intersect to empty, not union"
            );
            // Two disjoint non-empty allow-lists meet to Only(∅) (deny-all),
            // which must serialize as "only" — never collapse to "all" (the
            // empty-list = allow-all trap) nor optimize to "none".
            assert_eq!(
                case.expected.bash_allowed_kind, "only",
                "disjoint case: Only(∅) must stay \"only\""
            );
        }
        if case.name == "bash_all_allowed_kind_idempotent" {
            assert_eq!(case.expected.bash_allowed_kind, "all");
        }
    }
}
