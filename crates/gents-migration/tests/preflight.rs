use gents_migration::{check_migration_compatibility, ensure_migrations, Error};
use gents_protocol::schemas;
mod common;
use common::fresh_node;

#[tokio::test]
async fn preflight_accepts_the_same_legacy_lineage_without_mutating_it() {
    let node = fresh_node().await;
    let previous = schemas::INFERENCE_PROFILE.replace("    service_tier: String\n", "");
    node.add_collection(&previous).await.unwrap();
    let before = node.get_all_collection_versions().await.unwrap();
    check_migration_compatibility(&before).unwrap();
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(node.get_all_collection_versions().await.unwrap()).unwrap()
    );
    ensure_migrations(node.as_ref()).await.unwrap();
    check_migration_compatibility(&node.get_all_collection_versions().await.unwrap()).unwrap();
    node.shutdown().await;
}

#[tokio::test]
async fn preflight_and_startup_refuse_the_same_unknown_lineage() {
    let node = fresh_node().await;
    node.add_collection("type InferenceProfile { alien: String }")
        .await
        .unwrap();
    let before = node.get_all_collection_versions().await.unwrap();
    assert!(matches!(
        check_migration_compatibility(&before),
        Err(Error::UnknownLineage { .. })
    ));
    assert!(matches!(
        ensure_migrations(node.as_ref()).await,
        Err(Error::UnknownLineage { .. })
    ));
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(node.get_all_collection_versions().await.unwrap()).unwrap()
    );
    node.shutdown().await;
}
