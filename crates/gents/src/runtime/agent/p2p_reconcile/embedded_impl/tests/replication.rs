use super::fixtures::{
    collection_id, collection_values, runtime_test_node, signed_p2p_node_without_schemas,
    test_node, wait_for_active_peer, wait_for_peer_info,
};
use super::seeding::{seed_agent_request, seed_agent_tool_call};
use super::*;

#[tokio::test]
async fn exact_enrollment_push_does_not_itself_propagate_native_reader_grants() -> anyhow::Result<()>
{
    use crate::store::config_client::{ConfigAccess, GraphqlEndpoint, NodePermission};
    use anyhow::Context;
    let model = &crate::lean_vocab_test::lean_contract_snapshot().identity_receiver_grant_cases;
    let expected = model
        .iter()
        .find(|case| case["sender_granted"] == true && case["delivered"] == false)
        .unwrap()["receiver_granted"]
        .as_bool()
        .unwrap();
    let directory = tempfile::tempdir()?;
    let candidate_path = directory.path().join("candidate");
    let admin_path = directory.path().join("admin");
    let candidate = KeyIdentity::load_or_create(&candidate_path, None)?;
    let admin = KeyIdentity::load_or_create(&admin_path, None)?;
    let user = KeyIdentity::load_or_create(directory.path().join("user"), None)?;
    let agent = KeyIdentity::load_or_create(directory.path().join("agent"), None)?;
    let sibling = KeyIdentity::load_or_create(directory.path().join("sibling"), None)?;
    let outsider = KeyIdentity::load_or_create(directory.path().join("outsider"), None)?;
    let candidate_key =
        crate::store::identity::load_file_identity(&candidate_path)?.private_key_bytes();
    let admin_key = crate::store::identity::load_file_identity(&admin_path)?.private_key_bytes();
    let candidate_address = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?;
    let admin_address = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?;
    let candidate_test =
        signed_p2p_node_without_schemas(&candidate, candidate_key, candidate_address).await;
    let admin_test = signed_p2p_node_without_schemas(&admin, admin_key, admin_address).await;
    let candidate_node = &candidate_test.node;
    let admin_node = &admin_test.node;
    let schema = gents_protocol::schemas::NETWORK_ENROLLMENT_REQUEST;
    for (node, actor) in [(candidate_node, &candidate), (admin_node, &admin)] {
        node.add_dac_policy_with_counter_as(
            &defra_core::RequestContext::default().with_identity(Some(actor.did().to_owned())),
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
            0,
        )
        .await?;
        node.add_collection(schema).await?;
    }
    for node in [candidate_node, admin_node] {
        node.add_collection(gents_protocol::schemas::AGENT_SESSION).await?;
    }
    admin_node
        .add_collection(gents_protocol::schemas::NODE)
        .await?;
    let candidate_local =
        ConfigAccess::local_as(candidate_node.clone(), identity::Did::new(candidate.did())?);
    let admin_local = ConfigAccess::local_as(admin_node.clone(), identity::Did::new(admin.did())?);
    for (access, other) in [
        (&candidate_local, &admin),
        (&admin_local, &candidate),
        (&admin_local, &user),
        (&admin_local, &agent),
        (&admin_local, &sibling),
        (&admin_local, &outsider),
    ] {
        for permission in [NodePermission::DocumentRead, NodePermission::CollectionGet] {
            access
                .grant_node_permission(&identity::Did::new(other.did())?, permission)
                .await?;
        }
    }
    for actor in [&user, &candidate] {
        admin_local
            .grant_node_permission(
                &identity::Did::new(actor.did())?,
                NodePermission::DocumentUpdate,
            )
            .await?;
    }
    let receiver_model =
        &crate::lean_vocab_test::lean_contract_snapshot().identity_replica_grant_cases;
    let case = |name: &str| {
        receiver_model
            .iter()
            .find(|case| case["name"] == name)
            .with_context(|| format!("missing replica case {name}"))
    };
    let reader = identity::Did::new(admin.did())?;
    let issuer = crate::runtime::agent::p2p_reconcile::EnrollmentOfferIssuer::new(
        Arc::new(KeyIdentity::load_or_create(&admin_path, None)?),
        crate::store::identity::node_p2p(admin_node).context("admin P2P unavailable")?,
        "transport-test".into(),
        admin.did().to_owned(),
        "client".into(),
    );
    let candidate_admin = EmbeddedRemoteP2pAdmin::new(candidate_node.clone());
    let receiving_admin = EmbeddedRemoteP2pAdmin::new(admin_node.clone());
    let _ = wait_for_peer_info(&candidate_admin).await;
    let addresses = wait_for_peer_info(&receiving_admin).await;
    let offer = issuer.mint().await?;
    let candidate_p2p =
        crate::store::identity::node_p2p(candidate_node).context("candidate P2P unavailable")?;
    let candidate_peer = candidate_p2p
        .local_peer_id()
        .await
        .map_err(anyhow::Error::msg)?;
    let candidate_ticket =
        crate::runtime::agent::p2p_reconcile::enrollment_candidate::local_enrollment_ticket(
            &candidate_p2p,
            &candidate_peer,
        )
        .await?;
    let request =
        crate::runtime::agent::p2p_reconcile::enrollment_candidate::sign_enrollment_request(
            &candidate,
            &offer.offer,
            &offer.token,
            candidate_peer.clone(),
            candidate_ticket.clone(),
        )
        .await?;
    anyhow::ensure!(
        candidate
            .verify(
                candidate.did(),
                &request.signing_payload(),
                &request.candidate_sig
            )
            .await?,
        "candidate request signature must be valid"
    );
    anyhow::ensure!(
        admin
            .verify(
                admin.did(),
                &offer.offer.signing_payload(),
                &offer.offer.admin_sig
            )
            .await?,
        "admin offer signature must be valid"
    );
    let input =
        crate::runtime::agent::p2p_reconcile::enrollment_candidate::enrollment_request_input(
            &request,
        );
    let mutation =
        format!("mutation {{ create_NetworkEnrollmentRequest(input: {input}) {{_docID}} }}");
    let doc = candidate_local
        .transact_write("test.enrollment_native.create", |txn| {
            let mutation = &mutation;
            let reader = &reader;
            Box::pin(async move {
                let response = txn.execute(mutation).await?;
                let doc = gents_protocol::graphql::extract_mutation_doc_id(
                    &response,
                    "NetworkEnrollmentRequest",
                )?;
                txn.add_actor_relationship("NetworkEnrollmentRequest", &doc, "reader", reader)
                    .await?;
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await?;
    let http = |address, actor: &KeyIdentity| -> anyhow::Result<ConfigAccess> {
        Ok(ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), actor.did())
                .with_client_signing_capability(actor.client_signing_capability()?)?,
        ))
    };
    let query = format!(
        r#"{{ NetworkEnrollmentRequest(filter: {{_docID: {{_eq:"{}"}}}}) {{_docID offer_token}} }}"#,
        escape_graphql_string(&doc)
    );
    let count = |value: &serde_json::Value| {
        gents_protocol::graphql::graphql_rows_from_response(value, "NetworkEnrollmentRequest").len()
    };
    assert_eq!(
        count(&http(candidate_address, &admin)?.execute(&query).await?),
        1
    );
    candidate_admin.connect(&addresses).await?;
    wait_for_active_peer(&candidate_admin).await;
    let mut arrivals = crate::subscribe_collections(admin_node, ["NetworkEnrollmentRequest"])?;
    let filters = [(
        "NetworkEnrollmentRequest".to_owned(),
        equality_filter("request_id", &request.request_id),
    )]
    .into_iter()
    .collect();
    candidate_admin
        .add_replicator(
            &addresses,
            &["NetworkEnrollmentRequest".to_owned()],
            &filters,
        )
        .await?;
    let peer = admin_node
        .p2p()
        .context("admin has no P2P")?
        .local_peer_id()
        .await
        .map_err(anyhow::Error::msg)?;
    push_documents_to_peer(
        candidate_node,
        &peer,
        vec![P2pDocumentRequest {
            collection: "NetworkEnrollmentRequest".into(),
            doc_id: doc.clone(),
        }],
    )
    .await?;
    tokio::time::timeout(Duration::from_secs(20), arrivals.recv())
        .await?
        .context("replicated enrollment notification closed")?;
    assert_eq!(
        count(&http(admin_address, &candidate)?.execute(&query).await?),
        1,
        "candidate-owned genesis must arrive and be visible to its actual author"
    );
    assert_eq!(
        count(&http(admin_address, &admin)?.execute(&query).await?) != 0,
        expected,
        "source reader tuple must not be assumed on receiver"
    );
    assert_eq!(
        count(&http(admin_address, &outsider)?.execute(&query).await?),
        0
    );
    assert_eq!(
        count(&admin_local.execute(&query).await?) != 0,
        expected,
        "embedded receiver runtime must not inherit the origin's reader tuple"
    );
    let receiver_author =
        ConfigAccess::local_as(admin_node.clone(), identity::Did::new(candidate.did())?);
    assert_eq!(
        count(&receiver_author.execute(&query).await?),
        1,
        "signed genesis must retain its actual creator on the receiver"
    );
    {
        let txn = admin_node.database().new_txn(false).await?;
        let view = txn.datastore()?;
        crate::store::config_client::transport_publication::project_transport_document(
            &view,
            admin_node.document_acp().as_ref(),
            &reader,
            "NetworkEnrollmentRequest",
            &doc,
        )
        .await?;
        drop(view);
        txn.commit().await?;
    }
    assert_eq!(
        count(&admin_local.execute(&query).await?) != 0,
        case("pending")?["runtime_read"] == true
    );
    assert_eq!(
        count(&http(admin_address, &user)?.execute(&query).await?) != 0,
        case("pending")?["user_read"] == true
    );
    let node = http(admin_address, &user)?
        .write(
            "test.transport.create_node",
            &format!(
                r#"mutation {{ create_Node(input: {{node_did: "{}"}}) {{_docID}} }}"#,
                escape_graphql_string(admin.did())
            ),
        )
        .await?;
    let parent = gents_protocol::graphql::extract_mutation_doc_id(&node, "Node")?;
    {
        let txn = admin_node.database().new_txn(false).await?;
        let view = txn.datastore()?;
        crate::store::config_client::transport_publication::reconcile_transport_documents(
            &view,
            admin_node.document_acp().as_ref(),
            &reader,
        )
        .await?;
        drop(view);
        txn.commit().await?;
    }
    assert_eq!(
        count(&http(admin_address, &user)?.execute(&query).await?) != 0,
        case("linked")?["user_read"] == true
    );
    for actor in [&agent, &sibling, &outsider] {
        assert_eq!(
            count(&http(admin_address, actor)?.execute(&query).await?),
            0
        );
    }
    let denied = http(admin_address, &user)?.write(
        "test.transport.denied_write",
        &format!(r#"mutation {{ update_NetworkEnrollmentRequest(docID: "{}", input: {{offer_token:"forged"}}) {{_docID}} }}"#,
            escape_graphql_string(&doc)),
    ).await;
    assert!(
        denied.is_err()
            || denied.as_ref().is_ok_and(|response| {
                gents_protocol::graphql::graphql_rows_from_response(
                    response,
                    "update_NetworkEnrollmentRequest",
                )
                .is_empty()
            })
    );
    assert_eq!(
        admin_local.execute(&query).await?["data"]["NetworkEnrollmentRequest"][0]["offer_token"],
        offer.token
    );
    {
        let txn = admin_node.database().new_txn(false).await?;
        let view = txn.datastore()?;
        admin_node
            .document_acp()
            .change_node_admin_relationship(
                view.sibling(storage::namespace::Namespace::Acpstore),
                &reader,
                acp::Subject::entity_set("nodes", parent, ""),
                gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
                "enrollment_records",
                &doc,
                "parent",
                true,
            )
            .await?;
        drop(view);
        txn.commit().await?;
    }
    {
        let txn = admin_node.database().new_txn(false).await?;
        let view = txn.datastore()?;
        crate::store::config_client::transport_publication::reconcile_transport_documents(
            &view,
            admin_node.document_acp().as_ref(),
            &reader,
        )
        .await?;
        drop(view);
        txn.commit().await?;
    }
    assert_eq!(
        count(&http(admin_address, &user)?.execute(&query).await?) != 0,
        case("after-repair")?["user_read"] == true
    );
    let session = http(admin_address, &candidate)?.write(
        "test.transport.private_session",
        &format!(r#"mutation {{ create_AgentSession(input:{{session_id:"private",node_did:"{}",agent_did:"{}"}}) {{_docID}} }}"#,
            escape_graphql_string(candidate.did()), escape_graphql_string(agent.did())),
    ).await?;
    let session = gents_protocol::graphql::extract_mutation_doc_id(&session, "AgentSession")?;
    let session_query = format!(
        r#"{{ AgentSession(filter: {{_docID: {{_eq: "{}"}}}}) {{_docID}} }}"#,
        escape_graphql_string(&session)
    );
    assert!(gents_protocol::graphql::graphql_rows_from_response(
        &admin_local.execute(&session_query).await?,
        "AgentSession"
    )
    .is_empty());
    assert!(gents_protocol::graphql::graphql_rows_from_response(
        &http(admin_address, &outsider)?
            .execute(&session_query)
            .await?,
        "AgentSession"
    )
    .is_empty());

    let public_mutation = format!(
        r#"mutation {{ create_PeerRegistry(input:{{peer_id:"{}",node_did:"{}",addresses:["{}"]}}) {{_docID}} }}"#,
        escape_graphql_string(&candidate_peer),
        escape_graphql_string(candidate.did()),
        escape_graphql_string(&candidate_ticket),
    );
    let public_doc = candidate_local
        .transact_write("test.transport.public_registry", |txn| {
            let public_mutation = &public_mutation;
            Box::pin(async move {
                let response = txn.execute(public_mutation).await?;
                let doc =
                    gents_protocol::graphql::extract_mutation_doc_id(&response, "PeerRegistry")?;
                txn.add_actor_relationship(
                    "PeerRegistry",
                    &doc,
                    "reader",
                    &identity::Did::wildcard(),
                )
                .await?;
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await?;
    let mut public_arrivals = crate::subscribe_collections(admin_node, ["PeerRegistry"])?;
    candidate_admin
        .add_replicator(
            &addresses,
            &["PeerRegistry".to_owned()],
            &Default::default(),
        )
        .await?;
    push_documents_to_peer(
        candidate_node,
        &peer,
        vec![P2pDocumentRequest {
            collection: "PeerRegistry".into(),
            doc_id: public_doc.clone(),
        }],
    )
    .await?;
    tokio::time::timeout(Duration::from_secs(20), public_arrivals.recv())
        .await?
        .context("replicated public registry notification closed")?;
    {
        let txn = admin_node.database().new_txn(false).await?;
        let view = txn.datastore()?;
        crate::store::config_client::transport_publication::project_transport_document(
            &view,
            admin_node.document_acp().as_ref(),
            &reader,
            "PeerRegistry",
            &public_doc,
        )
        .await?;
        drop(view);
        txn.commit().await?;
    }
    let public_query = format!(
        r#"{{ PeerRegistry(filter: {{_docID: {{_eq: "{}"}}}}) {{peer_id addresses}} }}"#,
        escape_graphql_string(&public_doc)
    );
    let public = http(admin_address, &outsider)?
        .execute(&public_query)
        .await?;
    let public_expected = crate::lean_vocab_test::lean_contract_snapshot()
        .identity_discovery_policy_cases
        .iter()
        .find(|case| case["name"] == "outsider-read")
        .context("missing public discovery case")?["allowed"]
        == true;
    assert_eq!(
        !gents_protocol::graphql::graphql_rows_from_response(&public, "PeerRegistry").is_empty(),
        public_expected
    );
    assert_eq!(public["data"]["PeerRegistry"][0]["peer_id"], candidate_peer);
    assert_eq!(
        count(&http(admin_address, &outsider)?.execute(&query).await?) != 0,
        case("after-repair")?["outsider_read"] == true
    );
    assert_eq!(
        !gents_protocol::graphql::graphql_rows_from_response(
            &admin_local.execute(&session_query).await?,
            "AgentSession"
        )
        .is_empty(),
        case("after-repair")?["app_read"] == true
    );

    let runtime_registry = admin_local.write(
        "test.transport.runtime_registry",
        &format!(r#"mutation {{ create_PeerRegistry(input: {{peer_id:"runtime-owned",node_did:"{}",addresses:["local"]}}) {{_docID}} }}"#, escape_graphql_string(admin.did())),
    ).await?;
    let runtime_registry =
        gents_protocol::graphql::extract_mutation_doc_id(&runtime_registry, "PeerRegistry")?;
    {
        let txn = admin_node.database().new_txn(false).await?;
        let view = txn.datastore()?;
        crate::store::config_client::transport_publication::project_transport_document(
            &view,
            admin_node.document_acp().as_ref(),
            &reader,
            "PeerRegistry",
            &runtime_registry,
        )
        .await?;
        drop(view);
        txn.commit().await?;
    }
    let registry_mutation = |id: &str| {
        format!(
            r#"mutation {{ update_PeerRegistry(docID:"{}",input:{{status:"updated"}}) {{_docID}} }}"#,
            escape_graphql_string(id)
        )
    };
    let discovery_cases =
        &crate::lean_vocab_test::lean_contract_snapshot().identity_discovery_policy_cases;
    let discovery_expected = |name: &str| {
        discovery_cases
            .iter()
            .find(|case| case["name"] == name)
            .with_context(|| format!("missing discovery case {name}"))
    };
    let user_write = http(admin_address, &user)?
        .write(
            "test.transport.user_registry",
            &registry_mutation(&runtime_registry),
        )
        .await;
    assert_eq!(
        user_write
            .as_ref()
            .is_ok_and(|value| gents_protocol::graphql::extract_mutation_doc_id(
                value,
                "PeerRegistry"
            )
            .is_ok()),
        discovery_expected("local-user-write")?["allowed"] == true
    );

    let user_registry = http(admin_address, &user)?.write(
        "test.transport.user_registry_create",
        &format!(r#"mutation {{ create_PeerRegistry(input: {{peer_id:"user-owned",node_did:"{}",addresses:["local"]}}) {{_docID}} }}"#, escape_graphql_string(admin.did())),
    ).await?;
    let user_registry =
        gents_protocol::graphql::extract_mutation_doc_id(&user_registry, "PeerRegistry")?;
    {
        let txn = admin_node.database().new_txn(false).await?;
        let view = txn.datastore()?;
        crate::store::config_client::transport_publication::project_transport_document(
            &view,
            admin_node.document_acp().as_ref(),
            &reader,
            "PeerRegistry",
            &user_registry,
        )
        .await?;
        drop(view);
        txn.commit().await?;
    }
    let writer_cases =
        &crate::lean_vocab_test::lean_contract_snapshot().identity_local_registry_writer_cases;
    let modeled_runtime = writer_cases
        .iter()
        .find(|case| case["name"] == "user-owner")
        .context("missing user-owned registry writer case")?["writer"]
        == "runtime";
    let runtime_write = admin_local
        .write(
            "test.transport.runtime_registry_update",
            &registry_mutation(&user_registry),
        )
        .await;
    assert_eq!(
        runtime_write
            .as_ref()
            .is_ok_and(|value| gents_protocol::graphql::extract_mutation_doc_id(
                value,
                "PeerRegistry"
            )
            .is_ok()),
        modeled_runtime
    );
    let foreign_write = http(admin_address, &user)?
        .write(
            "test.transport.foreign_registry_update",
            &registry_mutation(&public_doc),
        )
        .await;
    let modeled_foreign = writer_cases
        .iter()
        .find(|case| case["name"] == "foreign-owner")
        .context("missing foreign registry writer case")?["writer"]
        .is_string();
    assert_eq!(
        foreign_write.as_ref().is_ok_and(|value| {
            gents_protocol::graphql::extract_mutation_doc_id(value, "PeerRegistry").is_ok()
        }),
        modeled_foreign
    );
    {
        let txn = admin_node.database().new_txn(false).await?;
        let view = txn.datastore()?;
        admin_node
            .document_acp()
            .change_node_admin_relationship(
                view.sibling(storage::namespace::Namespace::Acpstore),
                &reader,
                acp::Subject::entity(identity::Did::new(user.did())?),
                gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
                "discovery",
                &runtime_registry,
                "writer",
                true,
            )
            .await?;
        crate::store::config_client::transport_publication::project_transport_document(
            &view,
            admin_node.document_acp().as_ref(),
            &reader,
            "PeerRegistry",
            &runtime_registry,
        )
        .await?;
        drop(view);
        txn.commit().await?;
    }
    let revoked_write = http(admin_address, &user)?
        .write(
            "test.transport.revoked_registry_update",
            &registry_mutation(&runtime_registry),
        )
        .await;
    assert_eq!(
        revoked_write
            .as_ref()
            .is_ok_and(|value| gents_protocol::graphql::extract_mutation_doc_id(
                value,
                "PeerRegistry"
            )
            .is_ok()),
        discovery_expected("revoked-user-write")?["allowed"] == true
    );

    candidate_local
        .delete_actor_relationship("NetworkEnrollmentRequest", &doc, "reader", &reader)
        .await?;
    assert_eq!(
        count(&http(candidate_address, &admin)?.execute(&query).await?),
        0
    );
    candidate_node.shutdown().await?;
    admin_node.shutdown().await?;
    let reopened_address = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?;
    let reopened_key = crate::store::identity::load_file_identity(&admin_path)?.private_key_bytes();
    let reopened = Arc::new(
        EmbeddedNode::builder()
            .data_path(admin_test._tempdir.path())
            .with_node_identity_did(admin.did())
            .with_signing_key(defra_node::SigningKey::Ed25519(reopened_key))
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_addr(reopened_address))
            .build()
            .await?,
    );
    {
        let txn = reopened.database().new_txn(false).await?;
        let view = txn.datastore()?;
        crate::store::config_client::transport_publication::reconcile_transport_documents(
            &view,
            reopened.document_acp().as_ref(),
            &reader,
        )
        .await?;
        drop(view);
        txn.commit().await?;
    }
    let reopened_local = ConfigAccess::local_as(reopened.clone(), reader.clone());
    assert_eq!(
        count(&reopened_local.execute(&query).await?) != 0,
        case("after-repair")?["runtime_read"] == true
    );
    assert_eq!(
        count(&http(reopened_address, &user)?.execute(&query).await?) != 0,
        case("after-repair")?["user_read"] == true
    );
    let reopened_write = http(reopened_address, &user)?
        .write(
            "test.transport.revoked_registry_after_restart",
            &registry_mutation(&runtime_registry),
        )
        .await;
    assert_eq!(
        reopened_write.as_ref().is_ok_and(|value| {
            gents_protocol::graphql::extract_mutation_doc_id(value, "PeerRegistry").is_ok()
        }),
        discovery_expected("revoked-user-write")?["allowed"] == true
    );
    assert_eq!(
        !gents_protocol::graphql::graphql_rows_from_response(
            &reopened_local.execute(&session_query).await?,
            "AgentSession"
        )
        .is_empty(),
        case("after-repair")?["app_read"] == true
    );
    reopened.shutdown().await?;
    Ok(())
}

#[test]
fn adapter_preserves_conjunctive_filters_as_conditions() {
    let filters = [(
        "AgentRequest".to_string(),
        combine_filters(
            equality_filter("requester_did", "did:key:phone"),
            equality_filter("lifecycle_state", "pending"),
        ),
    )]
    .into_iter()
    .collect();

    let converted = to_replication_filters(&filters).expect("supported filters");
    assert_eq!(
        converted["AgentRequest"].conditions.as_ref(),
        serde_json::json!({
            "_and": [
                { "requester_did": { "_eq": "did:key:phone" } },
                { "lifecycle_state": { "_eq": "pending" } }
            ]
        })
        .as_object()
    );
}

#[tokio::test]
async fn collection_name_resolution_accepts_names_and_ids() {
    let test = test_node().await;
    let definition = test
        .node
        .get_collection("P2pReconcileThing")
        .expect("collection lookup")
        .expect("observed fixture schema");
    let admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&test.node));

    assert_eq!(
        admin
            .resolve_collection_name("P2pReconcileThing")
            .await
            .expect("resolve name token"),
        Some("P2pReconcileThing".to_string())
    );
    assert_eq!(
        admin
            .resolve_collection_name(&definition.collection_id)
            .await
            .expect("resolve id token"),
        Some("P2pReconcileThing".to_string())
    );
}

#[tokio::test]
async fn embedded_collections_round_trip() {
    let test = runtime_test_node().await;
    let node = Arc::clone(&test.node);
    let admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&node));
    let collection_name = gents_protocol::schemas::NODE_DIRECTORY_ENTRY_NAME.to_string();

    admin
        .add_p2p_collections(std::slice::from_ref(&collection_name))
        .await
        .expect("add collection");

    let collections = admin
        .list_p2p_collections()
        .await
        .expect("list collections");
    assert_eq!(collections, vec![collection_name]);

    node.shutdown().await.expect("node shutdown");
}

/// Every builtin template's filter fields must be `@immutable` in the
/// target collection's schema, or `add_replicator` rejects the install at
/// pairing time.
#[tokio::test]
async fn embedded_all_builtin_template_filters_pass_replicator_validation() {
    let local_test = runtime_test_node().await;
    let remote_test = runtime_test_node().await;
    let local = Arc::clone(&local_test.node);
    let remote = Arc::clone(&remote_test.node);
    let local_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&local));
    let remote_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&remote));
    let remote_addresses = wait_for_peer_info(&remote_admin).await;
    local_admin
        .connect(&remote_addresses)
        .await
        .expect("connect remote");
    wait_for_active_peer(&local_admin).await;
    wait_for_active_peer(&remote_admin).await;

    for template in crate::runtime::agent::p2p_reconcile::templates::builtin_templates() {
        if template.collections.is_empty() {
            continue;
        }
        let filters = match &template.scope {
            Scope::ClientRoute => {
                crate::runtime::agent::p2p_reconcile::policy::resolve_template_filters(
                    template,
                    crate::runtime::agent::p2p_reconcile::policy::PairingDirection::ClientToRuntime,
                    "did:key:z6MkPeerForFilterValidation",
                    "did:key:z6MkSelfForFilterValidation",
                )
            }
            _ => crate::runtime::agent::p2p_reconcile::templates::scope_filter(
                &template.scope,
                template.collections,
                "did:key:z6MkPeerForFilterValidation",
                "did:key:z6MkSelfForFilterValidation",
            ),
        };
        match &template.scope {
            Scope::Unscoped => assert!(
                filters.is_empty(),
                "unscoped template '{}' unexpectedly has filters",
                template.id
            ),
            Scope::PerCollection(rules) => {
                let filter_collections =
                    filters.keys().map(String::as_str).collect::<BTreeSet<_>>();
                let scoped_collections = rules
                    .iter()
                    .map(|rule| rule.collection)
                    .collect::<BTreeSet<_>>();
                assert_eq!(
                    filter_collections, scoped_collections,
                    "per-collection template '{}' must filter every scoped collection",
                    template.id
                );
            }
            Scope::PeerDid { .. } => {}
            Scope::ClientRoute => assert!(!filters.is_empty()),
        }
        // Register the same collection set production installs: a client
        // route carries a direction-specific set, not the template's.
        let outbound_collections = match &template.scope {
            Scope::ClientRoute => {
                crate::runtime::agent::p2p_reconcile::policy::client_route_collections(
                    crate::runtime::agent::p2p_reconcile::policy::PairingDirection::ClientToRuntime,
                )
            }
            _ => template.collections,
        };
        local_admin
            .add_replicator(
                &remote_addresses,
                &outbound_collections
                    .iter()
                    .map(|c| (*c).to_string())
                    .collect::<Vec<_>>(),
                &filters,
            )
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "template '{}' filter set rejected by add_replicator: {error:#}",
                    template.id
                )
            });
        if matches!(&template.scope, Scope::ClientRoute) {
            let returning = crate::runtime::agent::p2p_reconcile::policy::resolve_template_filters(
                template,
                crate::runtime::agent::p2p_reconcile::policy::PairingDirection::RuntimeToClient,
                "did:key:z6MkPeerForFilterValidation",
                "did:key:z6MkSelfForFilterValidation",
            );
            local_admin
                .add_replicator(
                    &remote_addresses,
                    &crate::runtime::agent::p2p_reconcile::policy::client_route_collections(
                        crate::runtime::agent::p2p_reconcile::policy::PairingDirection::RuntimeToClient,
                    )
                    .iter()
                    .map(|c| (*c).to_string())
                    .collect::<Vec<_>>(),
                    &returning,
                )
                .await
                .expect("client return route filters must pass replicator validation");
        }
    }

    local.shutdown().await.expect("node shutdown");
    remote.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn embedded_replicators_round_trip() {
    let local_test = runtime_test_node().await;
    let remote_test = runtime_test_node().await;
    let local = Arc::clone(&local_test.node);
    let remote = Arc::clone(&remote_test.node);
    let local_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&local));
    let remote_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&remote));
    let remote_addresses = wait_for_peer_info(&remote_admin).await;
    let collection_name = gents_protocol::schemas::NODE_DIRECTORY_ENTRY_NAME.to_string();
    let expected_collection_id =
        collection_id(&local, gents_protocol::schemas::NODE_DIRECTORY_ENTRY_NAME);

    local_admin
        .connect(&remote_addresses)
        .await
        .expect("connect remote");
    local_admin
        .add_replicator(
            &remote_addresses,
            std::slice::from_ref(&collection_name),
            &PairingFilters::default(),
        )
        .await
        .expect("add replicator");

    let replicators = local_admin
        .list_replicators()
        .await
        .expect("list replicators");
    assert!(
        replicators
            .iter()
            .any(|r| r.collections.contains(&expected_collection_id)),
        "replicators={replicators:?}"
    );

    local.shutdown().await.expect("node shutdown");
    remote.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn embedded_filtered_replicator_refuses_runtime_local_request_collections() {
    let sender_test = runtime_test_node().await;
    let receiver_test = runtime_test_node().await;
    let sender = Arc::clone(&sender_test.node);
    let receiver = Arc::clone(&receiver_test.node);
    let sender_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&sender));
    let receiver_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&receiver));
    let sender_addresses = wait_for_peer_info(&sender_admin).await;
    let receiver_addresses = wait_for_peer_info(&receiver_admin).await;

    seed_agent_request(&sender, "child-match", "did:key:coord").await;
    seed_agent_tool_call(&sender, "bridge-match", "did:key:host").await;

    sender_admin
        .connect(&receiver_addresses)
        .await
        .expect("connect sender to receiver");
    wait_for_active_peer(&sender_admin).await;
    wait_for_active_peer(&receiver_admin).await;

    let collections = vec!["AgentRequest".to_string(), "AgentToolCall".to_string()];
    let mut filters = PairingFilters::new();
    filters.insert(
        "AgentRequest".to_string(),
        equality_filter("requester_did", "did:key:coord"),
    );
    filters.insert(
        "AgentToolCall".to_string(),
        equality_filter("requester_did", "did:key:host"),
    );
    let receiver_refused = receiver_admin
        .add_replicator(&sender_addresses, &collections, &filters)
        .await;
    assert!(
        matches!(
            receiver_refused,
            Err(RemoteP2pAdminError::RuntimeLocalCollection(ref name)) if name == "AgentRequest"
        ),
        "{receiver_refused:?}"
    );
    let sender_refused = sender_admin
        .add_replicator(&receiver_addresses, &collections, &filters)
        .await;
    assert!(
        matches!(
            sender_refused,
            Err(RemoteP2pAdminError::RuntimeLocalCollection(ref name)) if name == "AgentRequest"
        ),
        "{sender_refused:?}"
    );
    assert!(sender_admin
        .list_p2p_collections()
        .await
        .expect("list sender P2P subscriptions")
        .is_empty());
    assert!(receiver_admin
        .list_p2p_collections()
        .await
        .expect("list receiver P2P subscriptions")
        .is_empty());
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        collection_values(&receiver, "AgentRequest", "request_id")
            .await
            .is_empty(),
        "runtime-local requests must not replicate over P2P"
    );

    sender.shutdown().await.expect("node shutdown");
    receiver.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn selected_push_refuses_app_collections_before_transport() {
    let sender_test = runtime_test_node().await;
    let receiver_test = runtime_test_node().await;
    let sender = &sender_test.node;
    let receiver = &receiver_test.node;
    for node in [sender, receiver] {
        node.add_collection("type PacedDeliveryProbe { value: Int }")
            .await
            .unwrap();
    }
    let sender_admin = EmbeddedRemoteP2pAdmin::new(sender.clone());
    let receiver_admin = EmbeddedRemoteP2pAdmin::new(receiver.clone());
    let sender_addresses = wait_for_peer_info(&sender_admin).await;
    let receiver_addresses = wait_for_peer_info(&receiver_admin).await;
    sender_admin.connect(&receiver_addresses).await.unwrap();
    wait_for_active_peer(&sender_admin).await;
    wait_for_active_peer(&receiver_admin).await;
    let refused = receiver_admin
        .add_replicator(
            &sender_addresses,
            &["PacedDeliveryProbe".into()],
            &PairingFilters::default(),
        )
        .await;
    assert!(
        matches!(
            refused,
            Err(RemoteP2pAdminError::RuntimeLocalCollection(ref name)) if name == "PacedDeliveryProbe"
        ),
        "{refused:?}"
    );
    let written = crate::store::config_client::ConfigAccess::write_local(
        sender,
        "test.paced_delivery.seed",
        "mutation { create_PacedDeliveryProbe(input: [{value: 1}]) { _docID } }",
    )
    .await
    .unwrap();
    let doc_id = crate::store::graphql::created_doc_id(&written, "PacedDeliveryProbe").unwrap();
    let peer = receiver.p2p().unwrap().local_peer_id().await.unwrap();
    let pushed = push_documents_to_peer(
        sender,
        &peer,
        vec![P2pDocumentRequest {
            collection: "PacedDeliveryProbe".into(),
            doc_id,
        }],
    )
    .await;
    assert!(pushed.is_err(), "app collection reached the P2P transport");
    assert!(sender_admin
        .list_p2p_collections()
        .await
        .unwrap()
        .is_empty());

    sender.shutdown().await.expect("node shutdown");
    receiver.shutdown().await.expect("node shutdown");
}
