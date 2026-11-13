use super::command_validation::validate_network_mode;
use super::*;

#[test]
fn command_policy_effective_network_mode_matches_lean() {
    gents_lean_vocab::families::command_policy_effective_network_mode::replay(
        |i: &gents_lean_vocab::families::command_policy_effective_network_mode::Input<
            CommandExecutionMode,
            CommandNetworkMode,
        >| {
            let policy = CommandExecutionPolicy::write_capable()
                .with_mode(i.mode)
                .with_network_mode(i.network);
            let effective = policy.effective_network_mode();
            if i.mode.protects_credentials() && effective == CommandNetworkMode::Disabled {
                assert!(policy
                    .network_enforcement_disclosure()
                    .starts_with("required_at_execution:"));
            }
            effective
        },
    );
}

#[test]
fn disclosure_tracks_the_execution_time_network_validator() {
    let unrestricted = CommandExecutionPolicy::write_capable()
        .with_mode(CommandExecutionMode::Unrestricted)
        .with_network_mode(CommandNetworkMode::Disabled);
    assert!(validate_network_mode("echo", &[], &unrestricted).is_err());
    assert!(unrestricted
        .network_enforcement_disclosure()
        .starts_with("fail_closed:"));

    for mode in [
        CommandExecutionMode::WorkspaceWrite,
        CommandExecutionMode::ArtifactWrite,
    ] {
        let sandboxed = CommandExecutionPolicy::write_capable()
            .with_mode(mode)
            .with_network_mode(CommandNetworkMode::Disabled);
        assert!(validate_network_mode("echo", &[], &sandboxed).is_ok());
        assert!(sandboxed
            .network_enforcement_disclosure()
            .starts_with("required_at_execution:"));
    }

    let read_only = CommandExecutionPolicy::read_only(vec!["curl".into()])
        .with_network_mode(CommandNetworkMode::Disabled);
    assert!(validate_network_mode("curl", &[], &read_only).is_err());
    assert!(validate_network_mode("echo", &[], &read_only).is_ok());
    assert!(read_only
        .network_enforcement_disclosure()
        .starts_with("validated_at_execution:"));

    let inherited = CommandExecutionPolicy::read_only(Vec::new());
    assert!(validate_network_mode("curl", &[], &inherited).is_ok());
    assert_eq!(
        inherited.network_enforcement_disclosure(),
        "network is not disabled by this agent policy"
    );
}
