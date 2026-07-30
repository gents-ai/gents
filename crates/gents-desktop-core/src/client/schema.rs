use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::schemas::{
    ALL_COLLECTION_NAMES, BRANCHABLE_COLLECTION_NAMES, RUNTIME_COLLECTION_NAMES,
};

pub async fn ensure_runtime_schemas(node: &EmbeddedNode) -> Result<()> {
    gents_migration::ensure_migrations(node)
        .await
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!(e))
        .context("ensure_migrations")
}

#[allow(dead_code)]
pub async fn ensure_schemas(node: &EmbeddedNode) -> Result<()> {
    ensure_runtime_schemas(node).await
}

pub async fn subscribe_all_collections(node: &EmbeddedNode) -> Result<()> {
    let p2p = node.p2p().context("desktop node missing P2P support")?;

    for name in subscribed_collection_names() {
        match p2p.add_collections(vec![name.to_owned()]).await {
            Ok(()) => {}
            Err(error) => {
                if error.to_string().contains("already") {
                    tracing::debug!(collection = name, "collection already subscribed");
                } else {
                    return Err(error.into());
                }
            }
        }
    }

    Ok(())
}

pub fn subscribed_collection_names() -> Vec<&'static str> {
    RUNTIME_COLLECTION_NAMES
        .iter()
        .chain(ALL_COLLECTION_NAMES.iter())
        .copied()
        .collect()
}

pub fn branchable_collection_names() -> Vec<&'static str> {
    BRANCHABLE_COLLECTION_NAMES.to_vec()
}
