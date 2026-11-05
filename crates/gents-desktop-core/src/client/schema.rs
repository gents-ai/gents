use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use gents_protocol::schemas::ALL_COLLECTION_NAMES;

pub async fn ensure_runtime_schemas(node: &EmbeddedNode) -> Result<()> {
    gents::store::schema::ensure_runtime_schemas(node).await
}

#[allow(dead_code)]
pub async fn ensure_schemas(node: &EmbeddedNode) -> Result<()> {
    ensure_runtime_schemas(node).await
}

pub async fn subscribe_all_collections(node: &EmbeddedNode) -> Result<()> {
    let p2p = node.p2p().context("desktop node missing P2P support")?;
    subscribe_runtime_collections(p2p).await
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
    use gents::runtime::agent::p2p_reconcile::templates::CLIENT_INDEX_COLLECTIONS;

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
        assert!(CLIENT_INDEX_COLLECTIONS.is_empty());
    }

    #[test]
    fn generated_file_review_rules_exclude_whole_collection_delivery() {
        #[derive(serde::Deserialize)]
        struct Case {
            collection: String,
            replicated: bool,
        }
        let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases: Vec<Case> =
            serde_json::from_value(snapshot["file_review_subscription_cases"].clone()).unwrap();
        assert!(!cases.is_empty());
        let subscriptions = subscribed_collection_names();
        for case in cases {
            let name = case.collection.as_str();
            assert_eq!(
                gents_protocol::schemas::is_replicated_protocol_collection(name),
                case.replicated,
                "{name}"
            );
            assert!(gents_protocol::schemas::ALL_COLLECTION_NAMES.contains(&name));
            assert_eq!(
                subscriptions.contains(&name),
                gents_protocol::schemas::is_replicated_protocol_collection(name),
                "{name}"
            );
            assert_eq!(
                gents_protocol::schemas::BRANCHABLE_COLLECTION_NAMES.contains(&name),
                gents_protocol::schemas::is_replicated_protocol_collection(name),
                "{name}"
            );
        }
    }
}
