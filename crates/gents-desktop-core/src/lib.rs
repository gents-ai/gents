pub mod client;
pub mod error;
pub mod home_reset;
pub mod inference;
pub mod local_runtime;
pub mod preference;
pub mod remote_admin;

#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) type TestError = Box<dyn std::error::Error + Send + Sync>;
    pub(crate) type TestResult<T = ()> = Result<T, TestError>;

    pub(crate) fn node_builder() -> defra_node::NodeBuilder {
        use gents::NodeIdentity as _;

        let directory = tempfile::tempdir().unwrap();
        let identity =
            gents::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
        defra_node::EmbeddedNode::builder().with_node_identity_did(identity.did())
    }
}
