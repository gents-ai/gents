//! Guard for #1123/#1125: client-authored collections MUST remain fresh-apply
//! compatible. `EmbeddedNode::add_collection` here and a paired client's FFI both
//! ingest SDL into an empty store and converge on the same collection-creation
//! path in the pinned defradb, so the genesis CIDs minted here stand in for
//! what a phone mints. The two apply paths seed their SDL parsers with
//! different known-type sets, which is only guaranteed equivalent while every
//! client-authored SDL is single-type and relation-free — a relation field's
//! CID embeds the target collection's identity, making genesis CIDs depend on
//! the co-registered set.

use gents_migration::{ensure_migrations, CLIENT_AUTHORED_COLLECTIONS};

mod common;
use common::fresh_node;

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

#[tokio::test(flavor = "multi_thread")]
async fn goal_controls_bind_agent_ownership_policy() {
    let node = fresh_node().await;
    node.add_collection(gents_protocol::schemas::AGENT_SESSION_GOAL_CONTROL)
        .await
        .expect("goal control schema");
    let collection = node
        .get_collection(gents_protocol::schemas::AGENT_SESSION_GOAL_CONTROL_NAME)
        .expect("collection lookup")
        .expect("goal control collection");
    let policy = collection.policy.expect("goal control policy");
    assert_eq!(
        policy.id,
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID
    );
    assert_eq!(policy.resource_name, "goal_controls");
    assert!(!collection.is_branchable);
    assert_eq!(collection.collection_id, collection.version_id);
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
        ensure_migrations(server.as_ref())
            .await
            .expect("server ensure_migrations");
        server
    };
    let client_arm = async {
        let client = fresh_node().await;
        for &name in CLIENT_AUTHORED_COLLECTIONS {
            client
                .add_collection(current_sdl(name))
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
                 collection gained a post-baseline migration step or its baseline pin is \
                 stale — client-authored collections must be re-pinned to the \
                 fresh-apply CID, never chained; see #1123/#1125",
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
        AGENT_OWNERSHIP_POLICY_ID, ALL, ALL_COLLECTION_NAMES, IDENTITY_POLICY_BINDINGS,
        RUNTIME_ALL, RUNTIME_COLLECTION_NAMES,
    };

    let node = fresh_node().await;
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
            sdl.contains(AGENT_OWNERSHIP_POLICY_ID),
            binding.is_some(),
            "{name} identity policy directive and startup catalog must agree"
        );
        if let Some((_, resource)) = binding {
            node.add_collection(sdl)
                .await
                .expect("apply policy-bound schema");
            let collection = node.get_collection(name).unwrap().expect("collection");
            let policy = collection.policy.expect("native policy binding");
            assert_eq!(policy.id, AGENT_OWNERSHIP_POLICY_ID, "{name}");
            assert_eq!(policy.resource_name, *resource, "{name}");
            assert!(!collection.is_branchable, "{name}");
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
}

#[tokio::test(flavor = "multi_thread")]
async fn client_journal_applies_private_policy() {
    use gents_protocol::schemas::{
        CLIENT_JOURNAL_POLICY_ID, CLIENT_JOURNAL_POLICY_RESOURCE, PENDING_CHAT_SUBMISSION,
        PENDING_CHAT_SUBMISSION_NAME,
    };
    let node = fresh_node().await;
    node.add_collection(PENDING_CHAT_SUBMISSION)
        .await
        .expect("journal schema");
    let collection = node
        .get_collection(PENDING_CHAT_SUBMISSION_NAME)
        .unwrap()
        .unwrap();
    let policy = collection.policy.expect("journal policy binding");
    assert_eq!(policy.id, CLIENT_JOURNAL_POLICY_ID);
    assert_eq!(policy.resource_name, CLIENT_JOURNAL_POLICY_RESOURCE);
    assert!(!collection.is_branchable);
}

#[tokio::test(flavor = "multi_thread")]
async fn mailbox_applies_recipient_policy() {
    use gents_protocol::schemas::{
        MAILBOX_ITEM, MAILBOX_ITEM_NAME, MAILBOX_POLICY_ID, MAILBOX_POLICY_RESOURCE,
    };
    let node = fresh_node().await;
    node.add_collection(MAILBOX_ITEM)
        .await
        .expect("mailbox schema");
    let collection = node.get_collection(MAILBOX_ITEM_NAME).unwrap().unwrap();
    let policy = collection.policy.expect("mailbox policy binding");
    assert_eq!(policy.id, MAILBOX_POLICY_ID);
    assert_eq!(policy.resource_name, MAILBOX_POLICY_RESOURCE);
    assert!(!collection.is_branchable);
    node.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn enrollment_operations_match_baseline_pins() {
    use gents_protocol::schemas::{
        DATA_PLANE_PAIRING_DESIRED, DATA_PLANE_PAIRING_DESIRED_NAME, ENROLLMENT_OPERATOR_NONCE,
        ENROLLMENT_OPERATOR_NONCE_NAME, NETWORK_ADMIN_PIN, NETWORK_ADMIN_PIN_NAME,
        PEER_PAIRING_APPLIED, PEER_PAIRING_APPLIED_NAME, PEER_PAIRING_DESIRED,
        PEER_PAIRING_DESIRED_NAME,
    };
    let node = fresh_node().await;
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (name, sdl) in [
        (ENROLLMENT_OPERATOR_NONCE_NAME, ENROLLMENT_OPERATOR_NONCE),
        (NETWORK_ADMIN_PIN_NAME, NETWORK_ADMIN_PIN),
        (PEER_PAIRING_APPLIED_NAME, PEER_PAIRING_APPLIED),
        (PEER_PAIRING_DESIRED_NAME, PEER_PAIRING_DESIRED),
        (DATA_PLANE_PAIRING_DESIRED_NAME, DATA_PLANE_PAIRING_DESIRED),
    ] {
        node.add_collection(sdl).await.unwrap();
        actual.push((name, node.get_collection(name).unwrap().unwrap().version_id));
        expected.push((
            name,
            gents_migration::DEFAULT_BASELINE
                .iter()
                .find(|entry| entry.name == name)
                .unwrap()
                .expected_version
                .unwrap()
                .to_owned(),
        ));
    }
    assert_eq!(actual, expected);
    node.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn unbound_pairing_overlay_is_rejected_without_data_changes() {
    use gents_protocol::schemas::{AGENT_OWNERSHIP_POLICY_ID, DATA_PLANE_PAIRING_DESIRED};
    let node = fresh_node().await;
    let directive =
        format!("@policy(id: \"{AGENT_OWNERSHIP_POLICY_ID}\", resource: \"configuration\") ");
    node.add_collection(&DATA_PLANE_PAIRING_DESIRED.replace(&directive, ""))
        .await
        .unwrap();
    let created = node.execute(r#"mutation { create_DataPlanePairingDesired(input: {peer_id:"retained", collections:["example"], replicator_addresses:["address"]}) {_docID} }"#).await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let query = "{ DataPlanePairingDesired {_docID peer_id} }";
    let before = node.execute(query).await;
    assert!(matches!(ensure_migrations(&node).await.unwrap_err(),
        gents_migration::Error::IncompatibleIdentityPolicy { ref collection } if collection == "DataPlanePairingDesired"));
    assert_eq!(node.execute(query).await.data, before.data);
    node.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn unbound_identity_collections_are_rejected_without_data_changes() {
    use gents_protocol::schemas::{
        AGENT_OWNERSHIP_POLICY_ID, ENROLLMENT_OPERATOR_NONCE, ENROLLMENT_OPERATOR_NONCE_NAME,
        FILE_IMAGE_HYDRATION_REQUEST, FILE_IMAGE_HYDRATION_REQUEST_NAME,
        FILE_INVENTORY_IMAGE_REQUEST, FILE_INVENTORY_IMAGE_REQUEST_NAME, FILE_INVENTORY_REQUEST,
        FILE_INVENTORY_REQUEST_NAME, NETWORK_ADMIN_PIN, NETWORK_ADMIN_PIN_NAME, NODE_NAME,
        NODE_RUNTIME, NODE_RUNTIME_NAME, OAUTH_CREDENTIAL, OAUTH_CREDENTIAL_NAME,
        PEER_PAIRING_APPLIED, PEER_PAIRING_APPLIED_NAME, PEER_PAIRING_DESIRED,
        PEER_PAIRING_DESIRED_NAME, PROJECTION_ACP_BINDING, PROJECTION_ACP_BINDING_NAME,
        SESSION_HYDRATION_REQUEST, SESSION_HYDRATION_REQUEST_NAME, TOOL_SERVICE_HEALTH_STATE,
        TOOL_SERVICE_HEALTH_STATE_NAME,
    };
    for (name, sdl, resource, key) in [
        (
            PEER_PAIRING_DESIRED_NAME,
            PEER_PAIRING_DESIRED,
            "pairing_observations",
            "peer_id",
        ),
        (
            PEER_PAIRING_APPLIED_NAME,
            PEER_PAIRING_APPLIED,
            "pairing_observations",
            "peer_id",
        ),
        (
            PROJECTION_ACP_BINDING_NAME,
            PROJECTION_ACP_BINDING,
            "configuration",
            "binding_id",
        ),
        (NODE_RUNTIME_NAME, NODE_RUNTIME, "operations", "node_did"),
        (
            TOOL_SERVICE_HEALTH_STATE_NAME,
            TOOL_SERVICE_HEALTH_STATE,
            "operations",
            "service_id",
        ),
        (
            OAUTH_CREDENTIAL_NAME,
            OAUTH_CREDENTIAL,
            "secret_configuration",
            "credential_id",
        ),
        (
            ENROLLMENT_OPERATOR_NONCE_NAME,
            ENROLLMENT_OPERATOR_NONCE,
            "operations",
            "nonce_key",
        ),
        (
            NETWORK_ADMIN_PIN_NAME,
            NETWORK_ADMIN_PIN,
            "operations",
            "pin_key",
        ),
        (
            SESSION_HYDRATION_REQUEST_NAME,
            SESSION_HYDRATION_REQUEST,
            "configuration",
            "request_key",
        ),
        (
            FILE_IMAGE_HYDRATION_REQUEST_NAME,
            FILE_IMAGE_HYDRATION_REQUEST,
            "configuration",
            "request_key",
        ),
        (
            FILE_INVENTORY_IMAGE_REQUEST_NAME,
            FILE_INVENTORY_IMAGE_REQUEST,
            "configuration",
            "request_key",
        ),
        (
            FILE_INVENTORY_REQUEST_NAME,
            FILE_INVENTORY_REQUEST,
            "configuration",
            "request_key",
        ),
    ] {
        let node = fresh_node().await;
        let directive =
            format!("@policy(id: \"{AGENT_OWNERSHIP_POLICY_ID}\", resource: \"{resource}\") ");
        let old = sdl.replace(&directive, "");
        assert_ne!(old, sdl, "{name}: expected policy directive was not removed");
        node.add_collection(&old).await.unwrap();
        let extra = if name == FILE_INVENTORY_IMAGE_REQUEST_NAME {
            ", inventory_reference: {}"
        } else {
            ""
        };
        let created = node
            .execute(&format!(
                "mutation {{ create_{name}(input: {{ {key}: \"retained\" {extra} }}) {{ _docID }} }}"
            ))
            .await;
        assert!(!created.has_errors(), "{name}: {:?}", created.errors);
        let query = format!("{{ {name} {{ _docID {key} }} }}");
        let before = node.execute(&query).await;
        let error = ensure_migrations(&node).await.unwrap_err();
        assert!(
            matches!(error, gents_migration::Error::IncompatibleIdentityPolicy { ref collection } if collection == name),
            "{name}: {error}"
        );
        let after = node.execute(&query).await;
        assert!(!after.has_errors(), "{name}: {:?}", after.errors);
        assert_eq!(before.data, after.data, "{name}");
        assert!(node.get_collection(NODE_NAME).unwrap().is_none());
        node.shutdown().await.unwrap();
    }
}
