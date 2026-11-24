use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use gents_store::node_extension::Extensions;
use gents_store::test_support::TestStore;

use crate::inference_backend::AdvertisedProfilePolicy;

/// A [`TestStore`] with an enrolled User whose key the test holds, as a
/// client holds it, and the runtime's Node document the User published.
/// Each access runs as exactly one actor: the User's client-signed access,
/// the runtime Node's own, or a managed Agent's.
pub struct SignedHome {
    pub store: TestStore,
    pub user: KeyIdentity,
    pub user_access: ConfigAccess,
}

impl std::ops::Deref for SignedHome {
    type Target = TestStore;

    fn deref(&self) -> &TestStore {
        &self.store
    }
}

impl SignedHome {
    pub async fn start(name: &str, composition: Extensions) -> Self {
        Self::enroll(TestStore::start(name, composition).await, name).await
    }

    pub async fn enroll(store: TestStore, name: &str) -> Self {
        let fixture_name = name.to_owned();
        let user = tokio::task::spawn_blocking(move || {
            gents_store::test_support::test_identity(&fixture_name)
        })
        .await
        .unwrap();
        assert_ne!(store.node_identity.did(), user.did());
        let user_access = store.client_access(&user);
        super::enroll_test_user(
            &store.node,
            store.node_identity.as_ref(),
            &user,
            &user_access,
        )
        .await;
        Self {
            store,
            user,
            user_access,
        }
    }

    pub fn node_did(&self) -> &str {
        self.store.node_identity.did()
    }

    pub fn node_access(&self) -> ConfigAccess {
        ConfigAccess::local_as(
            self.store.node.clone(),
            identity::Did::new(self.node_did()).unwrap(),
        )
    }

    /// An Agent identity the runtime provisions for the User through the
    /// authorized owner, as a client's Agent creation obtains one.
    pub async fn provision_agent(&self) -> AgentIdentityBinding {
        gents_store::identity::provision_agent_identity_authorized(
            &self.store.node,
            self.store.node_identity.as_ref(),
            &identity::Did::new(self.user.did()).unwrap(),
        )
        .await
        .unwrap()
    }

    /// Provisions an Agent and installs its documents as the User.
    pub async fn create_agent(
        &self,
        display_name: &str,
        profiles: &dyn AdvertisedProfilePolicy,
    ) -> AgentIdentityBinding {
        let binding = self.provision_agent().await;
        super::install_test_agent_as(
            &self.user_access,
            self.node_did(),
            display_name,
            &binding,
            profiles,
        )
        .await;
        binding
    }

    pub async fn select_default_agent(&self, agent_did: &str) {
        super::set_test_default_agent_as(&self.user_access, self.node_did(), agent_did).await;
    }

    pub fn agent_access(&self, binding: &AgentIdentityBinding) -> ConfigAccess {
        let agent = gents_store::identity::resolve_agent_identity(
            self.store.node_identity.as_ref(),
            binding,
        )
        .unwrap();
        ConfigAccess::local_managed_agent(
            self.store.node.clone(),
            identity::Did::new(agent.did()).unwrap(),
            agent.as_ref(),
        )
        .unwrap()
    }

    pub fn into_parts(self) -> (TestStore, KeyIdentity, ConfigAccess) {
        (self.store, self.user, self.user_access)
    }
}
