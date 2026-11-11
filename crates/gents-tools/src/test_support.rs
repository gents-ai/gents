pub fn tool_policy(ceiling: &crate::surface::ToolCeiling) -> &crate::surface::ToolPolicySurface {
    ceiling.policy()
}

/// Fixture identities shared by tool tests: the pinned root the runtime holds
/// and the Agents derived from it.
pub fn test_root_identity() -> gents_store::identity::KeyIdentity {
    let home = tempfile::tempdir().unwrap();
    gents_session::request::lifecycle::test_support::pin_fixed_signing_identity(home.path())
}

pub fn test_agent_identity() -> std::sync::Arc<dyn gents_store::identity::NodeIdentity> {
    let root = test_root_identity();
    let binding = gents_protocol::agent_identity::AgentIdentityBinding {
        creation_nonce: "c".repeat(64),
        agent_did: gents_store::identity::derive_agent_identity(&root, &"c".repeat(64))
            .unwrap()
            .did()
            .to_owned(),
    };
    gents_store::identity::resolve_agent_identity(&root, &binding).unwrap()
}

pub fn test_agent_did() -> String {
    use gents_store::identity::NodeIdentity as _;
    test_agent_identity().did().to_owned()
}

pub fn test_agent_access(
    node: &std::sync::Arc<defra_node::Node>,
) -> gents_store::config_client::ConfigAccess {
    use gents_store::identity::NodeIdentity as _;
    let agent = test_agent_identity();
    gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap()
}
