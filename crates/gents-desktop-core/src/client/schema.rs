use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
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
        .filter(|name| !gents_protocol::schemas::is_local_audit_collection(name))
        .filter(|name| !gents_protocol::schemas::is_local_only_collection(name))
        .filter(|name| !gents_protocol::schemas::is_credential_collection(name))
        .filter(|name| !gents_protocol::schemas::requires_explicit_delivery(name))
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::subscribed_collection_names;
    use gents::runtime::agent::p2p_reconcile::templates::CLIENT_INDEX_COLLECTIONS;

    #[test]
    fn the_desktop_does_not_replicate_plaintext_provider_bodies_or_credentials() {
        let names = subscribed_collection_names();
        for sensitive in gents_protocol::schemas::LOCAL_AUDIT_COLLECTION_NAMES
            .iter()
            .chain(gents_protocol::schemas::CREDENTIAL_COLLECTION_NAMES)
            .chain(gents_protocol::schemas::LOCAL_ONLY_COLLECTION_NAMES)
        {
            assert!(
                !names.contains(sensitive),
                "{sensitive} must stay out of the desktop replication set: {names:?}"
            );
        }
        assert!(
            names.contains(&"AgentRequest") && names.contains(&"InferenceProfile"),
            "the exclusions must not have emptied the set: {names:?}"
        );
    }

    #[test]
    fn index_collections_are_the_client_index_and_are_branchable() {
        for name in CLIENT_INDEX_COLLECTIONS {
            assert!(
                gents_protocol::schemas::BRANCHABLE_COLLECTION_NAMES.contains(&name),
                "{name} must be branchable for DAG sync"
            );
        }
        assert!(gents_protocol::schemas::NODE_READINESS.contains("@branchable"));
    }

    #[test]
    fn node_readiness_remains_subscribed() {
        let names = subscribed_collection_names();
        assert!(names.contains(&"NodeReadiness"));
        assert!(gents_protocol::schemas::ALL_COLLECTION_NAMES.contains(&"NodeReadiness"));
        assert!(!gents_protocol::schemas::BRANCHABLE_COLLECTION_NAMES.contains(&"NodeReadiness"));
    }

    #[test]
    fn local_admin_pin_is_never_subscribed_or_broad_synced() {
        let names = subscribed_collection_names();
        assert!(!names.contains(&"NetworkAdminPin"));
        assert!(!names.contains(&"EnrollmentOperatorNonce"));
        assert!(!gents_protocol::schemas::BRANCHABLE_COLLECTION_NAMES.contains(&"NetworkAdminPin"));
        assert!(!gents_protocol::schemas::BRANCHABLE_COLLECTION_NAMES
            .contains(&"EnrollmentOperatorNonce"));
        assert!(!names.contains(&gents_protocol::schemas::PENDING_CHAT_SUBMISSION_NAME));
        assert!(!gents_protocol::schemas::BRANCHABLE_COLLECTION_NAMES
            .contains(&gents_protocol::schemas::PENDING_CHAT_SUBMISSION_NAME));
    }

    #[test]
    fn enrollment_receipt_is_subscribed_but_excluded_from_broad_sync() {
        let names = subscribed_collection_names();
        assert!(names.contains(&"NetworkEnrollmentRouteReceipt"));
        assert!(gents_protocol::schemas::ALL_COLLECTION_NAMES
            .contains(&"NetworkEnrollmentRouteReceipt"));
        assert!(!gents_protocol::schemas::BRANCHABLE_COLLECTION_NAMES
            .contains(&"NetworkEnrollmentRouteReceipt"));
    }

    #[test]
    fn generated_file_review_rules_exclude_whole_collection_delivery() {
        #[derive(serde::Deserialize)]
        struct Case {
            collection: String,
            explicit_delivery: bool,
        }
        let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases: Vec<Case> =
            serde_json::from_value(snapshot["file_review_subscription_cases"].clone()).unwrap();
        assert!(!cases.is_empty());
        let subscriptions = subscribed_collection_names();
        for case in cases {
            let name = case.collection.as_str();
            assert_eq!(
                gents_protocol::schemas::requires_explicit_delivery(name),
                case.explicit_delivery,
                "{name}"
            );
            assert!(gents_protocol::schemas::ALL_COLLECTION_NAMES.contains(&name));
            assert_eq!(
                subscriptions.contains(&name),
                !case.explicit_delivery,
                "{name}"
            );
            assert_eq!(
                gents_protocol::schemas::BRANCHABLE_COLLECTION_NAMES.contains(&name),
                !case.explicit_delivery,
                "{name}"
            );
        }
    }
}
