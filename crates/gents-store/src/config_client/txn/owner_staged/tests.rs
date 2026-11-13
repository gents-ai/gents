use std::sync::Arc;

use gents_lean_vocab::families::{owner_staged_pairing, owner_staged_relations};
use gents_protocol::owner_staged::OwnerStagedChange;

use crate::config_client::{
    ConfigAccess, ConfigApplyTxn, ConfigClientError, GraphqlEndpoint, PairingObservation,
    PairingReadReason,
};

const POLICY: &str = "name: OwnerStaged\nresources:\n  - name: rows\n    relations:\n      - name: writer\n      - name: editor\n    permissions:\n      - name: read\n        expr: writer + editor\n      - name: update\n        expr: editor\n      - name: delete\n";

fn rows(response: &serde_json::Value) -> usize {
    response["data"]["OwnerStagedRow"]
        .as_array()
        .map_or(0, Vec::len)
}

/// Lean `Conformance.OwnerStaged.relations`: a requester's own change to a
/// relation that only the document's runtime owner manages is refused on the
/// public route, including for the node's NAC owner; the runtime stages the
/// same change as the owner for a requester that the modeled authority admits
/// and stages nothing for one it refuses.
#[tokio::test]
async fn owner_staged_relationships_follow_the_document_owner() -> anyhow::Result<()> {
    use crate::identity::NodeIdentity;
    let directory = tempfile::tempdir()?;
    let runtime =
        crate::identity::KeyIdentity::load_or_create(directory.path().join("runtime"), None)?;
    // An embedded configuration transaction acts as the runtime or as one of
    // its managed Agents, so every modeled requester is a managed Agent.
    let managed = || -> anyhow::Result<Arc<dyn NodeIdentity>> {
        let binding = crate::identity::provision_agent_identity(&runtime)?;
        Ok(crate::identity::resolve_agent_identity(&runtime, &binding)?)
    };
    let (user, admin, outsider, target) = (managed()?, managed()?, managed()?, managed()?);
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr()?);
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .with_node_identity_did(runtime.did())
            .with_node_acp_owner(user.did().to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    let owner_context =
        defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    for actor in [admin.did(), outsider.did(), runtime.did(), target.did()] {
        for permission in [
            acp::nac::NodePermission::CollectionGet,
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::DocumentUpdate,
            acp::nac::NodePermission::DacRelationAdd,
            acp::nac::NodePermission::DacRelationDelete,
        ] {
            node.add_nac_actor_relationship_as(&owner_context, permission.as_str(), actor)
                .await?;
        }
    }
    let household = gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("configuration policy model");
    node.add_dac_policy_with_counter_as(&owner_context, &household.policy_yaml, 0)
        .await?;
    let policy = node.add_dac_policy_as(&owner_context, POLICY).await?;
    node.add_collection_as(&owner_context, gents_protocol::schemas::NODE)
        .await?;
    node.add_collection_as(
        &owner_context,
        &format!(
            "type OwnerStagedRow @policy(id: \"{policy}\", resource: \"rows\") {{ label: String }}"
        ),
    )
    .await?;
    let client = |key: &Arc<dyn NodeIdentity>| -> anyhow::Result<ConfigAccess> {
        Ok(ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(&endpoint, key.did())
                .with_client_signing_capability(key.client_signing_capability()?)?,
        ))
    };
    let runtime_did = identity::Did::new(runtime.did())?;
    let user_did = identity::Did::new(user.did())?;
    let target_did = identity::Did::new(target.did())?;
    let user_access = client(&user)?;
    let parent = user_access
        .write(
            "test.owner_staged.node",
            &format!(
                r#"mutation {{ create_Node(input: {{node_did: "{}"}}) {{_docID}} }}"#,
                crate::graphql::escape_graphql_string(runtime.did())
            ),
        )
        .await?;
    let parent = gents_protocol::graphql::extract_mutation_doc_id(&parent, "Node")?;
    user_access
        .add_actor_relationship(
            "Node",
            &parent,
            "administrator",
            &identity::Did::new(admin.did())?,
        )
        .await?;
    // The runtime reads its own Node row, on which Node authority is decided.
    user_access
        .add_actor_relationship("Node", &parent, "runtime_reader", &runtime_did)
        .await?;
    let runtime_access = ConfigAccess::local_as(node.clone(), runtime_did.clone());
    let created = runtime_access
        .write(
            "test.owner_staged.row",
            "mutation { add_OwnerStagedRow(input: {label: \"row\"}) { _docID } }",
        )
        .await?;
    let doc = created["data"]["add_OwnerStagedRow"][0]["_docID"]
        .as_str()
        .expect("row ID")
        .to_owned();
    runtime_access
        .transact_write("test.owner_staged.editor", |txn| {
            let (doc, user_did) = (&doc, &user_did);
            Box::pin(async move {
                txn.add_actor_relationship("OwnerStagedRow", doc, "editor", user_did)
                    .await
            })
        })
        .await?;
    let target_access = ConfigAccess::local_as(node.clone(), target_did.clone());
    let visible = || async {
        anyhow::Ok(rows(
            &target_access
                .execute("{ OwnerStagedRow { _docID } }")
                .await?,
        ))
    };

    let staging = |key: &Arc<dyn NodeIdentity>, did: identity::Did| {
        let signer = key.managed_agent_signer().expect("managed Agent signer");
        ConfigApplyTxn::begin_local_with_signer(&node, did, signer)
    };

    // The owner and Node-authority decision on a generic runtime-owned row;
    // target kinds, Agent-row derivation and pairing readers are replayed on
    // their real collections over signed HTTP (`owner_staged_route::tests`).
    for case in owner_staged_relations::cases() {
        if !matches!(
            case.input.path.as_str(),
            "event_cursor_rebind" | "declared_placement_readers" | "workspace_selection"
        ) {
            continue;
        }
        let (key, did) = match case.input.requester.as_str() {
            "user" => (&user, user_did.clone()),
            "outsider" => (&outsider, identity::Did::new(outsider.did())?),
            _ => continue,
        };
        let input = &case.input;
        assert_eq!(visible().await?, 0, "{input:?}");
        let direct = client(key)?
            .transact_write("test.owner_staged.direct", |txn| {
                let (doc, target_did) = (&doc, &target_did);
                Box::pin(async move {
                    txn.add_actor_relationship("OwnerStagedRow", doc, "writer", target_did)
                        .await
                })
            })
            .await
            .is_ok();
        assert_eq!(visible().await? == 1, direct, "{input:?}");
        if direct {
            crate::test_support::relationships::actor_in_txn(
                &runtime_access,
                "OwnerStagedRow",
                &doc,
                "writer",
                &target_did,
                true,
            )
            .await?;
        }
        let txn = staging(key, did).await?;
        let result = txn
            .change_owner_staged(
                "OwnerStagedRow",
                &doc,
                "writer",
                target_did.as_str(),
                false,
                &super::Authority::Configuration(super::TargetKind::Any),
            )
            .await;
        let outcome = decided(&doc, &result);
        if result.is_ok() {
            txn.commit().await?;
        } else {
            txn.discard().await?;
        }
        let staged = visible().await? == 1;
        if staged {
            let txn = staging(&user, user_did.clone()).await?;
            txn.change_owner_staged(
                "OwnerStagedRow",
                &doc,
                "writer",
                target_did.as_str(),
                true,
                &super::Authority::Configuration(super::TargetKind::Any),
            )
            .await?;
            txn.commit().await?;
        }
        case.check(owner_staged_relations::RelationOutput {
            direct,
            staged,
            outcome: outcome.to_owned(),
        });
    }
    // `unauthorized_refusal_independent_of_document`: an outsider learns
    // nothing from a row that does not exist.
    let missing = {
        let txn = ConfigApplyTxn::begin_local(&node, Some(runtime_did.clone())).await?;
        let created = txn
            .execute("mutation { add_OwnerStagedRow(input: {label: \"missing\"}) { _docID } }")
            .await?;
        txn.discard().await?;
        created["data"]["add_OwnerStagedRow"][0]["_docID"]
            .as_str()
            .expect("row ID")
            .to_owned()
    };
    for row in [&doc, &missing] {
        let txn = staging(&outsider, identity::Did::new(outsider.did())?).await?;
        let refused = txn
            .change_owner_staged(
                "OwnerStagedRow",
                row,
                "writer",
                target_did.as_str(),
                false,
                &super::Authority::Configuration(super::TargetKind::Any),
            )
            .await;
        assert!(
            matches!(
                &refused,
                Err(ConfigClientError::OwnerStagedRelationshipDenied { doc_id, .. }) if doc_id == row
            ),
            "{row}: {refused:?}"
        );
        txn.commit().await?;
        assert_eq!(visible().await?, 0, "{row}");
    }
    node.shutdown().await?;
    Ok(())
}

/// Lean `Conformance.OwnerStaged.pairing` on the real pairing schema and
/// `pairing_observations` policy: a requester that may update the runtime's
/// own Node gets readers staged only on a pairing observation the runtime
/// owns; every other request is refused before anything is staged.
#[tokio::test]
async fn pairing_readers_are_staged_only_on_runtime_owned_observations() -> anyhow::Result<()> {
    use crate::identity::NodeIdentity;
    let directory = tempfile::tempdir()?;
    let runtime =
        crate::identity::KeyIdentity::load_or_create(directory.path().join("runtime"), None)?;
    let stranger =
        crate::identity::KeyIdentity::load_or_create(directory.path().join("stranger"), None)?;
    let managed = || -> anyhow::Result<Arc<dyn NodeIdentity>> {
        let binding = crate::identity::provision_agent_identity(&runtime)?;
        Ok(crate::identity::resolve_agent_identity(&runtime, &binding)?)
    };
    let (user, admin, outsider, other_admin, target) =
        (managed()?, managed()?, managed()?, managed()?, managed()?);
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr()?);
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .with_node_identity_did(runtime.did())
            .with_node_acp_owner(user.did().to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    let owner_context =
        defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    for actor in [
        admin.did(),
        outsider.did(),
        other_admin.did(),
        runtime.did(),
        target.did(),
        stranger.did(),
    ] {
        for permission in [
            acp::nac::NodePermission::CollectionGet,
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::DocumentUpdate,
            acp::nac::NodePermission::DacRelationAdd,
            acp::nac::NodePermission::DacRelationDelete,
        ] {
            node.add_nac_actor_relationship_as(&owner_context, permission.as_str(), actor)
                .await?;
        }
    }
    node.add_dac_policy_with_counter_as(
        &owner_context,
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await?;
    for schema in [
        gents_protocol::schemas::NODE,
        gents_protocol::schemas::PEER_PAIRING_DESIRED,
    ] {
        node.add_collection_as(&owner_context, schema).await?;
    }
    let client = |key: &Arc<dyn NodeIdentity>| -> anyhow::Result<ConfigAccess> {
        Ok(ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(&endpoint, key.did())
                .with_client_signing_capability(key.client_signing_capability()?)?,
        ))
    };
    let did = |key: &str| identity::Did::new(key);
    let user_access = client(&user)?;
    // The runtime's Node, administered by `admin`, and another Node with a
    // different `node_did`, administered by `other_admin`.
    for (node_did, administrator) in [(runtime.did(), &admin), (stranger.did(), &other_admin)] {
        let created = user_access
            .write(
                "test.owner_staged.pairing_node",
                &format!(
                    r#"mutation {{ create_Node(input: {{node_did: "{}"}}) {{_docID}} }}"#,
                    crate::graphql::escape_graphql_string(node_did)
                ),
            )
            .await?;
        let created = gents_protocol::graphql::extract_mutation_doc_id(&created, "Node")?;
        user_access
            .add_actor_relationship(
                "Node",
                &created,
                "administrator",
                &did(administrator.did())?,
            )
            .await?;
        if node_did == runtime.did() {
            user_access
                .add_actor_relationship("Node", &created, "runtime_reader", &did(runtime.did())?)
                .await?;
        }
    }
    let pairing = |author: &str, peer: &str| {
        format!(
            r#"mutation {{ create_PeerPairingDesired(input: {{node_did: "{author}", actor_did: "{author}",
            peer_id: "{peer}", source: "enrollment", collections: ["AgentSessionInputEdit"],
            replicator_addresses: ["{peer}-address"], parent_linked: false}}) {{_docID}} }}"#,
            author = crate::graphql::escape_graphql_string(author),
        )
    };
    let runtime_access = ConfigAccess::local_as(node.clone(), did(runtime.did())?);
    let route = runtime_access
        .write("test.owner_staged.route", &pairing(runtime.did(), "route"))
        .await?;
    let route = gents_protocol::graphql::extract_mutation_doc_id(&route, "PeerPairingDesired")?;
    // The runtime may read the foreign observation, so it observes its owner.
    let stranger_access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(&endpoint, stranger.did())
            .with_client_signing_capability(stranger.client_signing_capability()?)?,
    );
    let foreign = stranger_access
        .write(
            "test.owner_staged.foreign",
            &pairing(stranger.did(), "foreign"),
        )
        .await?;
    let foreign = gents_protocol::graphql::extract_mutation_doc_id(&foreign, "PeerPairingDesired")?;
    stranger_access
        .add_actor_relationship(
            "PeerPairingDesired",
            &foreign,
            "route_reader",
            &did(runtime.did())?,
        )
        .await?;
    let missing = {
        let txn = ConfigApplyTxn::begin_local(&node, Some(did(runtime.did())?)).await?;
        let created = txn.execute(&pairing(runtime.did(), "missing")).await?;
        txn.discard().await?;
        gents_protocol::graphql::extract_mutation_doc_id(&created, "PeerPairingDesired")?
    };
    let target_did = did(target.did())?;
    let target_access = ConfigAccess::local_as(node.clone(), target_did.clone());
    let readable = || async {
        anyhow::Ok(
            gents_protocol::graphql::graphql_rows_from_response(
                &target_access
                    .execute("{ PeerPairingDesired { _docID } }")
                    .await?,
                "PeerPairingDesired",
            )
            .into_iter()
            .filter_map(|row| row["_docID"].as_str().map(str::to_owned))
            .collect::<Vec<_>>(),
        )
    };
    let staging = |key: &Arc<dyn NodeIdentity>| {
        let signer = key.managed_agent_signer().expect("managed Agent signer");
        ConfigApplyTxn::begin_local_with_signer(&node, did(key.did()).unwrap(), signer)
    };

    for case in owner_staged_pairing::cases() {
        let input = &case.input;
        let key = match input.requester.as_str() {
            "user" => &user,
            "admin" => &admin,
            "outsider" => &outsider,
            "other-admin" => &other_admin,
            other => panic!("unknown modeled requester {other}"),
        };
        let doc = match input.document.as_str() {
            "route" => &route,
            "foreign" => &foreign,
            "missing" => &missing,
            other => panic!("unknown modeled document {other}"),
        };
        assert!(readable().await?.is_empty(), "{input:?}");
        let direct = client(key)?
            .transact_write("test.owner_staged.pairing_direct", |txn| {
                let target_did = &target_did;
                Box::pin(async move {
                    txn.add_actor_relationship(
                        "PeerPairingDesired",
                        doc,
                        "route_reader",
                        target_did,
                    )
                    .await
                })
            })
            .await
            .is_ok();
        assert_eq!(readable().await? == vec![doc.clone()], direct, "{input:?}");
        if direct {
            let txn = staging(&user).await?;
            txn.set_pairing_reader(
                PairingObservation::Desired,
                doc,
                PairingReadReason::RouteAdmission,
                &target_did,
                false,
            )
            .await?;
            txn.commit().await?;
        }
        let txn = staging(key).await?;
        let result = txn
            .set_pairing_reader(
                PairingObservation::Desired,
                doc,
                PairingReadReason::RouteAdmission,
                &target_did,
                true,
            )
            .await;
        let outcome = decided(doc, &result);
        // A refused request staged nothing, even once its transaction commits.
        txn.commit().await?;
        let staged = readable().await? == vec![doc.clone()];
        if staged {
            let txn = staging(&user).await?;
            txn.set_pairing_reader(
                PairingObservation::Desired,
                doc,
                PairingReadReason::RouteAdmission,
                &target_did,
                false,
            )
            .await?;
            txn.commit().await?;
        }
        // The runtime's owner-staged route runs the same request as the
        // authenticated client, without its key
        // (`outcome_is_transport_independent`).
        let routed_access = ConfigAccess::local_as(node.clone(), did(key.did())?);
        let reader = |doc: &str| OwnerStagedChange::PairingReader {
            observation: PairingObservation::Desired,
            doc_id: doc.to_owned(),
            reason: PairingReadReason::RouteAdmission,
            reader_did: target_did.to_string(),
        };
        let routed = routed_access
            .request_verified_client_owner_staged(&reader(doc), true)
            .await;
        let http = decided(doc, &routed);
        assert_eq!(readable().await? == vec![doc.clone()], staged, "{input:?}");
        if staged {
            routed_access
                .request_verified_client_owner_staged(&reader(doc), false)
                .await?;
            assert!(readable().await?.is_empty(), "{input:?}");
        }
        case.check(owner_staged_pairing::PairingOutput {
            direct,
            staged,
            outcome: outcome.to_owned(),
            http: http.to_owned(),
        });
    }
    // A route transaction never runs as the runtime itself.
    assert!(matches!(
        ConfigAccess::local_as(node.clone(), did(runtime.did())?)
            .request_verified_client_owner_staged(
                &OwnerStagedChange::PairingReader {
                    observation: PairingObservation::Desired,
                    doc_id: route.clone(),
                    reason: PairingReadReason::RouteAdmission,
                    reader_did: target_did.to_string(),
                },
                true,
            )
            .await,
        Err(ConfigClientError::TransactionActorMismatch)
    ));
    node.shutdown().await?;
    Ok(())
}

fn decided(doc: &str, result: &Result<(), ConfigClientError>) -> &'static str {
    match result {
        Ok(()) => "staged",
        Err(ConfigClientError::OwnerStagedRelationshipDenied { doc_id, .. }) if doc_id == doc => {
            "denied"
        }
        Err(ConfigClientError::OwnerStagedRelationshipUnmanaged { doc_id, .. })
            if doc_id == doc =>
        {
            "unmanaged"
        }
        Err(ConfigClientError::OwnerStagedTargetRefused { doc_id, .. }) if doc_id == doc => {
            "out_of_kind"
        }
        Err(error) => panic!("unexpected owner-staged result for {doc}: {error:#}"),
    }
}
