//! DefraDB hashes versioned patches over their parent heads, so a fresh store and a
//! patched one never share a version. A fresh store registers the current SDL, which is the
//! canonical version; stores from before the native identity baseline are refused without
//! converting their schema or identifiers.

use gents_migration::{
    canonical_schema_plan, canonical_versions, ensure_migrations, is_known_off_lineage,
    CLIENT_AUTHORED_COLLECTIONS, DEFAULT_BASELINE,
};
use gents_protocol::schemas;

mod common;
use common::fresh_node;

const MESSAGE_RC0: &str = "bafyreibpbyfmfgfookleozyqvuwse6qtfnjf7ipxjq6ryypovs226ziw6a";
/// v1.0.0-rc1's AgentMessage: patched onto an rc0 store, and registered fresh.
const MESSAGE_RC1_PATCHED: &str = "bafyreiawf2s2jdgsajh6rff4plix6e4fawkkqu22dy3ade5pnzaaomqtj4";
const MESSAGE_RC1_FRESH: &str = "bafyreih6qxw3cq3uswastxlatdplwmkpknqkf7csgecoswijez5o2uihhm";
const MESSAGE_RC1_PATCH: &str = r#"[{"op":"add","path":"/AgentMessage/Fields/-","value":{"Name":"native_request_id","Kind":"String","Immutable":true}}]"#;

/// A collection that replays no patches has the root a fresh store registers from the
/// current SDL as its canonical version, and no canonical version is one the release names as
/// off its own lineage.
#[test]
fn canonical_versions_are_the_fresh_baseline_roots() {
    let canonical = canonical_versions().unwrap();
    for entry in DEFAULT_BASELINE.iter() {
        let version = canonical.get(entry.name).map(String::as_str);
        if canonical_schema_plan(entry.name).unwrap().is_none() {
            assert_eq!(version, entry.expected_version, "{}", entry.name);
        }
        assert!(
            !is_known_off_lineage(entry.name, version.unwrap()),
            "{} canonical version is listed off its lineage",
            entry.name
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

/// A peer is judged only by versions known to be off the lineage, never by being another
/// release: rc1's `AgentMessage` versions are named, the canonical ones and unknown versions
/// are not.
#[test]
fn only_known_off_lineage_versions_name_a_peer() {
    assert!(is_known_off_lineage(
        schemas::AGENT_MESSAGE_NAME,
        MESSAGE_RC1_FRESH
    ));
    assert!(is_known_off_lineage(
        schemas::AGENT_MESSAGE_NAME,
        MESSAGE_RC1_PATCHED
    ));
    assert!(!is_known_off_lineage(
        schemas::AGENT_MESSAGE_NAME,
        MESSAGE_RC0
    ));
    assert!(!is_known_off_lineage(
        schemas::AGENT_SESSION_NAME,
        "bafyother"
    ));
}

/// An rc0 store that v1.0.0-rc1 upgraded took rc1's `AgentMessage.native_request_id` patch.
/// It is refused, and its `AgentMessage` takes no step back or forward.
#[tokio::test]
async fn rc0_store_upgraded_by_rc1_is_refused_without_conversion() {
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
    let before = node.get_all_collection_versions().await.unwrap();

    assert!(ensure_migrations(node.as_ref()).await.is_err());
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(node.get_all_collection_versions().await.unwrap()).unwrap()
    );
    let active = node
        .get_collection(schemas::AGENT_MESSAGE_NAME)
        .unwrap()
        .unwrap();
    assert_eq!(active.version_id, MESSAGE_RC1_PATCHED);
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

/// A collection whose fresh store replays patches registers its rc0 root, never a version a
/// direct registration of the current SDL produces, so it starts where an rc0 store is.
#[test]
fn a_replayed_collection_registers_its_rc0_root() {
    let rc0 = rc0_baseline();
    for entry in DEFAULT_BASELINE.iter() {
        let name = &entry.name;
        let Some(plan) = canonical_schema_plan(name).unwrap() else {
            continue;
        };
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
