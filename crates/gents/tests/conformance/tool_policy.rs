//! Compare generated composition results with the existing permission owner.
//! The companion module only converts fixture views to/from runtime values.

use crate::lean_vocab_test::{lean_goal_capability_resolution_cases, lean_tool_policy_cases};

#[path = "tool_policy_codec.rs"]
mod tool_policy_codec;

pub(super) fn generated_tool_policy_cases_match_lean_composition() {
    let cases = lean_tool_policy_cases();
    assert_eq!(
        cases.len(),
        17,
        "Lean tool-policy composition matrix drifted"
    );
    for case in cases {
        let actual = tool_policy_codec::compose(&case.behavior, &case.ceiling, &case.runtime);
        assert_eq!(
            actual, case.expected,
            "{}: production permission composition",
            case.name
        );
    }
}

pub(super) fn generated_goal_capability_resolution_matches_rust_decoder() {
    let cases = lean_goal_capability_resolution_cases();
    assert_eq!(cases.len(), 4, "goal capability resolution matrix drifted");
    for case in cases {
        let (goal_tools, goal_creation) = gents::tool_surface::resolve_goal_capabilities(
            case.explicit_goal_tools,
            case.explicit_goal_create,
        );
        assert_eq!(
            (goal_tools, goal_creation),
            (case.expected_goal_tools, case.expected_goal_create),
            "Lean capability resolution case {}",
            case.name
        );
    }
}
