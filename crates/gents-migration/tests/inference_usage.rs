use gents_migration::ensure_migrations;
use gents_protocol::schemas;

mod common;

#[tokio::test]
async fn fresh_inference_usage_keeps_its_canonical_root_and_single_indexes() {
    let node = common::fresh_node().await;
    node.add_collection(schemas::INFERENCE_CALL).await.unwrap();
    let before = node.get_collection("InferenceCall").unwrap().unwrap();
    ensure_migrations(node.as_ref()).await.unwrap();
    let after = node.get_collection("InferenceCall").unwrap().unwrap();
    assert_eq!(after.version_id, before.version_id);
    for name in ["session_id", "model_name", "profile_id"] {
        assert_eq!(
            after
                .indexes
                .iter()
                .filter(|index| {
                    !index.resolved_unique()
                        && index.fields.len() == 1
                        && index.fields[0].name == name
                        && !index.fields[0].descending
                })
                .count(),
            1
        );
    }
    node.shutdown().await;
}

#[tokio::test]
async fn incompatible_inference_usage_stays_refused_without_schema_changes() {
    let node = common::fresh_node().await;
    let incompatible =
        schemas::INFERENCE_CALL.replace("prompt_tokens: Int", "prompt_tokens: String");
    node.add_collection(&incompatible).await.unwrap();
    let before = node.get_collection("InferenceCall").unwrap().unwrap();
    let error = ensure_migrations(node.as_ref()).await.unwrap_err();
    assert!(matches!(
        error,
        gents_migration::Error::UnknownLineage { .. }
    ));
    let versions = node.get_all_collection_versions().await.unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].version_id, before.version_id);
    node.shutdown().await;
}
