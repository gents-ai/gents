//! Phase A conformance: baseline registration, idempotence, single-version DAG.

use std::{collections::BTreeSet, sync::Arc};

use defra_node::EmbeddedNode;
use gents_migration::{
    ensure_migrations, ensure_migrations_dynamic, ensure_migrations_with_registry,
    BaselineCollectionOwned, CollectionExpectation, DynamicRegistry, Error, Registry,
};

#[test]
fn default_baseline_matches_ordered_protocol_catalog() {
    let actual = gents_migration::DEFAULT_BASELINE
        .iter()
        .map(|entry| (entry.name, entry.sdl))
        .collect::<Vec<_>>();
    let expected = gents_protocol::schemas::RUNTIME_COLLECTION_NAMES
        .iter()
        .copied()
        .zip(gents_protocol::schemas::RUNTIME_ALL.iter().copied())
        .chain(
            gents_protocol::schemas::ALL_COLLECTION_NAMES
                .iter()
                .copied()
                .zip(gents_protocol::schemas::ALL.iter().copied()),
        )
        .collect::<Vec<_>>();

    assert_eq!(
        actual.len(),
        expected.len(),
        "ordered baseline catalog length mismatch"
    );
    assert_eq!(actual, expected);
}

async fn fresh_node() -> Arc<EmbeddedNode> {
    let dir = tempfile::tempdir().expect("tempdir");
    let node = EmbeddedNode::builder()
        .data_path(dir.path())
        .build()
        .await
        .expect("build node");
    // Keep tempdir alive for the node lifetime by leaking (test process short).
    std::mem::forget(dir);
    Arc::new(node)
}

#[test]
fn default_baseline_covers_every_protocol_collection_once() {
    let baseline_names = gents_migration::DEFAULT_BASELINE
        .iter()
        .map(|entry| entry.name)
        .collect::<BTreeSet<_>>();
    let protocol_names = gents_protocol::schemas::ALL_COLLECTION_NAMES
        .iter()
        .chain(gents_protocol::schemas::RUNTIME_COLLECTION_NAMES.iter())
        .copied()
        .collect::<BTreeSet<_>>();

    assert_eq!(
        gents_migration::DEFAULT_BASELINE.len(),
        baseline_names.len(),
        "migration baseline must not contain duplicate collections"
    );
    assert!(
        gents_migration::DEFAULT_BASELINE
            .iter()
            .all(|entry| entry.expected_version.is_some()),
        "production migration baseline must pin every root version"
    );
    assert_eq!(
        baseline_names, protocol_names,
        "migration baseline must cover the full protocol schema catalog"
    );
}

#[tokio::test]
async fn ensure_migrations_registers_baseline_and_is_idempotent() {
    let node = fresh_node().await;
    let report1 = ensure_migrations(node.as_ref())
        .await
        .expect("first ensure");
    assert!(
        report1.baseline_registered + report1.baseline_already_present
            >= gents_migration::DEFAULT_BASELINE.len(),
        "expected full baseline coverage, got {report1:?}"
    );
    assert_eq!(report1.steps_applied, 0);

    let report2 = ensure_migrations(node.as_ref())
        .await
        .expect("second ensure");
    assert_eq!(report2.steps_applied, 0);
    assert!(
        report2.baseline_already_present >= gents_migration::DEFAULT_BASELINE.len()
            || report2.baseline_registered + report2.baseline_already_present
                >= gents_migration::DEFAULT_BASELINE.len(),
        "re-run should be cheap/idempotent: {report2:?}"
    );

    // Every managed collection is present and active.
    for entry in gents_migration::DEFAULT_BASELINE {
        let cv = node
            .get_collection(entry.name)
            .expect("get_collection")
            .unwrap_or_else(|| panic!("missing collection {}", entry.name));
        assert!(cv.is_active, "{} should be active", entry.name);
        assert!(
            !cv.is_placeholder,
            "{} should not be placeholder",
            entry.name
        );
    }

    node.shutdown().await;
}

#[tokio::test]
async fn multi_version_lineage_is_rejected() {
    let node = fresh_node().await;
    ensure_migrations(node.as_ref()).await.expect("baseline");

    // Apply a field patch outside the registry → foreign multi-version DAG.
    let patch = r#"[{"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"__foreign_test_field","Kind":"String"}}]"#;
    let _ = node
        .patch_collection("AgentRequest", patch)
        .await
        .expect("foreign patch");

    let err = ensure_migrations(node.as_ref())
        .await
        .expect_err("foreign multi-version DAG must fail");
    match err {
        Error::UnknownLineage { collection, .. } | Error::ForeignVersion { collection, .. } => {
            assert_eq!(collection, "AgentRequest");
        }
        other => panic!("unexpected error: {other}"),
    }

    node.shutdown().await;
}

#[tokio::test]
async fn single_version_unknown_root_is_rejected() {
    const EXPECTED_SDL: &str = "type PinnedFixture { name: String label: String }";
    const FOREIGN_SDL: &str = "type PinnedFixture { name: String }";

    let authoring_node = fresh_node().await;
    authoring_node
        .add_schema(EXPECTED_SDL)
        .await
        .expect("register expected root");
    let expected_root = authoring_node
        .get_collection("PinnedFixture")
        .expect("load expected root")
        .expect("expected root exists")
        .version_id;
    authoring_node.shutdown().await;

    let node = fresh_node().await;
    node.add_schema(FOREIGN_SDL)
        .await
        .expect("register foreign root");
    let registry = DynamicRegistry {
        baseline: vec![BaselineCollectionOwned {
            name: "PinnedFixture".into(),
            sdl: EXPECTED_SDL.into(),
            expected_version: Some(expected_root),
            expected_state: CollectionExpectation::dag_only(),
        }],
        steps: vec![],
    };

    let err = ensure_migrations_dynamic(node.as_ref(), &registry)
        .await
        .expect_err("single unknown root must fail closed");
    assert!(
        matches!(err, Error::UnknownLineage { ref collection, .. } if collection == "PinnedFixture"),
        "unexpected error: {err}"
    );

    node.shutdown().await;
}

#[tokio::test]
async fn empty_registry_injectable_for_tests() {
    // Engine accepts custom registries (conformance injects crash chains later).
    let empty = Registry {
        baseline: &[],
        steps: &[],
    };
    let node = fresh_node().await;
    let report = ensure_migrations_with_registry(node.as_ref(), &empty)
        .await
        .expect("empty registry");
    assert_eq!(report.baseline_registered, 0);
    node.shutdown().await;
}
