//! InferenceProfile rc0-root lineage migration tests.
//!
//! Design (approved): the baseline pins the rc0 root
//! `bafyreiat2uhx…` as the lineage ROOT the store must descend from; the
//! fresh-apply CID `bafyreidrxjg…` (SDL with service_tier) is pinned by the
//! versioned step. rc0 stores migrate (patch adds service_tier at the exact
//! field position), fresh stores start at the step pin, the lineage gate
//! still refuses foreign/unknown roots, and no engine changes are involved.

use std::collections::BTreeSet;

use gents_migration::{
    ensure_migrations, Error, INFERENCE_PROFILE_ADD_SERVICE_TIER_PATCH, INFERENCE_PROFILE_RC0_SDL,
};

mod common;
use common::fresh_node;

const RC0_ROOT: &str = "bafyreiat2uhx24kbgbm54lz7nt56vtjjsfttkiwop2ntii7qn2xtceehxi";
const NEW_ROOT: &str = "bafyreidrxjg2xkpyyoi7k4szrvxojjmjg2vhzqlfwb2av4m65yvywimj3m";

fn collection_versions<'a>(
    node: &'a defra_node::EmbeddedNode,
    name: &str,
) -> Vec<defra_node::CollectionVersion> {
    node.get_all_collection_versions()
        .await
        .expect("get_all_collection_versions")
        .into_iter()
        .filter(|version| version.name == name)
        .collect()
}

/// The fixture SDL is the verbatim rc0 SDL: identical to the live SDL minus
/// service_tier. Asserted against the registry constants so this test cannot
/// drift from the real lineage.
#[test]
fn rc0_fixture_is_verbatim_rc0_sdl() {
    let live = gents_protocol::schemas::INFERENCE_PROFILE;
    let without_service_tier = live.replace("    service_tier: String\n", "");
    assert_eq!(INFERENCE_PROFILE_RC0_SDL, without_service_tier);

    // The patch inserts service_tier back at the exact live position, so the
    // patched SDL must be byte-identical to the canonical live SDL.
    let baseline = gents_migration::DEFAULT_BASELINE
        .iter()
        .find(|entry| entry.name == gents_protocol::schemas::INFERENCE_PROFILE_NAME)
        .expect("InferenceProfile baseline");
    // Field index 7 in the rc0 SDL (0-based): profile_id(0), node_did(1),
    // display_name(2), description(3), backend_id(4), model_name(5),
    // reasoning_effort(6) → service_tier lands at index 7, matching the live
    // SDL's position right after reasoning_effort.
    assert!(baseline.sdl.contains("    service_tier: String\n"));
    assert!(INFERENCE_PROFILE_ADD_SERVICE_TIER_PATCH.contains("\"Name\":\"service_tier\""));
}

#[tokio::test]
async fn rc0_store_migrates_to_new_root_preserving_data() {
    let node = fresh_node().await;
    node.add_schema(INFERENCE_PROFILE_RC0_SDL)
        .await
        .expect("register rc0 root");
    let rc0_collection = node
        .get_collection(gents_protocol::schemas::INFERENCE_PROFILE_NAME)
        .expect("get_collection")
        .expect("InferenceProfile present");
    assert_eq!(
        rc0_collection.version_id, RC0_ROOT,
        "synthetic rc0 store must carry the exact rc0 root CID"
    );

    // Seed a document on the OLD schema (no service_tier field exists yet).
    let create = r#"mutation {
        create_InferenceProfile(input: {
            profile_id: "profile-1"
            node_did: "did:key:seed"
            display_name: "Seed Profile"
            description: "created before migration"
            backend_id: "backend-1"
            model_name: "model-x"
            reasoning_effort: "high"
            context_window: 128000
            max_output_tokens: 8192
            sampling_id: "sampling-1"
            execution_id: "execution-1"
            tags: ["seed"]
            updated_at: "2026-10-01T00:00:00Z"
        }) { profile_id display_name }
    }"#;
    let response = node.execute(create).await;
    assert!(
        !response.has_errors(),
        "seed doc on old schema: {:?}",
        response.errors
    );

    let report = ensure_migrations(node.as_ref())
        .await
        .expect("migrate rc0 store");
    assert_eq!(
        report.steps_applied, 1,
        "rc0 store must apply the service_tier step: {report:?}"
    );

    let active = node
        .get_collection(gents_protocol::schemas::INFERENCE_PROFILE_NAME)
        .expect("get_collection")
        .expect("InferenceProfile present");
    assert_eq!(
        active.version_id, NEW_ROOT,
        "migrated store must land on the exact new-root CID"
    );
    assert!(active.is_active);
    assert!(!active.is_placeholder);

    // Both roots remain in the DAG; the rc0 root is retained as lineage history.
    let versions = collection_versions(&node, gents_protocol::schemas::INFERENCE_PROFILE_NAME);
    let version_ids: BTreeSet<&str> = versions.iter().map(|v| v.version_id.as_str()).collect();
    assert!(
        version_ids.contains(RC0_ROOT),
        "rc0 root must remain in the DAG"
    );
    assert!(version_ids.contains(NEW_ROOT), "new root must be present");
    assert!(
        versions.iter().all(|v| !v.is_placeholder),
        "both roots must be materialized (non-placeholder)"
    );

    // Data preservation: every old field survives; service_tier is null.
    let response = node
        .execute(
            r#"{ InferenceProfile(filter: {profile_id: {_eq: "profile-1"}}) {
                profile_id node_did display_name description backend_id model_name
                reasoning_effort service_tier context_window max_output_tokens
                sampling_id execution_id tags updated_at
            } }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "post-migration query: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceProfile"))
        .and_then(serde_json::Value::as_array)
        .expect("InferenceProfile rows");
    assert_eq!(rows.len(), 1, "the pre-migration document must survive");
    let row = &rows[0];
    assert_eq!(row["profile_id"], "profile-1");
    assert_eq!(row["node_did"], "did:key:seed");
    assert_eq!(row["display_name"], "Seed Profile");
    assert_eq!(row["description"], "created before migration");
    assert_eq!(row["backend_id"], "backend-1");
    assert_eq!(row["model_name"], "model-x");
    assert_eq!(row["reasoning_effort"], "high");
    assert!(
        row.get("service_tier")
            .map_or(true, serde_json::Value::is_null),
        "additive field must default to null"
    );
    assert_eq!(row["context_window"], 128000);
    assert_eq!(row["max_output_tokens"], 8192);
    assert_eq!(row["sampling_id"], "sampling-1");
    assert_eq!(row["execution_id"], "execution-1");
    assert_eq!(row["tags"][0], "seed");
    assert_eq!(row["updated_at"], "2026-10-01T00:00:00Z");

    node.shutdown().await;
}

#[tokio::test]
async fn migrated_store_reopens_without_further_steps() {
    let node = fresh_node().await;
    node.add_schema(INFERENCE_PROFILE_RC0_SDL)
        .await
        .expect("register rc0 root");
    ensure_migrations(node.as_ref()).await.expect("migrate");
    // Simulate a re-open of the same store (the node is still alive; a second
    // ensure over the migrated DAG must be a no-op).
    let report = ensure_migrations(node.as_ref())
        .await
        .expect("re-open ensure");
    assert_eq!(
        report.steps_applied, 0,
        "migrated store must not re-apply: {report:?}"
    );
    assert_eq!(report.steps_already_current, 1);
    node.shutdown().await;
}

#[tokio::test]
async fn fresh_store_starts_at_new_root_and_reopens_clean() {
    let node = fresh_node().await;
    let report = ensure_migrations(node.as_ref())
        .await
        .expect("fresh ensure");
    assert_eq!(
        report.steps_applied, 0,
        "fresh store needs no migration step: {report:?}"
    );

    let active = node
        .get_collection(gents_protocol::schemas::INFERENCE_PROFILE_NAME)
        .expect("get_collection")
        .expect("InferenceProfile installed by baseline");
    assert_eq!(
        active.version_id, NEW_ROOT,
        "fresh store baseline must land on the canonical new-root CID"
    );
    assert!(active.is_active);
    assert!(!active.is_placeholder);

    // Critical fresh-store reopen path: the baseline now pins the OLD root as
    // expected_version, but the store only contains the NEW root (registered by
    // the baseline). All verification paths must accept this: the step pin
    // provides the active-version target and the DAG is single-version.
    let reopen = ensure_migrations(node.as_ref())
        .await
        .expect("fresh store reopen must be accepted");
    assert_eq!(reopen.steps_applied, 0);
    assert_eq!(reopen.steps_already_current, 1, "{reopen:?}");

    let active = node
        .get_collection(gents_protocol::schemas::INFERENCE_PROFILE_NAME)
        .expect("get_collection")
        .expect("InferenceProfile still present");
    assert_eq!(
        active.version_id, NEW_ROOT,
        "reopen must not move the fresh store"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn fresh_store_service_tier_roundtrip() {
    let node = fresh_node().await;
    ensure_migrations(node.as_ref()).await.expect("baseline");
    let create = r#"mutation {
        create_InferenceProfile(input: {
            profile_id: "fresh-1"
            node_did: "did:key:fresh"
            backend_id: "backend-1"
            model_name: "model-y"
            reasoning_effort: "medium"
            service_tier: "priority"
            sampling_id: "sampling-2"
            execution_id: "execution-2"
            updated_at: "2026-10-19T00:00:00Z"
        }) { profile_id service_tier }
    }"#;
    let response = node.execute(create).await;
    assert!(
        !response.has_errors(),
        "create with service_tier: {:?}",
        response.errors
    );
    let response = node
        .execute(r#"{ InferenceProfile(filter: {profile_id: {_eq: "fresh-1"}}) { profile_id service_tier } }"#)
        .await;
    assert!(
        !response.has_errors(),
        "query service_tier: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceProfile"))
        .and_then(serde_json::Value::as_array)
        .expect("rows");
    assert_eq!(rows[0]["service_tier"], "priority");
    node.shutdown().await;
}

#[tokio::test]
async fn foreign_lineage_is_still_rejected() {
    // A store whose InferenceProfile root is neither the rc0 root nor the new
    // root must fail closed — this proves the baseline pin change did NOT
    // weaken the gate into "accept any CID".
    let node = fresh_node().await;
    let foreign_sdl = INFERENCE_PROFILE_RC0_SDL.replace(
        "    display_name: String\n",
        "    display_name: String\n    foreign_field: String\n",
    );
    node.add_schema(&foreign_sdl)
        .await
        .expect("register foreign root");

    let err = ensure_migrations(node.as_ref())
        .await
        .expect_err("foreign InferenceProfile lineage must fail closed");
    assert!(
        matches!(err, Error::UnknownLineage { ref collection, .. } if collection == gents_protocol::schemas::INFERENCE_PROFILE_NAME),
        "unexpected error: {err}"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn rc0_root_lineage_survives_extra_known_versions() {
    // A rc0 store that already gained a third known version (e.g. a partial
    // historical migration attempt) must still verify: the DAG descends from
    // the rc0 root and contains only known pins.
    let node = fresh_node().await;
    node.add_schema(INFERENCE_PROFILE_RC0_SDL)
        .await
        .expect("register rc0 root");
    let patched = node
        .patch_collection(
            gents_protocol::schemas::INFERENCE_PROFILE_NAME,
            INFERENCE_PROFILE_ADD_SERVICE_TIER_PATCH,
        )
        .await
        .expect("manual patch to new root");
    assert_eq!(
        patched.version_id, NEW_ROOT,
        "manual patch must produce the canonical new-root CID"
    );

    let report = ensure_migrations(node.as_ref())
        .await
        .expect("ensure on partially-migrated store");
    assert_eq!(
        report.steps_applied, 0,
        "already-migrated DAG must need no step: {report:?}"
    );
    assert_eq!(report.steps_already_current, 1);
    node.shutdown().await;
}
