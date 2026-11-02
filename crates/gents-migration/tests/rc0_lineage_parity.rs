//! One canonical lineage per collection. DefraDB mints a patched version ID over the
//! previous version's heads, so a store that registers the current SDL directly never
//! shares a version with a v1.0.0-rc0 store patched to that SDL, and stores on different
//! versions cannot merge each other's blocks (`gents_protocol::peer_schema`).
//! - Client-authored collections never change schema: phones and apps register them from
//!   their own SDL, so their version is the v1.0.0-rc0 one everywhere (#1123/#1125).
//! - A runtime-authored collection changed since rc0 is registered at its rc0 root on a fresh
//!   store and takes the same patches an upgraded rc0 store takes
//!   (`upgrades::CANONICAL_ROOTS`).
//! These tests hold a store born on rc0 and one born on this build to the same versions for
//! every managed collection, and keep the stores v1.0.0-rc1 produced starting.

use gents_migration::{
    canonical_schema_plan, canonical_versions, ensure_migrations, is_known_off_lineage,
    non_canonical_store_lineages, CLIENT_AUTHORED_COLLECTIONS, DEFAULT_BASELINE,
};
use gents_protocol::schemas;

mod common;
use common::fresh_node;

const MESSAGE_RC0: &str = "bafyreibpbyfmfgfookleozyqvuwse6qtfnjf7ipxjq6ryypovs226ziw6a";
/// v1.0.0-rc1's AgentMessage: patched onto an rc0 store, and registered fresh.
const MESSAGE_RC1_PATCHED: &str = "bafyreiawf2s2jdgsajh6rff4plix6e4fawkkqu22dy3ade5pnzaaomqtj4";
const MESSAGE_RC1_FRESH: &str = "bafyreih6qxw3cq3uswastxlatdplwmkpknqkf7csgecoswijez5o2uihhm";
const MESSAGE_RC1_PATCH: &str = r#"[{"op":"add","path":"/AgentMessage/Fields/-","value":{"Name":"native_request_id","Kind":"String","Immutable":true}}]"#;
const BACKEND_RC1_FRESH: &str = "bafyreibyanrs5nrmpd6dp5pa74agyp5njjfztpctt3ls5qyl5pt2mkojd4";

/// The AgentMessage SDL v1.0.0-rc1 shipped: this release's plus `native_request_id`.
fn rc1_agent_message_sdl() -> String {
    let anchor = "    blocks: JSON @immutable\n";
    assert!(schemas::AGENT_MESSAGE.contains(anchor));
    assert!(!schemas::AGENT_MESSAGE.contains("native_request_id"));
    schemas::AGENT_MESSAGE.replace(
        anchor,
        &format!("    native_request_id: String @immutable\n{anchor}"),
    )
}

/// Where both an upgraded rc0 store and a fresh store land.
const CANONICAL: &[(&str, &str)] = &[
    (schemas::AGENT_MESSAGE_NAME, MESSAGE_RC0),
    (
        schemas::INFERENCE_BACKEND_NAME,
        "bafyreic7xg6tbg3rooyghvyy7tho52yynp7yqsu4tqbvjmsfnyouv2ieme",
    ),
    (
        schemas::NODE_NAME,
        "bafyreibibfxzzu4wbr2z5w3fxdfik6vnalwg3mkxqm66sivdoesqpjosli",
    ),
];

async fn active_versions(
    node: &defra_node::EmbeddedNode,
) -> std::collections::BTreeMap<String, String> {
    node.get_all_collection_versions()
        .await
        .unwrap()
        .into_iter()
        .filter(|version| version.is_active && !version.is_placeholder)
        .map(|version| (version.name, version.version_id))
        .collect()
}

#[test]
fn canonical_versions_are_the_rc0_upgrade_targets() {
    let canonical = canonical_versions().unwrap();
    for (name, version) in CANONICAL {
        assert_eq!(
            canonical.get(*name).map(String::as_str),
            Some(*version),
            "{name}"
        );
    }
}

#[derive(serde::Deserialize)]
struct Rc0Baseline {
    baseline: Vec<Rc0Collection>,
}

#[derive(serde::Deserialize)]
struct Rc0Collection {
    name: String,
    sdl: String,
    version: String,
}

/// v1.0.0-rc0's own baseline, dumped from the tag's registry: registering it in order is
/// exactly what a fresh rc0 store did (that release applied no steps on a fresh store).
fn rc0_baseline() -> Vec<Rc0Collection> {
    serde_json::from_str::<Rc0Baseline>(include_str!("fixtures/rc0_baseline.json"))
        .unwrap()
        .baseline
}

/// Every store born on v1.0.0-rc0 (the whole fleet reports exactly these pins) upgrades onto
/// the versions a store born on this build gets, for every collection this build manages.
#[tokio::test]
async fn rc0_born_store_upgrades_onto_every_fresh_version() {
    let upgraded = fresh_node().await;
    for collection in rc0_baseline() {
        upgraded
            .add_collection(&collection.sdl)
            .await
            .unwrap_or_else(|error| panic!("rc0 {}: {error}", collection.name));
        let registered = upgraded.get_collection(&collection.name).unwrap().unwrap();
        assert_eq!(
            registered.version_id, collection.version,
            "{} does not reproduce its rc0 pin",
            collection.name
        );
        if let Some(plan) = canonical_schema_plan(&collection.name).unwrap() {
            assert_eq!(
                plan.root_sdl, collection.sdl,
                "{} rc0 root SDL drifted",
                collection.name
            );
            assert_eq!(
                plan.root_version, collection.version,
                "{} rc0 root",
                collection.name
            );
        }
    }
    let before = upgraded.get_all_collection_versions().await.unwrap();
    assert_eq!(
        non_canonical_store_lineages(&before).unwrap(),
        vec![],
        "an rc0 store's steps must land it on the canonical versions"
    );
    let report = ensure_migrations(upgraded.as_ref()).await.unwrap();
    assert!(report.steps_applied > 0);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let rerun = ensure_migrations(upgraded.as_ref()).await.unwrap();
    assert_eq!(rerun.steps_applied, 0);

    let fresh = fresh_node().await;
    let report = ensure_migrations(fresh.as_ref()).await.unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let upgraded_versions = active_versions(&upgraded).await;
    let fresh_versions = active_versions(&fresh).await;
    let splits = gents_migration::DEFAULT_BASELINE
        .iter()
        .filter(|entry| upgraded_versions.get(entry.name) != fresh_versions.get(entry.name))
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    assert_eq!(
        splits,
        Vec::<&str>::new(),
        "a store born on rc0 and one born on this build disagree; a collection with a \
         versioned step on its rc0 pin needs a CANONICAL_ROOTS entry"
    );
    for (name, version) in CANONICAL {
        assert_eq!(
            fresh_versions.get(*name).map(String::as_str),
            Some(*version),
            "{name}"
        );
    }
    upgraded.shutdown().await;
    fresh.shutdown().await;
}

/// A store born on v1.0.0-rc1 registered rc1's SDL directly. It must still start, and say
/// which collections it cannot sync.
#[tokio::test]
async fn rc1_born_store_starts_and_reports_its_lineage_split() {
    let node = fresh_node().await;
    node.add_collection(&rc1_agent_message_sdl()).await.unwrap();
    node.add_collection(schemas::INFERENCE_BACKEND)
        .await
        .unwrap();
    let registered = |name: &str| node.get_collection(name).unwrap().unwrap().version_id;
    assert_eq!(registered(schemas::AGENT_MESSAGE_NAME), MESSAGE_RC1_FRESH);
    assert_eq!(
        registered(schemas::INFERENCE_BACKEND_NAME),
        BACKEND_RC1_FRESH
    );

    let report = ensure_migrations(node.as_ref()).await.unwrap();
    let flagged = |name: &str| report.warnings.iter().any(|w| w.starts_with(name));
    assert!(
        flagged(schemas::AGENT_MESSAGE_NAME),
        "{:?}",
        report.warnings
    );
    assert!(
        flagged(schemas::INFERENCE_BACKEND_NAME),
        "{:?}",
        report.warnings
    );

    let versions = node.get_all_collection_versions().await.unwrap();
    let mismatches = non_canonical_store_lineages(&versions).unwrap();
    assert_eq!(
        mismatches
            .iter()
            .map(|m| m.collection.as_str())
            .collect::<Vec<_>>(),
        vec![schemas::AGENT_MESSAGE_NAME, schemas::INFERENCE_BACKEND_NAME]
    );
    assert_eq!(mismatches[0].active, MESSAGE_RC1_FRESH);
    assert_eq!(mismatches[0].canonical, MESSAGE_RC0);
    let rerun = ensure_migrations(node.as_ref()).await.unwrap();
    assert_eq!(rerun.steps_applied, 0);
    // A peer is judged only by versions known to be off the lineage, never by being
    // another release.
    for mismatch in &mismatches {
        assert!(
            is_known_off_lineage(&mismatch.collection, &mismatch.active),
            "{mismatch}"
        );
        assert!(!is_known_off_lineage(
            &mismatch.collection,
            &mismatch.canonical
        ));
    }
    assert!(!is_known_off_lineage(
        schemas::AGENT_MESSAGE_NAME,
        MESSAGE_RC0
    ));
    assert!(!is_known_off_lineage(
        schemas::AGENT_SESSION_NAME,
        "bafyother"
    ));
    node.shutdown().await;
}

/// An rc0 store that v1.0.0-rc1 upgraded took rc1's `AgentMessage.native_request_id` patch.
/// It must still start, take no step back or forward, and say it cannot sync AgentMessage.
#[tokio::test]
async fn rc0_store_upgraded_by_rc1_starts_and_reports_agent_message() {
    let node = fresh_node().await;
    let rc0 = rc0_baseline();
    let message = rc0
        .iter()
        .find(|c| c.name == schemas::AGENT_MESSAGE_NAME)
        .unwrap();
    node.add_collection(&message.sdl).await.unwrap();
    let patched = node
        .patch_collection(schemas::AGENT_MESSAGE_NAME, MESSAGE_RC1_PATCH)
        .await
        .unwrap();
    node.set_active_collection_version(&patched.version_id)
        .await
        .unwrap();
    assert_eq!(patched.version_id, MESSAGE_RC1_PATCHED);

    let report = ensure_migrations(node.as_ref()).await.unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.starts_with(schemas::AGENT_MESSAGE_NAME)),
        "{:?}",
        report.warnings
    );
    let active = node
        .get_collection(schemas::AGENT_MESSAGE_NAME)
        .unwrap()
        .unwrap();
    assert_eq!(active.version_id, MESSAGE_RC1_PATCHED);
    let versions = node.get_all_collection_versions().await.unwrap();
    let mismatches = non_canonical_store_lineages(&versions).unwrap();
    assert_eq!(
        mismatches
            .iter()
            .map(|m| (m.collection.as_str(), m.active.as_str()))
            .collect::<Vec<_>>(),
        vec![(schemas::AGENT_MESSAGE_NAME, MESSAGE_RC1_PATCHED)]
    );
    let rerun = ensure_migrations(node.as_ref()).await.unwrap();
    assert_eq!(rerun.steps_applied, 0);
    node.shutdown().await;
}

/// Phones and apps register client-authored collections from the SDL they were built with,
/// so changing one splits every client built before the change off the fleet (#1123/#1125).
/// An intended change needs a mobile plan, and then an update to this test.
#[test]
fn client_authored_collections_keep_their_rc0_schema() {
    let rc0 = rc0_baseline();
    for name in CLIENT_AUTHORED_COLLECTIONS {
        let head = DEFAULT_BASELINE
            .iter()
            .find(|entry| entry.name == *name)
            .unwrap_or_else(|| panic!("{name} is not in the baseline"));
        let then = rc0
            .iter()
            .find(|collection| collection.name == *name)
            .unwrap_or_else(|| panic!("{name} is not in the v1.0.0-rc0 baseline"));
        assert_eq!(
            head.sdl, then.sdl,
            "client-authored {name} changed its schema since v1.0.0-rc0"
        );
        assert_eq!(head.expected_version, Some(then.version.as_str()), "{name}");
        assert!(
            canonical_schema_plan(name).unwrap().is_none(),
            "client-authored {name} must not chain from an rc0 root"
        );
    }
}
