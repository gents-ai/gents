pub struct SelfConfigFixture {
    pub db: crate::support::TestDb,
    pub user_access: gents_store::config_client::ConfigAccess,
    pub agent_did: String,
}

impl SelfConfigFixture {
    pub async fn new(name: &str) -> Self {
        let (db, _user, user_access, binding) =
            crate::test_support::root::signed_test_node(name).await;
        gents_toolcalls::test_support::grant_modeled_direct_sets(
            &db.node,
            db.node_identity.did(),
            "default-agent",
            &binding.agent_did,
        )
        .await;
        Self {
            db,
            user_access,
            agent_did: binding.agent_did,
        }
    }

    pub async fn install_sibling(&self, display_name: &str) -> String {
        let user = identity::Did::new(self.user_access.actor_did().unwrap()).unwrap();
        let binding = gents_store::identity::provision_agent_identity_authorized(
            &self.db.node,
            self.db.node_identity.as_ref(),
            &user,
        )
        .await
        .unwrap();
        gents_inference::test_support::install_test_agent_binding_with_access(
            self.db.node_identity.did(),
            display_name,
            &binding,
            &self.user_access,
        )
        .await;
        binding.agent_did
    }

    pub async fn grant_user_relation_delete(&self) {
        let admin = gents_store::config_client::ConfigAccess::local_as(
            self.db.node.clone(),
            identity::Did::new(self.db.node_identity.did()).unwrap(),
        );
        let user = identity::Did::new(self.user_access.actor_did().unwrap()).unwrap();
        admin
            .grant_node_permission(
                &user,
                gents_store::config_client::NodePermission::DacRelationDelete,
            )
            .await
            .unwrap();
    }

    pub fn agent_did(&self) -> &str {
        &self.agent_did
    }

    pub fn node_did(&self) -> &str {
        self.db.node_identity.did()
    }

    pub fn user_access(&self) -> &gents_store::config_client::ConfigAccess {
        &self.user_access
    }
}
