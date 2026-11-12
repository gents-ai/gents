#[tokio::test]
async fn applied_state_persist_collapses_duplicates_and_recovers_a_missing_row() {
    use gents_store::identity::NodeIdentity;
    let tempdir = tempfile::tempdir().unwrap();
    let identity = Arc::new(
        gents_store::identity::KeyIdentity::load_or_create(tempdir.path().join("node.key"), None)
            .unwrap(),
    );
    let signing_key = gents_store::identity::load_file_identity(&tempdir.path().join("node.key"))
        .unwrap()
        .private_key_bytes();
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .with_signing_key(defra_node::SigningKey::Ed25519(signing_key))
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    node.add_dac_policy_with_counter_as(
        &defra_core::RequestContext::default().with_identity(Some(identity.did().to_owned())),
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await
    .unwrap();
    node.add_collection(gents_protocol::schemas::NODE)
        .await
        .unwrap();
    let schema = gents_protocol::schemas::PEER_PAIRING_APPLIED.replace(
        "@index(fields: [\"node_did\", \"peer_id\"], unique: true)",
        "",
    );
    node.add_collection(&schema).await.unwrap();
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(identity.did()).unwrap(),
    );
    access
        .write(
            "test.pairing.node",
            &format!(
                "mutation {{ create_Node(input: {{node_did: \"{}\"}}) {{_docID}} }}",
                gents_store::graphql::escape_graphql_string(identity.did())
            ),
        )
        .await
        .unwrap();
    for (address, timestamp) in [
        ("old-a", "2026-08-19T00:00:00Z"),
        ("old-b", "2026-08-19T00:01:00Z"),
    ] {
        let response = access
            .write(
                "test.pairing.seed",
                &format!(
                    r#"mutation {{
                    create_PeerPairingApplied(input: {{
                        peer_id: "peer-a",
                        node_did: "{}",
                        parent_linked: false,
                        replicator_addresses: ["{address}"],
                        created_at: "{timestamp}",
                        updated_at: "{timestamp}"
                    }}) {{ _docID }}
                }}"#,
                    gents_store::graphql::escape_graphql_string(identity.did())
                ),
            )
            .await;
        assert!(response.is_ok(), "seed failed: {response:?}");
    }
    let store = GraphqlPairingStateStore::for_explicit_desired(node.clone(), identity);
    let mut applied = store.load_applied("peer-a").await.expect("load duplicates");
    applied.state = PairingApplied {
        replicator_addresses: set(&["fresh"]),
        ..Default::default()
    };
    store
        .persist_applied("peer-a", &applied)
        .await
        .expect("duplicate-tolerant save");

    let response = store
        .runtime_query(
            r#"{
                PeerPairingApplied(filter: { peer_id: { _eq: "peer-a" } }) {
                    _docID
                    replicator_addresses
                }
            }"#,
        )
        .await
        .unwrap();
    let mut persisted_rows = rows::<AppliedStateRow>(&response, "PeerPairingApplied").unwrap();
    persisted_rows.sort_by(|left, right| left.doc_id.cmp(&right.doc_id));
    assert_eq!(persisted_rows.len(), 1);
    assert_eq!(
        persisted_rows[0].replicator_addresses,
        Some(vec!["fresh".into()])
    );

    let mut applied = store.load_applied("peer-a").await.expect("reload applied");
    let response = access
        .write(
            "test.pairing.delete",
            r#"mutation {
                delete_PeerPairingApplied(filter: { peer_id: { _eq: "peer-a" } }) { _docID }
            }"#,
        )
        .await;
    assert!(response.is_ok(), "delete failed: {response:?}");
    applied.state.replicator_addresses = set(&["recovered"]);
    store
        .persist_applied("peer-a", &applied)
        .await
        .expect("save after concurrent delete");

    let response = store
        .runtime_query(
            r#"{
                PeerPairingApplied(filter: { peer_id: { _eq: "peer-a" } }) {
                    _docID
                    replicator_addresses
                }
            }"#,
        )
        .await
        .unwrap();
    let rows = rows::<AppliedStateRow>(&response, "PeerPairingApplied").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].replicator_addresses, Some(vec!["recovered".into()]));
}

#[test]
fn nullable_graphql_arrays_emit_null_when_empty() {
    assert_eq!(graphql_nullable_string_array(&BTreeSet::new()), "null");
    assert_eq!(
        graphql_nullable_string_array(&set(&["a", "b"])),
        r#"["a", "b"]"#
    );
}

#[tokio::test]
async fn applied_pairing_is_runtime_owned_and_deferred_until_node_exists() -> anyhow::Result<()> {
    use gents_lean_vocab::lean_contract_snapshot;
    use gents_store::config_client::{
        ConfigAccess, GraphqlEndpoint, NodePermission, PairingObservation, PairingReadReason,
    };
    use gents_store::identity::NodeIdentity;
    let mut linked = std::collections::BTreeMap::new();
    let mut granted = std::collections::BTreeMap::new();
    let mut read = std::collections::BTreeMap::new();
    let mut managed = std::collections::BTreeMap::new();
    let directory = tempfile::tempdir()?;
    let runtime = Arc::new(gents_store::identity::KeyIdentity::load_or_create(
        directory.path().join("runtime"),
        None,
    )?);
    let signing_key =
        gents_store::identity::load_file_identity(&directory.path().join("runtime"))?
            .private_key_bytes();
    let user = gents_store::identity::KeyIdentity::load_or_create(directory.path().join("user"), None)?;
    let agent = gents_store::identity::KeyIdentity::load_or_create(directory.path().join("agent"), None)?;
    let sibling = gents_store::identity::KeyIdentity::load_or_create(directory.path().join("sibling"), None)?;
    let outsider = gents_store::identity::KeyIdentity::load_or_create(directory.path().join("outsider"), None)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(runtime.did())
            .with_signing_key(defra_node::SigningKey::Ed25519(signing_key.clone()))
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .with_storage_backend(defra_node::StorageBackend::Regolith)
            .data_path(directory.path().join("database"))
            .build()
            .await?,
    );
    node.add_dac_policy_with_counter_as(
        &defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned())),
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await?;
    for schema in [
        gents_protocol::schemas::NODE,
        gents_protocol::schemas::PEER_PAIRING_APPLIED,
        gents_protocol::schemas::PEER_PAIRING_DESIRED,
        gents_protocol::schemas::ENROLLMENT_OPERATOR_NONCE,
        gents_protocol::schemas::ENROLLMENT_RENEWAL_POLICY,
        gents_protocol::schemas::NETWORK_ADMIN_PIN,
        gents_protocol::schemas::OWED_DELIVERY,
    ] {
        node.add_collection(schema).await?;
    }
    let runtime_access = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
    for actor in [&user, &agent, &sibling, &outsider] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
        ] {
            runtime_access
                .grant_node_permission(&identity::Did::new(actor.did())?, permission)
                .await?;
        }
    }
    let http = |actor: &gents_store::identity::KeyIdentity| -> anyhow::Result<ConfigAccess> {
        Ok(ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), actor.did())
                .with_client_signing_capability(actor.client_signing_capability()?)?,
        ))
    };
    let user_access = http(&user)?;
    let store = GraphqlPairingStateStore::for_explicit_desired(node.clone(), runtime.clone());
    let mut state = LoadedPairingApplied {
        state: PairingApplied {
            replicator_addresses: set(&["first"]),
            ..Default::default()
        },
        ..Default::default()
    };
    store.persist_applied("peer", &state).await?;
    let own = runtime_access
        .execute("{ PeerPairingApplied {_docID parent_linked} }")
        .await?;
    let doc = own["data"]["PeerPairingApplied"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    linked.insert(
        "before-node",
        own["data"]["PeerPairingApplied"][0]["parent_linked"]
            .as_bool()
            .expect("parent_linked marker"),
    );
    for actor in [&user, &agent, &sibling, &outsider] {
        let response = http(actor)?
            .execute("{ PeerPairingApplied {_docID} }")
            .await?;
        assert!(gents_protocol::graphql::graphql_rows_from_response(
            &response,
            "PeerPairingApplied"
        )
        .is_empty());
    }
    let parent = user_access
        .write(
            "test.pairing.node",
            &format!(
                r#"mutation {{ create_Node(input: {{node_did: "{}"}}) {{_docID}} }}"#,
                gents_store::graphql::escape_graphql_string(runtime.did())
            ),
        )
        .await?;
    let parent = gents_protocol::graphql::extract_mutation_doc_id(&parent, "Node")?;
    user_access
        .add_actor_relationship(
            "Node",
            &parent,
            "runtime_reader",
            &identity::Did::new(runtime.did())?,
        )
        .await?;
    gents_store::config_client::recover_deferred_node_operations(&node, runtime.did()).await?;
    let query = format!(
        r#"{{ PeerPairingApplied(filter: {{_docID: {{_eq: "{}"}}}}) {{_docID parent_linked replicator_addresses}} }}"#,
        gents_store::graphql::escape_graphql_string(&doc)
    );
    let visible = user_access.execute(&query).await?;
    linked.insert(
        "success",
        visible["data"]["PeerPairingApplied"][0]["parent_linked"]
            .as_bool()
            .expect("parent_linked marker"),
    );
    let reader_cases = &lean_contract_snapshot().identity_pairing_reader_cases;
    let reader_expected = |name: &str| -> bool {
        reader_cases
            .iter()
            .find(|case| case.name == name)
            .unwrap()
            .expected_granted
    };
    let agent_did = identity::Did::new(agent.did())?;
    let agent_access = http(&agent)?;
    let sibling_access = http(&sibling)?;
    let outsider_access = http(&outsider)?;
    assert!(outsider_access
        .set_pairing_reader(
            PairingObservation::Applied,
            &doc,
            PairingReadReason::RouteAdmission,
            &agent_did,
            true,
        )
        .await
        .is_err());
    let aborted = user_access
        .transact_write("test.pairing.reader_abort", |txn| {
            let doc = &doc;
            let agent_did = &agent_did;
            Box::pin(async move {
                txn.set_pairing_reader(
                    PairingObservation::Applied,
                    doc,
                    PairingReadReason::RouteAdmission,
                    agent_did,
                    true,
                )
                .await?;
                Err::<(), anyhow::Error>(anyhow::anyhow!("abort staged reader"))
            })
        })
        .await;
    assert!(aborted.is_err());
    let after_abort = agent_access.execute(&query).await?;
    assert_eq!(
        !gents_protocol::graphql::graphql_rows_from_response(&after_abort, "PeerPairingApplied")
            .is_empty(),
        reader_expected("aborted-grant")
    );
    user_access
        .set_pairing_reader(
            PairingObservation::Applied,
            &doc,
            PairingReadReason::RouteAdmission,
            &agent_did,
            true,
        )
        .await?;
    let visible_to_agent = |response: &serde_json::Value| {
        !gents_protocol::graphql::graphql_rows_from_response(response, "PeerPairingApplied")
            .is_empty()
    };
    assert_eq!(
        visible_to_agent(&agent_access.execute(&query).await?),
        reader_expected("initial-grant")
    );
    read.insert(
        "sibling",
        visible_to_agent(&sibling_access.execute(&query).await?),
    );
    assert!(!visible_to_agent(&outsider_access.execute(&query).await?));

    // Exercise the authenticated server boundary directly; a local signed-update
    // preparation error would not demonstrate the native write denial.
    let endpoint =
        GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), agent.did())
            .with_client_signing_capability(agent.client_signing_capability()?)?;
    let rejected = reqwest::Client::new().post(endpoint.url())
        .header(reqwest::header::AUTHORIZATION, endpoint.authorization()?.unwrap())
        .json(&serde_json::json!({"query": format!(
            r#"mutation {{ update_PeerPairingApplied(docID: "{}", input: {{replicator_addresses: ["forged"]}}) {{_docID}} }}"#,
            gents_store::graphql::escape_graphql_string(&doc))}))
        .send().await?;
    let status = rejected.status();
    let denied: serde_json::Value = rejected.json().await?;
    assert!(
        !status.is_success()
            || denied
                .get("errors")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|errors| !errors.is_empty())
            || denied
                .pointer("/data/update_PeerPairingApplied")
                .and_then(serde_json::Value::as_array)
                .is_some_and(Vec::is_empty),
        "native GraphQL did not reject the write: {denied}"
    );

    user_access
        .set_pairing_reader(
            PairingObservation::Applied,
            &doc,
            PairingReadReason::ToolView,
            &agent_did,
            true,
        )
        .await?;
    user_access
        .set_pairing_reader(
            PairingObservation::Applied,
            &doc,
            PairingReadReason::ToolView,
            &agent_did,
            false,
        )
        .await?;
    read.insert(
        "tool-revoked",
        visible_to_agent(&agent_access.execute(&query).await?),
    );
    user_access
        .set_pairing_reader(
            PairingObservation::Applied,
            &doc,
            PairingReadReason::RouteAdmission,
            &agent_did,
            false,
        )
        .await?;
    assert_eq!(
        visible_to_agent(&agent_access.execute(&query).await?),
        reader_expected("explicit-revocation")
    );
    user_access
        .set_pairing_reader(
            PairingObservation::Applied,
            &doc,
            PairingReadReason::ToolView,
            &agent_did,
            true,
        )
        .await?;
    read.insert(
        "tool-only",
        visible_to_agent(&agent_access.execute(&query).await?),
    );
    user_access
        .set_pairing_reader(
            PairingObservation::Applied,
            &doc,
            PairingReadReason::ToolView,
            &agent_did,
            false,
        )
        .await?;
    read.insert(
        "both-revoked",
        visible_to_agent(&agent_access.execute(&query).await?),
    );
    let desired_doc = runtime_access.transact_write("test.pairing.desired", |txn| {
        let actor = identity::Did::new(runtime.did()).unwrap();
        Box::pin(async move {
            let response = txn.execute(&format!(r#"mutation {{ create_PeerPairingDesired(input: {{
                node_did: "{}", actor_did: "{}", peer_id: "peer", source: "enrollment",
                collections: ["AgentSessionInputEdit"], replicator_addresses: ["peer-address"], parent_linked: false
            }}) {{_docID}} }}"#, gents_store::graphql::escape_graphql_string(actor.as_str()),
                gents_store::graphql::escape_graphql_string(actor.as_str()))).await?;
            let doc = gents_protocol::graphql::extract_mutation_doc_id(&response, "PeerPairingDesired")?;
            gents_store::config_client::maybe_link_deferred_operation_in_txn(
                txn, "PeerPairingDesired", &doc, &actor,
            ).await?;
            Ok::<_, anyhow::Error>(doc)
        })
    }).await?;
    let desired_query = format!(
        r#"{{ PeerPairingDesired(filter: {{_docID: {{_eq: "{}"}}}}) {{_docID}} }}"#,
        gents_store::graphql::escape_graphql_string(&desired_doc)
    );
    assert!(gents_protocol::graphql::graphql_rows_from_response(
        &agent_access.execute(&desired_query).await?,
        "PeerPairingDesired"
    )
    .is_empty());
    user_access
        .set_pairing_reader(
            PairingObservation::Desired,
            &desired_doc,
            PairingReadReason::RouteAdmission,
            &agent_did,
            true,
        )
        .await?;
    assert_eq!(
        gents_protocol::graphql::graphql_rows_from_response(
            &agent_access.execute(&desired_query).await?,
            "PeerPairingDesired"
        )
        .len(),
        1
    );
    assert!(gents_protocol::graphql::graphql_rows_from_response(
        &sibling_access.execute(&desired_query).await?,
        "PeerPairingDesired"
    )
    .is_empty());
    user_access
        .set_pairing_reader(
            PairingObservation::Desired,
            &desired_doc,
            PairingReadReason::RouteAdmission,
            &agent_did,
            false,
        )
        .await?;
    assert!(gents_protocol::graphql::graphql_rows_from_response(
        &agent_access.execute(&desired_query).await?,
        "PeerPairingDesired"
    )
    .is_empty());

    let before = runtime_access.execute(&query).await?;
    let foreign = user_access.write("test.pairing.foreign", &format!(r#"mutation {{ create_PeerPairingApplied(input: {{node_did: "{}", peer_id: "peer", parent_linked: true, replicator_addresses: ["foreign"]}}) {{_docID}} }}"#,
        gents_store::graphql::escape_graphql_string(user.did()))).await?;
    let foreign = gents_protocol::graphql::extract_mutation_doc_id(&foreign, "PeerPairingApplied")?;
    user_access
        .add_resource_relationship("PeerPairingApplied", &foreign, "parent", "nodes", &parent)
        .await?;
    let other = LoadedPairingApplied {
        state: PairingApplied {
            replicator_addresses: set(&["other"]),
            ..Default::default()
        },
        ..Default::default()
    };
    store.persist_applied("other-peer", &other).await?;
    let selected = store.load_applied("peer").await?;
    let partition = &lean_contract_snapshot().identity_pairing_partition_cases;
    for (name, candidate) in [("local", doc.as_str()), ("foreign", foreign.as_str())] {
        let modeled = partition
            .iter()
            .find(|case| case.name == name)
            .unwrap()
            .selected;
        assert_eq!(
            selected.doc_id.as_deref() == Some(candidate)
                || selected.duplicate_doc_ids.iter().any(|id| id == candidate),
            modeled
        );
    }
    assert!(selected.duplicate_doc_ids.is_empty());
    let forged = user_access.write("test.pairing.forged", &format!(r#"mutation {{ create_PeerPairingApplied(input: {{node_did: "{}", peer_id: "forged", parent_linked: true, replicator_addresses: ["foreign"]}}) {{_docID}} }}"#,
        gents_store::graphql::escape_graphql_string(runtime.did()))).await?;
    let forged = gents_protocol::graphql::extract_mutation_doc_id(&forged, "PeerPairingApplied")?;
    user_access
        .add_resource_relationship("PeerPairingApplied", &forged, "parent", "nodes", &parent)
        .await?;
    let forged_query = format!(
        r#"{{ PeerPairingApplied(filter: {{_docID: {{_eq: "{}"}}}}) {{_docID replicator_addresses}} }}"#,
        gents_store::graphql::escape_graphql_string(&forged)
    );
    let forged_before = user_access.execute(&forged_query).await?;
    let forged_expected = partition
        .iter()
        .find(|case| case.name == "forged")
        .unwrap()
        .selected;
    assert_eq!(
        store.load_applied("forged").await?.doc_id.is_some(),
        forged_expected
    );
    assert!(store.persist_applied("forged", &other).await.is_err());
    assert_eq!(user_access.execute(&forged_query).await?, forged_before);
    let denied = http(&outsider)?.write("test.pairing.denied", &format!(r#"mutation {{ update_PeerPairingApplied(docID: "{}", input: {{replicator_addresses: ["forged"]}}) {{_docID}} }}"#,
        gents_store::graphql::escape_graphql_string(&doc))).await;
    match denied {
        Ok(value) => assert!(gents_protocol::graphql::graphql_rows_from_response(
            &value,
            "update_PeerPairingApplied"
        )
        .is_empty()),
        Err(error) => assert!(
            error.is_permission_denied() || error.is_graphql_rejection(),
            "{error}"
        ),
    }
    assert_eq!(runtime_access.execute(&query).await?, before);
    runtime_access
        .delete_actor_relationship(
            "PeerPairingApplied",
            &doc,
            "reader_manager",
            &identity::Did::new(user.did())?,
        )
        .await?;
    gents_store::config_client::recover_deferred_node_operations(&node, runtime.did()).await?;
    managed.insert(
        "manager-revoked",
        user_access
            .set_pairing_reader(
                PairingObservation::Applied,
                &doc,
                PairingReadReason::ToolView,
                &agent_did,
                true,
            )
            .await
            .is_ok(),
    );
    runtime_access
        .delete_resource_relationship("PeerPairingApplied", &doc, "parent", "nodes", &parent)
        .await?;
    state = store.load_applied("peer").await?;
    state.state.replicator_addresses = set(&["refresh"]);
    store.persist_applied("peer", &state).await?;
    granted.insert(
        "revoked-after-publication",
        !gents_protocol::graphql::graphql_rows_from_response(
            &user_access.execute(&query).await?,
            "PeerPairingApplied",
        )
        .is_empty(),
    );
    assert!(!visible_to_agent(&agent_access.execute(&query).await?));
    drop(agent_access);
    drop(sibling_access);
    drop(outsider_access);
    drop(store);
    drop(runtime_access);
    drop(user_access);
    node.shutdown().await?;
    drop(node);
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let reopened = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(runtime.did())
            .with_signing_key(defra_node::SigningKey::Ed25519(signing_key))
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .with_storage_backend(defra_node::StorageBackend::Regolith)
            .data_path(directory.path().join("database"))
            .build()
            .await?,
    );
    let store = GraphqlPairingStateStore::for_explicit_desired(reopened.clone(), runtime.clone());
    state = store.load_applied("peer").await?;
    state.state.replicator_addresses = set(&["restart"]);
    store.persist_applied("peer", &state).await?;
    let user_access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
            .with_client_signing_capability(user.client_signing_capability()?)?,
    );
    assert!(gents_protocol::graphql::graphql_rows_from_response(
        &user_access.execute(&query).await?,
        "PeerPairingApplied"
    )
    .is_empty());
    let agent_access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), agent.did())
            .with_client_signing_capability(agent.client_signing_capability()?)?,
    );
    assert!(!visible_to_agent(&agent_access.execute(&query).await?));
    reopened.shutdown().await?;
    for case in gents_lean_vocab::families::identity_operations_deferred_applied_linked::cases() {
        let linked = linked
            .remove(case.input.name.as_str())
            .unwrap_or_else(|| panic!("the applied-pairing scenario never reached {}", case.input.name));
        case.check(gents_lean_vocab::families::identity_operations_deferred_applied_linked::Linked { linked });
    }
    for case in gents_lean_vocab::families::identity_operations_deferred_applied_granted::cases() {
        let granted = granted
            .remove(case.input.name.as_str())
            .unwrap_or_else(|| panic!("the applied-pairing scenario never reached {}", case.input.name));
        case.check(gents_lean_vocab::families::identity_operations_deferred_applied_granted::Granted { granted });
    }
    for case in gents_lean_vocab::families::identity_operations_reader_policy_read::cases() {
        let read = read
            .remove(case.input.name.as_str())
            .unwrap_or_else(|| panic!("the applied-pairing scenario never reached {}", case.input.name));
        case.check(gents_lean_vocab::families::identity_operations_reader_policy_read::ReaderRead { read });
    }
    for case in gents_lean_vocab::families::identity_operations_reader_policy_manage::cases() {
        let manage_reader = managed
            .remove(case.input.name.as_str())
            .unwrap_or_else(|| panic!("the applied-pairing scenario never reached {}", case.input.name));
        case.check(gents_lean_vocab::families::identity_operations_reader_policy_manage::ManageReader { manage_reader });
    }
    assert!(
        linked.is_empty() && granted.is_empty() && read.is_empty() && managed.is_empty(),
        "unmodeled applied-pairing observations"
    );
    Ok(())
}
