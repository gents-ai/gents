//! CommandPolicy conformance home: generated policy/sandbox/env contract
//! rows (fail-closed ordering, prefix matching, sandbox + env invariants).

use super::*;

#[test]
fn generated_command_policy_cases_cover_policy_sandbox_and_env_contracts() {
    let forbidden = lean_command_policy_case("forbidden_prefix_wins_over_allowed_prefix_order");
    assert_eq!(forbidden.category, "forbidden_prefix");
    assert_eq!(forbidden.decision, "deny");
    assert_eq!(forbidden.denial_reason.as_deref(), Some("forbiddenPrefix"));
    assert_eq!(
        forbidden.matched_prefix.as_ref(),
        Some(&vec!["git".to_string()])
    );
    let second_forbidden = lean_command_policy_case("forbidden_prefix_second_configured_match");
    assert_eq!(
        second_forbidden.matched_prefix.as_ref(),
        Some(&vec!["git".to_string(), "diff".to_string()])
    );

    let allowed =
        lean_command_policy_case("allowed_prefix_required_precedes_network_and_allowlist");
    assert_eq!(allowed.decision, "deny");
    assert_eq!(
        allowed.denial_reason.as_deref(),
        Some("allowedPrefixRequired")
    );
    assert_eq!(
        allowed.denied_argv.as_ref(),
        Some(&vec!["curl".to_string(), "https://example.com".to_string()])
    );

    let configured =
        lean_command_policy_case("allowed_prefix_authorizes_read_only_diagnostic_command");
    assert_eq!(configured.category, "read_only_configured_prefix");
    assert_eq!(configured.decision, "allow");

    let configured_forbidden =
        lean_command_policy_case("forbidden_prefix_overrides_configured_read_only_diagnostic");
    assert_eq!(configured_forbidden.decision, "deny");
    assert_eq!(
        configured_forbidden.denial_reason.as_deref(),
        Some("forbiddenPrefix")
    );

    let curl = lean_command_policy_case("disabled_network_read_only_curl_denies_before_allowlist");
    assert_eq!(
        curl.denial_reason.as_deref(),
        Some("disabledNetworkCommand")
    );
    assert_eq!(curl.denied_command.as_deref(), Some("curl"));

    let workspace = lean_command_sandbox_case("workspace_write_enforced_selects_macos_seatbelt");
    assert_eq!(workspace.decision, "selected");
    assert_eq!(workspace.sandbox.as_deref(), Some("macos_seatbelt"));

    let unrestricted = lean_command_sandbox_case("unrestricted_selects_unsandboxed_unrestricted");
    assert_eq!(
        unrestricted.sandbox.as_deref(),
        Some("unsandboxed_unrestricted")
    );

    let key = lean_command_env_case("env_key_marker_dropped");
    assert_eq!(key.input_name, "OPENAI_API_KEY");
    assert_eq!(key.expected_output_value, None);

    let pager = lean_command_env_case("env_pager_forced_cat");
    assert_eq!(pager.expected_output_value.as_deref(), Some("cat"));
    let pager_absent = lean_command_env_case("env_pager_absent_still_forced_cat");
    assert_eq!(pager_absent.expected_output_value.as_deref(), Some("cat"));
}
