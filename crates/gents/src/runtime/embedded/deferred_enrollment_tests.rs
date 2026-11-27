use gents_store::node_scope::ContextCalls as _;
use std::sync::Arc;

use anyhow::{Context, Result};

use gents_store::config_client::node_deferred_operation::*;
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint, NodePermission};
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;

#[tokio::test]
async fn deferred_enrollment_documents_remain_private_then_follow_the_node() -> Result<()> {
    let mut observed = std::collections::BTreeMap::new();
    let access_checks =
        gents_lean_vocab::families::household_operations_deferred_enrollment::cases();
    let mut observed_access = vec![Vec::new(); access_checks.len()];
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
    node.add_collection(
        &gents_protocol::schemas::HomePolicyIds::node_store().render(gents_protocol::schemas::NODE),
    )
    .await?;
    node.add_collection(
        &gents_protocol::schemas::HomePolicyIds::node_store()
            .render(gents_protocol::schemas::NODE_READINESS),
    )
    .await?;
    for schema in [
        gents_protocol::schemas::ENROLLMENT_OPERATOR_NONCE,
        gents_protocol::schemas::ENROLLMENT_RENEWAL_POLICY,
        gents_protocol::schemas::NETWORK_ADMIN_PIN,
        gents_protocol::schemas::OWED_DELIVERY,
        gents_protocol::schemas::PEER_PAIRING_APPLIED,
        gents_protocol::schemas::PEER_PAIRING_DESIRED,
    ] {
        node.add_collection(&gents_protocol::schemas::HomePolicyIds::node_store().render(schema))
            .await?;
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
        observe(
            &mut observed,
            "before-node",
            deferred_state(&runtime_access, &user_access, collection, &doc, None).await?,
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
    let node_mutation = format!(
        r#"mutation {{ create_Node(input: {{node_did: "{}"}}) {{_docID}} }}"#,
        escape_graphql_string(runtime.did()),
    );
    let unregistered =
        ConfigAccess::write_local(&node, "test.deferred.unregistered_node", &node_mutation).await?;
    let unregistered = gents_protocol::graphql::extract_mutation_doc_id(&unregistered, "Node")?;
    node.document_acp()
        .unregister_doc_object(
            gents_protocol::schemas::HomePolicyIds::node_store()
                .id(gents_protocol::schemas::BuiltInPolicy::Household),
            "nodes",
            &unregistered,
        )
        .await?;
    let owners = runtime_access
        .transact_readonly("test.deferred.unregistered_owner", |txn| {
            let ids = std::slice::from_ref(&unregistered);
            Box::pin(async move { txn.document_owners("Node", ids).await })
        })
        .await?;
    assert!(owners.len() == 1 && owners[0].visible && owners[0].owner.is_none());
    for (collection, doc) in &created {
        let _refused = runtime_access
            .transact_write("test.deferred.unregistered_node_link", |txn| {
                let actor = &actor;
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        maybe_link_deferred_operation_in_txn(txn, collection, doc, actor).await?,
                    )
                })
            })
            .await;
        observe(
            &mut observed,
            "unregistered-node",
            deferred_state(
                &runtime_access,
                &user_access,
                collection,
                doc,
                Some(unregistered.as_str()),
            )
            .await?,
        );
    }
    ConfigAccess::write_local(
        &node,
        "test.deferred.remove_unregistered_node",
        &format!(
            r#"mutation {{ delete_Node(docID: "{}") {{_docID}} }}"#,
            escape_graphql_string(&unregistered)
        ),
    )
    .await?;
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
        .write("test.deferred.node", &node_mutation)
        .await?;
    let parent = gents_protocol::graphql::extract_mutation_doc_id(&parent, "Node")?;
    user_access
        .add_actor_relationship("Node", &parent, "runtime_reader", &actor)
        .await?;
    assert!(recover_deferred_node_operations(&node, outsider.did())
        .await
        .is_err());
    for (collection, doc) in &created {
        observe(
            &mut observed,
            "foreign-runtime",
            deferred_state(
                &runtime_access,
                &user_access,
                collection,
                doc,
                Some(parent.as_str()),
            )
            .await?,
        );
    }
    let (aborted_collection, aborted_doc) = &created[0];
    let (aborted, mutations) =
        gents_store::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
            Some(1),
            runtime_access.transact_write("test.deferred.failed_commit", |txn| {
                let actor = &actor;
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        maybe_link_deferred_operation_in_txn(
                            txn,
                            aborted_collection,
                            aborted_doc,
                            actor,
                        )
                        .await?,
                    )
                })
            }),
        )
        .await;
    assert!(aborted.is_err() && mutations == 1, "{aborted:?}");
    observe(
        &mut observed,
        "failed-commit",
        deferred_state(
            &runtime_access,
            &user_access,
            aborted_collection,
            aborted_doc,
            Some(parent.as_str()),
        )
        .await?,
    );
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
        observe(
            &mut observed,
            "success",
            deferred_state(
                &runtime_access,
                &user_access,
                collection,
                doc,
                Some(parent.as_str()),
            )
            .await?,
        );
        let mutation = format!(
            r#"mutation {{ update_{collection}(docID: "{}", input: {{node_doc_id: "forged"}}) {{_docID}} }}"#,
            gents_store::graphql::escape_graphql_string(doc)
        );
        for (index, check) in access_checks.iter().enumerate() {
            let check = &check.input;
            anyhow::ensure!(check.phase == "attached", "unmodeled phase {check:?}");
            let actor = match check.role.as_str() {
                "user" => &user,
                "agent" => &agent,
                "sibling" => &sibling,
                "outsider" => &outsider,
                other => anyhow::bail!("unmodeled operations actor {other}"),
            };
            let allowed = match check.permission.as_str() {
                "update" => access(actor)?
                    .write("test.deferred.denied", &mutation)
                    .await
                    .is_ok_and(|response| {
                        gents_protocol::graphql::extract_mutation_doc_id(&response, collection)
                            .is_ok()
                    }),
                "read" => {
                    let response = access(actor)?.execute(&query).await?;
                    !gents_protocol::graphql::graphql_rows_from_response(&response, collection)
                        .is_empty()
                }
                other => anyhow::bail!("unmodeled operations permission {other}"),
            };
            observed_access[index].push(allowed);
        }
        runtime_access
            .delete_resource_relationship(collection, doc, "parent", "nodes", &parent)
            .await?;
    }
    for (check, observed) in access_checks.iter().zip(&observed_access) {
        anyhow::ensure!(
            observed
                .iter()
                .all(|allowed| Some(allowed) == observed.first()),
            "deferred documents disagree for {:?}: {observed:?}",
            check.input
        );
        check.check(*observed.first().context("no deferred document observed")?);
    }
    recover_deferred_node_operations(&node, runtime.did()).await?;
    for (collection, doc) in &created {
        observe(
            &mut observed,
            "revoked-after-publication",
            deferred_state(
                &runtime_access,
                &user_access,
                collection,
                doc,
                Some(parent.as_str()),
            )
            .await?,
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
    let runtime_access = ConfigAccess::local_as(reopened.clone(), actor.clone());
    for (collection, doc) in &created {
        observe(
            &mut observed,
            "revoked-after-publication",
            deferred_state(
                &runtime_access,
                &user_access,
                collection,
                doc,
                Some(parent.as_str()),
            )
            .await?,
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
    observe(
        &mut observed,
        "foreign-owner",
        deferred_state(
            &user_access,
            &runtime_access,
            "NetworkAdminPin",
            &foreign,
            Some(parent.as_str()),
        )
        .await?,
    );
    reopened.shutdown().await?;
    for case in gents_lean_vocab::families::identity_operations_deferred::cases() {
        let (parent, granted, linked) = observed
            .remove(case.input.name.as_str())
            .with_context(|| format!("the deferred scenario never reached {}", case.input.name))?;
        case.check(
            gents_lean_vocab::families::identity_operations_deferred::Deferred {
                parent,
                granted,
                linked,
            },
        );
    }
    assert!(
        observed.is_empty(),
        "unmodeled deferred observations: {observed:?}"
    );
    Ok(())
}

async fn deferred_state(
    owner: &ConfigAccess,
    parent_reader: &ConfigAccess,
    collection: &str,
    doc: &str,
    node: Option<&str>,
) -> Result<(Option<String>, bool, bool)> {
    let query = format!(
        r#"{{ {collection}(filter: {{_docID: {{_eq: "{}"}}}}) {{_docID node_doc_id parent_linked}} }}"#,
        escape_graphql_string(doc)
    );
    let owned = owner.execute(&query).await?;
    let row = &owned["data"][collection][0];
    let parent = row["node_doc_id"].as_str().map(|id| {
        if Some(id) == node {
            "node".to_owned()
        } else {
            id.to_owned()
        }
    });
    let linked = row["parent_linked"]
        .as_bool()
        .context("parent_linked marker")?;
    let read = parent_reader.execute(&query).await?;
    let granted =
        !gents_protocol::graphql::graphql_rows_from_response(&read, collection).is_empty();
    Ok((parent, granted, linked))
}

fn observe(
    observed: &mut std::collections::BTreeMap<&'static str, (Option<String>, bool, bool)>,
    name: &'static str,
    value: (Option<String>, bool, bool),
) {
    if let Some(previous) = observed.insert(name, value.clone()) {
        assert_eq!(
            previous, value,
            "{name}: every observation of one case agrees"
        );
    }
}
