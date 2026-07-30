//! Phase B: inactive+fields lock, PatchVersioned (lensless + fixture lens),
//! crash-window resume, chain-replay pin authoring.

use std::sync::Arc;

use defra_node::EmbeddedNode;
use gents_migration::{
    ensure_migrations_dynamic, fixture_lens_wasm, predict_transform_id, BaselineCollectionOwned,
    CollectionExpectation, DynamicRegistry, LensSpec, LensSpecOwned, MigrationStepOwned,
};

const FIXTURE_SDL: &str = r#"
type FixtureDoc {
    name: String
}
"#;

const ADD_LABEL_PATCH: &str = r#"[
  {"op":"add","path":"/FixtureDoc/Fields/-","value":{"Name":"label","Kind":"String"}},
  {"op":"replace","path":"/IsActive","value":false}
]"#;

async fn fresh_node() -> Arc<EmbeddedNode> {
    let dir = tempfile::tempdir().expect("tempdir");
    let node = EmbeddedNode::builder()
        .data_path(dir.path())
        .build()
        .await
        .expect("build node");
    std::mem::forget(dir);
    Arc::new(node)
}

/// Discover the destination CID of an inactive field-add patch (authoring tool).
async fn discover_inactive_patch_pin(node: &EmbeddedNode) -> (String, String) {
    node.add_schema(FIXTURE_SDL).await.expect("add FixtureDoc");
    let v0 = node
        .get_collection("FixtureDoc")
        .expect("get")
        .expect("exists")
        .version_id;
    let patched = node
        .patch_collection("FixtureDoc", ADD_LABEL_PATCH)
        .await
        .expect("patch inactive");
    assert!(
        !patched.is_active,
        "IsActive:false must leave the new version inactive"
    );
    let still_active = node
        .get_collection("FixtureDoc")
        .expect("get")
        .expect("exists");
    assert_eq!(
        still_active.version_id, v0,
        "old version must stay active while new is inactive"
    );
    assert!(still_active.is_active);
    (v0, patched.version_id)
}

#[tokio::test]
async fn inactive_field_add_keeps_old_version_active() {
    let node = fresh_node().await;
    let (v0, v1) = discover_inactive_patch_pin(node.as_ref()).await;
    assert_ne!(v0, v1);

    // Activate the new pin — single-txn flip.
    node.set_active_collection_version(&v1)
        .await
        .expect("activate");
    let active = node
        .get_collection("FixtureDoc")
        .expect("get")
        .expect("exists");
    assert_eq!(active.version_id, v1);
    assert!(active.is_active);
    assert!(
        active.fields.iter().any(|f| f.name == "label"),
        "label field present after activation"
    );

    node.shutdown().await;
}

#[tokio::test]
async fn patch_versioned_lensless_attach_patch_activate_and_idempotent() {
    // Node A: discover pins only.
    let discover = fresh_node().await;
    let (v0, v1) = discover_inactive_patch_pin(discover.as_ref()).await;
    discover.shutdown().await;

    // Node B: apply via the engine with discovered pins.
    let node = fresh_node().await;
    let registry = DynamicRegistry {
        baseline: vec![BaselineCollectionOwned {
            name: "FixtureDoc".into(),
            sdl: FIXTURE_SDL.into(),
            expected_version: Some(v0.clone()),
            expected_state: CollectionExpectation::dag_only(),
        }],
        steps: vec![MigrationStepOwned::PatchVersioned {
            id: "fixture-add-label-lensless".into(),
            collection: "FixtureDoc".into(),
            patch: ADD_LABEL_PATCH.into(),
            lens: None,
            expected_version: Some(v1.clone()),
            expected_transform: None,
            expected_state: CollectionExpectation::fields(&["name", "label"]),
        }],
    };

    let report1 = ensure_migrations_dynamic(node.as_ref(), &registry)
        .await
        .expect("first ensure");
    assert_eq!(report1.steps_applied, 1, "{report1:?}");

    let active = node
        .get_collection("FixtureDoc")
        .expect("get")
        .expect("exists");
    assert_eq!(active.version_id, v1);
    assert!(active.is_active);
    assert!(active.fields.iter().any(|f| f.name == "label"));

    // Both versions known pins → multi-version DAG accepted.
    let all = node.get_all_collection_versions().await.expect("versions");
    let fixture_versions: Vec<_> = all
        .iter()
        .filter(|v| v.name == "FixtureDoc" && !v.is_placeholder)
        .collect();
    assert_eq!(fixture_versions.len(), 2);

    let report2 = ensure_migrations_dynamic(node.as_ref(), &registry)
        .await
        .expect("second ensure");
    assert_eq!(report2.steps_applied, 0, "idempotent: {report2:?}");
    assert!(report2.steps_already_current >= 1);

    node.shutdown().await;
}

#[tokio::test]
async fn patch_versioned_with_fixture_lens_registers_transform() {
    let wasm = fixture_lens_wasm();
    // Stub wasm is 8 bytes — skip full lens path if build was stubbed.
    if wasm.len() <= 16 {
        eprintln!(
            "skipping fixture lens e2e: stub wasm (set wasm target / unset GENTS_SKIP_LENS_BUILD)"
        );
        return;
    }

    let discover = fresh_node().await;
    let (v0, v1) = discover_inactive_patch_pin(discover.as_ref()).await;
    discover.shutdown().await;

    let lens = LensSpec {
        wasm,
        args_json: None,
    };
    let predicted_tx = predict_transform_id(&lens);

    let node = fresh_node().await;
    // Seed a document at baseline so a later read exercises the lens path.
    {
        let reg0 = DynamicRegistry {
            baseline: vec![BaselineCollectionOwned {
                name: "FixtureDoc".into(),
                sdl: FIXTURE_SDL.into(),
                expected_version: Some(v0.clone()),
                expected_state: CollectionExpectation::dag_only(),
            }],
            steps: vec![],
        };
        ensure_migrations_dynamic(node.as_ref(), &reg0)
            .await
            .expect("baseline only");
        let create = r#"mutation { create_FixtureDoc(input: { name: "alice" }) { _docID name } }"#;
        let resp = node.execute(create).await;
        assert!(!resp.has_errors(), "create doc: {:?}", resp.errors);
    }

    let registry = DynamicRegistry {
        baseline: vec![BaselineCollectionOwned {
            name: "FixtureDoc".into(),
            sdl: FIXTURE_SDL.into(),
            expected_version: Some(v0.clone()),
            expected_state: CollectionExpectation::dag_only(),
        }],
        steps: vec![MigrationStepOwned::PatchVersioned {
            id: "fixture-add-label-lens".into(),
            collection: "FixtureDoc".into(),
            patch: ADD_LABEL_PATCH.into(),
            lens: Some(LensSpecOwned {
                wasm: wasm.to_vec(),
                args_json: None,
            }),
            expected_version: Some(v1.clone()),
            expected_transform: Some(predicted_tx.clone()),
            expected_state: CollectionExpectation::fields(&["name", "label"]),
        }],
    };

    let report = ensure_migrations_dynamic(node.as_ref(), &registry)
        .await
        .expect("lens step");
    assert_eq!(report.steps_applied, 1, "{report:?}");
    assert_eq!(report.materialization.collections_attempted, 1);
    assert!(!report.materialization.skipped_upstream_missing);
    assert_eq!(report.materialization.read_through_scans, 0);

    let active = node
        .get_collection("FixtureDoc")
        .expect("get")
        .expect("exists");
    assert_eq!(active.version_id, v1);
    let transform = active
        .previous_version
        .as_ref()
        .and_then(|pv| pv.transform.clone());
    assert_eq!(
        transform.as_deref(),
        Some(predicted_tx.as_str()),
        "transform pin must match content-derived id"
    );

    // The activation reindex and eager materializer persist the transformed row.
    let q = r#"{ FixtureDoc { name label } }"#;
    let resp = node.execute(q).await;
    assert!(!resp.has_errors(), "lens read: {:?}", resp.errors);
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("FixtureDoc"))
        .and_then(|value| value.as_array())
        .expect("FixtureDoc rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], serde_json::json!("alice"));
    assert_eq!(rows[0]["label"], serde_json::json!("ALICE"));

    // Idempotent re-run.
    let report2 = ensure_migrations_dynamic(node.as_ref(), &registry)
        .await
        .expect("idempotent lens step");
    assert_eq!(report2.steps_applied, 0);

    node.shutdown().await;
}

#[tokio::test]
async fn crash_resume_after_inactive_patch_before_activate() {
    let discover = fresh_node().await;
    let (v0, v1) = discover_inactive_patch_pin(discover.as_ref()).await;
    discover.shutdown().await;

    let node = fresh_node().await;
    // Manually reach "complete inactive" without activate.
    node.add_schema(FIXTURE_SDL).await.expect("schema");
    let patched = node
        .patch_collection("FixtureDoc", ADD_LABEL_PATCH)
        .await
        .expect("patch");
    assert_eq!(patched.version_id, v1);
    assert!(!patched.is_active);

    let registry = DynamicRegistry {
        baseline: vec![BaselineCollectionOwned {
            name: "FixtureDoc".into(),
            sdl: FIXTURE_SDL.into(),
            expected_version: Some(v0.clone()),
            expected_state: CollectionExpectation::dag_only(),
        }],
        steps: vec![MigrationStepOwned::PatchVersioned {
            id: "fixture-add-label-lensless".into(),
            collection: "FixtureDoc".into(),
            patch: ADD_LABEL_PATCH.into(),
            lens: None,
            expected_version: Some(v1.clone()),
            expected_transform: None,
            expected_state: CollectionExpectation::fields(&["name", "label"]),
        }],
    };

    let report = ensure_migrations_dynamic(node.as_ref(), &registry)
        .await
        .expect("resume activate");
    // Should activate without re-patching (steps_applied counts activate path).
    assert!(
        report.steps_applied + report.steps_already_current >= 1,
        "{report:?}"
    );
    let active = node
        .get_collection("FixtureDoc")
        .expect("get")
        .expect("exists");
    assert_eq!(active.version_id, v1);
    assert!(active.is_active);

    node.shutdown().await;
}

#[tokio::test]
async fn chain_replay_prints_baseline_pins_for_authoring() {
    // Authoring aid: print every production baseline collection's root VersionID.
    // Not asserted against frozen pins yet — freeze in a follow-up once CI has
    // a stable defradb pin and the list is reviewed.
    let node = fresh_node().await;
    let report = gents_migration::ensure_migrations(node.as_ref())
        .await
        .expect("production baseline");
    assert!(report.baseline_registered + report.baseline_already_present > 0);

    println!("=== baseline version pins (authoring paste targets) ===");
    for entry in gents_migration::DEFAULT_BASELINE {
        let cv = node
            .get_collection(entry.name)
            .expect("get")
            .unwrap_or_else(|| panic!("missing {}", entry.name));
        println!(
            "  {} => {}, fields={}",
            entry.name,
            cv.version_id,
            cv.fields.len()
        );
    }

    // The pinned DefraDB surface performs eager datastore materialization.
    assert!(!report.materialization.skipped_upstream_missing);
    assert_eq!(report.materialization.read_through_scans, 0);

    node.shutdown().await;
}
