use std::sync::Arc;

use anyhow::{Context, Result};

use gents_lean_vocab::{lean_contract_snapshot, lean_identity_household_cases, LeanIdentityAction};
use gents_store::config_client::node_deferred_operation::*;
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint, NodePermission};
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;

#[tokio::test]
async fn deferred_enrollment_documents_remain_private_then_follow_the_node() -> Result<()> {
    let cases = &lean_contract_snapshot().identity_deferred_operation_cases;
    let case = |name: &str| {
        cases
            .iter()
            .find(|case| case["name"] == name)
            .with_context(|| format!("missing deferred operation model {name}"))
    };
    let before = case("before-node")?;
    let success = case("success")?;
    let revoked = case("revoked-after-publication")?;
    let foreign_owner = case("foreign-owner")?;
    let operations = lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "operations-parent-publication")
        .context("missing operations policy model")?;
    let access_checks: Vec<_> = operations
        .actions
        .iter()
        .filter_map(|action| match action {
            LeanIdentityAction::Check { expected, .. } => Some(*expected),
            _ => None,
        })
        .collect();
    let dir = tempfile::tempdir()?;
    let user = gents_store::identity::KeyIdentity::load_or_create(dir.path().join("user"), None)?;
    let runtime =
        gents_store::identity::KeyIdentity::load_or_create(dir.path().join("runtime"), None)?;
    let agent = gents_store::identity::KeyIdentity::load_or_create(dir.path().join("agent"), None)?;
    let sibling =
        gents_store::identity::KeyIdentity::load_or_create(dir.path().join("sibling"), None)?;
    let outsider =
        gents_store::identity::KeyIdentity::load_or_create(dir.path().join("outsider"), None)?;
    let data_path = dir.path().join("data");
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .with_storage_backend(defra_node::StorageBackend::Regolith)
            .data_path(&data_path)
            .build()
            .await?,
    );
    node.add_dac_policy_with_counter_as(
        &defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned())),
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await?;
    node.add_collection(gents_protocol::schemas::NODE).await?;
    node.add_collection(gents_protocol::schemas::NODE_READINESS)
        .await?;
    for schema in [
        gents_protocol::schemas::ENROLLMENT_OPERATOR_NONCE,
        gents_protocol::schemas::ENROLLMENT_RENEWAL_POLICY,
        gents_protocol::schemas::NETWORK_ADMIN_PIN,
        gents_protocol::schemas::OWED_DELIVERY,
        gents_protocol::schemas::PEER_PAIRING_APPLIED,
        gents_protocol::schemas::PEER_PAIRING_DESIRED,
    ] {
        node.add_collection(schema).await?;
    }
    let access = |actor: &gents_store::identity::KeyIdentity| -> Result<ConfigAccess> {
        Ok(ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), actor.did())
                .with_client_signing_capability(actor.client_signing_capability()?)?,
        ))
    };
    let runtime_access = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
    for actor in [&user, &agent, &sibling, &outsider] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
        ] {
            runtime_access
                .grant_node_permission(&identity::Did::new(actor.did())?, permission)
                .await?;
        }
    }
    // Public relationship routes require `get-collection`, which production
    // Users hold (`GrantSets.operatorUser`).
    runtime_access
        .grant_node_permission(
            &identity::Did::new(user.did())?,
            NodePermission::CollectionGet,
        )
        .await?;
    let user_access = access(&user)?;
    let actor = identity::Did::new(runtime.did())?;
    let offer = gents_protocol::enrollment::EnrollmentOfferRecord {
        version: 1,
        offer_id: "offer".into(),
        challenge: "challenge".into(),
        network_id: "network".into(),
        admin_did: "did:test:admin".into(),
        server_peer: "peer".into(),
        server_ticket: "ticket".into(),
        owner_node_did: "did:test:server".into(),
        profile: "profile".into(),
        schema_fingerprint: "schema".into(),
        live_schema_digest: format!("sha256:{}", "a".repeat(64)),
        issued_at: "2026-01-01T00:00:00Z".into(),
        expires_at: "2027-01-01T00:00:00Z".into(),
        admin_sig: Vec::new(),
    };
    gents_p2p::p2p_reconcile::enrollment_candidate::confirm_network_admin_pin(&node, &offer)
        .await?;
    gents_p2p::p2p_reconcile::enrollment_candidate::confirm_network_admin_pin(&node, &offer)
        .await?;
    let mut conflict = offer.clone();
    conflict.admin_did = "did:test:other-admin".into();
    assert!(
        gents_p2p::p2p_reconcile::enrollment_candidate::confirm_network_admin_pin(&node, &conflict)
            .await
            .is_err()
    );
    let applied_mutation = format!(
        r#"mutation {{ create_PeerPairingApplied(input: {{node_did:"{}", peer_id:"peer", parent_linked:false}}) {{_docID}} }}"#,
        escape_graphql_string(runtime.did())
    );
    let desired_mutation = format!(
        r#"mutation {{ create_PeerPairingDesired(input: {{node_did:"remote", actor_did:"{}", peer_id:"peer", parent_linked:false}}) {{_docID}} }}"#,
        escape_graphql_string(runtime.did())
    );
    let mutations = [
        (
            "EnrollmentOperatorNonce",
            Some(
                r#"mutation { create_EnrollmentOperatorNonce(input: {nonce_key:"nonce", admin_did:"did:test:admin", expires_at:"2027-01-01T00:00:00Z", consumed_at:"2026-01-01T00:00:00Z", parent_linked:false}) {_docID} }"#,
            ),
        ),
        (
            "EnrollmentRenewalPolicy",
            Some(
                r#"mutation { create_EnrollmentRenewalPolicy(input: {network_id:"network", enabled:true, parent_linked:false}) {_docID} }"#,
            ),
        ),
        ("NetworkAdminPin", None),
        ("PeerPairingApplied", Some(applied_mutation.as_str())),
        ("PeerPairingDesired", Some(desired_mutation.as_str())),
    ];
    let mut created = Vec::new();
    for (collection, mutation) in mutations {
        let doc = if let Some(mutation) = mutation {
            runtime_access
                .transact_write("test.deferred.create", |txn| {
                    let actor = &actor;
                    Box::pin(async move {
                        let response = txn.execute(mutation).await?;
                        let doc = gents_protocol::graphql::extract_mutation_doc_id(
                            &response, collection,
                        )?;
                        maybe_link_deferred_operation_in_txn(txn, collection, &doc, actor).await?;
                        Ok::<_, anyhow::Error>(doc)
                    })
                })
                .await?
        } else {
            let response = runtime_access
                .execute("{ NetworkAdminPin { _docID } }")
                .await?;
            response["data"]["NetworkAdminPin"][0]["_docID"]
                .as_str()
                .context("pin ID")?
                .to_owned()
        };
        let query = format!("{{ {collection} {{_docID parent_linked}} }}");
        let own = runtime_access.execute(&query).await?;
        assert_eq!(
            own["data"][collection][0]["parent_linked"],
            before["expected_linked"]
        );
        for actor in [&user, &agent, &sibling, &outsider] {
            let response = access(actor)?.execute(&query).await?;
            assert!(
                gents_protocol::graphql::graphql_rows_from_response(&response, collection)
                    .is_empty()
            );
        }
        created.push((collection, doc));
    }
    node.shutdown().await?;
    drop(runtime_access);
    drop(user_access);
    drop(node);
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .with_storage_backend(defra_node::StorageBackend::Regolith)
            .data_path(&data_path)
            .build()
            .await?,
    );
    let access = |actor: &gents_store::identity::KeyIdentity| -> Result<ConfigAccess> {
        Ok(ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), actor.did())
                .with_client_signing_capability(actor.client_signing_capability()?)?,
        ))
    };
    let runtime_access = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
    let user_access = access(&user)?;
    let parent = user_access
        .write(
            "test.deferred.node",
            &format!(
                r#"mutation {{ create_Node(input: {{node_did: "{}"}}) {{_docID}} }}"#,
                gents_store::graphql::escape_graphql_string(runtime.did()),
            ),
        )
        .await?;
    let parent = gents_protocol::graphql::extract_mutation_doc_id(&parent, "Node")?;
    user_access
        .add_actor_relationship("Node", &parent, "runtime_reader", &actor)
        .await?;
    gents_p2p::p2p_reconcile::enrollment_candidate::confirm_network_admin_pin(&node, &offer)
        .await?;
    let readiness = gents_protocol::node_readiness::NodeReadinessSnapshot {
        format_version: gents_protocol::node_readiness::NODE_READINESS_FORMAT_VERSION,
        process_state: gents_protocol::node_readiness::NodeReadinessProcessState::Recovering,
        active_generation: 0,
        router_generation: 0,
        default_agent_did: String::new(),
        agents: Vec::new(),
    };
    crate::runtime::node_readiness_publisher::upsert_node_readiness(
        &node,
        runtime.did(),
        &readiness,
        "2026-01-01T00:00:00Z",
    )
    .await?;
    for (collection, doc) in &created {
        let query = format!("{{ {collection} {{_docID node_doc_id parent_linked}} }}");
        let response = user_access.execute(&query).await?;
        let row = &response["data"][*collection][0];
        assert_eq!(row["parent_linked"], success["expected_linked"]);
        assert_eq!(row["node_doc_id"], parent);
        let mutation = format!(
            r#"mutation {{ update_{collection}(docID: "{}", input: {{node_doc_id: "forged"}}) {{_docID}} }}"#,
            gents_store::graphql::escape_graphql_string(doc)
        );
        let updated = user_access.write("test.deferred.denied", &mutation).await;
        assert_eq!(
            updated.is_ok_and(|response| gents_protocol::graphql::extract_mutation_doc_id(
                &response, collection
            )
            .is_ok()),
            access_checks[4]
        );
        for (actor, expected) in [
            (&agent, access_checks[6]),
            (&sibling, access_checks[7]),
            (&outsider, access_checks[11]),
        ] {
            let response = access(actor)?.execute(&query).await?;
            assert_eq!(
                !gents_protocol::graphql::graphql_rows_from_response(&response, collection)
                    .is_empty(),
                expected
            );
        }
        runtime_access
            .delete_resource_relationship(collection, doc, "parent", "nodes", &parent)
            .await?;
    }
    recover_deferred_node_operations(&node, runtime.did()).await?;
    for (collection, _) in &created {
        let response = user_access
            .execute(&format!("{{ {collection} {{_docID}} }}"))
            .await?;
        assert_eq!(
            !gents_protocol::graphql::graphql_rows_from_response(&response, collection).is_empty(),
            revoked["expected_granted"]
                .as_bool()
                .context("model grant")?
        );
    }
    node.shutdown().await?;
    drop(runtime_access);
    drop(user_access);
    drop(node);
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let reopened = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .with_storage_backend(defra_node::StorageBackend::Regolith)
            .data_path(&data_path)
            .build()
            .await?,
    );
    recover_deferred_node_operations(&reopened, runtime.did()).await?;
    let user_access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
            .with_client_signing_capability(user.client_signing_capability()?)?,
    );
    for (collection, _) in &created {
        let response = user_access
            .execute(&format!("{{ {collection} {{_docID}} }}"))
            .await?;
        assert_eq!(
            !gents_protocol::graphql::graphql_rows_from_response(&response, collection).is_empty(),
            revoked["expected_granted"]
                .as_bool()
                .context("model grant")?
        );
    }
    let foreign = user_access.write("test.deferred.foreign", r#"mutation { create_NetworkAdminPin(input: {pin_key:"foreign-pin", network_id:"foreign", admin_did:"did:test:admin", offer_id:"foreign", confirmed_at:"2026-01-01T00:00:00Z", parent_linked:false}) {_docID} }"#).await?;
    let foreign = gents_protocol::graphql::extract_mutation_doc_id(&foreign, "NetworkAdminPin")?;
    user_access
        .add_resource_relationship("NetworkAdminPin", &foreign, "parent", "nodes", &parent)
        .await?;
    assert!(recover_deferred_node_operations(&reopened, runtime.did())
        .await
        .is_err());
    let observed = user_access
        .execute(&format!(
            r#"{{ NetworkAdminPin(filter: {{_docID: {{_eq: "{}"}}}}) {{parent_linked}} }}"#,
            gents_store::graphql::escape_graphql_string(&foreign),
        ))
        .await?;
    assert_eq!(
        observed["data"]["NetworkAdminPin"][0]["parent_linked"],
        foreign_owner["expected_linked"]
    );
    reopened.shutdown().await?;
    Ok(())
}
