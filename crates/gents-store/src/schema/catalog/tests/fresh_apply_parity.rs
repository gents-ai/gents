//! Client-authored collections MUST remain fresh-apply
//! compatible. `EmbeddedNode::add_collection` here and a paired client's FFI both
//! ingest SDL into an empty store and converge on the same collection-creation
//! path in the pinned defradb, so the genesis CIDs minted here stand in for
//! what a phone mints. The two apply paths seed their SDL parsers with
//! different known-type sets, which is only guaranteed equivalent while every
//! client-authored SDL is single-type and relation-free — a relation field's
//! CID embeds the target collection's identity, making genesis CIDs depend on
//! the co-registered set.

use super::common::fresh_node;
use crate::schema::catalog::{ensure_runtime, SchemaError};

const CLIENT_AUTHORED_COLLECTIONS: &[&str] = &[
    gents_protocol::schemas::PEER_REGISTRY_NAME,
    gents_protocol::schemas::AGENT_REQUEST_NAME,
    gents_protocol::schemas::AGENT_MESSAGE_NAME,
    gents_protocol::schemas::AGENT_TOOL_CALL_NAME,
    gents_protocol::schemas::AGENT_OUTPUT_SEGMENT_NAME,
    gents_protocol::schemas::AGENT_SESSION_NAME,
    gents_protocol::schemas::COMPACTION_ENTRY_NAME,
    gents_protocol::schemas::NETWORK_ENROLLMENT_REQUEST_NAME,
    gents_protocol::schemas::NETWORK_ENROLLMENT_DECISION_NAME,
    gents_protocol::schemas::NETWORK_AUTHORIZATION_REVISION_NAME,
    gents_protocol::schemas::NETWORK_ENROLLMENT_ROUTE_RECEIPT_NAME,
    gents_protocol::schemas::FILE_INVENTORY_REQUEST_NAME,
    gents_protocol::schemas::AGENT_SESSION_INPUT_EDIT_NAME,
    gents_protocol::schemas::AGENT_SESSION_GOAL_CONTROL_NAME,
    gents_protocol::schemas::AGENT_SESSION_GOAL_RECEIPT_NAME,
    gents_protocol::schemas::AGENT_TOOL_CONTROL_NAME,
    gents_protocol::schemas::NODE_DIRECTORY_ENTRY_NAME,
    gents_protocol::schemas::MAILBOX_ITEM_NAME,
];

fn current_sdl(name: &str) -> &'static str {
    gents_protocol::schemas::RUNTIME_COLLECTION_NAMES
        .iter()
        .copied()
        .zip(gents_protocol::schemas::RUNTIME_ALL.iter().copied())
        .chain(
            gents_protocol::schemas::ALL_COLLECTION_NAMES
                .iter()
                .copied()
                .zip(gents_protocol::schemas::ALL.iter().copied()),
        )
        .find_map(|(catalog_name, sdl)| (catalog_name == name).then_some(sdl))
        .unwrap_or_else(|| panic!("{name} is not in the protocol schema catalog"))
}

/// Precondition of the client model (see module doc): every client-authored
/// SDL uses only scalar field types.
#[test]
fn client_authored_sdls_are_relation_free() {
    const SCALAR_KINDS: &[&str] = &["String", "Int", "Float", "Boolean", "DateTime", "JSON"];
    for &name in CLIENT_AUTHORED_COLLECTIONS {
        for line in current_sdl(name).lines() {
            // GraphQL SDL comments run from `#` to end of line; the `type`
            // header is skipped: its directives may embed `fields: [...]`
            // arguments that are not field declarations.
            let code = line.split('#').next().unwrap_or_default();
            let trimmed = code.trim();
            if trimmed.is_empty() || trimmed.starts_with("type ") {
                continue;
            }
            let Some((_, after_colon)) = code.split_once(": ") else {
                continue;
            };
            let kind = after_colon
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_matches(|c| matches!(c, '[' | ']' | '!'));
            assert!(
                SCALAR_KINDS.contains(&kind),
                "{name} field `{}` uses non-scalar type {kind:?}; client-authored \
                 collections must stay relation-free (or, if DefraDB gained a new scalar \
                 kind, extend SCALAR_KINDS) — see the module doc",
                line.trim()
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn client_authored_collections_stay_fresh_apply_compatible() {
    let server_arm = async {
        let server = fresh_node().await;
        ensure_runtime(server.as_ref())
            .await
            .expect("server ensure");
        server
    };
    let client_arm = async {
        let client = fresh_node().await;
        for &name in CLIENT_AUTHORED_COLLECTIONS {
            client
                .add_collection(
                    &gents_protocol::schemas::HomePolicyIds::node_store().render(current_sdl(name)),
                )
                .await
                .unwrap_or_else(|error| panic!("client fresh-apply {name}: {error}"));
        }
        client
    };
    let (server, client) = tokio::join!(server_arm, client_arm);

    let mut mismatches = Vec::new();
    for &name in CLIENT_AUTHORED_COLLECTIONS {
        let server_cv = server
            .get_collection(name)
            .expect("server get_collection")
            .unwrap_or_else(|| panic!("server missing {name}"));
        let client_cv = client
            .get_collection(name)
            .expect("client get_collection")
            .unwrap_or_else(|| panic!("client missing {name}"));
        if server_cv.version_id != client_cv.version_id {
            mismatches.push(format!(
                "{name}: server active version {} != client fresh-apply version {} — \
                 its baseline pin is stale — client-authored collections must be pinned to \
                 the fresh-apply CID",
                server_cv.version_id, client_cv.version_id
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));

    server.shutdown().await.expect("shut down test node");
    client.shutdown().await.expect("shut down test node");
}

#[tokio::test(flavor = "multi_thread")]
async fn identity_policy_catalog_matches_applied_schemas() {
    use gents_protocol::schemas::{
        BuiltInPolicy, HomePolicyIds, ALL, ALL_COLLECTION_NAMES, CLIENT_JOURNAL_POLICY_RESOURCE,
        IDENTITY_POLICY_BINDINGS, MAILBOX_ITEM, MAILBOX_ITEM_NAME, MAILBOX_POLICY_RESOURCE,
        PENDING_CHAT_SUBMISSION, PENDING_CHAT_SUBMISSION_NAME, RUNTIME_ALL,
        RUNTIME_COLLECTION_NAMES,
    };

    let node = fresh_node().await;
    let ids = HomePolicyIds::node_store();
    let schemas = ALL_COLLECTION_NAMES
        .iter()
        .copied()
        .zip(ALL.iter().copied())
        .chain(
            RUNTIME_COLLECTION_NAMES
                .iter()
                .copied()
                .zip(RUNTIME_ALL.iter().copied()),
        );
    for (name, sdl) in schemas {
        let binding = IDENTITY_POLICY_BINDINGS
            .iter()
            .find(|(bound, _)| *bound == name);
        assert_eq!(
            sdl.contains(BuiltInPolicy::Household.placeholder()),
            binding.is_some(),
            "{name} identity policy directive and startup catalog must agree"
        );
        if let Some((_, resource)) = binding {
            node.add_collection(&ids.render(sdl))
                .await
                .expect("apply policy-bound schema");
            let collection = node.get_collection(name).unwrap().expect("collection");
            let policy = collection.policy.expect("native policy binding");
            assert_eq!(policy.id, ids.id(BuiltInPolicy::Household), "{name}");
            assert_eq!(policy.resource_name, *resource, "{name}");
            // Identity-bound collections replicate only through the protocol allowlist.
            assert_eq!(
                collection.is_branchable,
                gents_protocol::schemas::BRANCHABLE_COLLECTION_NAMES.contains(&name),
                "{name}"
            );
        }
    }
    for (name, _) in IDENTITY_POLICY_BINDINGS {
        assert_eq!(
            IDENTITY_POLICY_BINDINGS
                .iter()
                .filter(|(other, _)| other == name)
                .count(),
            1,
            "duplicate {name}"
        );
        assert!(
            ALL_COLLECTION_NAMES.contains(name) || RUNTIME_COLLECTION_NAMES.contains(name),
            "missing {name}"
        );
    }

    node.add_collection(&ids.render(PENDING_CHAT_SUBMISSION))
        .await
        .expect("journal schema");
    let journal = node
        .get_collection(PENDING_CHAT_SUBMISSION_NAME)
        .unwrap()
        .unwrap();
    let journal_policy = journal.policy.expect("journal policy binding");
    assert_eq!(journal_policy.id, ids.id(BuiltInPolicy::ClientJournal));
    assert_eq!(journal_policy.resource_name, CLIENT_JOURNAL_POLICY_RESOURCE);
    assert!(!journal.is_branchable);

    node.add_collection(&ids.render(MAILBOX_ITEM))
        .await
        .expect("mailbox schema");
    let mailbox = node.get_collection(MAILBOX_ITEM_NAME).unwrap().unwrap();
    let mailbox_policy = mailbox.policy.expect("mailbox policy binding");
    assert_eq!(mailbox_policy.id, ids.id(BuiltInPolicy::Mailbox));
    assert_eq!(mailbox_policy.resource_name, MAILBOX_POLICY_RESOURCE);
    assert!(!mailbox.is_branchable);

    node.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn unbound_identity_collection_is_rejected_without_data_changes() {
    use gents_protocol::schemas::{
        identity_policy_resource, BuiltInPolicy, DATA_PLANE_PAIRING_DESIRED,
        DATA_PLANE_PAIRING_DESIRED_NAME, NODE_NAME,
    };

    let node = fresh_node().await;
    let resource = identity_policy_resource(DATA_PLANE_PAIRING_DESIRED_NAME).unwrap();
    let directive = format!(
        "@policy(id: \"{}\", resource: \"{resource}\") ",
        BuiltInPolicy::Household.placeholder()
    );
    let unbound = DATA_PLANE_PAIRING_DESIRED.replace(&directive, "");
    assert_ne!(unbound, DATA_PLANE_PAIRING_DESIRED);
    node.add_collection(&gents_protocol::schemas::HomePolicyIds::node_store().render(&unbound))
        .await
        .unwrap();
    let created = node.execute(r#"mutation { create_DataPlanePairingDesired(input: {peer_id:"retained", collections:["example"], replicator_addresses:["address"]}) {_docID} }"#).await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let query = "{ DataPlanePairingDesired {_docID peer_id} }";
    let before = node.execute(query).await;
    assert!(matches!(
        ensure_runtime(&node).await.unwrap_err(),
        SchemaError::IncompatibleIdentityPolicy { ref collection } if collection == DATA_PLANE_PAIRING_DESIRED_NAME
    ));
    assert_eq!(node.execute(query).await.data, before.data);
    assert!(node.get_collection(NODE_NAME).unwrap().is_none());
    node.shutdown().await.unwrap();
}
