use std::collections::BTreeSet;

use gents_migration::{
    ensure_migrations, Error, INFERENCE_PROFILE_ADD_SERVICE_TIER_PATCH, INFERENCE_PROFILE_RC0_SDL,
};

mod common;
use common::fresh_node;

const RC0_ROOT: &str = "bafyreiat2uhx24kbgbm54lz7nt56vtjjsfttkiwop2ntii7qn2xtceehxi";
const MIGRATED_VERSION: &str = "bafyreieksrjdkg546ymilx5qrpbtciuyr3cwqyq25u52o64lobpoek3qqi";

async fn collection_versions(
    node: &defra_node::EmbeddedNode,
    name: &str,
) -> Vec<defra_node::CollectionVersion> {
    node.get_all_collection_versions()
        .await
        .expect("get_all_collection_versions")
        .into_iter()
        .filter(|version| version.name == name)
        .collect()
}

#[test]
fn rc0_fixture_is_verbatim_rc0_sdl() {
    let live = gents_protocol::schemas::INFERENCE_PROFILE;
    let without_service_tier = live.replace("    service_tier: String\n", "");
    assert_eq!(INFERENCE_PROFILE_RC0_SDL, without_service_tier);

    let baseline = gents_migration::DEFAULT_BASELINE
        .iter()
        .find(|entry| entry.name == gents_protocol::schemas::INFERENCE_PROFILE_NAME)
        .expect("InferenceProfile baseline");
    assert!(
        !baseline.sdl.contains("    service_tier: String\n"),
        "baseline must be the frozen rc0 SDL, without service_tier"
    );
    let patched = INFERENCE_PROFILE_RC0_SDL.replace(
        "    reasoning_effort: String\n",
        "    reasoning_effort: String\n    service_tier: String\n",
    );
    assert_eq!(
        patched, live,
        "patched SDL must equal the canonical live SDL"
    );
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
        report.baseline_registered,
        gents_migration::DEFAULT_BASELINE.len() - 1,
        "only the missing collections should be registered: {report:?}"
    );
    assert_eq!(
        report.steps_applied, 1,
        "rc0 store must apply the service_tier step: {report:?}"
    );

    let active = node
        .get_collection(gents_protocol::schemas::INFERENCE_PROFILE_NAME)
        .expect("get_collection")
        .expect("InferenceProfile present");
    assert_eq!(
        active.version_id, MIGRATED_VERSION,
        "migrated store must land on the exact new-root CID"
    );
    assert!(active.is_active);
    assert!(!active.is_placeholder);
    assert_eq!(
        active.fields[..rc0_collection.fields.len()]
            .iter()
            .map(|f| (&f.id, &f.name))
            .collect::<Vec<_>>(),
        rc0_collection
            .fields
            .iter()
            .map(|f| (&f.id, &f.name))
            .collect::<Vec<_>>()
    );

    let versions =
        collection_versions(&node, gents_protocol::schemas::INFERENCE_PROFILE_NAME).await;
    let version_ids: BTreeSet<&str> = versions.iter().map(|v| v.version_id.as_str()).collect();
    assert!(
        version_ids.contains(RC0_ROOT),
        "rc0 root must remain in the DAG"
    );
    assert!(
        version_ids.contains(MIGRATED_VERSION),
        "new root must be present"
    );
    assert!(
        versions.iter().all(|v| !v.is_placeholder),
        "both roots must be materialized (non-placeholder)"
    );

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
async fn migrated_store_ensure_is_idempotent() {
    let node = fresh_node().await;
    node.add_schema(INFERENCE_PROFILE_RC0_SDL)
        .await
        .expect("register rc0 root");
    ensure_migrations(node.as_ref()).await.expect("migrate");
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
async fn fresh_store_baseline_is_rc0_then_step_applies() {
    let node = fresh_node().await;
    let report = ensure_migrations(node.as_ref())
        .await
        .expect("fresh ensure");
    assert_eq!(
        report.baseline_registered,
        gents_migration::DEFAULT_BASELINE.len(),
        "fresh store registers the entire baseline: {report:?}"
    );
    assert_eq!(
        report.steps_applied, 1,
        "fresh store must apply the service_tier step exactly once: {report:?}"
    );

    let active = node
        .get_collection(gents_protocol::schemas::INFERENCE_PROFILE_NAME)
        .expect("get_collection")
        .expect("InferenceProfile installed");
    assert_eq!(
        active.version_id, MIGRATED_VERSION,
        "fresh store must land on the canonical new-root CID"
    );
    assert!(active.is_active);
    assert!(!active.is_placeholder);

    let versions =
        collection_versions(&node, gents_protocol::schemas::INFERENCE_PROFILE_NAME).await;
    let version_ids: BTreeSet<&str> = versions.iter().map(|v| v.version_id.as_str()).collect();
    assert!(
        version_ids.contains(RC0_ROOT),
        "rc0 root must be present in a fresh store too"
    );
    assert!(
        version_ids.contains(MIGRATED_VERSION),
        "new root must be present"
    );
    assert!(
        versions.iter().all(|v| !v.is_placeholder),
        "both roots must be materialized (non-placeholder)"
    );

    let step = gents_migration::DEFAULT_STEPS
        .iter()
        .find(|step| step.id() == "inference-profile-add-service-tier")
        .expect("service_tier step");
    let expected_state = match step {
        gents_migration::MigrationStep::PatchVersioned { expected_state, .. } => expected_state,
        _ => panic!("service_tier step must be PatchVersioned"),
    };
    expected_state
        .verify(&active)
        .expect("step expected_state must verify against the migrated active version");

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
        active.version_id, MIGRATED_VERSION,
        "reopen must not move the fresh store"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn crash_between_patch_and_set_active_recovers() {
    let node = fresh_node().await;
    node.add_schema(INFERENCE_PROFILE_RC0_SDL)
        .await
        .expect("register rc0 root");
    let patched = node
        .patch_collection(
            gents_protocol::schemas::INFERENCE_PROFILE_NAME,
            r#"[{"op":"add","path":"/InferenceProfile/Fields/-","value":{"Name":"service_tier","Kind":"String"}},{"op":"replace","path":"/InferenceProfile/IsActive","value":false}]"#,
        )
        .await
        .expect("crash-window manual patch (inactive NEW)");
    assert_eq!(patched.version_id, MIGRATED_VERSION);
    let patched_version = node
        .get_collection_by_version_id(MIGRATED_VERSION)
        .await
        .expect("lookup NEW")
        .expect("NEW present");
    assert!(
        !patched_version.is_active,
        "crash-window shape: NEW exists but INACTIVE"
    );

    let report = ensure_migrations(node.as_ref())
        .await
        .expect("recovery ensure");
    assert_eq!(
        report.steps_applied, 1,
        "crash-window store must complete the interrupted step: {report:?}"
    );

    let active = node
        .get_collection(gents_protocol::schemas::INFERENCE_PROFILE_NAME)
        .expect("get_collection")
        .expect("present");
    assert_eq!(
        active.version_id, MIGRATED_VERSION,
        "recovery must activate the already-patched NEW version"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn both_versions_present_with_old_active_recovers() {
    let node = fresh_node().await;
    node.add_schema(INFERENCE_PROFILE_RC0_SDL)
        .await
        .expect("register rc0 root");
    node.patch_collection(
        gents_protocol::schemas::INFERENCE_PROFILE_NAME,
        r#"[{"op":"add","path":"/InferenceProfile/Fields/-","value":{"Name":"service_tier","Kind":"String"}},{"op":"replace","path":"/InferenceProfile/IsActive","value":false}]"#,
    )
    .await
    .expect("create NEW inactive");
    let versions =
        collection_versions(&node, gents_protocol::schemas::INFERENCE_PROFILE_NAME).await;
    let ids: BTreeSet<&str> = versions.iter().map(|v| v.version_id.as_str()).collect();
    assert!(
        ids.contains(RC0_ROOT) && ids.contains(MIGRATED_VERSION),
        "both versions present: {ids:?}"
    );

    let report = ensure_migrations(node.as_ref())
        .await
        .expect("recovery ensure");
    assert!(
        report.steps_applied <= 1,
        "recovery applies at most the activation: {report:?}"
    );
    let active = node
        .get_collection(gents_protocol::schemas::INFERENCE_PROFILE_NAME)
        .expect("get_collection")
        .expect("present");
    assert_eq!(
        active.version_id, MIGRATED_VERSION,
        "both-versions recovery must end with NEW active"
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
async fn new_root_only_store_is_rejected() {
    let node2 = fresh_node().await;
    node2
        .add_schema(gents_protocol::schemas::INFERENCE_PROFILE)
        .await
        .expect("register canonical new root");
    let err = ensure_migrations(node2.as_ref())
        .await
        .expect_err("NEW-only store must be rejected by the retained-root gate");
    assert!(
        matches!(err, Error::UnknownLineage { ref collection, .. } if collection == gents_protocol::schemas::INFERENCE_PROFILE_NAME),
        "unexpected error: {err}"
    );
    node2.shutdown().await;
}

#[tokio::test]
async fn rc0_root_lineage_survives_extra_known_versions() {
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
        patched.version_id, MIGRATED_VERSION,
        "manual patch must produce the pinned migrated version"
    );
    let active = node
        .get_collection(gents_protocol::schemas::INFERENCE_PROFILE_NAME)
        .expect("get_collection")
        .expect("present");
    assert_eq!(
        active.version_id, MIGRATED_VERSION,
        "plain patch activates NEW by default"
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
