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
        let subscribed = subscribed_collection_names();
        let cases = gents_lean_vocab::families::agent_identity_desktop_subscription::cases();
        for case in &cases {
            case.check(
                gents_lean_vocab::families::agent_identity_desktop_subscription::LocalityOutput {
                    replicates: subscribed.contains(&case.input.collection.as_str()),
                },
            );
        }
        for name in &subscribed {
            assert!(
                cases.iter().any(|case| case.input.collection == *name),
                "{name} is subscribed without a modeled protocol collection"
            );
        }
    }

    #[test]
    fn generated_file_review_rules_exclude_whole_collection_delivery() {
        let cases = &gents_lean_vocab::lean_contract_snapshot().file_review_subscription_cases;
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
