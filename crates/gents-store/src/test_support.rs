pub mod acp_policies;
pub mod relationships;

pub fn attach_node_extensions(
    node: &std::sync::Arc<defra_node::Node>,
    extensions: &crate::node_extension::Extensions,
) {
    crate::node_extension::attach_all(node, extensions).expect("fixture extensions attach once");
}

const PIN_FIXED_KEY_HEX: &str = "4cbf8c1186d2fcb70559342fd142650a5ec5938d26a187d87e2c061b530d7be46edb79d5f548207182f7911b55709c9e4b9961c709486e5ce920e306470fe6d6";
pub const PIN_FIXED_DID: &str = "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7";

pub fn pin_fixed_signing_identity(dir: &std::path::Path) -> crate::identity::KeyIdentity {
    let key_bytes: Vec<u8> = (0..PIN_FIXED_KEY_HEX.len())
        .step_by(2)
        .map(|offset| u8::from_str_radix(&PIN_FIXED_KEY_HEX[offset..offset + 2], 16).unwrap())
        .collect();
    let mut key_file = tempfile::NamedTempFile::new_in(dir).expect("create private fixed key");
    std::io::Write::write_all(&mut key_file, &key_bytes).expect("write fixed pinning key");
    let identity = crate::identity::KeyIdentity::load_existing(key_file.path(), None)
        .expect("load fixed identity");
    assert_eq!(
        crate::identity::NodeIdentity::did(&identity),
        PIN_FIXED_DID,
        "fixed pinning key derives a stable DID"
    );
    identity
}

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
    pub fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
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

/// Writes the private key this process holds for `node_did` to `path`.
pub fn write_held_identity_key(node_did: &str, path: &std::path::Path) {
    let config = defra_core::signing::get_identity(node_did)
        .unwrap_or_else(|| panic!("fixture node identity {node_did} is not held in this process"));
    assert!(
        config.has_local_private_key(),
        "fixture node identity {node_did} has no local private key"
    );
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path).expect("create held identity key");
    file.write_all(&config.private_key_bytes)
        .expect("write held identity key");
}

/// Recovers the runtime root held by this process for `node_did`. Loading a
/// `KeyIdentity` registers its signing key with DefraDB; Agent derivation
/// needs the held key, which `RegisteredIdentity` never carries.
pub fn held_runtime_identity(node_did: &str) -> crate::identity::KeyIdentity {
    use crate::identity::NodeIdentity as _;
    let dir = tempfile::tempdir().expect("held identity directory");
    let path = dir.path().join("runtime.key");
    write_held_identity_key(node_did, &path);
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

/// The verified signer of `cid`, read under document ACP as `reader`: a
/// private row's block is visible only to an actor its policy admits, so an
/// anonymous node read of it fails closed.
pub async fn verified_block_signer_as(node: &defra_node::Node, reader: &str, cid: &str) -> String {
    let (block, signature) = node
        .authorized_signed_block_bytes(cid, Some(reader))
        .await
        .unwrap_or_else(|error| panic!("read {cid} as {reader}: {error:#}"));
    let block = defra_core::block::Block::from_dag_cbor(&block).expect("decode signed block");
    let signature =
        defra_core::block::Signature::from_dag_cbor(&signature).expect("decode block signature");
    db::block::verify::verified_signature_signer_did(&block, &signature)
        .unwrap_or_else(|error| panic!("verify {cid}: {error}"))
}
