use std::sync::Arc;

use crypto::Key;
use defra_core::{
    signing::{SigningConfig, SigningKeyType},
    RequestContext,
};
use defra_node::Node as EmbeddedNode;
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
    for (policy, expected) in [
        (
            gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY,
            gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_ID,
        ),
        (
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
        ),
    ] {
        let actual = node
            .add_dac_policy_with_counter_as(&node_context(&node), policy, 0)
            .await
            .expect("fixture policy");
        assert_eq!(actual, expected);
    }
    std::mem::forget(dir);
    Arc::new(node)
}

#[allow(dead_code)]
pub fn node_context(node: &EmbeddedNode) -> RequestContext {
    RequestContext::default().with_identity(node.node_identity_did().map(str::to_owned))
}

#[allow(dead_code)]
pub async fn patch_collection(
    node: &EmbeddedNode,
    collection: &str,
    patch: &str,
) -> schema::CollectionVersion {
    let before = node
        .get_all_collection_versions()
        .await
        .expect("versions before patch");
    node.patch_collection_as(&node_context(node), patch, None)
        .await
        .expect("patch collection");
    let after = node
        .get_all_collection_versions()
        .await
        .expect("versions after patch");
    after
        .into_iter()
        .find(|version| {
            version.name == collection
                && !before
                    .iter()
                    .any(|old| old.version_id == version.version_id)
        })
        .unwrap_or_else(|| {
            node.get_collection(collection)
                .expect("collection lookup")
                .expect("collection exists")
        })
}

/// Versioned steps a fresh store takes to reach the canonical versions of collections
/// changed since v1.0.0-rc0 (their rc0 root plus the patches an upgraded store takes).
#[allow(dead_code)]
pub fn fresh_canonical_steps() -> usize {
    gents_migration::DEFAULT_BASELINE
        .iter()
        .filter_map(|entry| gents_migration::canonical_schema_plan(entry.name).unwrap())
        .map(|plan| plan.patches.len())
        .sum()
}
