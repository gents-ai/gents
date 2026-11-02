//! One canonical lineage per collection. DefraDB mints a patched version ID over the
//! previous version's heads, so a store that registers the current SDL directly never
//! shares a version with a v1.0.0-rc0 store patched to that SDL, and stores on different
//! versions cannot merge each other's blocks (`gents_protocol::peer_schema`). Fresh stores
//! therefore register the rc0 root of every collection changed since rc0 and replay the
//! same patches (`upgrades::CANONICAL_ROOTS`). These tests hold a store born on rc0 and one
//! born on this build to the same versions for every managed collection.

use gents_migration::{canonical_versions, ensure_migrations, non_canonical_store_lineages};
use gents_protocol::schemas;

mod common;
use common::fresh_node;

/// Version IDs v1.0.0-rc0 (689d75369) registered for the collections this build has
/// changed since, taken from that tag's `registry.rs` baseline.
const RC0_PINS: &[(&str, &str)] = &[
    (
        schemas::AGENT_MESSAGE_NAME,
        "bafyreibpbyfmfgfookleozyqvuwse6qtfnjf7ipxjq6ryypovs226ziw6a",
    ),
    (
        schemas::INFERENCE_BACKEND_NAME,
        "bafyreige56taagffni2nkys46sezgidvf3wf3jbov6jmqsfsaae3f6oike",
    ),
    (
        schemas::NODE_NAME,
        "bafyreiebppz3w6ri6lg6ft2jy3il7gk6bexfy3wjqizbcct4pzx3puppby",
    ),
];

/// Where both an upgraded rc0 store and a fresh store land.
const CANONICAL: &[(&str, &str)] = &[
    (
        schemas::AGENT_MESSAGE_NAME,
        "bafyreiawf2s2jdgsajh6rff4plix6e4fawkkqu22dy3ade5pnzaaomqtj4",
    ),
    (
        schemas::INFERENCE_BACKEND_NAME,
        "bafyreic7xg6tbg3rooyghvyy7tho52yynp7yqsu4tqbvjmsfnyouv2ieme",
    ),
    (
        schemas::NODE_NAME,
        "bafyreibibfxzzu4wbr2z5w3fxdfik6vnalwg3mkxqm66sivdoesqpjosli",
    ),
];

fn rc0_sdl(name: &str) -> String {
    let (current, added) = match name {
        schemas::AGENT_MESSAGE_NAME => (
            schemas::AGENT_MESSAGE,
            "    native_request_id: String @immutable\n",
        ),
        schemas::INFERENCE_BACKEND_NAME => {
            (schemas::INFERENCE_BACKEND, "    claude_platform: JSON\n")
        }
        schemas::NODE_NAME => (schemas::NODE, "    avatar: String\n"),
        other => panic!("no rc0 fixture for {other}"),
    };
    assert!(
        current.contains(added),
        "{name} no longer declares {added:?}"
    );
    current.replace(added, "")
}

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

#[tokio::test]
async fn rc0_upgraded_store_and_fresh_store_share_every_version() {
    let upgraded = fresh_node().await;
    for (name, pin) in RC0_PINS {
        upgraded.add_collection(&rc0_sdl(name)).await.unwrap();
        let registered = upgraded.get_collection(name).unwrap().unwrap();
        assert_eq!(registered.version_id, *pin, "{name} rc0 fixture drifted");
    }
    let before = upgraded.get_all_collection_versions().await.unwrap();
    assert_eq!(
        non_canonical_store_lineages(&before).unwrap(),
        vec![],
        "an rc0 store's steps must land it on the canonical versions"
    );
    let report = ensure_migrations(upgraded.as_ref()).await.unwrap();
    assert_eq!(report.steps_applied, RC0_PINS.len());
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let fresh = fresh_node().await;
    let report = ensure_migrations(fresh.as_ref()).await.unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let again = ensure_migrations(fresh.as_ref()).await.unwrap();
    assert_eq!(again.steps_applied, 0);

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

/// A store born on v1.0.0-rc1 registered the patched SDL directly. It must still start,
/// and say which collections it cannot sync.
#[tokio::test]
async fn rc1_born_store_starts_and_reports_its_lineage_split() {
    let node = fresh_node().await;
    node.add_collection(schemas::AGENT_MESSAGE).await.unwrap();
    node.add_collection(schemas::INFERENCE_BACKEND)
        .await
        .unwrap();

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
    assert_eq!(
        mismatches[0].active,
        "bafyreih6qxw3cq3uswastxlatdplwmkpknqkf7csgecoswijez5o2uihhm"
    );
    node.shutdown().await;
}
