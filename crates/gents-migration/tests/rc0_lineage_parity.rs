//! DefraDB hashes versioned patches over their parent heads. Retained backend
//! lineages replay their original chain; identity schemas use the current
//! baseline and reject old stores without converting their identifiers.

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
];

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

#[tokio::test]
async fn legacy_node_identity_schema_is_not_converted() {
    let node = fresh_node().await;
    let old = rc0_baseline()
        .into_iter()
        .find(|collection| collection.name == schemas::NODE_NAME)
        .unwrap();
    node.add_collection(&old.sdl).await.unwrap();
    assert!(ensure_migrations(&node).await.is_err());
    let current = node.get_collection(schemas::NODE_NAME).unwrap().unwrap();
    assert_eq!(current.version_id, old.version);
    assert!(current
        .fields
        .iter()
        .any(|field| field.name == "default_agent_id"));
    assert!(!current
        .fields
        .iter()
        .any(|field| field.name == "default_agent_did"));
    node.shutdown().await.expect("shut down test node");
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
    node.shutdown().await.expect("shut down test node");
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
    let patched =
        common::patch_collection(&node, schemas::AGENT_MESSAGE_NAME, MESSAGE_RC1_PATCH).await;
    node.set_active_collection_version_as(&common::node_context(&node), &patched.version_id)
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
    node.shutdown().await.expect("shut down test node");
}

#[test]
fn client_authored_collections_register_the_current_baseline_without_legacy_chains() {
    let canonical = canonical_versions().unwrap();
    for name in CLIENT_AUTHORED_COLLECTIONS {
        let entry = DEFAULT_BASELINE
            .iter()
            .find(|entry| entry.name == *name)
            .unwrap();
        assert_eq!(
            canonical.get(*name).map(String::as_str),
            entry.expected_version,
            "{name}"
        );
        assert!(canonical_schema_plan(name).unwrap().is_none(), "{name}");
    }
}

/// The default registry registers each collection changed since v1.0.0-rc0 at its rc0 root,
/// never at a version a direct registration of the current SDL produces (which is off the
/// lineage). Any store built from the baseline starts where an rc0 store is.
#[test]
fn the_baseline_is_the_rc0_root_of_every_changed_collection() {
    let rc0 = rc0_baseline();
    for (name, _) in CANONICAL {
        let Some(plan) = canonical_schema_plan(name).unwrap() else {
            continue;
        };
        let entry = DEFAULT_BASELINE.iter().find(|e| e.name == *name).unwrap();
        let then = rc0.iter().find(|c| c.name == *name).unwrap();
        assert_eq!(
            entry.sdl, then.sdl,
            "{name} baseline SDL is not its rc0 SDL"
        );
        assert_eq!(
            entry.expected_version,
            Some(then.version.as_str()),
            "{name}"
        );
        assert_eq!(plan.root_version, then.version, "{name}");
        assert!(!plan.patches.is_empty(), "{name}");
        assert!(!is_known_off_lineage(name, entry.expected_version.unwrap()));
    }
}

#[tokio::test]
async fn current_node_sdl_is_the_canonical_identity_baseline() {
    let node = fresh_node().await;
    node.add_collection(schemas::NODE).await.unwrap();
    let version = node
        .get_collection(schemas::NODE_NAME)
        .unwrap()
        .unwrap()
        .version_id;
    assert_eq!(version, canonical_versions().unwrap()[schemas::NODE_NAME]);
    assert!(canonical_schema_plan(schemas::NODE_NAME).unwrap().is_none());
    assert!(!is_known_off_lineage(schemas::NODE_NAME, &version));
    let report = ensure_migrations(&node).await.unwrap();
    assert!(report
        .warnings
        .iter()
        .all(|warning| !warning.starts_with(schemas::NODE_NAME)));
    assert_eq!(
        node.get_collection(schemas::NODE_NAME)
            .unwrap()
            .unwrap()
            .version_id,
        version
    );
    node.shutdown().await.expect("shut down test node");
}
