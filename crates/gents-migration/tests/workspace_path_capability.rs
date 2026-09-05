//! Real predecessor-version migration, not a load-time missing-field default.
use gents_migration::{
    ensure_migrations, ensure_migrations_with_registry, lens_config, predict_transform_id,
    MigrationStep, Registry, DEFAULT_BASELINE, DEFAULT_STEPS,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

mod common;
use common::fresh_node;

const COMPATIBILITY: &str = r#"{"mode":"unrestrictedCompatibility"}"#;

fn compatibility_step_start() -> usize {
    DEFAULT_STEPS
        .iter()
        .position(|step| step.id() == "isolated-workspace-add-path-capability")
        .expect("registered workspace capability migration")
}

async fn execute(node: &defra_node::EmbeddedNode, query: &str) -> Value {
    let response = node.execute(query).await;
    assert!(!response.has_errors(), "{}: {:?}", query, response.errors);
    response.data.expect("query data")
}

#[tokio::test]
async fn legacy_workspace_and_receipt_gain_explicit_compatibility_only_from_old_version() {
    let node = fresh_node().await;
    let before = Registry {
        baseline: DEFAULT_BASELINE,
        steps: &DEFAULT_STEPS[..compatibility_step_start()],
    };
    ensure_migrations_with_registry(node.as_ref(), &before)
        .await
        .expect("pre-capability production schema");
    execute(node.as_ref(), r#"mutation {
      create_IsolatedWorkspace(input: {workspace_id: "legacy-workspace", work_unit_id: "unit", base_sha: "base"}) { _docID }
      create_WorkspaceReceipt(input: {receipt_id: "legacy-receipt", workspace_id: "legacy-workspace", kind: "writer", seal_hash: "seal"}) { _docID }
    }"#).await;

    ensure_migrations(node.as_ref())
        .await
        .expect("migrate old rows");
    let digest = hex::encode(Sha256::digest(COMPATIBILITY.as_bytes()));
    let old = execute(node.as_ref(), r#"{
      IsolatedWorkspace(filter: {workspace_id: {_eq: "legacy-workspace"}}) { workspace_id work_unit_id base_sha path_capability }
      WorkspaceReceipt(filter: {receipt_id: {_eq: "legacy-receipt"}}) { workspace_id kind seal_hash path_capability_digest }
    }"#).await;
    assert_eq!(
        old["IsolatedWorkspace"],
        json!([{
            "workspace_id": "legacy-workspace", "work_unit_id": "unit", "base_sha": "base",
            "path_capability": COMPATIBILITY,
        }])
    );
    assert_eq!(
        old["WorkspaceReceipt"],
        json!([{
            "workspace_id": "legacy-workspace", "kind": "writer", "seal_hash": "seal",
            "path_capability_digest": digest,
        }])
    );

    // Schema fields remain nullable for migration. Runtime admission must reject
    // these current-version rows; the migration must never grant compatibility.
    execute(node.as_ref(), r#"mutation {
      create_IsolatedWorkspace(input: {workspace_id: "current-missing"}) { _docID }
      create_WorkspaceReceipt(input: {receipt_id: "current-missing", workspace_id: "current-missing"}) { _docID }
      create_IsolatedWorkspace(input: {workspace_id: "current-exact", path_capability: "{\"mode\":\"exactPaths\",\"paths\":[]}"}) { _docID }
    }"#).await;
    ensure_migrations(node.as_ref())
        .await
        .expect("idempotent replay and materialization");
    let current = execute(
        node.as_ref(),
        r#"{
      IsolatedWorkspace(filter: {workspace_id: {_eq: "current-missing"}}) { path_capability }
      WorkspaceReceipt(filter: {receipt_id: {_eq: "current-missing"}}) { path_capability_digest }
    }"#,
    )
    .await;
    assert!(current["IsolatedWorkspace"][0]["path_capability"].is_null());
    assert!(current["WorkspaceReceipt"][0]["path_capability_digest"].is_null());
    let exact = execute(node.as_ref(), r#"{ IsolatedWorkspace(filter: {workspace_id: {_eq: "current-exact"}}) { path_capability } }"#).await;
    assert_eq!(
        exact["IsolatedWorkspace"][0]["path_capability"],
        r#"{"mode":"exactPaths","paths":[]}"#
    );
    let old_after = execute(node.as_ref(), r#"{
      IsolatedWorkspace(filter: {workspace_id: {_eq: "legacy-workspace"}}) { workspace_id work_unit_id base_sha path_capability }
      WorkspaceReceipt(filter: {receipt_id: {_eq: "legacy-receipt"}}) { workspace_id kind seal_hash path_capability_digest }
    }"#).await;
    assert_eq!(old_after, old);

    for query in [
        r#"mutation { update_IsolatedWorkspace(filter: {workspace_id: {_eq: "legacy-workspace"}}, input: {path_capability: "changed"}) { _docID } }"#,
        r#"mutation { update_WorkspaceReceipt(filter: {receipt_id: {_eq: "legacy-receipt"}}, input: {path_capability_digest: "changed"}) { _docID } }"#,
    ] {
        let rejected = node.execute(query).await;
        assert!(
            rejected.has_errors(),
            "migrated authority must be immutable"
        );
    }
    node.shutdown().await;
}

#[tokio::test]
async fn workspace_capability_production_versions_and_transforms_are_pinned() {
    let node = fresh_node().await;
    // Author the new destinations with the low-level inactive patch API, as
    // phase_b_steps does. Production ensure_migrations correctly rejects an
    // unpinned step, so it cannot be the mechanism that discovers that pin.
    let before = Registry {
        baseline: DEFAULT_BASELINE,
        steps: &DEFAULT_STEPS[..compatibility_step_start()],
    };
    ensure_migrations_with_registry(node.as_ref(), &before)
        .await
        .expect("pinned predecessor chain");
    let mut mismatches = Vec::new();
    for step in &DEFAULT_STEPS[compatibility_step_start()..] {
        let MigrationStep::PatchVersioned {
            collection,
            lens: Some(lens),
            expected_version,
            expected_transform,
            patch,
            ..
        } = step
        else {
            continue;
        };
        assert!(lens.wasm.len() > 8, "production lens must never be a stub");
        let source = node
            .get_collection(collection)
            .expect("collection lookup")
            .expect("collection");
        let destination = node
            .patch_collection(collection, patch)
            .await
            .expect("author inactive destination");
        assert!(!destination.is_active);
        node.set_migration(lens_config(
            lens,
            &source.version_id,
            &destination.version_id,
        ))
        .await
        .expect("attach production source-version lens");
        node.set_active_collection_version(&destination.version_id)
            .await
            .expect("activate authored destination");
        let active = node
            .get_collection(collection)
            .expect("active lookup")
            .expect("active collection");
        let transform = predict_transform_id(lens);
        if *expected_version != Some(active.version_id.as_str())
            || *expected_transform != Some(transform.as_str())
        {
            mismatches.push(format!(
                "{}: expected_version=Some({:?}), expected_transform=Some({:?})",
                step.id(),
                active.version_id,
                transform
            ));
        }
        assert_eq!(
            active
                .previous_version
                .as_ref()
                .and_then(|version| version.transform.as_deref()),
            Some(transform.as_str())
        );
    }
    node.shutdown().await;
    assert!(
        mismatches.is_empty(),
        "author production pins before publication:\n{}",
        mismatches.join("\n")
    );
}

/// Generated cases drive the actual source-version transform for predecessor
/// inputs, and actual current-schema DB persistence for current inputs. The
/// old-row WASM migration test above separately fences transport/registration;
/// injected old-schema fields are intentionally tested at the production lens
/// boundary because that input cannot be authored through the old schema.
#[tokio::test]
async fn generated_workspace_capability_migrations_drive_real_lens_and_current_rows() {
    use std::collections::{BTreeSet, HashMap};

    let snapshot: Value = gents_lean_contract::load_contract_snapshot().expect("Lean snapshot");
    let cases = snapshot["workspace_capability_migration_cases"]
        .as_array()
        .expect("generated workspace migration cases");
    let names = cases
        .iter()
        .map(|case| case["name"].as_str().expect("case name"))
        .collect::<BTreeSet<_>>();
    assert_eq!(cases.len(), 5, "review new generated cases explicitly");
    assert_eq!(
        names,
        BTreeSet::from([
            "legacy_missing_explicitly_migrates",
            "new_missing_stays_missing",
            "legacy_injected_exact_overwritten",
            "exact_capability_preserved",
            "explicit_legacy_value_preserved",
        ])
    );
    let node = fresh_node().await;
    ensure_migrations(node.as_ref())
        .await
        .expect("current production schema");
    for case in cases {
        let name = case["name"].as_str().expect("case name");
        let stored = case
            .get("stored")
            .expect("stored capability or explicit null");
        let expected = case
            .get("expected")
            .expect("expected capability or explicit null");
        let observed = if case["legacy_source"]
            .as_bool()
            .expect("source version classification")
        {
            let mut old = HashMap::from([("workspace_id".to_owned(), json!(name))]);
            if !stored.is_null() {
                old.insert(
                    "path_capability".to_owned(),
                    Value::String(stored.to_string()),
                );
            }
            let migrated = gents_lens_workspace_capability::forward(old);
            assert_eq!(migrated.get("workspace_id"), Some(&json!(name)));
            serde_json::from_str::<Value>(
                migrated["path_capability"]
                    .as_str()
                    .expect("production lens value"),
            )
            .expect("production lens canonical capability JSON")
        } else {
            let name_literal = format!(
                "\"{}\"",
                gents_protocol::graphql::escape_graphql_string(name)
            );
            let capability_field = if stored.is_null() {
                String::new()
            } else {
                format!(
                    ", path_capability: \"{}\"",
                    gents_protocol::graphql::escape_graphql_string(&stored.to_string())
                )
            };
            execute(node.as_ref(), &format!("mutation {{ create_IsolatedWorkspace(input: {{ workspace_id: {name_literal}{capability_field} }}) {{ _docID }} }}")).await;
            ensure_migrations(node.as_ref())
                .await
                .expect("current-row materialization does not apply predecessor lens");
            let current = execute(node.as_ref(), &format!("{{ IsolatedWorkspace(filter: {{workspace_id: {{_eq: {name_literal}}}}}) {{ path_capability }} }}")).await;
            let rows = current["IsolatedWorkspace"]
                .as_array()
                .expect("persisted current row");
            assert_eq!(rows.len(), 1, "{name}");
            match &rows[0]["path_capability"] {
                Value::Null => Value::Null,
                Value::String(value) => {
                    serde_json::from_str(value).expect("stored capability JSON")
                }
                other => panic!("{name}: unexpected stored capability {other}"),
            }
        };
        assert_eq!(&observed, expected, "generated case {name}");
    }
    node.shutdown().await;
}
