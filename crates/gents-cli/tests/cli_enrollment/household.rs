use super::*;
use std::sync::Arc;
use support::mocks::fake_llm::{ChatAction, FakeLlm};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fresh_enrolled_client_receives_node_administration_and_creates_agent() -> Result<()> {
    let _guard = enrollment_e2e_lock().lock().await;
    let model = FakeLlm::start(
        "household",
        None,
        Arc::new(|_| ChatAction::Sse(completion_text_sse("ok"))),
    )?;
    let temp = tempfile::tempdir()?;
    let runtime_home = temp.path().join("runtime");
    let client_home = temp.path().join("client");
    let owner_home = temp.path().join("owner");
    let owner_home_arg = owner_home.to_str().context("owner home UTF-8")?;
    let owner_env = [("GENTS_DESKTOP_HOME", owner_home_arg)];
    fs::create_dir_all(&runtime_home)?;
    let home = runtime_home.to_str().context("runtime home UTF-8")?;
    let init = run_cli_json_with_env(
        &runtime_home,
        &[
            "init",
            "--store-key-custody",
            "file",
            "--home",
            home,
            "--node-name",
            "household",
            "--model-name",
            "household",
            "--inference-url",
            model.endpoint(),
        ],
        &owner_env,
    )?;
    let owner_identity = gents_desktop_core::client::PrincipalIdentity::load_existing(
        &DesktopPaths::from_root(owner_home.join("cli")),
    )?;
    let node_did = node_did_from_init(&init)?;
    let default_agent = agent_did_from_init(&init)?;
    assert_ne!(
        owner_identity.did(),
        node_did,
        "the client and runtime have distinct keys"
    );
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let (mut server, _) =
        spawn_server_with_ready_json(&runtime_home, port, &["--home", home], &owner_env)?;
    wait_for_port(port, &mut server)?;

    let (client_did, new_agent_did) = server.capturing(async {
        wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
        let core = ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(&client_home), ClientCoreOptions::local_only(),
        ).await?;
        let (status, _) = wait_for_enrollment_token(&format!("http://127.0.0.1:{port}")).await?;
        let pending = core.request_status_enrollment_with_label(&status, Some("household client")).await?;
        wait_for_runtime_enrollment_request(&graphql, &pending.request_id).await?;
        run_cli_json_with_env(&runtime_home, &["p2p", "enrollment", "grant", &pending.request_id, "--home", home], &owner_env)?;
        run_cli_json_with_env(&runtime_home, &["p2p", "enrollment", "approve", &pending.request_id, "--home", home], &owner_env)?;
        wait_for_chat_ready_enrollment(&core, &node_did).await?;
        wait_for_client_node_readiness(&core, &node_did).await?;

        let mut node = core.store().snapshot().nodes.iter()
            .find(|row| row.node_did == node_did).cloned()
            .context("enrolled client did not observe Node")?;
        node.display_name = Some("household administrator update".into());
        core.save_node(&node).await.context("updating Node as delegated administrator")?;

        let mut agent = core.store().snapshot().agents.iter()
            .find(|row| row.node_did == node_did && row.agent_did == default_agent)
            .cloned().context("enrolled client did not observe default Agent")?;
        let binding = core.provision_agent_identity(&node_did).await
            .context("provisioning Agent identity through the paired runtime")?;
        assert_ne!(binding.agent_did, default_agent);
        assert_ne!(binding.agent_did, node_did);
        agent.agent_did = binding.agent_did.clone();
        agent.creation_nonce = binding.creation_nonce;
        agent.display_name = Some("household second agent".into());
        core.save_agent(&agent).await.context("saving Agent as enrolled client")?;
        let client_did = core.node_identity().did().to_owned();
        assert_ne!(client_did, owner_identity.did());
        let access = core.operator_access(&node_did)?;
        let response = access.execute(&format!(
            "{{ Agent(filter: {{ agent_did: {{ _eq: \"{}\" }} }}) {{ agent_did creation_nonce }} }}",
            escape_graphql_string(&agent.agent_did),
        )).await?;
        let rows = response["data"]["Agent"].as_array().context("Agent query failed")?;
        assert_eq!(rows.len(), 1, "client cannot read the Agent it created: {response}");
        assert_eq!(rows[0]["creation_nonce"], agent.creation_nonce);
        Arc::new(core).shutdown().await?;
        Ok((client_did, binding.agent_did))
    }).await?;
    drop(server);

    let node_identity = identity_from_init(&init)?;
    let data = runtime_home.join("data");
    let node = Arc::new(
        gents::store::local::store_key::open_home_store_key(&runtime_home, &data)
            .await?
            .encrypt(
                gents::defra_node::Node::builder()
                    .data_path(&data)
                    .with_storage_backend(gents::defra_node::StorageBackend::Regolith),
            )
            .with_node_identity_did(&node_did)
            .build()
            .await?,
    );
    let access = gents::store::config_client::ConfigAccess::local_as(
        node.clone(),
        gents::NodeIdentity::did(&node_identity).parse()?,
    );
    let owners = access.transact_readonly("test.household.owners", |txn| {
        let node_did = node_did.clone();
        let new_agent_did = new_agent_did.clone();
        let default_agent = default_agent.clone();
        Box::pin(async move {
            let query = format!(
                "{{ Node(filter: {{ node_did: {{ _eq: \"{}\" }} }}) {{ _docID }} Agent(filter: {{ agent_did: {{ _eq: \"{}\" }} }}) {{ _docID }} }}",
                escape_graphql_string(&node_did), escape_graphql_string(&new_agent_did),
            );
            let result = txn.execute(&query).await?;
            let node_doc = result["data"]["Node"][0]["_docID"].as_str().context("Node missing")?.to_owned();
            let agent_doc = result["data"]["Agent"][0]["_docID"].as_str().context("Agent missing")?.to_owned();
            let default = txn.execute(&format!(
                "{{ Agent(filter: {{ agent_did: {{ _eq: \"{}\" }} }}) {{ _docID }} }}",
                escape_graphql_string(&default_agent),
            )).await?;
            let default_doc = default["data"]["Agent"][0]["_docID"].as_str().context("default Agent missing")?.to_owned();
            let node_owner = txn.document_owners("Node", std::slice::from_ref(&node_doc)).await?;
            let agent_owner = txn.document_owners("Agent", std::slice::from_ref(&agent_doc)).await?;
            let default_owner = txn.document_owners("Agent", std::slice::from_ref(&default_doc)).await?;
            Ok::<_, anyhow::Error>((node_owner, agent_owner, agent_doc, node_doc, default_owner, default_doc))
        })
    }).await?;
    assert_eq!(
        owners.0[0].owner.as_ref().map(|did| did.as_str()),
        Some(owner_identity.did()),
        "the original client must remain the native Node owner"
    );
    assert_eq!(
        owners.1[0].owner.as_ref().map(|did| did.as_str()),
        Some(client_did.as_str()),
        "client must own the Agent it creates"
    );
    assert_eq!(
        owners.4[0].owner.as_ref().map(|did| did.as_str()),
        Some(owner_identity.did()),
        "the initial default Agent must be owned by the creating client"
    );
    for (label, doc_id, creator) in [
        ("Node", &owners.3, owner_identity.did()),
        ("default Agent", &owners.5, owner_identity.did()),
        ("enrolled client's Agent", &owners.2, client_did.as_str()),
    ] {
        let commits = gents::store::graphql::composite_commits(
            &node,
            doc_id,
            "test.household.creation_commits",
        )
        .await?;
        let creation = commits
            .iter()
            .min_by_key(|commit| commit.height)
            .with_context(|| format!("{label} has no composite creation commit"))?;
        let (block, signature) = node
            .authorized_signed_block_bytes(&creation.cid, Some(creator))
            .await?;
        let block = defra_core::block::Block::from_dag_cbor(&block)?;
        let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
        let signer = db::block::verify::verified_signature_signer_did(&block, &signature)
            .map_err(anyhow::Error::msg)?;
        assert_eq!(
            signer, creator,
            "{label} creation must be signed by its client creator"
        );
    }
    node.shutdown().await?;
    Ok(())
}
