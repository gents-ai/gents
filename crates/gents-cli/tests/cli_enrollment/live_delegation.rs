use super::*;
use gents::store::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, ConfigAccess,
    DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::{Collection, Tools};
use serde_json::json;
use std::sync::Arc;

async fn publish(access: &ConfigAccess, documents: Vec<(Collection, Value)>) -> Result<()> {
    let plan = DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )?;
    access
        .transact_write("test.live_delegation_config", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paired_user_sees_and_controls_live_delegated_child() -> Result<()> {
    let _guard = enrollment_e2e_lock().lock().await;
    let (model_endpoint, model_name) = resolve_live_inference().await?;
    let home_dir = tempfile::Builder::new()
        .prefix("gents-live-delegation-")
        .tempdir()?
        .keep();
    tracing::info!(path = %home_dir.display(), "retained live delegation fixture");
    let runtime_home = home_dir.join("runtime");
    let client_home = home_dir.join("client");
    let owner_home = home_dir.join("owner");
    fs::create_dir_all(&runtime_home)?;
    let home = runtime_home.to_str().context("runtime home UTF-8")?;
    let owner_env = [(
        "GENTS_DESKTOP_HOME",
        owner_home.to_str().context("owner home UTF-8")?,
    )];
    let init = run_cli_json_with_env(
        &runtime_home,
        &[
            "init",
            "--store-key-custody",
            "file",
            "--home",
            home,
            "--node-name",
            "live-delegation",
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
    let (mut server, _) =
        spawn_server_with_ready_json(&runtime_home, port, &["--home", home], &owner_env)?;
    wait_for_port(port, &mut server)?;
    let (bindings, owned) = server.capturing(async {
        wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
        let core = ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(&client_home), ClientCoreOptions::local_only(),
        ).await?;
        let (status, _) = wait_for_enrollment_token(&format!("http://127.0.0.1:{port}")).await?;
        let pending = core.request_status_enrollment_with_label(&status, Some("live delegation")).await?;
        wait_for_runtime_enrollment_request(&graphql, &pending.request_id).await?;
        run_cli_json_with_env(&runtime_home, &["p2p", "enrollment", "grant", &pending.request_id, "--home", home], &owner_env)?;
        run_cli_json_with_env(&runtime_home, &["p2p", "enrollment", "approve", &pending.request_id, "--home", home], &owner_env)?;
        wait_for_chat_ready_enrollment(&core, &node_did).await?;
        wait_for_client_node_readiness(&core, &node_did).await?;
        let user = core.node_identity().did().to_owned();
        assert_ne!(user, node_did);
        let snapshot = core.store().snapshot();
        let template = snapshot.agents.iter().find(|row| row.node_did == node_did && row.agent_did == default_agent)
            .cloned().context("default Agent unavailable")?;
        let context = snapshot.contexts.iter().find(|row| Some(&row.context_id) == template.context_id.as_ref() && row.node_did == node_did)
            .cloned().context("default AgentContext unavailable")?;
        let a = core.provision_agent_identity(&node_did).await?;
        let b = core.provision_agent_identity(&node_did).await?;
        assert_ne!(a.agent_did, b.agent_did);
        let access = core.operator_access(&node_did)?;
        let tools_a = Tools { node_did: node_did.clone(), tools_id: "delegation-a-tools".into(), ..Default::default() };
        let tools_b = Tools { node_did: node_did.clone(), tools_id: "delegation-b-tools".into(), ..Default::default() };
        let mut context_a = context.clone();
        context_a.context_id = "delegation-a-context".into();
        context_a.tools_id = Some(tools_a.tools_id.clone());
        context_a.skill_ids.clear();
        context_a.compaction_id = None;
        context_a.system_prompt = Some("You are the coordinator. When asked to delegate to child, call the agent_new tool with agent child and the requested prompt. Do not pretend to delegate.".into());
        let mut context_b = context;
        context_b.context_id = "delegation-b-context".into();
        context_b.tools_id = Some(tools_b.tools_id.clone());
        context_b.skill_ids.clear();
        context_b.compaction_id = None;
        context_b.system_prompt = Some("You are the child worker. Answer the task directly and briefly.".into());
        publish(&access, vec![
            (Collection::Tools, serde_json::to_value(&tools_a)?),
            (Collection::Tools, serde_json::to_value(&tools_b)?),
            (Collection::AgentContext, serde_json::to_value(&context_a)?),
            (Collection::AgentContext, serde_json::to_value(&context_b)?),
        ]).await?;
        for (binding, context_id, label) in [(&a, &context_a.context_id, "coordinator"), (&b, &context_b.context_id, "child")] {
            let mut agent = template.clone();
            agent.agent_did = binding.agent_did.clone();
            agent.creation_nonce = binding.creation_nonce.clone();
            agent.context_id = Some(context_id.clone());
            agent.display_name = Some(label.into());
            core.save_agent(&agent).await?;
        }
        let target = gents::documents::AgentTargetDocument {
            target_id: "live-delegation-child".into(), node_did: node_did.clone(),
            target_node_did: node_did.clone(), agent_did: b.agent_did.clone(),
            name: "child".into(), description: Some("A child worker".into()), tags: Vec::new(),
        };
        publish(&access, vec![(Collection::AgentTarget, serde_json::to_value(&target)?)]).await?;
        let mut tools_a = tools_a;
        tools_a.agents = Some(gents::documents::AgentTools { enabled: Some(true), target_ids: vec![target.target_id.clone()] });
        publish(&access, vec![(Collection::Tools, serde_json::to_value(&tools_a)?)]).await?;
        access.transact_write("test.live_delegation_invoke", |txn| {
            let (node_did, a_did, b_did) = (&node_did, &a.agent_did, &b.agent_did);
            Box::pin(async move {
                let (doc, _) = read_desired_state_record_in_txn(txn, Collection::Agent, node_did, b_did)
                    .await?.context("child Agent unavailable")?;
                txn.add_actor_relationship("Agent", &doc, "session_writer", &a_did.parse()?).await?;
                Ok::<_, anyhow::Error>(())
            })
        }).await?;
        let parent_session = Uuid::new_v4().to_string();
        let prompt = format!("Use agent_new once to delegate to child: 'Say hello and identify yourself as child.' Reference {}. Do not answer in place of the child.", Uuid::new_v4());
        let parent = core.submit_request(&parent_session, &node_did, &prompt, Some(&a.agent_did)).await?;
        let parent_rows = access.execute(&format!("{{ AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}) {{_docID}} }}", escape_graphql_string(&parent.request_id))).await?;
        let parent_doc = parent_rows["data"]["AgentRequest"][0]["_docID"].as_str().context("parent request doc missing")?.to_owned();
        let deadline = Instant::now() + Duration::from_secs(240);
        let (child_request, child_session, child_doc, tool_doc) = loop {
            let rows = access.execute(&format!("{{ AgentRequest(filter: {{caused_by_parent_request_doc_id: {{_eq: \"{}\"}}}}) {{_docID request_id session_id agent_did requester_did caused_by_parent_request_doc_id caused_by_parent_tool_call_doc_id}} }}", escape_graphql_string(&parent_doc))).await?;
            let rows = rows["data"]["AgentRequest"].as_array().context("child requests unavailable to paired User")?;
            let matching: Vec<_> = rows.iter().filter(|r| r["agent_did"] == b.agent_did).collect();
            anyhow::ensure!(matching.len() <= 1, "multiple child requests: {rows:?}");
            if let Some(row) = matching.first() {
                anyhow::ensure!(row["requester_did"] == a.agent_did, "child not requested by coordinator: {row}");
                break (
                    row["request_id"].as_str().context("child request id")?.to_owned(),
                    row["session_id"].as_str().context("child session id")?.to_owned(),
                    row["_docID"].as_str().context("child request doc")?.to_owned(),
                    row["caused_by_parent_tool_call_doc_id"].as_str().context("child tool doc")?.to_owned(),
                );
            }
            anyhow::ensure!(Instant::now() < deadline, "model never published a child request: {rows:?}");
            sleep(Duration::from_millis(250)).await;
        };
        let child_reply = wait_for_complete_agent_response_with_access(&access, &child_request, &child_session, Duration::from_secs(240)).await?;
        anyhow::ensure!(!child_reply.text.trim().is_empty(), "child did not respond");
        let child_page = gents_desktop_core::client::load_session_transcript_page_on(
            &access, &child_session, Some(&node_did), Some(&a.agent_did), None, Some(40),
        ).await?;
        anyhow::ensure!(child_page.store.transcript_messages.iter().any(|row|
            row.doc_id == child_reply.header_doc_id && row.message.request_doc_id.as_deref() == Some(child_doc.as_str())
        ), "paired User cannot read the child's selected canonical header");
        let observed = access.execute(&format!("{{ AgentToolCall(filter: {{_docID: {{_eq: \"{}\"}}}}) {{_docID request_doc_id tool_name agent_did creator_did parent_linked lifecycle_state tool_failure_class}} AgentSession(filter: {{session_id: {{_eq: \"{}\"}}}}) {{_docID agent_did requester_did provenance archive_revision}} InferenceCall(filter: {{request_id: {{_in: [\"{}\", \"{}\"]}}}}) {{_docID request_id agent_did model_name call_state}} }}", escape_graphql_string(&tool_doc), escape_graphql_string(&child_session), escape_graphql_string(&parent.request_id), escape_graphql_string(&child_request))).await?;
        let tool = &observed["data"]["AgentToolCall"][0];
        assert_eq!(tool["tool_name"], "agent_new");
        assert_eq!(tool["request_doc_id"], parent_doc);
        assert_eq!(tool["agent_did"], a.agent_did);
        assert_eq!(tool["creator_did"], a.agent_did);
        assert_eq!(tool["parent_linked"], true);
        assert!(tool["tool_failure_class"].is_null(), "delegation failed: {tool}");
        assert!(!matches!(tool["lifecycle_state"].as_str(), Some("failed" | "timedOut" | "cancelled")), "delegation did not complete successfully: {tool}");
        let session = &observed["data"]["AgentSession"][0];
        assert_eq!(session["agent_did"], b.agent_did);
        assert_eq!(session["requester_did"], a.agent_did);
        assert_eq!(session["provenance"]["parent_request_doc_id"], parent_doc);
        let session_doc = session["_docID"].as_str().context("child session doc")?.to_owned();
        let calls = observed["data"]["InferenceCall"].as_array().context("User cannot see inference calls")?;
        let mut inference_docs = Vec::new();
        for (id, agent) in [(&parent.request_id, &a.agent_did), (&child_request, &b.agent_did)] {
            let call = calls.iter().find(|r| r["request_id"] == *id && r["agent_did"] == *agent && r["model_name"] == model_name && r["call_state"] == "completed").with_context(|| format!("no completed model call for {id}: {calls:?}"))?;
            inference_docs.push(call["_docID"].as_str().context("inference doc")?.to_owned());
        }
        let header = access.execute(&format!("{{ AgentMessage(filter: {{_docID: {{_eq: \"{}\"}}}}) {{_docID request_doc_id session_doc_id creator_did agent_did parent_linked}} }}", escape_graphql_string(&child_reply.header_doc_id))).await?;
        let header = &header["data"]["AgentMessage"][0];
        assert_eq!(header["request_doc_id"], child_doc);
        assert_eq!(header["session_doc_id"], session_doc);
        assert_eq!(header["creator_did"], b.agent_did);
        assert_eq!(header["agent_did"], b.agent_did);
        assert_eq!(header["parent_linked"], true);
        inference_docs.push(child_reply.header_doc_id.clone());
        let revision = session["archive_revision"].as_u64().unwrap_or(0);
        let archived = core.set_session_archived(&node_did, &child_session, revision, true).await?;
        assert_eq!(archived.outcome, gents_protocol::session::SessionArchiveOutcome::Applied);
        assert!(archived.archived_at.is_some());
        let next_revision = archived.revision.parse::<u64>()?;
        let restored = core.set_session_archived(&node_did, &child_session, next_revision, false).await?;
        assert_eq!(restored.outcome, gents_protocol::session::SessionArchiveOutcome::Applied);
        assert!(restored.archived_at.is_none());
        // Revoke only the explicit caller-to-target authority after the successful
        // delegation, then exercise the same target in a fresh full Agent turn.
        access.transact_write("test.live_delegation_revoke", |txn| {
            let (node_did, a_did, b_did) = (&node_did, &a.agent_did, &b.agent_did);
            Box::pin(async move {
                let (doc, _) = read_desired_state_record_in_txn(txn, Collection::Agent, node_did, b_did)
                    .await?.context("child Agent unavailable to its User owner")?;
                txn.delete_actor_relationship("Agent", &doc, "session_writer", &a_did.parse()?).await?;
                Ok::<_, anyhow::Error>(())
            })
        }).await?;
        let runtime_identity = identity_from_init(&init)?;
        let a_signer = gents::store::identity::resolve_agent_identity(&runtime_identity, &a)?;
        let agent_access = ConfigAccess::Graphql(
            gents::store::config_client::GraphqlEndpoint::as_principal(&graphql, &a.agent_did)
                .with_client_signing_capability(a_signer.client_signing_capability()?)?,
        );
        for did in [&b.agent_did, &format!("{}:missing", b.agent_did)] {
            let rows = agent_access.execute(&format!("{{ Agent(filter: {{agent_did: {{_eq: \"{}\"}}}}) {{_docID}} }}", escape_graphql_string(did))).await?;
            assert_eq!(rows["data"]["Agent"], json!([]), "hidden and missing targets must both be opaque: {rows}");
        }
        let denied_session = Uuid::new_v4().to_string();
        let denied_prompt = format!("Use agent_new once to delegate to child: 'Say hello.' Request {}. Do not answer in place of the child.", Uuid::new_v4());
        let denied = core.submit_request(&denied_session, &node_did, &denied_prompt, Some(&a.agent_did)).await?;
        let deadline = Instant::now() + Duration::from_secs(240);
        let (denied_doc, denied_tool) = loop {
            let rows = access.execute(&format!("{{ AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}) {{_docID lifecycle_state}} AgentToolCall(filter: {{request_id: {{_eq: \"{}\"}}, tool_name: {{_eq: \"agent_new\"}}}}) {{_docID lifecycle_state tool_failure_class}} }}", escape_graphql_string(&denied.request_id), escape_graphql_string(&denied.request_id))).await?;
            let request = &rows["data"]["AgentRequest"][0];
            let tools = rows["data"]["AgentToolCall"].as_array().context("denied turn tool rows unavailable")?;
            anyhow::ensure!(tools.len() <= 1, "more than one agent_new in denied turn: {tools:?}");
            if request["lifecycle_state"] == "completed" {
                let tool = tools.first().context("the model completed without an agent_new attempt; denial was not exercised")?;
                anyhow::ensure!(tool["lifecycle_state"] == "failed" && tool["tool_failure_class"] == "serviceUnavailable", "agent_new was not denied as unavailable: {tool}");
                break (request["_docID"].as_str().context("denied request doc")?.to_owned(), tool["_docID"].as_str().context("denied tool doc")?.to_owned());
            }
            anyhow::ensure!(request["lifecycle_state"] != "failed", "denied parent turn failed before model-facing result: {request}");
            anyhow::ensure!(Instant::now() < deadline, "denied turn did not complete: {rows}");
            sleep(Duration::from_millis(250)).await;
        };
        let children = access.execute(&format!("{{ AgentRequest(filter: {{caused_by_parent_request_doc_id: {{_eq: \"{}\"}}}}) {{_docID}} }}", escape_graphql_string(&denied_doc))).await?;
        assert_eq!(children["data"]["AgentRequest"], json!([]), "denied tool published a child: {children}");
        let calls = access.execute(&format!("{{ InferenceCall(filter: {{request_id: {{_eq: \"{}\"}}}}) {{agent_did model_name call_state}} }}", escape_graphql_string(&denied.request_id))).await?;
        anyhow::ensure!(calls["data"]["InferenceCall"].as_array().is_some_and(|rows| rows.iter().any(|row| row["agent_did"] == a.agent_did && row["model_name"] == model_name && row["call_state"] == "completed")), "denied turn did not run the selected model: {calls}");
        let segments = access.execute(&format!("{{ AgentOutputSegment(filter: {{request_doc_id: {{_eq: \"{}\"}}}}) {{source ordinal payload close}} }}", escape_graphql_string(&denied_doc))).await?;
        let mut tool_segments: Vec<_> = segments["data"]["AgentOutputSegment"].as_array().context("denied tool output unavailable")?.iter()
            .filter(|row| row["source"]["kind"] == "tool_call" && row["source"]["tool_call_doc_id"] == denied_tool)
            .collect();
        tool_segments.sort_by_key(|row| row["ordinal"].as_u64().unwrap_or(u64::MAX));
        anyhow::ensure!(tool_segments.iter().any(|row| !row["close"].is_null()), "denied tool output has no durable close");
        let output: String = tool_segments.iter().filter_map(|row| row["payload"].as_str()).collect();
        anyhow::ensure!(!output.is_empty(), "denied tool output is empty");
        assert!(!output.contains(&b.agent_did), "denied tool disclosed the hidden Agent DID: {output}");
        assert!(!output.contains("no longer exists"), "denied tool asserted hidden target nonexistence: {output}");
        Arc::new(core).shutdown().await?;
        Ok(([a, b], (child_doc, session_doc, tool_doc, inference_docs)))
    }).await?;
    drop(server);
    let runtime_identity = identity_from_init(&init)?;
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
            .with_node_acp_enabled()
            .build()
            .await?,
    );
    for (binding, documents) in [
        (
            &bindings[0],
            vec![
                ("AgentRequest", &owned.0),
                ("AgentSession", &owned.1),
                ("AgentToolCall", &owned.2),
                ("InferenceCall", &owned.3[0]),
            ],
        ),
        (
            &bindings[1],
            vec![
                ("InferenceCall", &owned.3[1]),
                ("AgentMessage", &owned.3[2]),
            ],
        ),
    ] {
        let signer = gents::store::identity::resolve_agent_identity(&runtime_identity, binding)?;
        let access = ConfigAccess::local_managed_agent(
            node.clone(),
            binding.agent_did.parse()?,
            signer.as_ref(),
        )?;
        for (collection, doc) in documents {
            let owners = access
                .transact_readonly("test.live_delegation_owner", |txn| {
                    Box::pin(async move {
                        txn.document_owners(collection, std::slice::from_ref(doc))
                            .await
                            .map_err(anyhow::Error::from)
                    })
                })
                .await?;
            assert_eq!(
                owners[0].owner.as_ref().map(|did| did.as_str()),
                Some(binding.agent_did.as_str()),
                "wrong native {collection} owner"
            );
        }
    }
    node.shutdown().await?;
    Ok(())
}
