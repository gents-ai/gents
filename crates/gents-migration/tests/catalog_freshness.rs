use gents_protocol::schemas;

mod common;

#[tokio::test]
async fn catalog_freshness_preserves_existing_backend_lineage_and_fields() {
    let node = common::fresh_node().await;
    let previous = schemas::INFERENCE_BACKEND
        .lines()
        .filter(|line| {
            !["catalog_freshness:", "claude_platform:"]
                .iter()
                .any(|field| line.trim_start().starts_with(field))
        })
        .collect::<Vec<_>>()
        .join("\n");
    node.add_collection(&previous).await.unwrap();
    let before = node.get_collection("InferenceBackend").unwrap().unwrap();
    let created = node
        .execute(
            r#"mutation { add_InferenceBackend(input: {
        backend_id: "retained", name: "retained backend"
    }) { _docID } }"#,
        )
        .await;
    assert!(!created.has_errors(), "{created:?}");
    let document = created.data.unwrap()["add_InferenceBackend"][0]["_docID"].clone();
    gents_migration::ensure_migrations(&node).await.unwrap();
    let after = node.get_collection("InferenceBackend").unwrap().unwrap();
    assert_eq!(before.collection_id, after.collection_id);
    for field in before.fields {
        let retained = after
            .fields
            .iter()
            .find(|retained| retained.name == field.name)
            .unwrap();
        assert_eq!(retained.id, field.id, "{} moved", field.name);
        assert_eq!(retained.kind, field.kind, "{} changed kind", field.name);
    }
    let repeat = gents_migration::ensure_migrations(&node).await.unwrap();
    assert_eq!(repeat.steps_applied, 0);
    let retained = node
        .execute("{ InferenceBackend { _docID name catalog_freshness } }")
        .await;
    assert!(!retained.has_errors(), "{retained:?}");
    let retained = retained.data.unwrap();
    assert_eq!(retained["InferenceBackend"][0]["_docID"], document);
    assert_eq!(retained["InferenceBackend"][0]["name"], "retained backend");
    assert!(retained["InferenceBackend"][0]["catalog_freshness"].is_null());
    node.shutdown().await.expect("shut down test node");
    // A fresh store reaches the canonical version, which is not this store's lineage (the
    // original RC1 catalog-freshness one): each keeps its own.
    let fresh = common::fresh_node().await;
    gents_migration::ensure_migrations(&fresh).await.unwrap();
    let version = fresh.get_collection("InferenceBackend").unwrap().unwrap();
    assert_eq!(
        version.version_id,
        gents_migration::canonical_versions().unwrap()["InferenceBackend"]
    );
    fresh.shutdown().await.expect("shut down test node");
}
