use std::sync::Arc;

use gents_documents::test_support::SignedHome;
use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;

/// A [`SignedHome`] with its default Agent: the Agent's identity and the
/// managed access that owns its request, queue and output rows.
pub struct AgentHome {
    pub home: SignedHome,
    pub binding: AgentIdentityBinding,
    pub agent_identity: Arc<dyn NodeIdentity>,
    pub agent_access: ConfigAccess,
}

impl std::ops::Deref for AgentHome {
    type Target = SignedHome;

    fn deref(&self) -> &SignedHome {
        &self.home
    }
}

impl AgentHome {
    pub async fn start(name: &str) -> Self {
        Self::start_with(name, super::no_lifecycle_plugins()).await
    }

    /// A home composed with `composition`, for tests that need a plugin (such
    /// as Goals) that the session crate cannot name.
    pub async fn start_with(
        name: &str,
        composition: gents_store::node_extension::Extensions,
    ) -> Self {
        let home = SignedHome::start(name, composition).await;
        let binding = gents_inference::test_support::default_agent(&home).await;
        let agent_identity =
            gents_store::identity::resolve_agent_identity(home.node_identity.as_ref(), &binding)
                .expect("derived test agent identity");
        let agent_access = home.agent_access(&binding);
        Self {
            home,
            binding,
            agent_identity,
            agent_access,
        }
    }

    pub fn agent_did(&self) -> &str {
        self.agent_identity.did()
    }
}
