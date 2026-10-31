use gents_protocol::schemas;

mod common;

fn without(fields: &[&str]) -> String {
    schemas::INFERENCE_BACKEND
        .lines()
        .filter(|line| {
            !fields
                .iter()
                .any(|field| line.trim_start().starts_with(&format!("{field}:")))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A store at `schema` holds one backend document; migrating it keeps the collection
/// lineage, every earlier field id and the document, and a second pass applies nothing.
async fn migrates_from(schema: &str) {
    let node = common::fresh_node().await;
    node.add_collection(schema).await.unwrap();
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
    assert!(after
        .fields
        .iter()
        .any(|field| field.name == "claude_platform"));
    let repeat = gents_migration::ensure_migrations(&node).await.unwrap();
    assert_eq!(repeat.steps_applied, 0);
    let retained = node
        .execute("{ InferenceBackend { _docID name claude_platform catalog_freshness } }")
        .await;
    assert!(!retained.has_errors(), "{retained:?}");
    let retained = retained.data.unwrap();
    assert_eq!(retained["InferenceBackend"][0]["_docID"], document);
    assert_eq!(retained["InferenceBackend"][0]["name"], "retained backend");
    assert!(retained["InferenceBackend"][0]["claude_platform"].is_null());
    node.shutdown().await;
}

#[tokio::test]
async fn claude_platform_extends_a_store_created_with_catalog_freshness() {
    migrates_from(&without(&["claude_platform"])).await;
}

#[tokio::test]
async fn claude_platform_extends_the_original_rc1_lineage_through_catalog_freshness() {
    migrates_from(&without(&["claude_platform", "catalog_freshness"])).await;
}

#[tokio::test]
async fn a_fresh_store_registers_the_current_backend_baseline() {
    let fresh = common::fresh_node().await;
    fresh
        .add_collection(schemas::INFERENCE_BACKEND)
        .await
        .unwrap();
    let version = fresh.get_collection("InferenceBackend").unwrap().unwrap();
    let expected = gents_migration::DEFAULT_BASELINE
        .iter()
        .find(|entry| entry.name == "InferenceBackend")
        .unwrap();
    assert_eq!(Some(version.version_id.as_str()), expected.expected_version);
    fresh.shutdown().await;
}
