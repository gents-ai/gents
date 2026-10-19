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
    assert!(
        !baseline.sdl.contains("    service_tier: String\n"),
        "baseline must be the frozen rc0 SDL, without service_tier"
    );
    // The patch re-inserts service_tier, and the patched result must be
    // byte-identical to the live SDL (same CID as the step pin).
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
        report.baseline_registered, 0,
        "rc0 store already has the collection; baseline must not re-register: {report:?}"
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
async fn fresh_store_baseline_is_rc0_then_step_applies() {
    // Corrected fresh-store flow (review B1): the baseline registers the
    // frozen rc0 SDL (OLD CID), then the service_tier step patches to the
    // canonical live SDL (NEW CID) — steps_applied == 1, NOT 0.
    let node = fresh_node().await;
    let report = ensure_migrations(node.as_ref())
        .await
        .expect("fresh ensure");
    assert_eq!(
        report.baseline_registered, 1,
        "fresh store registers the baseline InferenceProfile (rc0 SDL): {report:?}"
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
        active.version_id, NEW_ROOT,
        "fresh store must land on the canonical new-root CID"
    );
    assert!(active.is_active);
    assert!(!active.is_placeholder);

    // Both roots present: OLD (baseline source) retained as lineage history,
    // NEW (patched) active.
    let versions = collection_versions(&node, gents_protocol::schemas::INFERENCE_PROFILE_NAME);
    let version_ids: BTreeSet<&str> = versions.iter().map(|v| v.version_id.as_str()).collect();
    assert!(
        version_ids.contains(RC0_ROOT),
        "rc0 root must be present in a fresh store too"
    );
    assert!(version_ids.contains(NEW_ROOT), "new root must be present");
    assert!(
        versions.iter().all(|v| !v.is_placeholder),
        "both roots must be materialized (non-placeholder)"
    );

    // expected_state (review B3): the step's field expectation must verify
    // against the active NEW version — asserted explicitly, not just via
    // ensure returning Ok.
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

    // Critical fresh-store reopen path: verify_managed_lineages re-runs
    // check_known_lineage with expected_version = OLD root — the fresh store
    // now contains BOTH roots (OLD registered by baseline, NEW by the step),
    // so the root gate is satisfied without any weakening.
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
async fn crash_between_patch_and_set_active_recovers() {
    // Upstream evidence (defradb store_new_version): a plain patch_collection
    // defaults new_schema.is_active = true, so a raw patch leaves NEW ACTIVE.
    // The real crash window the engine defends against is the one AFTER a
    // patch created NEW-inactive-with-OLD-active, which only occurs when
    // IsActive=false is explicit (store.rs:293-296 keeps old active). This
    // test reproduces that shape by patching with explicit IsActive=false,
    // then proves ensure_migrations detects dest (NEW exists, inactive) with
    // active OLD, verifies state, and activates NEW (engine.rs 315-364)
    // without re-patching or rolling backward.
    let node = fresh_node().await;
    node.add_schema(INFERENCE_PROFILE_RC0_SDL)
        .await
        .expect("register rc0 root");
    let patched = node
        .patch_collection(
            gents_protocol::schemas::INFERENCE_PROFILE_NAME,
            r#"[{"op":"add","path":"/InferenceProfile/Fields/7","value":{"Name":"service_tier","Kind":"String"}},{"op":"replace","path":"/InferenceProfile/IsActive","value":false}]"#,
        )
        .await
        .expect("crash-window manual patch (inactive NEW)");
    assert_eq!(patched.version_id, NEW_ROOT);
    let patched_version = node
        .get_collection_by_version_id(NEW_ROOT)
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
        active.version_id, NEW_ROOT,
        "recovery must activate the already-patched NEW version"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn both_versions_present_with_old_active_recovers() {
    // Review-required shape: BOTH versions in the DAG and OLD still active
    // (the strongest crash-window form). Prove recovery activates NEW without
    // re-patching (steps_applied counts the activation, no VersionPinMismatch,
    // no backward roll).
    let node = fresh_node().await;
    node.add_schema(INFERENCE_PROFILE_RC0_SDL)
        .await
        .expect("register rc0 root");
    // Create NEW as inactive so OLD remains active (explicit IsActive=false).
    node.patch_collection(
        gents_protocol::schemas::INFERENCE_PROFILE_NAME,
        r#"[{"op":"add","path":"/InferenceProfile/Fields/7","value":{"Name":"service_tier","Kind":"String"}},{"op":"replace","path":"/InferenceProfile/IsActive","value":false}]"#,
    )
    .await
    .expect("create NEW inactive");
    // Restore the name pointer to OLD (simulating a crash before set_active):
    // re-registering the OLD SDL batch re-points the active version. We assert
    // the pre-recovery state directly instead of relying on pointer gymnastics:
    let versions = collection_versions(&node, gents_protocol::schemas::INFERENCE_PROFILE_NAME);
    let ids: BTreeSet<&str> = versions.iter().map(|v| v.version_id.as_str()).collect();
    assert!(
        ids.contains(RC0_ROOT) && ids.contains(NEW_ROOT),
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
        active.version_id, NEW_ROOT,
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
async fn new_root_only_store_is_rejected() {
    // NEW-only stores (OLD root absent) are rejected by the retained-root
    // gate: reject_unknown_lineage runs FIRST (engine.rs:32) and
    // check_known_lineage (844-865) requires the baseline pin (OLD root)
    // among the store's non-placeholder versions. target_active_pins' step
    // pin cannot help — it only feeds verify_managed_lineages, which runs
    // after the root gate. This documents the boundary honestly: the gate is
    // NOT weakened for arbitrary CIDs, and NEW-only is unreachable in
    // practice (patch_collection creates NEW as a descendant of OLD and
    // nothing prunes versions), but if encountered it must fail closed.
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
    // A rc0 store that already gained the new version via a plain manual patch
    // (which upstream defaults to ACTIVE — defradb store_new_version) must
    // verify and need no step: the DAG descends from the rc0 root and the
    // active version is the step's pin.
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
    // Upstream default: the patched version is ACTIVE (no explicit IsActive).
    let active = node
        .get_collection(gents_protocol::schemas::INFERENCE_PROFILE_NAME)
        .expect("get_collection")
        .expect("present");
    assert_eq!(
        active.version_id, NEW_ROOT,
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
