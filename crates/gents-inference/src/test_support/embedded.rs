use gents_documents::test_support::SignedHome;

/// Creates the User's default fixture Agent through the provider profile
/// owner, marks its backend healthy and selects it.
pub async fn default_agent(
    home: &SignedHome,
) -> gents_protocol::agent_identity::AgentIdentityBinding {
    let binding = home
        .create_agent(
            "Default fixture agent",
            &crate::resolved::ProviderProfilePolicy,
        )
        .await;
    mark_backend_healthy(&home.user_access, home.node_did(), &binding).await;
    home.select_default_agent(&binding.agent_did).await;
    binding
}

pub(super) async fn mark_backend_healthy(
    access: &gents_store::config_client::ConfigAccess,
    owner: &str,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
) {
    let backend = format!("{}:backend", binding.agent_did);
    crate::backends::registry::set_backend_probe_status_as(access, owner, &backend, "healthy")
        .await
        .unwrap();
}
