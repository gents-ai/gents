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
    test_agent_identity().did().to_owned()
}

pub fn test_agent_access(
    node: &std::sync::Arc<defra_node::Node>,
) -> gents_store::config_client::ConfigAccess {
    let agent = test_agent_identity();
    gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap()
}

/// One store written by two Agents that sign their commits and, between
/// reopenings, without any signing identity. A Gents node always signs, so a
/// commit without a signature comes from a store opened without one.
pub struct MixedSigningStore {
    dir: tempfile::TempDir,
    runtime: gents_store::identity::KeyIdentity,
    agents: [std::sync::Arc<dyn gents_store::identity::NodeIdentity>; 2],
    node: Option<std::sync::Arc<defra_node::Node>>,
    signing: bool,
}

impl MixedSigningStore {
    pub async fn new(sdl: &str) -> Self {
        use gents_store::identity::NodeIdentity as _;
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            gents_store::identity::KeyIdentity::load_or_create(dir.path().join("node.key"), None)
                .unwrap();
        let agents = [(), ()].map(|()| {
            let binding = gents_store::identity::provision_agent_identity(&runtime).unwrap();
            gents_store::identity::resolve_agent_identity(&runtime, &binding).unwrap()
        });
        let node = Self::open(dir.path(), Some(runtime.did())).await;
        node.add_collection(sdl).await.unwrap();
        Self {
            dir,
            runtime,
            agents,
            node: Some(node),
            signing: true,
        }
    }

    async fn open(dir: &std::path::Path, did: Option<&str>) -> std::sync::Arc<defra_node::Node> {
        let builder = defra_node::Node::builder().data_path(dir.join("store"));
        let builder = match did {
            Some(did) => builder.with_node_identity_did(did),
            None => builder,
        };
        std::sync::Arc::new(builder.build().await.unwrap())
    }

    pub fn node(&self) -> &std::sync::Arc<defra_node::Node> {
        self.node.as_ref().expect("store is open")
    }

    pub fn agent_did(&self, index: usize) -> String {
        self.agents[index].did().to_owned()
    }

    /// Access that signs as Agent `index`; the store must be signing.
    pub fn agent_access(&self, index: usize) -> gents_store::config_client::ConfigAccess {
        assert!(self.signing, "an Agent signs only in a signing store");
        gents_store::config_client::ConfigAccess::local_managed_agent(
            self.node().clone(),
            identity::Did::new(self.agents[index].did()).unwrap(),
            self.agents[index].as_ref(),
        )
        .unwrap()
    }

    /// Access whose commits carry no signature; the store must not be signing.
    pub fn unsigned_access(&self) -> gents_store::config_client::ConfigAccess {
        assert!(
            !self.signing,
            "an unsigned write needs a store without a signer"
        );
        gents_store::config_client::ConfigAccess::local(self.node().clone())
    }

    pub async fn reopen(&mut self, signing: bool) {
        use gents_store::identity::NodeIdentity as _;
        if self.signing == signing {
            return;
        }
        let node = self.node.take().expect("store is open");
        node.shutdown().await.expect("shut down store");
        drop(node);
        self.node = Some(Self::open(self.dir.path(), signing.then(|| self.runtime.did())).await);
        self.signing = signing;
    }

    pub async fn shutdown(self) {
        self.node().shutdown().await.expect("shut down store");
    }
}
