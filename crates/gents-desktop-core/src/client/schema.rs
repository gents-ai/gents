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

/// Prompt-bearing local audit collections are deliberately excluded.
///
/// These rows carry either a full provider request or an exact reduction
/// checkpoint — conversation and tool material in plaintext, because
/// `RenderedRequest` carries no `@policy` and no field encryption (both blocked
/// on defradb.rs#1318). Capture is on by default and writes one row per turn per
/// attempt, so subscribing it would push a full conversation body per provider
/// call onto the gossip channel, to a device class that includes iOS, for a
/// collection nothing on the desktop reads.
///
/// This list is built from `ALL_COLLECTION_NAMES` verbatim, so a new collection
/// otherwise joins the subscription set with no decision being taken about it.
/// The same exclusion is applied to the desktop live-fixture runner.
pub fn subscribed_collection_names() -> Vec<&'static str> {
    RUNTIME_COLLECTION_NAMES
        .iter()
        .chain(ALL_COLLECTION_NAMES.iter())
        .filter(|name| !gents_protocol::schemas::is_local_audit_collection(name))
        .copied()
        .collect()
}

pub fn branchable_collection_names() -> Vec<&'static str> {
    BRANCHABLE_COLLECTION_NAMES.to_vec()
}

#[cfg(test)]
mod tests {
    use super::subscribed_collection_names;

    /// The subscription set is derived from a list that grows whenever a
    /// collection is added, so the exclusion has to be asserted rather than
    /// assumed. This mirrors the desktop live-fixture runner's test.
    #[test]
    fn the_desktop_does_not_replicate_plaintext_provider_bodies() {
        let names = subscribed_collection_names();
        for sensitive in gents_protocol::schemas::LOCAL_AUDIT_COLLECTION_NAMES {
            assert!(
                !names.contains(sensitive),
                "{sensitive} must stay out of the desktop replication set: {names:?}"
            );
        }
        assert!(
            names.iter().any(|name| *name == "AgentRequest"),
            "the exclusion must not have emptied the set: {names:?}"
        );
    }
}
