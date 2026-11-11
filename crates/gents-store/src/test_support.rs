pub mod acp_policies;

pub fn test_identity(name: &str) -> crate::identity::KeyIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    crate::identity::KeyIdentity::load_or_create(path, None).unwrap()
}

pub async fn delete_configuration_document(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    collection: &str,
    doc_id: &str,
) -> Result<(), crate::config_client::ConfigClientError> {
    txn.delete_configuration_document(collection, doc_id).await
}

/// A test's `#[tokio::test]` runtime is its entry point; constructors take
/// that runtime's handle from here.
#[allow(
    clippy::disallowed_methods,
    reason = "a test's #[tokio::test] runtime is its entry-point runtime"
)]
pub fn runtime_handle() -> tokio::runtime::Handle {
    tokio::runtime::Handle::current()
}

/// Fixtures that reopen a store must retain its original signing identity.
pub fn node_builder() -> defra_node::NodeBuilder {
    use crate::identity::NodeIdentity as _;

    let directory = tempfile::tempdir().unwrap();
    let identity =
        crate::identity::KeyIdentity::load_or_create(directory.path().join("node.key"), None)
            .unwrap();
    defra_node::Node::builder().with_node_identity_did(identity.did())
}

pub struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    pub(crate) fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(previous) => std::env::set_var(self.key, previous),
            None => std::env::remove_var(self.key),
        }
    }
}

/// Recovers the runtime root held by this process for `node_did`. Loading a
/// `KeyIdentity` registers its signing key with DefraDB; Agent derivation
/// needs the held key, which `RegisteredIdentity` never carries.
pub fn held_runtime_identity(node_did: &str) -> crate::identity::KeyIdentity {
    use crate::identity::NodeIdentity as _;
    let config = defra_core::signing::get_identity(node_did)
        .unwrap_or_else(|| panic!("fixture node identity {node_did} is not held in this process"));
    assert!(
        config.has_local_private_key(),
        "fixture node identity {node_did} has no local private key"
    );
    let dir = tempfile::tempdir().expect("held identity directory");
    let path = dir.path().join("runtime.key");
    {
        use std::io::Write as _;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&path).expect("create held identity key");
        file.write_all(&config.private_key_bytes)
            .expect("write held identity key");
    }
    let identity = crate::identity::KeyIdentity::load_existing(path.clone(), None)
        .expect("reload held runtime identity");
    assert_eq!(identity.did(), node_did);
    identity
}

pub fn first_content<T>(items: &[T]) -> &T {
    items.first().expect("non-empty content")
}

pub const CORRUPT_TOOL_ARGS_589: &str = "{\"raw_schema\": false, \
     \"service_id\": \"observability-mcp\", \"tool房\n</think\": \"\n<tool_call>\n\
     <function=describe_tool>\", \"raw_schema\": false, \
     \"service_id\": \"observability-mcp\", \"tool_name\": \"list_hosts\"}";
