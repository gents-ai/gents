pub mod agent_identity;
pub mod client;
pub mod error;
pub mod home_reset;
pub mod inference;
mod inference_activity;
pub mod local_runtime;
pub mod preference;
pub mod remote_admin;

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

    pub(crate) fn node_builder() -> defra_node::NodeBuilder {
        use gents::NodeIdentity as _;

        let directory = tempfile::tempdir().unwrap();
        let identity =
            gents::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
        defra_node::Node::builder().with_node_identity_did(identity.did())
    }
}
