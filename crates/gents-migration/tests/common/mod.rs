use std::sync::Arc;

use crypto::Key;
use defra_core::signing::{SigningConfig, SigningKeyType};
use defra_node::EmbeddedNode;
use identity::{Identity as _, RawIdentity};

pub fn node_identity() -> String {
    let key = crypto::generate_ed25519().expect("node key");
    let identity = RawIdentity::from_ed25519(key.clone()).expect("node identity");
    let did = identity.did().expect("node DID").to_string();
    let public = identity.public_key_bytes();
    defra_core::signing::store_identity(
        &did,
        SigningConfig {
            key_type: SigningKeyType::Ed25519,
            private_key_bytes: key.raw().to_vec(),
            public_key_hex: hex::encode(&public),
            public_key_bytes: public,
            remote_signer: None,
            signing_authorization: None,
        },
    );
    did
}

/// DefraDB can leave background sweep tasks alive after shutdown, so the temp store must outlive the node.
pub async fn fresh_node() -> Arc<EmbeddedNode> {
    let dir = tempfile::tempdir().expect("tempdir");
    let node = EmbeddedNode::builder()
        .data_path(dir.path())
        .with_node_identity_did(node_identity())
        .build()
        .await
        .expect("build node");
    let policy = node
        .add_dac_policy_with_counter(
            node.node_identity_did().unwrap(),
            gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY,
            0,
        )
        .await
        .expect("file image policy");
    assert_eq!(policy, gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_ID);
    std::mem::forget(dir);
    Arc::new(node)
}
