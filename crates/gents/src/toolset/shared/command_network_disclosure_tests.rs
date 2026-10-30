use super::*;

#[test]
fn generated_command_isolation_matches_native_policy() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().command_isolation_cases;
    assert_eq!(cases.len(), 12);
    for value in cases {
        let mode: CommandExecutionMode = serde_json::from_value(value["mode"].clone()).unwrap();
        let network: CommandNetworkMode = serde_json::from_value(value["network"].clone()).unwrap();
        let effective: CommandNetworkMode =
            serde_json::from_value(value["effectiveNetwork"].clone()).unwrap();
        let protects_credentials = value["protectsCredentials"].as_bool().unwrap();
        let policy = CommandExecutionPolicy::write_capable()
            .with_mode(mode)
            .with_network_mode(network);
        assert_eq!(policy.effective_network_mode(), effective, "{value}");
        assert_eq!(mode.protects_credentials(), protects_credentials, "{value}");
        if protects_credentials && effective == CommandNetworkMode::Disabled {
            assert!(policy
                .network_enforcement_disclosure()
                .starts_with("required_at_execution:"));
        }
    }
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
