use super::*;
use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_store::config_client::ConfigAccess;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "select GENTS_LIVE_TARGET and run make live-enrollment"]
async fn paired_user_observes_two_live_agent_sessions_with_native_isolation() -> Result<()> {
    let _guard = enrollment_e2e_lock().lock().await;
    let (model_endpoint, model_name) = resolve_live_inference().await?;
    let temp = tempfile::Builder::new()
        .prefix("gents-live-two-agent-")
        .tempdir()?
        .keep();
    tracing::info!(path = %temp.display(), "retained live isolation fixture");
    let runtime_home = temp.join("runtime");
    let client_home = temp.join("client");
    let owner_home = temp.join("owner");
    fs::create_dir_all(&runtime_home)?;
    let home = runtime_home.to_str().context("runtime home UTF-8")?;
    let owner_home_arg = owner_home.to_str().context("owner home UTF-8")?;
    let owner_env = [("GENTS_DESKTOP_HOME", owner_home_arg)];
    let init = run_cli_json_with_env(
        &runtime_home,
        &[
            "init",
            "--store-key-custody",
            "file",
            "--home",
            home,
            "--node-name",
            "live-two-agent",
            "--model-name",
            &model_name,
            "--max-concurrent",
            "2",
            "--max-queue-depth",
            "4",
            "--inference-url",
            &model_endpoint,
        ],
        &owner_env,
    )?;
    let node_did = node_did_from_init(&init)?;
    let default_agent = agent_did_from_init(&init)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let (server, _) =
        spawn_server_with_ready_json(&runtime_home, port, &["--home", home], &owner_env)?;

    let (bindings, requests, client_did) = server
        .capturing(async {
            wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
            let core = ClientCore::start_with_paths_and_options(
                DesktopPaths::from_root(&client_home),
                ClientCoreOptions::local_only(),
            )
            .await?;
            let (status, _) =
                wait_for_enrollment_token(&format!("http://127.0.0.1:{port}"), &graphql).await?;
            let pending = core
                .request_status_enrollment_with_label(&status, Some("live isolation"))
                .await
                .context("requesting fresh desktop enrollment")?;
            wait_for_runtime_enrollment_request(&graphql, &pending.request_id)
                .await
                .context("observing fresh desktop enrollment request on runtime")?;
            run_cli_json_with_env(
                &runtime_home,
                &[
                    "runtime",
                    "p2p",
                    "enrollment",
                    "grant",
                    &pending.request_id,
                    "--home",
                    home,
                ],
                &owner_env,
            )
            .context("granting Node administration to the enrolling desktop User")?;
            run_cli_json_with_env(
                &runtime_home,
                &[
                    "runtime",
                    "p2p",
                    "enrollment",
                    "approve",
                    &pending.request_id,
                    "--home",
                    home,
                ],
                &owner_env,
            )
            .context("approving fresh desktop enrollment")?;
            wait_for_chat_ready_enrollment(&core, &node_did)
                .await
                .context("waiting for enrolled desktop chat readiness")?;
            wait_for_client_node_readiness(&core, &node_did)
                .await
                .context("waiting for enrolled desktop Node readiness")?;

            let mut agent = core
                .stores()
                .snapshot(&node_did)
                .agents
                .iter()
                .find(|row| row.node_did == node_did && row.agent_did == default_agent)
                .cloned()
                .context("paired user cannot see the default Agent")?;
            // The default Agent is the Engineer, which `gents init` makes the
            // Node's administrator (Lean `SelfConfig.engineerInitRelations`), so
            // it reads every Agent's sessions by decision. Isolation is checked
            // between two Agents that hold no grant on each other.
            let engineer = AgentIdentityBinding {
                agent_did: agent.agent_did.clone(),
                creation_nonce: agent.creation_nonce.clone(),
            };
            let mut workers = Vec::new();
            for label in ["live first worker", "live second worker"] {
                let worker = core
                    .provision_agent_identity(&node_did)
                    .await
                    .with_context(|| format!("provisioning the {label} identity"))?;
                assert_ne!(engineer.agent_did, worker.agent_did);
                agent.agent_did = worker.agent_did.clone();
                agent.creation_nonce = worker.creation_nonce.clone();
                agent.display_name = Some(label.into());
                core.save_agent(&agent, None)
                    .await
                    .with_context(|| format!("publishing the {label} as the enrolled desktop User"))?;
                workers.push(worker);
            }
            let access = core.operator_access(&node_did)?;
            grant_configured_workspace_root_readers(&access, &graphql, &init, &owner_home, &workers.iter().collect::<Vec<_>>())
                .await
                .context("granting the worker Agents access to their configured WorkspaceRoot")?;
            let user_did = core.node_identity().did().to_owned();
            assert_ne!(user_did, node_did);
            let mut requests = Vec::new();
            for (index, binding) in workers.iter().enumerate() {
                let session = Uuid::new_v4().to_string();
                let prompt = format!(
                    "For isolation test {index}, give a short greeting: {}",
                    Uuid::new_v4()
                );
                let receipt = core
                    .submit_request(&session, &node_did, &prompt, Some(&binding.agent_did))
                    .await?;
                let reply = wait_for_complete_agent_response_with_access(
                    &access,
                    &receipt.request_id,
                    &session,
                    Duration::from_secs(240),
                )
                .await?;
                wait_for_client_complete_response(
                    &core,
                    &session,
                    &node_did,
                    &receipt.request_id,
                    &reply,
                )
                .await?;
                let calls = access.execute(&format!(
                    "{{ InferenceCall(filter: {{ request_id: {{ _eq: \"{}\" }} }}) {{ request_id agent_did model_name call_state }} }}",
                    escape_graphql_string(&receipt.request_id),
                )).await?;
                let rows = calls["data"]["InferenceCall"].as_array().context("inference calls not visible to paired user")?;
                assert!(rows.iter().any(|row| row["agent_did"] == binding.agent_did && row["model_name"] == model_name && row["call_state"] == "completed"), "missing completed selected-model call: {calls}");
                let ownership = access.transact_readonly("test.live_session_owners", |txn| {
                    let request_id = &receipt.request_id;
                    let session = &session;
                    Box::pin(async move {
                        let rows = txn.execute(&format!(
                            "{{ AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}) {{_docID}} AgentSession(filter: {{session_id: {{_eq: \"{}\"}}}}) {{_docID}} }}",
                            escape_graphql_string(request_id), escape_graphql_string(session),
                        )).await?;
                        let request_doc = rows["data"]["AgentRequest"][0]["_docID"].as_str().context("request is missing")?.to_owned();
                        let session_doc = rows["data"]["AgentSession"][0]["_docID"].as_str().context("session is missing")?.to_owned();
                        let request_owner = txn.document_owners("AgentRequest", &[request_doc]).await?;
                        let session_owner = txn.document_owners("AgentSession", &[session_doc]).await?;
                        Ok::<_, anyhow::Error>((request_owner, session_owner))
                    })
                }).await?;
                for owner in [ownership.0, ownership.1] {
                    assert_eq!(owner[0].owner.as_ref().map(|did| did.as_str()), Some(user_did.as_str()));
                }
                requests.push((receipt.request_id, session, reply));
            }
            Arc::new(core).shutdown().await?;
            let reopened = ClientCore::start_with_paths_and_options(
                DesktopPaths::from_root(&client_home),
                ClientCoreOptions::local_only(),
            )
            .await?;
            wait_for_chat_ready_enrollment(&reopened, &node_did).await?;
            wait_for_client_node_readiness(&reopened, &node_did).await?;
            for (request_id, session, reply) in &requests {
                wait_for_client_complete_response(&reopened, session, &node_did, request_id, reply)
                    .await?;
            }
            Arc::new(reopened).shutdown().await?;
            let [first, second]: [AgentIdentityBinding; 2] =
                workers.try_into().map_err(|_| anyhow::anyhow!("two worker Agents"))?;
            Ok((
                [engineer, first, second],
                requests.into_iter().map(|(request_id, session, _)| (request_id, session)).collect::<Vec<_>>(),
                user_did,
            ))
        })
        .await?;
    drop(server);

    let runtime_identity = identity_from_init(&init)?;
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
            .with_node_acp_enabled()
            .build()
            .await?,
    );
    let user_access = ConfigAccess::local_as(node.clone(), client_did.parse()?);
    let agent_doc = user_access
        .transact_readonly("test.live_agent_creation_owner", |txn| {
            let agent = &bindings[1].agent_did;
            let client_did = &client_did;
            Box::pin(async move {
                let rows = txn
                    .execute(&format!(
                        "{{ Agent(filter: {{agent_did: {{_eq: \"{}\"}}}}) {{_docID}} }}",
                        escape_graphql_string(agent),
                    ))
                    .await?;
                let [row] = rows["data"]["Agent"]
                    .as_array()
                    .context("created Agent query unavailable")?
                    .as_slice()
                else {
                    anyhow::bail!("created Agent must be uniquely visible to its User")
                };
                let doc = row["_docID"]
                    .as_str()
                    .context("created Agent missing physical ID")?
                    .to_owned();
                let owners = txn
                    .document_owners("Agent", std::slice::from_ref(&doc))
                    .await?;
                anyhow::ensure!(
                    matches!(owners.as_slice(), [owner] if owner.visible
                && owner.owner.as_ref().is_some_and(|did| did.as_str() == client_did.as_str())),
                    "created Agent must be owned by the paired User"
                );
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await?;
    let commits = gents_store::graphql::composite_commits(
        &node,
        &agent_doc,
        "test.live_agent_creation_signature",
    )
    .await?;
    let creation = commits
        .iter()
        .min_by_key(|commit| commit.height)
        .context("Agent creation commit missing")?;
    let (block, signature) = node
        .authorized_signed_block_bytes(&creation.cid, Some(&client_did))
        .await?;
    let block = defra_core::block::Block::from_dag_cbor(&block)?;
    let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
    let signer = db::block::verify::verified_signature_signer_did(&block, &signature)
        .map_err(anyhow::Error::msg)?;
    assert_eq!(
        signer, client_did,
        "paired User must sign the Agent creation block"
    );
    let mut engineer_reads: Vec<(&str, bool)> = Vec::new();
    let mut session_updates: Vec<(&str, bool)> = Vec::new();
    let actors = bindings
        .iter()
        .map(|binding| {
            let signer = gents_store::identity::resolve_agent_identity(&runtime_identity, binding)?;
            Ok(ConfigAccess::local_managed_agent(
                node.clone(),
                binding.agent_did.parse()?,
                signer.as_ref(),
            )?)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut session_docs = Vec::new();
    let (engineer_actor, workers) = actors.split_first().context("Engineer actor")?;
    let worker_bindings = &bindings[1..];
    for (target, (_, session_id)) in requests.iter().enumerate() {
        let result = workers[target]
            .execute(&format!(
                "{{ AgentSession(filter: {{session_id: {{_eq: \"{}\"}}}}) {{_docID}} }}",
                escape_graphql_string(session_id),
            ))
            .await?;
        session_docs.push(
            result["data"]["AgentSession"][0]["_docID"]
                .as_str()
                .context("working Agent cannot read its native session document")?
                .to_owned(),
        );
    }
    for (request_id, session_id) in &requests {
        let response = engineer_actor.execute(&format!(
            "{{ AgentRequest(filter: {{ request_id: {{ _eq: \"{}\" }} }}) {{ request_id }} AgentSession(filter: {{ session_id: {{ _eq: \"{}\" }} }}) {{ session_id }} }}",
            escape_graphql_string(request_id),
            escape_graphql_string(session_id),
        )).await?;
        for (collection, resource) in [
            ("AgentRequest", "delivered_requests"),
            ("AgentSession", "sessions"),
        ] {
            let rows = response["data"][collection]
                .as_array()
                .with_context(|| format!("{collection} query failed"))?;
            assert!(
                rows.len() <= 1,
                "Engineer (Node administrator) visibility for {collection}: {response}"
            );
            engineer_reads.push((resource, !rows.is_empty()));
        }
    }
    for (index, binding) in worker_bindings.iter().enumerate() {
        let actor = &workers[index];
        assert!(
            node.nac_manager()
                .check_permission(
                    &binding.agent_did.parse()?,
                    gents_store::config_client::NodePermission::DocumentUpdate
                )
                .await?,
            "Agent needs node-level update before the document ACP check"
        );
        for (target, (request_id, session_id)) in requests.iter().enumerate() {
            let response = actor.execute(&format!(
                "{{ AgentRequest(filter: {{ request_id: {{ _eq: \"{}\" }} }}) {{ request_id }} AgentSession(filter: {{ session_id: {{ _eq: \"{}\" }} }}) {{ session_id }} }}",
                escape_graphql_string(request_id),
                escape_graphql_string(session_id),
            )).await?;
            for collection in ["AgentRequest", "AgentSession"] {
                let rows = response["data"][collection]
                    .as_array()
                    .with_context(|| format!("{collection} query failed"))?;
                assert_eq!(
                    rows.len(),
                    usize::from(index == target),
                    "Agent {index} visibility for {collection} {target}: {response}"
                );
            }
        }
        for (target, doc_id) in session_docs.iter().enumerate() {
            let session = if index == target {
                "general-session"
            } else {
                "sibling-session"
            };
            let principal =
                gents::defra_node::AcpIdentity::Authenticated(binding.agent_did.parse()?);
            let allowed = node
                .document_acp()
                .check_doc_access(
                    &principal,
                    gents::defra_node::DocumentPermission::Update,
                    gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
                    "sessions",
                    doc_id,
                )
                .await?;
            session_updates.push((session, allowed));
            let before = workers[target]
                .execute(&format!(
                    "{{ AgentSession(filter: {{_docID: {{_eq: \"{}\"}}}}) {{tags}} }}",
                    escape_graphql_string(doc_id),
                ))
                .await?;
            let tags = &before["data"]["AgentSession"][0]["tags"];
            let label = format!("agent-{index}-target-{target}");
            let mutation = format!(
                "mutation {{ update_AgentSession(docID: \"{}\", input: {{tags:[\"{}\"]}}) {{_docID}} }}",
                escape_graphql_string(doc_id), escape_graphql_string(&label),
            );
            let updated = actor
                .write("test.live_cross_agent_session_update", &mutation)
                .await;
            let changed = updated.as_ref().is_ok_and(|response| {
                gents_protocol::graphql::extract_mutation_doc_id(response, "AgentSession").is_ok()
            });
            assert_eq!(
                changed, allowed,
                "Agent {index} update of session {target}: {updated:?}"
            );
            let after = workers[target]
                .execute(&format!(
                    "{{ AgentSession(filter: {{_docID: {{_eq: \"{}\"}}}}) {{tags}} }}",
                    escape_graphql_string(doc_id),
                ))
                .await?;
            let actual = &after["data"]["AgentSession"][0]["tags"];
            if index == target {
                assert_eq!(actual, &serde_json::json!([label]));
            } else {
                assert_eq!(actual, tags, "foreign session changed despite denial");
            }
        }
    }
    for case in gents_lean_vocab::families::household_live_administrator_read::cases() {
        let reads: Vec<bool> = engineer_reads
            .iter()
            .filter(|(resource, _)| *resource == case.input.resource)
            .map(|(_, visible)| *visible)
            .collect();
        anyhow::ensure!(
            !reads.is_empty() && reads.iter().all(|visible| *visible == reads[0]),
            "Engineer reads of {} disagree: {reads:?}",
            case.input.resource
        );
        case.check(Some(reads[0]));
    }
    for case in gents_lean_vocab::families::household_live_session_update::cases() {
        let updates: Vec<bool> = session_updates
            .iter()
            .filter(|(session, _)| *session == case.input.id)
            .map(|(_, allowed)| *allowed)
            .collect();
        anyhow::ensure!(
            !updates.is_empty() && updates.iter().all(|allowed| *allowed == updates[0]),
            "session updates of {} disagree: {updates:?}",
            case.input.id
        );
        case.check(Some(updates[0]));
    }
    node.shutdown().await?;
    Ok(())
}
