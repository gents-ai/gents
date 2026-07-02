//! ToolPolicy conformance home: feed the Lean-emitted inputs through the
//! production Rust policy resolver and assert it equals Lean's expected output.

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
            "case {}: production Rust resolver diverged from Lean effective surface",
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
            // The behavior and ceiling scopes are both non-empty, disjoint
            // `only` scopes. A correct key-intersection meet preserves the
            // `only` kind but drops every key; a union bug would keep one.
            assert_eq!(case.behavior.mcp_scope_kind, "only");
            assert_eq!(case.ceiling.mcp_scope_kind, "only");
            assert!(case.behavior.mcp_services.contains(&"svc-x".to_string()));
            assert!(case.ceiling.mcp_services.contains(&"svc-y".to_string()));
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
            assert_eq!(case.expected.mcp_scope_kind, "only");
            assert!(
                case.expected.mcp_services.is_empty(),
                "disjoint case: effective MCP Only scope must have no surviving keys"
            );
            // Two disjoint non-empty allow-lists meet to Only(∅) (deny-all),
            // which must serialize as "only" — never collapse to "all" (the
            // empty-list = allow-all trap) nor optimize to "none".
            assert_eq!(
                case.expected.bash_allowed_kind, "only",
                "disjoint case: Only(∅) must stay \"only\""
            );
            assert!(
                case.expected.bash_allowed_prefixes.is_empty(),
                "disjoint case: effective bash allowed-prefix scope must be Only(empty)"
            );
            // defra_collections is the blocker category: a disjoint meet must
            // produce Only(∅) deny-all, never collapse to "all" (which the
            // runtime would read as every-collection-readable).
            assert_eq!(
                case.expected.defra_collections_scope_kind, "only",
                "disjoint case: defra_collections Only(∅) must stay \"only\", not \"all\""
            );
            assert!(
                case.expected.defra_collections_keys.is_empty(),
                "disjoint case: effective defra_collections must have no surviving keys"
            );
        }
        if case.name == "ceiling_clamps_each_category" {
            // Every boolean capability the original 7-category view aliased away
            // is independently clamped off by the ceiling here. An `||`-vs-`&&`
            // wiring typo in any single one would leave it true and diverge from
            // the Lean-computed expected.
            for (label, value) in [
                ("memory", case.expected.memory),
                ("session_history", case.expected.session_history),
                ("context_budget", case.expected.context_budget),
                ("steering", case.expected.steering),
                ("background", case.expected.background),
                ("orchestration", case.expected.orchestration),
                ("cross_deployment", case.expected.cross_deployment),
                ("skills", case.expected.skills),
            ] {
                assert!(
                    !value,
                    "each-category case: {label} must clamp off when the ceiling denies it"
                );
            }
            // The four keyed scopes key-intersect down to the ceiling's narrow set.
            assert_eq!(case.expected.cli_keys, vec!["svc-a".to_string()]);
            assert_eq!(
                case.expected.defra_collections_keys,
                vec!["svc-a".to_string()]
            );
            assert_eq!(
                case.expected.subagent_targets_keys,
                vec!["did-a::beh-a".to_string()]
            );
            assert_eq!(
                case.expected.background_tools_keys,
                vec!["svc-a".to_string()]
            );
        }
        if case.name == "behavior_all_scopes_clamped_by_ceiling_only" {
            // Behavior left these scopes `All`; the ceiling's `Only` must win
            // (All ⊓ Only = Only) while the booleans stay on (no spurious clamp).
            assert!(case.expected.memory && case.expected.skills && case.expected.orchestration);
            assert_eq!(case.expected.cli_scope_kind, "only");
            assert_eq!(case.expected.defra_collections_scope_kind, "only");
            assert_eq!(case.expected.subagent_targets_scope_kind, "only");
            assert_eq!(case.expected.background_tools_scope_kind, "only");
            assert_eq!(
                case.expected.subagent_targets_keys,
                vec!["did-a::beh-a".to_string()]
            );
        }
        if case.name == "write_tool_collection_mismatch_denies" {
            // Behavior grants (wt, coll1); ceiling grants (wt, coll2). The
            // collection is part of the KEY, so the keys don't intersect and the
            // write tool is denied — effective fields empty. A tool-name-only
            // keying would merge the two and silently keep it active.
            assert!(case
                .behavior
                .write_grants
                .iter()
                .any(|grant| grant.tool == "wt" && grant.collection == "coll1"));
            assert!(case
                .ceiling
                .write_grants
                .iter()
                .any(|grant| grant.tool == "wt" && grant.collection == "coll2"));
            assert!(
                !case.behavior.write_fields.is_empty(),
                "collision case: behavior must grant the field at its own collection"
            );
            assert!(
                case.expected.write_fields.is_empty(),
                "collision case: a (tool, collection) mismatch must DENY (empty effective fields)"
            );
            assert!(
                case.expected.write_grants.is_empty(),
                "collision case: mismatched collections must leave no effective write grant"
            );
        }
        if case.name == "bash_all_allowed_kind_idempotent" {
            assert_eq!(case.expected.bash_allowed_kind, "all");
        }
    }
}
