use gents_documents::test_support::SignedHome;
use gents_store::identity::KeyIdentity;
use gents_store::test_support::TestStore;

pub async fn signed_test_node(
    name: &str,
    composition: gents_store::node_extension::Extensions,
) -> (
    TestStore,
    KeyIdentity,
    gents_store::config_client::ConfigAccess,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    signed_test_node_from_db(TestStore::start(name, composition).await, name).await
}

pub async fn signed_test_node_without_agent(
    name: &str,
    composition: gents_store::node_extension::Extensions,
) -> (
    TestStore,
    KeyIdentity,
    gents_store::config_client::ConfigAccess,
) {
    SignedHome::start(name, composition).await.into_parts()
}

pub async fn signed_test_node_without_agent_from_db(
    db: TestStore,
    name: &str,
) -> (
    TestStore,
    KeyIdentity,
    gents_store::config_client::ConfigAccess,
) {
    SignedHome::enroll(db, name).await.into_parts()
}

pub async fn signed_test_node_from_db(
    db: TestStore,
    name: &str,
) -> (
    TestStore,
    KeyIdentity,
    gents_store::config_client::ConfigAccess,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    let home = SignedHome::enroll(db, name).await;
    let binding = default_agent(&home).await;
    let (db, user, access) = home.into_parts();
    (db, user, access, binding)
}

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
