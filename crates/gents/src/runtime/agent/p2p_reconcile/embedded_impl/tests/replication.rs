use super::fixtures::{
    collection_id, collection_values, runtime_test_node, signed_p2p_node_without_schemas,
    test_node, wait_for_active_peer, wait_for_peer_info, wait_for_value,
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
    let schema = gents_protocol::schemas::NETWORK_ENROLLMENT_REQUEST.replace(
        "type NetworkEnrollmentRequest @branchable",
        "type NetworkEnrollmentRequest @branchable @policy(id: \"4bb3e26819f6b2d971472ad58838fe45b7f105c55d3d2763a7cb8fa21751e2d5\", resource: \"configuration\")");
    for (node, actor) in [(candidate_node, &candidate), (admin_node, &admin)] {
        node.add_dac_policy_with_counter_as(
            &defra_core::RequestContext::default().with_identity(Some(actor.did().to_owned())),
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
            0,
        )
        .await?;
        node.add_collection(&schema).await?;
    }
    let candidate_local =
        ConfigAccess::local_as(candidate_node.clone(), identity::Did::new(candidate.did())?);
    let admin_local = ConfigAccess::local_as(admin_node.clone(), identity::Did::new(admin.did())?);
    for (access, other) in [
        (&candidate_local, &admin),
        (&admin_local, &candidate),
        (&admin_local, &outsider),
    ] {
        for permission in [NodePermission::DocumentRead, NodePermission::CollectionGet] {
            access
                .grant_node_permission(&identity::Did::new(other.did())?, permission)
                .await?;
        }
    }
    let reader = identity::Did::new(admin.did())?;
    let mutation = format!(
        r#"mutation {{ create_NetworkEnrollmentRequest(input: {{request_id:"probe", candidate_did:"{}", admin_did:"{}", offer_token:"private-offer"}}) {{_docID}} }}"#,
        escape_graphql_string(candidate.did()),
        escape_graphql_string(admin.did())
    );
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
    let candidate_admin = EmbeddedRemoteP2pAdmin::new(candidate_node.clone());
    let receiving_admin = EmbeddedRemoteP2pAdmin::new(admin_node.clone());
    let addresses = wait_for_peer_info(&receiving_admin).await;
    candidate_admin.connect(&addresses).await?;
    wait_for_active_peer(&candidate_admin).await;
    let mut arrivals = crate::subscribe_collections(admin_node, ["NetworkEnrollmentRequest"])?;
    let filters = [(
        "NetworkEnrollmentRequest".to_owned(),
        equality_filter("request_id", "probe"),
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
    candidate_local
        .delete_actor_relationship("NetworkEnrollmentRequest", &doc, "reader", &reader)
        .await?;
    assert_eq!(
        count(&http(candidate_address, &admin)?.execute(&query).await?),
        0
    );
    candidate_node.shutdown().await?;
    admin_node.shutdown().await?;
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
    let test = test_node().await;
    let node = Arc::clone(&test.node);
    let admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&node));
    let collection_name = "P2pReconcileThing".to_string();

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
    let local_test = test_node().await;
    let remote_test = test_node().await;
    let local = Arc::clone(&local_test.node);
    let remote = Arc::clone(&remote_test.node);
    let local_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&local));
    let remote_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&remote));
    let remote_addresses = wait_for_peer_info(&remote_admin).await;
    let collection_name = "P2pReconcileThing".to_string();
    let expected_collection_id = collection_id(&local, "P2pReconcileThing");

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
async fn embedded_filtered_replicator_replays_existing_matching_documents() {
    let sender_test = runtime_test_node().await;
    let receiver_test = runtime_test_node().await;
    let sender = Arc::clone(&sender_test.node);
    let receiver = Arc::clone(&receiver_test.node);
    let sender_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&sender));
    let receiver_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&receiver));
    let sender_addresses = wait_for_peer_info(&sender_admin).await;
    let receiver_addresses = wait_for_peer_info(&receiver_admin).await;

    seed_agent_request(&sender, "child-match", "did:key:coord").await;
    seed_agent_request(&sender, "child-other", "did:key:other").await;
    seed_agent_tool_call(&sender, "bridge-match", "did:key:host").await;
    seed_agent_tool_call(&sender, "bridge-other", "did:key:other").await;

    sender_admin
        .connect(&receiver_addresses)
        .await
        .expect("connect sender to receiver");
    wait_for_active_peer(&sender_admin).await;
    wait_for_active_peer(&receiver_admin).await;

    let collections = vec!["AgentRequest".to_string(), "AgentToolCall".to_string()];
    receiver_admin
        .add_replicator(&sender_addresses, &collections, &PairingFilters::default())
        .await
        .expect("authorize sender as receiver-side replicator");
    assert!(
        sender_admin
            .list_p2p_collections()
            .await
            .expect("list sender P2P subscriptions")
            .is_empty(),
        "filtered Push sender must not subscribe to whole collections"
    );
    assert!(
        receiver_admin
            .list_p2p_collections()
            .await
            .expect("list receiver P2P subscriptions")
            .is_empty(),
        "filtered Push receiver must not subscribe to whole collections"
    );

    // Give any independently configured route a chance to replay before
    // installing the sender filter.
    tokio::time::sleep(Duration::from_millis(250)).await;
    let mut filters = PairingFilters::new();
    filters.insert(
        "AgentRequest".to_string(),
        equality_filter("requester_did", "did:key:coord"),
    );
    filters.insert(
        "AgentToolCall".to_string(),
        equality_filter("requester_did", "did:key:host"),
    );
    sender_admin
        .add_replicator(&receiver_addresses, &collections, &filters)
        .await
        .expect("install filtered sender to receiver replicator");

    wait_for_value(&receiver, "AgentRequest", "request_id", "child-match").await;
    wait_for_value(&receiver, "AgentToolCall", "tool_call_id", "bridge-match").await;

    let request_ids = collection_values(&receiver, "AgentRequest", "request_id").await;
    let tool_call_ids = collection_values(&receiver, "AgentToolCall", "tool_call_id").await;
    assert_eq!(
        request_ids,
        BTreeSet::from(["child-match".to_string()]),
        "filtered request replay should not leak non-matching rows"
    );
    assert_eq!(
        tool_call_ids,
        BTreeSet::from(["bridge-match".to_string()]),
        "filtered tool-call replay should not leak non-matching rows"
    );

    sender.shutdown().await.expect("node shutdown");
    receiver.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn selected_delivery_outlives_the_former_bulk_deadline() {
    assert_selected_delivery_outlives_deadline(test_node().await, test_node().await, 0).await;
}

#[tokio::test]
async fn selected_delivery_outlives_the_former_bulk_deadline_with_runtime_schemas() {
    assert_selected_delivery_outlives_deadline(
        runtime_test_node().await,
        runtime_test_node().await,
        1200,
    )
    .await;
}

async fn assert_selected_delivery_outlives_deadline(
    sender_test: super::fixtures::TestNode,
    receiver_test: super::fixtures::TestNode,
    minimum_document_count: usize,
) {
    let sender = &sender_test.node;
    let receiver = &receiver_test.node;
    for node in [sender, receiver] {
        node.add_collection("type PacedDeliveryProbe { value: Int }")
            .await
            .unwrap();
    }
    let pacing = db::merge::ReplayPushConfig::default();
    let former_deadline = Duration::from_secs(10);
    let document_count = (pacing.per_peer_rate_limit_burst as usize
        + (pacing.per_peer_rate_limit_rate * (former_deadline.as_secs_f64() + 2.0)).ceil()
            as usize)
        .max(minimum_document_count);
    let inputs = (0..document_count)
        .map(|value| format!("{{value: {value}}}"))
        .collect::<Vec<_>>()
        .join(",");
    let result = crate::store::config_client::ConfigAccess::write_local(
        sender,
        "test.paced_delivery.seed",
        &format!("mutation {{ create_PacedDeliveryProbe(input: [{inputs}]) {{ _docID }} }}"),
    )
    .await
    .unwrap();
    let documents = crate::store::graphql::mutation_documents(&result, "create_PacedDeliveryProbe")
        .unwrap()
        .expect("seeded selected documents")
        .iter()
        .map(|row| P2pDocumentRequest {
            collection: "PacedDeliveryProbe".into(),
            doc_id: row["_docID"].as_str().unwrap().into(),
        })
        .collect::<Vec<_>>();
    assert_eq!(documents.len(), document_count);
    let sender_admin = EmbeddedRemoteP2pAdmin::new(sender.clone());
    let receiver_admin = EmbeddedRemoteP2pAdmin::new(receiver.clone());
    let sender_addresses = wait_for_peer_info(&sender_admin).await;
    let receiver_addresses = wait_for_peer_info(&receiver_admin).await;
    sender_admin.connect(&receiver_addresses).await.unwrap();
    wait_for_active_peer(&sender_admin).await;
    wait_for_active_peer(&receiver_admin).await;
    receiver_admin
        .add_replicator(
            &sender_addresses,
            &["PacedDeliveryProbe".into()],
            &PairingFilters::default(),
        )
        .await
        .unwrap();
    let peer = receiver.p2p().unwrap().local_peer_id().await.unwrap();
    let expected = documents
        .iter()
        .map(|document| document.doc_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut updates = crate::subscribe_collections(receiver, ["PacedDeliveryProbe"]).unwrap();
    let started = tokio::time::Instant::now();
    push_documents_to_peer(sender, &peer, documents)
        .await
        .unwrap();
    assert!(
        started.elapsed() > former_deadline,
        "native pacing-derived workload must exercise the former deadline"
    );
    // ACKs admit heads. This healthy loopback fixture requires fresh merge progress
    // within five seconds and exact convergence within one native recovery window.
    let completion_deadline =
        tokio::time::Instant::now() + Duration::from_secs(p2p::PENDING_RECOVERY_WORST_CASE_SECS);
    let access = crate::store::config_client::ConfigAccess::local(receiver.clone());
    let query = format!(
        "{{ PacedDeliveryProbe(limit: {}) {{ _docID }} }}",
        document_count + 1
    );
    let read = || async {
        let received = access.execute(&query).await.unwrap();
        let observed = received["data"]["PacedDeliveryProbe"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["_docID"].as_str().unwrap().to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            observed.is_subset(&expected),
            "selected replay merged {} unexpected documents",
            observed.difference(&expected).count()
        );
        observed
    };
    let merged = tokio::time::timeout_at(completion_deadline, async {
        let mut seen = std::collections::BTreeSet::new();
        loop {
            let progress = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let observed = read().await;
                    if observed == expected || observed.difference(&seen).next().is_some() {
                        break observed;
                    }
                    updates
                        .recv()
                        .await
                        .expect("receiver collection subscription remains open");
                }
            })
            .await;
            let observed = match progress {
                Ok(observed) => observed,
                Err(_) => {
                    let observed = read().await;
                    if observed != expected && observed.difference(&seen).next().is_none() {
                        let status = receiver.p2p().unwrap().sync_status().await.unwrap();
                        panic!("receiver made no fresh merge progress for five seconds: expected={document_count} observed={} previously_seen={} status={status}", observed.len(), seen.len());
                    }
                    observed
                }
            };
            if observed == expected {
                break;
            }
            seen.extend(observed);
        }
    })
    .await;
    if merged.is_err() {
        let observed = read().await;
        if observed != expected {
            let status = receiver.p2p().unwrap().sync_status().await.unwrap();
            panic!("selected replay did not converge within the native recovery window: expected={document_count} observed={} status={status}", observed.len());
        }
    }
    sender.shutdown().await.expect("node shutdown");
    receiver.shutdown().await.expect("node shutdown");
}
