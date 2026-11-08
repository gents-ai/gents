pub mod agent_identity;
pub mod client;
pub mod error;
pub mod home_reset;
pub mod inference;
mod inference_activity;
pub mod local_runtime;
pub mod preference;
pub mod remote_admin;
pub mod workspace_root;

#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
pub mod test_fixtures;

#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) fn unbound_journal_schema() -> String {
        use gents_protocol::schemas::{CLIENT_JOURNAL_POLICY_ID, CLIENT_JOURNAL_POLICY_RESOURCE};
        let directive = format!(
            r#" @policy(id: "{CLIENT_JOURNAL_POLICY_ID}", resource: "{CLIENT_JOURNAL_POLICY_RESOURCE}")"#
        );
        let sdl = gents_protocol::schemas::PENDING_CHAT_SUBMISSION;
        assert!(sdl.contains(&directive));
        sdl.replacen(&directive, "", 1)
    }

    /// The desktop's local journal store: its node identity is the client key,
    /// as `bootstrap` opens it, so the client's journal writes are its own.
    pub(crate) async fn client_journal_node(
        client_did: &str,
    ) -> anyhow::Result<std::sync::Arc<defra_node::Node>> {
        let node = std::sync::Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(client_did)
                .build()
                .await?,
        );
        node.add_collection(&unbound_journal_schema()).await?;
        Ok(node)
    }

    pub(crate) fn node_builder() -> defra_node::NodeBuilder {
        use gents::NodeIdentity as _;

        let directory = tempfile::tempdir().unwrap();
        let identity =
            gents::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
        defra_node::Node::builder().with_node_identity_did(identity.did())
    }
}
