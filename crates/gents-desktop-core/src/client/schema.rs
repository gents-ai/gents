use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use gents_protocol::schemas::ALL_COLLECTION_NAMES;

#[cfg(test)]
pub async fn ensure_runtime_schemas(node: &EmbeddedNode) -> Result<()> {
    gents_store::schema::ensure_runtime_schemas(node).await
}

pub async fn subscribe_all_collections(node: &EmbeddedNode) -> Result<()> {
    let p2p = gents_store::identity::node_p2p(node)
        .context("desktop node missing identity-bound P2P support")?;
    subscribe_runtime_collections(p2p.as_ref()).await
}

pub(crate) async fn subscribe_runtime_collections(
    p2p: &dyn defra_p2p_adapter::P2POperations,
) -> Result<()> {
    p2p.add_collections(
        subscribed_collection_names()
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )
    .await?;
    Ok(())
}

pub fn subscribed_collection_names() -> Vec<&'static str> {
    ALL_COLLECTION_NAMES
        .iter()
        .copied()
        .filter(|name| gents_protocol::schemas::is_replicated_protocol_collection(name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::subscribed_collection_names;

    #[test]
    fn desktop_subscriptions_are_exactly_the_modeled_protocol_allowlist() {
        let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let contract = &snapshot["identity_contracts"][0];
        let expected: std::collections::BTreeSet<_> = contract["replicated_protocol_collections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect();
        assert_eq!(
            subscribed_collection_names()
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            expected
        );
    }

    #[test]
    fn generated_file_review_rules_exclude_whole_collection_delivery() {
        use gents_lean_vocab::families::file_review_route_contracts_subscription as subscription;
        let subscriptions = subscribed_collection_names();
        gents_lean_vocab::families::file_review_route_contracts_subscription::replay(
            |input: &subscription::SubscriptionInput| {
                let name = input.collection.as_str();
                let replicated = gents_protocol::schemas::is_replicated_protocol_collection(name);
                assert!(gents_protocol::schemas::ALL_COLLECTION_NAMES.contains(&name));
                assert_eq!(subscriptions.contains(&name), replicated, "{name}");
                assert_eq!(
                    gents_protocol::schemas::BRANCHABLE_COLLECTION_NAMES.contains(&name),
                    replicated,
                    "{name}"
                );
                subscription::SubscriptionOutput { replicated }
            },
        );
    }
}
