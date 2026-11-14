use gents_protocol::schemas;

use crate::common;

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

/// A store holding a backend lineage a build before the native identity baseline
/// registered (before `claude_platform`, or before `catalog_freshness` too) is refused, and
/// neither its schema nor its document is converted.
#[tokio::test]
async fn a_pre_identity_backend_lineage_is_refused_without_conversion() {
    for fields in [
        &["claude_platform"][..],
        &["claude_platform", "catalog_freshness"][..],
    ] {
        let node = common::fresh_node().await;
        node.add_collection(&without(fields)).await.unwrap();
        let before = node.get_all_collection_versions().await.unwrap();
        let created = node
            .execute(
                r#"mutation { add_InferenceBackend(input: {
            backend_id: "retained", name: "retained backend"
        }) { _docID } }"#,
            )
            .await;
        assert!(!created.has_errors(), "{created:?}");
        let document = created.data.unwrap()["add_InferenceBackend"][0]["_docID"].clone();
        assert!(
            matches!(
                gents_migration::ensure_migrations(&node).await,
                Err(gents_migration::Error::UnknownLineage { .. })
            ),
            "{fields:?}"
        );
        assert_eq!(
            serde_json::to_value(&before).unwrap(),
            serde_json::to_value(node.get_all_collection_versions().await.unwrap()).unwrap(),
            "{fields:?}"
        );
        let retained = node.execute("{ InferenceBackend { _docID name } }").await;
        assert!(!retained.has_errors(), "{retained:?}");
        let retained = retained.data.unwrap();
        assert_eq!(retained["InferenceBackend"][0]["_docID"], document);
        assert_eq!(retained["InferenceBackend"][0]["name"], "retained backend");
        node.shutdown().await.expect("shut down test node");
    }
}

/// A fresh store registers the current backend SDL directly; that version is canonical, so
/// the store never reports it as off its own lineage.
#[tokio::test]
async fn a_fresh_store_registers_the_current_backend_sdl_on_the_canonical_lineage() {
    let canonical =
        gents_migration::canonical_versions().unwrap()[schemas::INFERENCE_BACKEND_NAME].clone();
    let baseline = gents_migration::DEFAULT_BASELINE
        .iter()
        .find(|entry| entry.name == schemas::INFERENCE_BACKEND_NAME)
        .unwrap();
    assert_eq!(baseline.sdl, schemas::INFERENCE_BACKEND);
    assert_eq!(baseline.expected_version, Some(canonical.as_str()));

    let fresh = common::fresh_node().await;
    let report = gents_migration::ensure_migrations(&fresh).await.unwrap();
    assert!(
        report
            .warnings
            .iter()
            .all(|warning| !warning.starts_with(schemas::INFERENCE_BACKEND_NAME)),
        "{:?}",
        report.warnings
    );
    let version = fresh
        .get_collection(schemas::INFERENCE_BACKEND_NAME)
        .unwrap()
        .unwrap();
    assert_eq!(version.version_id, canonical);

    let direct = common::fresh_node().await;
    direct
        .add_collection(schemas::INFERENCE_BACKEND)
        .await
        .unwrap();
    let direct_version = direct
        .get_collection(schemas::INFERENCE_BACKEND_NAME)
        .unwrap()
        .unwrap();
    assert_eq!(direct_version.version_id, canonical);
    let versions = fresh.get_all_collection_versions().await.unwrap();
    assert!(gents_migration::non_canonical_store_lineages(&versions)
        .unwrap()
        .is_empty());
    fresh.shutdown().await.expect("shut down test node");
    direct.shutdown().await.expect("shut down test node");
}
