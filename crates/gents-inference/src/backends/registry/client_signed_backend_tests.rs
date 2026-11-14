type ProbeKey = (bool, bool, bool);
type DiscoveryKey = (bool, bool, bool, bool);

#[tokio::test]
async fn user_signed_backend_separates_metadata_and_private_credential() -> anyhow::Result<()> {
    use anyhow::Context;
    use gents_store::identity::NodeIdentity;
    use serde_json::json;
    use std::sync::Arc;
    let directory = tempfile::tempdir()?;
    let key_dir = directory.path().to_owned();
    let (user, runtime, agent, editor) =
        tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            Ok((
                gents_store::identity::KeyIdentity::load_or_create(key_dir.join("user"), None)?,
                gents_store::identity::KeyIdentity::load_or_create(key_dir.join("runtime"), None)?,
                gents_store::identity::KeyIdentity::load_or_create(key_dir.join("agent"), None)?,
                gents_store::identity::KeyIdentity::load_or_create(key_dir.join("editor"), None)?,
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
            && gents_store::collection::Collection::ALL
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
    let access = gents_store::config_client::ConfigAccess::Graphql(
        gents_store::config_client::GraphqlEndpoint::as_principal(&url, user.did())
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
            &gents_store::graphql::created_doc_id(&node_response, "Node")?,
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
    let mut backend: gents_documents::InferenceBackend = serde_json::from_value(json!({
        "node_did":runtime.did(), "backend_id":"backend", "name":"Backend",
        "provider_kind":"OpenAiCompatible", "endpoint":format!("http://{discovery_address}/v1"),
        "auth":{"kind":"api_key", "key":"first-private-key"}
    }))?;
    let metadata_doc = gents_documents::inference_backend::write_inference_backend_document(
        &access,
        &backend,
        &crate::resolved::ProviderProfilePolicy,
    )
    .await?;
    let runtime_access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(runtime.did())?,
    );
    // Native ACP denies the runtime either by refusing the mutation or by
    // matching no document it may update; the User's row must not change.
    match runtime_access.write("test.backend.runtime_config_denied", &format!(
        "mutation {{ update_InferenceBackendMetadata(docID: \"{}\", input: {{ endpoint: \"https://other.example\" }}) {{ _docID }} }}",
        gents_store::graphql::escape_graphql_string(&metadata_doc),
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
    stale_backend.auth = gents_documents::BackendAuth::Credential {
        id: metadata["credential_id"].as_str().unwrap().to_owned(),
        version: metadata["credential_version"].as_str().unwrap().to_owned(),
    };
    let mut moved = backend.clone();
    moved.endpoint = "https://different.example/v1".into();
    moved.auth = gents_documents::BackendAuth::Credential {
        id: metadata["credential_id"].as_str().unwrap().to_owned(),
        version: metadata["credential_version"].as_str().unwrap().to_owned(),
    };
    let mut probes: Vec<(ProbeKey, bool)> = Vec::new();
    let mut discovery: Vec<(DiscoveryKey, bool)> = Vec::new();
    let mut removals: Vec<(ProbeKey, [bool; 3])> = Vec::new();
    let moved_published = gents_documents::inference_backend::write_inference_backend_document(
        &access,
        &moved,
        &crate::resolved::ProviderProfilePolicy,
    )
    .await
    .is_ok();
    assert_eq!(read().await?, initial);
    runtime_access.write("test.backend.runtime_observation", "mutation { update_InferenceBackendObservation(filter: {backend_id: {_eq: \"backend\"}}, input: {probe_status: \"healthy\"}) { _docID } }").await?;
    backend.auth = gents_documents::BackendAuth::ApiKey {
        key: "rotated-private-key".into(),
    };
    gents_documents::inference_backend::write_inference_backend_document(
        &access,
        &backend,
        &crate::resolved::ProviderProfilePolicy,
    )
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
    probes.push((
        (true, false, true),
        crate::backends::registry::set_backend_probe_status_for_connection(
            &node,
            &stale_backend,
            &metadata_doc,
            "unhealthy",
            chrono::Utc::now(),
        )
        .await
        .is_ok(),
    ));
    assert_eq!(
        read().await?["data"]["InferenceBackendObservation"][0]["probe_status"],
        "healthy"
    );
    let agent_access = gents_store::config_client::ConfigAccess::Graphql(
        gents_store::config_client::GraphqlEndpoint::as_principal(&url, agent.did())
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
    discovery.push((
        (false, true, true, true),
        crate::backends::registry::discover_backend_for_actor(
            &node,
            &agent_did,
            runtime.did(),
            "backend",
        )
        .await
        .is_ok(),
    ));
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
    assert!(crate::backends::registry::discover_backend_for_actor(
        &node,
        &agent_did,
        runtime.did(),
        "backend",
    )
    .await
    .is_err());
    assert_eq!(discoveries.load(std::sync::atomic::Ordering::SeqCst), 0);
    node.add_dac_actor_relationship_as(
        &context,
        "InferenceBackendMetadata",
        &metadata_doc,
        "writer",
        editor.did(),
    )
    .await?;
    let editor_discovery = crate::backends::registry::discover_backend_for_actor(
        &node,
        &editor_did,
        runtime.did(),
        "backend",
    )
    .await;
    discovery.push(((true, true, true, true), editor_discovery.is_ok()));
    assert_eq!(editor_discovery?.len(), 1);
    assert_eq!(discoveries.load(std::sync::atomic::Ordering::SeqCst), 1);
    let (uncommitted, mutations) =
        gents_store::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
            Some(1),
            crate::backends::registry::discover_backend_for_actor(
                &node,
                &editor_did,
                runtime.did(),
                "backend",
            ),
        )
        .await;
    assert_eq!(mutations, 1);
    discovery.push(((true, true, true, false), uncommitted.is_ok()));
    assert_eq!(discoveries.load(std::sync::atomic::Ordering::SeqCst), 2);
    pause.store(true, std::sync::atomic::Ordering::SeqCst);
    let reconnect_node = node.clone();
    let reconnect_actor = editor_did.clone();
    let reconnect_node_did = runtime.did().to_owned();
    let mut reconnecting = tokio::spawn(async move {
        crate::backends::registry::discover_backend_for_actor(
            &reconnect_node,
            &reconnect_actor,
            &reconnect_node_did,
            "backend",
        )
        .await
    });
    tokio::select! {
        () = waiting.notified() => {}
        result = &mut reconnecting => panic!("discovery ended before provider request: {result:?}"),
    }
    backend.auth = gents_documents::BackendAuth::ApiKey {
        key: "reconnected-private-key".into(),
    };
    gents_documents::inference_backend::write_inference_backend_document(
        &access,
        &backend,
        &crate::resolved::ProviderProfilePolicy,
    )
    .await?;
    release.notify_one();
    discovery.push(((true, true, false, true), reconnecting.await?.is_ok()));
    pause.store(false, std::sync::atomic::Ordering::SeqCst);
    let rotated = read().await?;
    let catalog_before =
        read().await?["data"]["InferenceBackendObservation"][0]["catalogs"].clone();
    assert!(
        gents_documents::inference_backend::write_inference_backend_document(
            &agent_access,
            &backend,
            &crate::resolved::ProviderProfilePolicy
        )
        .await
        .is_err()
    );
    assert_eq!(
        read().await?["data"]["BackendCredential"][0]["secret"]["key"],
        "reconnected-private-key"
    );
    pause.store(true, std::sync::atomic::Ordering::SeqCst);
    let racing_node = node.clone();
    let racing_actor = editor_did.clone();
    let racing_node_did = runtime.did().to_owned();
    let mut racing = tokio::spawn(async move {
        crate::backends::registry::discover_backend_for_actor(
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
    discovery.push(((false, true, true, true), racing.await?.is_ok()));
    assert_eq!(
        read().await?["data"]["InferenceBackendObservation"][0]["catalogs"],
        catalog_before
    );
    assert!(crate::backends::registry::discover_backend_for_actor(
        &node,
        &editor_did,
        runtime.did(),
        "backend",
    )
    .await
    .is_err());
    assert_eq!(discoveries.load(std::sync::atomic::Ordering::SeqCst), 4);
    let removal =
        gents_documents::DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
            gents_store::collection::Collection::InferenceBackend,
            runtime.did().to_owned(),
            "backend".to_owned(),
        )])?;
    let denied = agent_access
        .transact_write("test.backend.remove.denied", |txn| {
            let removal = &removal;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    removal,
                    &crate::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await;
    assert!(denied.is_err());
    let records = |rows: &serde_json::Value| {
        [
            "InferenceBackendMetadata",
            "InferenceBackendObservation",
            "BackendCredential",
        ]
        .map(|collection| !rows["data"][collection].as_array().unwrap().is_empty())
    };
    removals.push(((false, true, true), records(&read().await?)));
    let bound_metadata = read().await?["data"]["InferenceBackendObservation"][0]["metadata_doc_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let rebind_observation = |metadata: &str| {
        format!(
            "mutation {{ update_InferenceBackendObservation(filter: {{backend_id: {{_eq: \"backend\"}}}}, input: {{metadata_doc_id: \"{}\"}}) {{ _docID }} }}",
            gents_store::graphql::escape_graphql_string(metadata),
        )
    };
    runtime_access
        .write(
            "test.backend.unmatched_observation",
            &rebind_observation("unmatched"),
        )
        .await?;
    let unmatched = access
        .transact_write("test.backend.remove.unmatched", |txn| {
            let removal = &removal;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    removal,
                    &crate::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await;
    removals.push(((true, false, true), records(&read().await?)));
    assert!(unmatched.is_err());
    runtime_access
        .write(
            "test.backend.matched_observation",
            &rebind_observation(&bound_metadata),
        )
        .await?;
    let (interrupted, applied) =
        gents_store::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
            Some(1),
            access.transact_write("test.backend.remove.rollback", |txn| {
                let removal = &removal;
                Box::pin(async move {
                    gents_documents::apply_desired_state_plan(
                        txn,
                        removal,
                        &crate::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            }),
        )
        .await;
    assert!(interrupted.is_err());
    assert_eq!(applied, 1);
    removals.push(((true, true, false), records(&read().await?)));
    access
        .transact_write("test.backend.remove", |txn| {
            let removal = &removal;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    removal,
                    &crate::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await?;
    removals.push(((true, true, true), records(&read().await?)));
    backend.auth = gents_documents::BackendAuth::Credential {
        id: rotated["data"]["InferenceBackendMetadata"][0]["credential_id"]
            .as_str()
            .unwrap()
            .to_owned(),
        version: rotated["data"]["InferenceBackendMetadata"][0]["credential_version"]
            .as_str()
            .unwrap()
            .to_owned(),
    };
    let recreated_doc = gents_documents::inference_backend::write_inference_backend_document(
        &access,
        &backend,
        &crate::resolved::ProviderProfilePolicy,
    )
    .await?;
    let genesis_distinct = recreated_doc != metadata_doc;
    let recreated = read().await?;
    probes.push((
        (false, true, true),
        crate::backends::registry::set_backend_probe_status_for_connection(
            &node,
            &backend,
            &metadata_doc,
            "healthy",
            chrono::Utc::now(),
        )
        .await
        .is_ok(),
    ));
    probes.push((
        (true, true, true),
        crate::backends::registry::set_backend_probe_status_for_connection(
            &node,
            &backend,
            &recreated_doc,
            "healthy",
            chrono::Utc::now(),
        )
        .await
        .is_ok(),
    ));
    let (uncommitted_probe, probe_mutations) =
        gents_store::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
            Some(1),
            crate::backends::registry::set_backend_probe_status_for_connection(
                &node,
                &backend,
                &recreated_doc,
                "unhealthy",
                chrono::Utc::now(),
            ),
        )
        .await;
    assert_eq!(probe_mutations, 1);
    probes.push(((true, true, false), uncommitted_probe.is_ok()));
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
    node.add_dac_actor_relationship_as(
        &context,
        "InferenceBackendMetadata",
        &recreated_doc,
        "writer",
        editor.did(),
    )
    .await?;
    pause.store(true, std::sync::atomic::Ordering::SeqCst);
    let replacing_node = node.clone();
    let replacing_actor = editor_did.clone();
    let replacing_node_did = runtime.did().to_owned();
    let mut replacing = tokio::spawn(async move {
        crate::backends::registry::discover_backend_for_actor(
            &replacing_node,
            &replacing_actor,
            &replacing_node_did,
            "backend",
        )
        .await
    });
    tokio::select! {
        () = waiting.notified() => {}
        result = &mut replacing => panic!("discovery ended before provider request: {result:?}"),
    }
    access
        .transact_write("test.backend.replace_during_discovery", |txn| {
            let removal = &removal;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    removal,
                    &crate::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await?;
    let replacement = gents_documents::inference_backend::write_inference_backend_document(
        &access,
        &backend,
        &crate::resolved::ProviderProfilePolicy,
    )
    .await?;
    assert_ne!(replacement, recreated_doc);
    release.notify_one();
    discovery.push(((true, false, true, true), replacing.await?.is_ok()));
    for case in gents_lean_vocab::families::household_backend_destination::cases() {
        case.check(moved_published);
    }
    for case in gents_lean_vocab::families::household_backend_probe::cases() {
        let key = (
            case.input.same_doc,
            case.input.same_connection,
            case.input.committed,
        );
        let observed: Vec<bool> = probes
            .iter()
            .filter(|(probe, _)| *probe == key)
            .map(|(_, published)| *published)
            .collect();
        assert!(
            !observed.is_empty() && observed.iter().all(|published| *published == observed[0]),
            "probe {key:?}: {observed:?}"
        );
        case.check(observed[0]);
    }
    for case in gents_lean_vocab::families::household_backend_discovery::cases() {
        let key = (
            case.input.authorized,
            case.input.same_doc,
            case.input.same_connection,
            case.input.committed,
        );
        let observed: Vec<bool> = discovery
            .iter()
            .filter(|(attempt, _)| *attempt == key)
            .map(|(_, published)| *published)
            .collect();
        assert!(
            !observed.is_empty() && observed.iter().all(|published| *published == observed[0]),
            "discovery {key:?}: {observed:?}"
        );
        case.check(observed[0]);
    }
    for case in gents_lean_vocab::families::household_backend_removal::cases() {
        let key = (
            case.input.authorized,
            case.input.matching,
            case.input.committed,
        );
        let [metadata, observation, credential] = removals
            .iter()
            .find(|(attempt, _)| *attempt == key)
            .map(|(_, records)| *records)
            .unwrap_or_else(|| panic!("removal {key:?} was not observed"));
        case.check(
            gents_lean_vocab::families::household_backend_removal::BackendRecords {
                metadata,
                observation,
                credential,
            },
        );
    }
    for case in gents_lean_vocab::families::household_backend_genesis::cases() {
        case.check(genesis_distinct);
    }
    discovery_server.abort();
    node.shutdown().await?;
    Ok(())
}
