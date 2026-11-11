use crate::store::config_client::client_signed_agent::{create_route, CreateRoute};

#[tokio::test]
async fn user_signed_backend_separates_metadata_and_private_credential() -> anyhow::Result<()> {
    use crate::store::identity::NodeIdentity;
    use anyhow::Context;
    use serde_json::json;
    use std::sync::Arc;
    let directory = tempfile::tempdir()?;
    let key_dir = directory.path().to_owned();
    let (user, runtime, agent, editor) =
        tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            Ok((
                crate::KeyIdentity::load_or_create(key_dir.join("user"), None)?,
                crate::KeyIdentity::load_or_create(key_dir.join("runtime"), None)?,
                crate::KeyIdentity::load_or_create(key_dir.join("agent"), None)?,
                crate::KeyIdentity::load_or_create(key_dir.join("editor"), None)?,
            ))
        })
        .await??;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await?
        .into_std()?;
    let address = listener.local_addr()?;
    let discovery_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let discovery_address = discovery_listener.local_addr()?;
    let discoveries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let discovered = discoveries.clone();
    let pause = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let waiting = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (pause_server, waiting_server, release_server) =
        (pause.clone(), waiting.clone(), release.clone());
    let app = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(move || {
            let discovered = discovered.clone();
            let (pause, waiting, release) = (
                pause_server.clone(),
                waiting_server.clone(),
                release_server.clone(),
            );
            async move {
                discovered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if pause.load(std::sync::atomic::Ordering::SeqCst) {
                    waiting.notify_one();
                    release.notified().await;
                }
                axum::Json(json!({"data":[{"id":"model"}]}))
            }
        }),
    );
    let discovery_server = tokio::spawn(async move { axum::serve(discovery_listener, app).await });
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .with_node_identity_did(runtime.did())
            .with_node_acp_owner(user.did().to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    let context = defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    node.add_dac_policy_with_counter_as(
        &context,
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await?;
    for schema in [
        gents_protocol::schemas::NODE,
        gents_protocol::schemas::INFERENCE_BACKEND,
        gents_protocol::schemas::INFERENCE_BACKEND_METADATA,
        gents_protocol::schemas::INFERENCE_BACKEND_OBSERVATION,
        gents_protocol::schemas::BACKEND_CREDENTIAL,
    ] {
        node.add_collection_as(&context, schema).await?;
    }
    // Configuration reference loading reads every configuration collection
    // on each apply, so the node must declare them all.
    for (name, schema) in gents_protocol::schemas::ALL_COLLECTION_NAMES
        .iter()
        .zip(gents_protocol::schemas::ALL)
    {
        if ![
            "Node",
            "InferenceBackendMetadata",
            "InferenceBackendObservation",
        ]
        .contains(name)
            && crate::Collection::ALL
                .iter()
                .any(|collection| collection.graphql_type() == *name)
        {
            node.add_collection_as(&context, schema).await?;
        }
    }
    for permission in [
        acp::nac::NodePermission::DocumentRead,
        acp::nac::NodePermission::DocumentUpdate,
        acp::nac::NodePermission::CollectionGet,
    ] {
        for actor in [agent.did(), runtime.did(), editor.did()] {
            node.add_nac_actor_relationship_as(&context, permission.as_str(), actor)
                .await?;
        }
    }
    let url = format!("http://{address}/api/v0/graphql");
    let access = crate::store::config_client::ConfigAccess::Graphql(
        crate::store::config_client::GraphqlEndpoint::as_principal(&url, user.did())
            .with_client_signing_capability(user.client_signing_capability()?)?
            .with_client_signed_agent_creates(),
    );
    let node_response = access
        .transact_write("test.backend.node", |txn| {
            let runtime_did = runtime.did();
            Box::pin(async move {
                txn.execute_client_signed_create(
            "mutation($input: NodeMutationInputArg!) { create_Node(input:$input) {_docID} }",
            &json!({"input":{"node_did":runtime_did}}), gents_protocol::schemas::NODE).await
            })
        })
        .await?;
    // The runtime observes configuration through its Node relationship, as
    // in production; native ACP has no Node fallback.
    access
        .add_actor_relationship(
            "Node",
            &crate::store::graphql::created_doc_id(&node_response, "Node")?,
            "runtime_reader",
            &identity::Did::new(runtime.did())?,
        )
        .await?;
    let legacy = access.transact_write("test.backend.protected_legacy", |txn| {
        let runtime_did = runtime.did();
        Box::pin(async move {
            txn.execute_client_signed_create(
                "mutation($input: InferenceBackendMutationInputArg!) { create_InferenceBackend(input:$input) { _docID _version { cid } } }",
                &json!({"input": {"node_did":runtime_did,"backend_id":"legacy", "name":"legacy", "provider_kind":"OpenAiCompatible", "endpoint":"https://provider.example", "auth":{"kind":"api_key", "key":"legacy-secret"}}}),
                gents_protocol::schemas::INFERENCE_BACKEND,
            ).await
        })
    }).await?;
    let legacy_cid = legacy["data"]["add_InferenceBackend"][0]["_version"][0]["cid"]
        .as_str()
        .context("protected legacy create has no composite CID")?
        .to_owned();
    let (legacy_block, legacy_signature) = node
        .authorized_signed_block_bytes(&legacy_cid, Some(user.did()))
        .await?;
    let legacy_block = defra_core::block::Block::from_dag_cbor(&legacy_block)?;
    let legacy_signature = defra_core::block::Signature::from_dag_cbor(&legacy_signature)?;
    assert_eq!(
        db::block::verify::verified_signature_signer_did(&legacy_block, &legacy_signature)
            .map_err(anyhow::Error::msg)?,
        user.did()
    );
    let mut backend: crate::InferenceBackend = serde_json::from_value(json!({
        "node_did":runtime.did(), "backend_id":"backend", "name":"Backend",
        "provider_kind":"OpenAiCompatible", "endpoint":format!("http://{discovery_address}/v1"),
        "auth":{"kind":"api_key", "key":"first-private-key"}
    }))?;
    let metadata_doc =
        crate::documents::inference_backend::write_inference_backend_document(&access, &backend)
            .await?;
    let runtime_access = crate::store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(runtime.did())?,
    );
    // Native ACP denies the runtime either by refusing the mutation or by
    // matching no document it may update; the User's row must not change.
    match runtime_access.write("test.backend.runtime_config_denied", &format!(
        "mutation {{ update_InferenceBackendMetadata(docID: \"{}\", input: {{ endpoint: \"https://other.example\" }}) {{ _docID }} }}",
        crate::store::graphql::escape_graphql_string(&metadata_doc),
    )).await {
        Ok(denied) => assert_eq!(
            denied["data"]["update_InferenceBackendMetadata"]
                .as_array()
                .map(Vec::len),
            Some(0),
            "runtime updated User configuration: {denied}"
        ),
        Err(error) => assert!(error.is_graphql_rejection(), "{error}"),
    }
    let unchanged = access
        .execute("{ InferenceBackendMetadata { endpoint } }")
        .await?;
    assert_eq!(
        unchanged["data"]["InferenceBackendMetadata"][0]["endpoint"],
        json!(backend.endpoint)
    );
    let read = || async {
        access.execute("{ InferenceBackendMetadata { _docID creation_nonce auth_kind credential_id credential_version _version { cid } } InferenceBackendObservation { _docID metadata_doc_id catalogs probe_status _version { cid } } BackendCredential { _docID credential_id version secret _version { cid } } }").await
    };
    let initial = read().await?;
    let metadata = &initial["data"]["InferenceBackendMetadata"][0];
    let secret = &initial["data"]["BackendCredential"][0];
    let observation = &initial["data"]["InferenceBackendObservation"][0];
    assert_eq!(metadata["_docID"], metadata_doc);
    assert_eq!(observation["metadata_doc_id"], metadata_doc);
    assert_eq!(metadata["credential_id"], secret["credential_id"]);
    assert_eq!(metadata["credential_version"], secret["version"]);
    assert!(!metadata.to_string().contains("first-private-key"));
    for row in [metadata, secret, observation] {
        let cid = row["_version"][0]["cid"]
            .as_str()
            .expect("signed composite");
        let (block, signature) = node
            .authorized_signed_block_bytes(cid, Some(user.did()))
            .await?;
        let block = defra_core::block::Block::from_dag_cbor(&block)?;
        let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
        assert_eq!(
            db::block::verify::verified_signature_signer_did(&block, &signature)
                .map_err(anyhow::Error::msg)?,
            user.did()
        );
    }
    let old_version = metadata["credential_version"].clone();
    let mut stale_backend = backend.clone();
    stale_backend.auth = crate::documents::BackendAuth::Credential {
        id: metadata["credential_id"].as_str().unwrap().to_owned(),
        version: metadata["credential_version"].as_str().unwrap().to_owned(),
    };
    let mut moved = backend.clone();
    moved.endpoint = "https://different.example/v1".into();
    moved.auth = crate::documents::BackendAuth::Credential {
        id: metadata["credential_id"].as_str().unwrap().to_owned(),
        version: metadata["credential_version"].as_str().unwrap().to_owned(),
    };
    let modeled = gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let moved_allowed = modeled
        ._backend_credential_cases
        .iter()
        .find(|case| case["name"] == "unapproved-destination")
        .unwrap()["may_use"]
        .as_bool()
        .unwrap();
    assert_eq!(
        crate::documents::inference_backend::write_inference_backend_document(&access, &moved)
            .await
            .is_ok(),
        moved_allowed
    );
    assert_eq!(read().await?, initial);
    runtime_access.write("test.backend.runtime_observation", "mutation { update_InferenceBackendObservation(filter: {backend_id: {_eq: \"backend\"}}, input: {probe_status: \"healthy\"}) { _docID } }").await?;
    backend.auth = crate::documents::BackendAuth::ApiKey {
        key: "rotated-private-key".into(),
    };
    crate::documents::inference_backend::write_inference_backend_document(&access, &backend)
        .await?;
    let rotated = read().await?;
    assert_ne!(
        rotated["data"]["InferenceBackendMetadata"][0]["credential_version"],
        old_version
    );
    assert_eq!(
        rotated["data"]["BackendCredential"][0]["secret"]["key"],
        "rotated-private-key"
    );
    let modeled = gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let stale_allowed = modeled
        .backend_probe_cases
        .iter()
        .find(|case| {
            case["same_doc"] == true
                && case["same_connection"] == false
                && case["committed"] == true
        })
        .unwrap()["published"]
        .as_bool()
        .unwrap();
    assert_eq!(
        crate::inference::backends::registry::set_backend_probe_status_for_connection(
            &node,
            &stale_backend,
            &metadata_doc,
            "unhealthy",
            chrono::Utc::now()
        )
        .await
        .is_ok(),
        stale_allowed
    );
    assert_eq!(
        read().await?["data"]["InferenceBackendObservation"][0]["probe_status"],
        "healthy"
    );
    let agent_access = crate::store::config_client::ConfigAccess::Graphql(
        crate::store::config_client::GraphqlEndpoint::as_principal(&url, agent.did())
            .with_client_signing_capability(agent.client_signing_capability()?)?
            .with_client_signed_agent_creates(),
    );
    let legacy_query = agent_access
        .execute("{ InferenceBackend { _docID auth } }")
        .await?;
    assert!(
        gents_protocol::graphql::graphql_rows_from_response(&legacy_query, "InferenceBackend")
            .is_empty()
    );
    assert!(node
        .authorized_signed_block_bytes(&legacy_cid, Some(agent.did()))
        .await
        .is_err());
    let agent_did = identity::Did::new(agent.did())?;
    let editor_did = identity::Did::new(editor.did())?;
    let modeled = gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let expected = |visible: bool, same_connection: bool| {
        modeled
            .backend_discovery_cases
            .iter()
            .find(|case| {
                case["authorized"] == visible
                    && case["same_doc"] == true
                    && case["same_connection"] == same_connection
                    && case["committed"] == true
            })
            .unwrap()["published"]
            .as_bool()
            .unwrap()
    };
    assert!(
        crate::inference::backends::registry::discover_backend_for_actor(
            &node,
            &agent_did,
            runtime.did(),
            "backend",
        )
        .await
        .is_ok()
            == expected(false, true)
    );
    assert_eq!(discoveries.load(std::sync::atomic::Ordering::SeqCst), 0);
    node.add_dac_actor_relationship_as(
        &context,
        "InferenceBackendMetadata",
        &metadata_doc,
        "reader",
        agent.did(),
    )
    .await?;
    let visible = agent_access.execute("{ InferenceBackendMetadata { _docID credential_id credential_version } BackendCredential { _docID secret } }").await?;
    assert_eq!(
        visible["data"]["InferenceBackendMetadata"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(visible["data"]["BackendCredential"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(
        crate::inference::backends::registry::discover_backend_for_actor(
            &node,
            &agent_did,
            runtime.did(),
            "backend",
        )
        .await
        .is_err()
    );
    assert_eq!(discoveries.load(std::sync::atomic::Ordering::SeqCst), 0);
    node.add_dac_actor_relationship_as(
        &context,
        "InferenceBackendMetadata",
        &metadata_doc,
        "writer",
        editor.did(),
    )
    .await?;
    let models = crate::inference::backends::registry::discover_backend_for_actor(
        &node,
        &editor_did,
        runtime.did(),
        "backend",
    )
    .await?;
    assert_eq!(models.len(), 1);
    assert!(expected(true, true));
    assert_eq!(discoveries.load(std::sync::atomic::Ordering::SeqCst), 1);
    let catalog_before =
        read().await?["data"]["InferenceBackendObservation"][0]["catalogs"].clone();
    assert!(
        crate::documents::inference_backend::write_inference_backend_document(
            &agent_access,
            &backend
        )
        .await
        .is_err()
    );
    assert_eq!(
        read().await?["data"]["BackendCredential"][0]["secret"]["key"],
        "rotated-private-key"
    );
    pause.store(true, std::sync::atomic::Ordering::SeqCst);
    let racing_node = node.clone();
    let racing_actor = editor_did.clone();
    let racing_node_did = runtime.did().to_owned();
    let mut racing = tokio::spawn(async move {
        crate::inference::backends::registry::discover_backend_for_actor(
            &racing_node,
            &racing_actor,
            &racing_node_did,
            "backend",
        )
        .await
    });
    tokio::select! {
        () = waiting.notified() => {}
        result = &mut racing => panic!("discovery ended before provider request: {result:?}"),
    }
    node.delete_dac_actor_relationship_as(
        &context,
        "InferenceBackendMetadata",
        &metadata_doc,
        "writer",
        editor.did(),
    )
    .await?;
    release.notify_one();
    assert_eq!(racing.await?.is_ok(), expected(false, true));
    assert_eq!(
        read().await?["data"]["InferenceBackendObservation"][0]["catalogs"],
        catalog_before
    );
    assert!(
        crate::inference::backends::registry::discover_backend_for_actor(
            &node,
            &editor_did,
            runtime.did(),
            "backend",
        )
        .await
        .is_err()
    );
    assert_eq!(discoveries.load(std::sync::atomic::Ordering::SeqCst), 2);
    let removal =
        crate::documents::DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
            crate::Collection::InferenceBackend,
            runtime.did().to_owned(),
            "backend".to_owned(),
        )])?;
    let denied = agent_access
        .transact_write("test.backend.remove.denied", |txn| {
            let removal = &removal;
            Box::pin(async move { crate::documents::apply_desired_state_plan(txn, removal).await })
        })
        .await;
    assert!(denied.is_err());
    let denied_case = modeled
        .backend_removal_cases
        .iter()
        .find(|case| {
            case["authorized"] == false && case["matching"] == true && case["committed"] == true
        })
        .unwrap();
    let retained = read().await?;
    for (collection, field) in [
        ("InferenceBackendMetadata", "metadata"),
        ("InferenceBackendObservation", "observation"),
        ("BackendCredential", "credential"),
    ] {
        assert_eq!(
            !retained["data"][collection].as_array().unwrap().is_empty(),
            denied_case[field].as_bool().unwrap()
        );
    }
    let (interrupted, applied) =
        crate::store::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
            Some(1),
            access.transact_write("test.backend.remove.rollback", |txn| {
                let removal = &removal;
                Box::pin(
                    async move { crate::documents::apply_desired_state_plan(txn, removal).await },
                )
            }),
        )
        .await;
    assert!(interrupted.is_err());
    assert_eq!(applied, 1);
    let rollback_case = modeled
        .backend_removal_cases
        .iter()
        .find(|case| {
            case["authorized"] == true && case["matching"] == true && case["committed"] == false
        })
        .unwrap();
    let rolled_back = read().await?;
    for (collection, field) in [
        ("InferenceBackendMetadata", "metadata"),
        ("InferenceBackendObservation", "observation"),
        ("BackendCredential", "credential"),
    ] {
        assert_eq!(
            !rolled_back["data"][collection]
                .as_array()
                .unwrap()
                .is_empty(),
            rollback_case[field].as_bool().unwrap()
        );
    }
    access
        .transact_write("test.backend.remove", |txn| {
            let removal = &removal;
            Box::pin(async move { crate::documents::apply_desired_state_plan(txn, removal).await })
        })
        .await?;
    let removed_case = modeled
        .backend_removal_cases
        .iter()
        .find(|case| {
            case["authorized"] == true && case["matching"] == true && case["committed"] == true
        })
        .unwrap();
    let after = read().await?;
    for (collection, field) in [
        ("InferenceBackendMetadata", "metadata"),
        ("InferenceBackendObservation", "observation"),
        ("BackendCredential", "credential"),
    ] {
        assert_eq!(
            !after["data"][collection].as_array().unwrap().is_empty(),
            removed_case[field].as_bool().unwrap()
        );
    }
    backend.auth = crate::documents::BackendAuth::Credential {
        id: rotated["data"]["InferenceBackendMetadata"][0]["credential_id"]
            .as_str()
            .unwrap()
            .to_owned(),
        version: rotated["data"]["InferenceBackendMetadata"][0]["credential_version"]
            .as_str()
            .unwrap()
            .to_owned(),
    };
    let recreated_doc =
        crate::documents::inference_backend::write_inference_backend_document(&access, &backend)
            .await?;
    assert_eq!(
        recreated_doc != metadata_doc,
        modeled.backend_genesis_distinct
    );
    let recreated = read().await?;
    let replaced_allowed = modeled
        .backend_probe_cases
        .iter()
        .find(|case| {
            case["same_doc"] == false
                && case["same_connection"] == true
                && case["committed"] == true
        })
        .unwrap()["published"]
        .as_bool()
        .unwrap();
    assert_eq!(
        crate::inference::backends::registry::set_backend_probe_status_for_connection(
            &node,
            &backend,
            &metadata_doc,
            "healthy",
            chrono::Utc::now()
        )
        .await
        .is_ok(),
        replaced_allowed
    );
    assert_ne!(
        recreated["data"]["InferenceBackendMetadata"][0]["creation_nonce"],
        metadata["creation_nonce"]
    );
    assert_eq!(
        recreated["data"]["InferenceBackendObservation"][0]["metadata_doc_id"],
        recreated_doc
    );
    assert_ne!(
        recreated["data"]["InferenceBackendObservation"][0]["_docID"],
        observation["_docID"]
    );
    let agent_after_recreate = agent_access
        .execute("{ InferenceBackendMetadata { _docID } }")
        .await?;
    assert!(gents_protocol::graphql::graphql_rows_from_response(
        &agent_after_recreate,
        "InferenceBackendMetadata"
    )
    .is_empty());
    discovery_server.abort();
    node.shutdown().await?;
    Ok(())
}
