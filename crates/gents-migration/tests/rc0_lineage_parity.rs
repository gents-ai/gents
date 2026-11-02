//! Lineage parity between a store born on v1.0.0-rc0 and one born on this
//! build. A DefraDB patch mints its version ID over the previous version's
//! heads, so "rc0 collection + patch" never gets the CID a fresh registration
//! of the current SDL gets. Peers and clients whose active versions differ
//! cannot merge each other's blocks (`gents_protocol::peer_schema`), so every
//! collection listed in `KNOWN_RC0_LINEAGE_SPLITS` splits the fleet by birth
//! cohort. This test pins that list: a new schema change that splits another
//! collection fails here, and a fix that converges a listed collection must
//! remove it.

use gents_migration::ensure_migrations;
use gents_protocol::schemas;

mod common;
use common::fresh_node;

/// Version IDs v1.0.0-rc0 (689d75369) registered for the collections this
/// build has changed since. Taken from that tag's `registry.rs` baseline.
const RC0_PINS: &[(&str, &str)] = &[
    (
        schemas::AGENT_MESSAGE_NAME,
        "bafyreibpbyfmfgfookleozyqvuwse6qtfnjf7ipxjq6ryypovs226ziw6a",
    ),
    (
        schemas::INFERENCE_BACKEND_NAME,
        "bafyreige56taagffni2nkys46sezgidvf3wf3jbov6jmqsfsaae3f6oike",
    ),
];

/// Collections whose rc0-upgraded version differs from a fresh store's.
const KNOWN_RC0_LINEAGE_SPLITS: &[&str] =
    &[schemas::AGENT_MESSAGE_NAME, schemas::INFERENCE_BACKEND_NAME];

fn rc0_sdl(name: &str) -> String {
    let (current, added) = match name {
        schemas::AGENT_MESSAGE_NAME => (
            schemas::AGENT_MESSAGE,
            "    native_request_id: String @immutable\n",
        ),
        schemas::INFERENCE_BACKEND_NAME => {
            (schemas::INFERENCE_BACKEND, "    claude_platform: JSON\n")
        }
        other => panic!("no rc0 fixture for {other}"),
    };
    assert!(
        current.contains(added),
        "{name} no longer declares {added:?}"
    );
    current.replace(added, "")
}

#[tokio::test]
async fn rc0_upgraded_store_matches_fresh_store_except_known_splits() {
    let upgraded = fresh_node().await;
    for (name, pin) in RC0_PINS {
        upgraded.add_collection(&rc0_sdl(name)).await.unwrap();
        let registered = upgraded.get_collection(name).unwrap().unwrap();
        assert_eq!(registered.version_id, *pin, "{name} rc0 fixture drifted");
    }
    let report = ensure_migrations(upgraded.as_ref()).await.unwrap();
    assert_eq!(report.steps_applied, RC0_PINS.len());

    let fresh = fresh_node().await;
    ensure_migrations(fresh.as_ref()).await.unwrap();

    let mut splits = Vec::new();
    for entry in gents_migration::DEFAULT_BASELINE {
        let left = upgraded.get_collection(entry.name).unwrap();
        let right = fresh.get_collection(entry.name).unwrap();
        let (Some(left), Some(right)) = (left, right) else {
            panic!("{} missing from one store", entry.name);
        };
        if left.version_id != right.version_id {
            splits.push(entry.name);
        }
    }
    splits.sort_unstable();
    let mut known = KNOWN_RC0_LINEAGE_SPLITS.to_vec();
    known.sort_unstable();
    assert_eq!(
        splits, known,
        "rc0-born and fresh stores disagree on a different set of collections; a patched \
         replicated collection splits the fleet by birth cohort"
    );
    upgraded.shutdown().await;
    fresh.shutdown().await;
}
