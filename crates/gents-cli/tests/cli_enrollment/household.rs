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
        let (status, _) = wait_for_enrollment_token(&format!("http://127.0.0.1:{port}"), &graphql).await?;
        let pending = core.request_status_enrollment_with_label(&status, Some("household client")).await?;
        wait_for_runtime_enrollment_request(&graphql, &pending.request_id).await?;
        run_cli_json_with_env(&runtime_home, &["runtime", "p2p", "enrollment", "grant", &pending.request_id, "--home", home], &owner_env)?;
        run_cli_json_with_env(&runtime_home, &["runtime", "p2p", "enrollment", "approve", &pending.request_id, "--home", home], &owner_env)?;
        wait_for_chat_ready_enrollment(&core, &node_did).await?;
        wait_for_client_node_readiness(&core, &node_did).await?;

        let mut node = core.stores().snapshot(&node_did).nodes.iter()
            .find(|row| row.node_did == node_did).cloned()
            .context("enrolled client did not observe Node")?;
        node.display_name = Some("household administrator update".into());
        core.save_node(&node).await.context("updating Node as delegated administrator")?;

        let mut agent = core.stores().snapshot(&node_did).agents.iter()
            .find(|row| row.node_did == node_did && row.agent_did == default_agent)
            .cloned().context("enrolled client did not observe default Agent")?;
        let binding = core.provision_agent_identity(&node_did).await
            .context("provisioning Agent identity through the paired runtime")?;
        assert_ne!(binding.agent_did, default_agent);
        assert_ne!(binding.agent_did, node_did);
        agent.agent_did = binding.agent_did.clone();
        agent.creation_nonce = binding.creation_nonce;
        agent.display_name = Some("household second agent".into());
        core.save_agent(&agent, None).await.context("saving Agent as enrolled client")?;
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
        gents_store::local::store_key::open_home_store_key(&runtime_home, &data)
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
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        gents_store::identity::NodeIdentity::did(&node_identity).parse()?,
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
        let commits = gents_store::graphql::composite_commits(
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

/// The desktop's Approve is admission (Lean
/// `Identity.EnrollmentGrant.applyDecision`): it grants the enrolled-User
/// authority, then approves. Deny grants nothing, and withdraws a grant whose
/// approval never committed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn desktop_approve_admits_the_member_and_deny_grants_nothing() -> Result<()> {
    use gents_service::server_host::admit_managed_enrollment_as;
    use gents_service::server_host::decide_managed_enrollment;
    use gents_service::server_host::ManagedEnrollmentDecision;
    use gents_store::identity::{load_enrollment_grant_receipts, GrantSet};
    let _guard = enrollment_e2e_lock().lock().await;
    let model = FakeLlm::start(
        "admission",
        None,
        Arc::new(|_| ChatAction::Sse(completion_text_sse("ok"))),
    )?;
    let temp = tempfile::tempdir()?;
    let runtime_home = temp.path().join("runtime");
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
            "admission",
            "--model-name",
            "admission",
            "--inference-url",
            model.endpoint(),
        ],
        &owner_env,
    )?;
    let owner_identity = gents_desktop_core::client::PrincipalIdentity::load_existing(
        &DesktopPaths::from_root(owner_home.join("cli")),
    )?;
    let node_did = node_did_from_init(&init)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let owner = gents_store::config_client::GraphqlEndpoint::as_principal(
        graphql.clone(),
        owner_identity.did(),
    )
    .with_client_signing_capability(
        gents_store::config_client::ClientSigningCapability::from_ed25519_key(
            owner_identity.private_key_bytes(),
        )?,
    )?
    .with_client_signed_agent_creates();
    let (mut server, _) =
        spawn_server_with_ready_json(&runtime_home, port, &["--home", home], &owner_env)?;
    wait_for_port(port, &mut server)?;

    let (approved, denied, granted_then_denied) = server
        .capturing(async {
            wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
            let mut members = Vec::new();
            for label in ["approved", "denied", "granted then denied"] {
                let core = ClientCore::start_with_paths_and_options(
                    DesktopPaths::from_root(temp.path().join(label.replace(' ', "-"))),
                    ClientCoreOptions::local_only(),
                )
                .await?;
                let (status, _) =
                    wait_for_enrollment_token(&format!("http://127.0.0.1:{port}"), &graphql)
                        .await?;
                let pending = core
                    .request_status_enrollment_with_label(&status, Some(label))
                    .await?;
                wait_for_runtime_enrollment_request(&graphql, &pending.request_id).await?;
                members.push((core, pending.request_id));
            }
            let (approved_core, approved_request) = &members[0];
            let outcome = admit_managed_enrollment_as(
                &runtime_home,
                owner.clone(),
                approved_request,
                gents_protocol::enrollment::DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
            )
            .await?;
            assert_eq!(outcome.state, "approved");
            wait_for_chat_ready_enrollment(approved_core, &node_did).await?;
            wait_for_client_node_readiness(approved_core, &node_did).await?;
            /* what an admitted client does: read and update the Node it administers */
            let mut node = approved_core
                .stores()
                .snapshot(&node_did)
                .nodes
                .iter()
                .find(|row| row.node_did == node_did)
                .cloned()
                .context("the admitted member did not observe the Node")?;
            node.display_name = Some("updated by the admitted member".into());
            approved_core
                .save_node(&node)
                .await
                .context("updating Node as the admitted member")?;

            let (_, denied_request) = &members[1];
            let outcome = decide_managed_enrollment(
                &runtime_home,
                &graphql,
                denied_request,
                ManagedEnrollmentDecision::Deny,
            )
            .await?;
            assert_eq!(outcome.state, "denied");

            /* a grant whose approval never committed, then a denial */
            let (_, withdrawn_request) = &members[2];
            run_cli_json_with_env(
                &runtime_home,
                &[
                    "runtime",
                    "p2p",
                    "enrollment",
                    "grant",
                    withdrawn_request,
                    "--home",
                    home,
                ],
                &owner_env,
            )?;
            let outcome = decide_managed_enrollment(
                &runtime_home,
                &graphql,
                withdrawn_request,
                ManagedEnrollmentDecision::Deny,
            )
            .await?;
            assert_eq!(outcome.state, "denied");

            let dids: Vec<String> = members
                .iter()
                .map(|(core, _)| core.node_identity().did().to_owned())
                .collect();
            for (core, _) in members {
                Arc::new(core).shutdown().await?;
            }
            Ok::<_, anyhow::Error>((dids[0].clone(), dids[1].clone(), dids[2].clone()))
        })
        .await?;
    drop(server);

    let node_identity = identity_from_init(&init)?;
    let data = runtime_home.join("data");
    let node = Arc::new(
        gents_store::local::store_key::open_home_store_key(&runtime_home, &data)
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
    let runtime = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        gents_store::identity::NodeIdentity::did(&node_identity).parse()?,
    );
    let holds = |did: &str| {
        let node = node.clone();
        let did = identity::Did::new(did).expect("member DID");
        async move {
            let mut held = Vec::new();
            for permission in GrantSet::OperatorUser.permissions() {
                if node
                    .nac_manager()
                    .check_permission(&did, *permission)
                    .await
                    .expect("NAC check")
                {
                    held.push(permission.as_str());
                }
            }
            held
        }
    };
    let enrolled_user: Vec<&str> = GrantSet::OperatorUser
        .permissions()
        .iter()
        .map(|p| p.as_str())
        .collect();
    assert_eq!(
        holds(&approved).await,
        enrolled_user,
        "Approve grants the enrolled-User permissions"
    );
    let approved_receipts = load_enrollment_grant_receipts(&runtime, &approved).await?;
    assert_eq!(approved_receipts.len(), 1);
    assert!(!approved_receipts[0].withdrawn);
    assert_eq!(
        approved_receipts[0].tuples.iter().map(|tuple| tuple.name()).collect::<Vec<_>>(),
        gents_store::identity::EnrollmentGrantTuple::derived().iter().map(|tuple| tuple.name()).collect::<Vec<_>>(),
        "the receipt accounts for the Node administrator relation and every enrolled-User permission",
    );
    assert!(holds(&denied).await.is_empty(), "Deny grants nothing");
    assert!(load_enrollment_grant_receipts(&runtime, &denied)
        .await?
        .is_empty());
    assert!(
        holds(&granted_then_denied).await.is_empty(),
        "Deny withdraws an unapproved grant"
    );
    assert!(
        load_enrollment_grant_receipts(&runtime, &granted_then_denied)
            .await?
            .iter()
            .all(|receipt| receipt.withdrawn)
    );
    node.shutdown().await?;
    Ok(())
}
