use std::sync::Arc;

use super::GrantSet;
use crate::identity::NodeIdentity;

pub fn set_named(name: &str) -> GrantSet {
    GrantSet::ALL
        .into_iter()
        .find(|set| set.name() == name)
        .unwrap_or_else(|| panic!("Lean grant set {name:?} has no Rust owner"))
}

pub async fn nac_node() -> (
    tempfile::TempDir,
    crate::identity::KeyIdentity,
    Arc<defra_node::Node>,
) {
    let directory = tempfile::tempdir().unwrap();
    let runtime =
        crate::identity::KeyIdentity::load_or_create(directory.path().join("runtime.key"), None)
            .unwrap();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    (directory, runtime, node)
}

/// A delegate acts as a managed Agent: native relation changes by a non-node actor
/// require its validated Agent signer.
pub fn agent_delegate(
    runtime: &crate::identity::KeyIdentity,
) -> (identity::Did, Arc<dyn NodeIdentity>) {
    let binding = crate::identity::provision_agent_identity(runtime).unwrap();
    let agent = crate::identity::resolve_agent_identity(runtime, &binding).unwrap();
    let did = identity::Did::new(agent.did()).unwrap();
    (did, agent)
}

pub fn did(directory: &tempfile::TempDir, name: &str) -> identity::Did {
    let key =
        crate::identity::KeyIdentity::load_or_create(directory.path().join(name), None).unwrap();
    identity::Did::new(key.did()).unwrap()
}
