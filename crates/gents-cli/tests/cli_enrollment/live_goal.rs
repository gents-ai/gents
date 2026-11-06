use super::*;
use gents::goal::{resume_goal_request, set_goal_from_access, GoalStatus};
use gents::store::config_client::{
    apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::{Collection, NodeIdentity, Tools};
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
        .transact_write("test.live_goal_config", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live GLM: set GENTS_CLI_E2E_MODEL_ENDPOINT and GENTS_CLI_E2E_MODEL_NAME"]
async fn paired_user_resumes_goal_and_observes_real_agent_turn() -> Result<()> {
    let _guard = enrollment_e2e_lock().lock().await;
    let (endpoint, model) = resolve_live_inference().await?;
    anyhow::ensure!(
        model == "GLM-5.3-Flash-NVFP4",
        "select the live GLM 5.3 Flash model, got {model}"
    );
    let temp = tempfile::Builder::new()
        .prefix("gents-live-user-goal-")
        .tempdir()?
        .keep();
    tracing::info!(path = %temp.display(), "retained live Goal fixture");
    let home = temp.join("runtime");
    let client_home = temp.join("client");
    let owner_home = temp.join("owner");
    fs::create_dir_all(&home)?;
    let home_arg = home.to_str().context("runtime home UTF-8")?;
    let owner_env = [(
        "GENTS_DESKTOP_HOME",
        owner_home.to_str().context("owner home UTF-8")?,
    )];
    let init = run_cli_json_with_env(
        &home,
        &[
            "init",
            "--store-key-custody",
            "file",
            "--home",
            home_arg,
            "--node-name",
            "live-user-goal",
            "--model-name",
            &model,
            "--max-concurrent",
            "1",
            "--max-queue-depth",
            "4",
            "--inference-url",
            &endpoint,
        ],
        &owner_env,
    )?;
    let node_did = node_did_from_init(&init)?;
    let default_agent = agent_did_from_init(&init)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let (mut server, _) =
        spawn_server_with_ready_json(&home, port, &["--home", home_arg], &owner_env)?;
    wait_for_port(port, &mut server)?;

    let result = server.capturing(async {
        wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
        let core = ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(&client_home), ClientCoreOptions::local_only(),
        ).await?;
        let outcome: Result<()> = async {
            let (status, _) = wait_for_enrollment_token(&format!("http://127.0.0.1:{port}")).await?;
            let pending = core.request_status_enrollment_with_label(&status, Some("live Goal")).await?;
            wait_for_runtime_enrollment_request(&graphql, &pending.request_id).await?;
            run_cli_json_with_env(&home, &["p2p", "enrollment", "grant", &pending.request_id, "--home", home_arg], &owner_env)?;
            run_cli_json_with_env(&home, &["p2p", "enrollment", "approve", &pending.request_id, "--home", home_arg], &owner_env)?;
            wait_for_chat_ready_enrollment(&core, &node_did).await?;
            wait_for_client_node_readiness(&core, &node_did).await?;
            let access = core.operator_access(&node_did)?;
            let user_did = core.node_identity().did().to_owned();
            let runtime = identity_from_init(&init)?;
            anyhow::ensure!(runtime.did() == node_did && user_did != node_did, "User and runtime identities differ from the initialized Node");

            let snapshot = core.store().snapshot();
            let template = snapshot.agents.iter().find(|row| row.node_did == node_did && row.agent_did == default_agent)
                .cloned().context("default Agent unavailable")?;
            let mut context = snapshot.contexts.iter().find(|row| row.node_did == node_did && Some(&row.context_id) == template.context_id.as_ref())
                .cloned().context("default AgentContext unavailable")?;
            let mut profile = snapshot.inference_profiles.iter().find(|row| row.node_did == node_did && row.profile_id == template.inference_profile_id)
                .cloned().context("default inference profile unavailable")?;
            profile.profile_id = "live-goal-profile".into();
            profile.execution_id = Some("live-goal-execution".into());
            profile.max_output_tokens = Some(1024);
            let tools = Tools { node_did: node_did.clone(), tools_id: "live-goal-tools".into(), ..Default::default() };
            context.context_id = "live-goal-context".into();
            context.tools_id = Some(tools.tools_id.clone());
            context.skill_ids.clear();
            context.compaction_id = None;
            context.system_prompt = Some("Answer directly and briefly in one sentence. Do not use tools.".into());
            publish(&access, vec![
                (Collection::InferenceExecution, json!({"node_did":node_did,"execution_id":"live-goal-execution","max_turns":1,"max_total_tokens":8192})),
                (Collection::InferenceProfile, serde_json::to_value(&profile)?),
                (Collection::Tools, serde_json::to_value(&tools)?),
                (Collection::AgentContext, serde_json::to_value(&context)?),
            ]).await?;
            let binding = core.provision_agent_identity(&node_did).await?;
            anyhow::ensure!(binding.agent_did != user_did && binding.agent_did != node_did, "selected Agent must be distinct from User and runtime");
            let mut agent = template;
            agent.agent_did = binding.agent_did.clone();
            agent.creation_nonce = binding.creation_nonce.clone();
            agent.context_id = Some(context.context_id.clone());
            agent.inference_profile_id = profile.profile_id.clone();
            agent.display_name = Some("live Goal Agent".into());
            core.save_agent(&agent).await?;

            let session = Uuid::new_v4().to_string();
            let predecessor = core.submit_request(&session, &node_did,
                "Answer briefly: what is one plus one?", Some(&binding.agent_did)).await?;
            let predecessor_reply = wait_for_complete_agent_response_with_access(
                &access, &predecessor.request_id, &session, Duration::from_secs(240),
            ).await?;
            anyhow::ensure!(!predecessor_reply.text.trim().is_empty(), "predecessor produced no response");
            let goal = set_goal_from_access(&access, &node_did, &session,
                Some("Give one short completion sentence for the resumed task."),
                Some(GoalStatus::Paused), Some(Some(16000)), None,
            ).await?;
            anyhow::ensure!(goal.status == "paused" && goal.assignment_root_request_doc_id.is_none(), "unexpected initial Goal: {goal:?}");
            let before = access.execute(&format!(
                r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID node_did requester_did agent_did }} AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID lifecycle_state }} Agent(filter: {{ agent_did: {{ _eq: "{}" }} }}) {{ _docID node_did agent_did }} }}"#,
                escape_graphql_string(&session), escape_graphql_string(&predecessor.request_id), escape_graphql_string(&binding.agent_did),
            )).await?;
            let sessions = before["data"]["AgentSession"].as_array().context("User cannot read session")?;
            let requests = before["data"]["AgentRequest"].as_array().context("User cannot read predecessor")?;
            let agents = before["data"]["Agent"].as_array().context("User cannot read selected Agent")?;
            anyhow::ensure!(sessions.len() == 1 && requests.len() == 1 && agents.len() == 1, "ambiguous session, Agent or predecessor: {before}");
            let session_doc = sessions[0]["_docID"].as_str().context("session physical ID")?.to_owned();
            let parent_doc = requests[0]["_docID"].as_str().context("predecessor physical ID")?.to_owned();
            let agent_doc = agents[0]["_docID"].as_str().context("Agent physical ID")?.to_owned();
            anyhow::ensure!(sessions[0]["node_did"] == node_did && sessions[0]["requester_did"] == user_did && sessions[0]["agent_did"] == binding.agent_did && requests[0]["lifecycle_state"] == "completed", "incorrect predecessor scope: {before}");
            let owners = access.transact_readonly("test.live_goal_owners", |txn| {
                let session_doc = &session_doc;
                let goal_doc = &goal.doc_id;
                Box::pin(async move {
                    let goal_owner = txn.document_owners("Goal", std::slice::from_ref(goal_doc)).await?;
                    let session_owner = txn.document_owners("AgentSession", std::slice::from_ref(session_doc)).await?;
                    Ok::<_, anyhow::Error>((goal_owner, session_owner))
                })
            }).await?;
            for owners in [owners.0, owners.1] {
                anyhow::ensure!(owners.len() == 1 && owners[0].owner.as_ref().map(|did| did.as_str()) == Some(user_did.as_str()), "Goal and session must be User-owned");
            }

            let resumed = resume_goal_request(&access, core.node_identity(), &node_did, &session, &predecessor.request_id).await?;
            anyhow::ensure!(resumed.created && resumed.goal_id == goal.goal_id, "unexpected signed resume receipt: {resumed:?}");
            let deadline = Instant::now() + Duration::from_secs(120);
            let claimed = loop {
                let response = access.execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID request_id node_did requester_did agent_did session_id session_doc_id agent_doc_id admission_kind admission_signer_did relationships_pending input caused_by_trigger_kind caused_by_trigger_id caused_by_parent_request_id caused_by_parent_request_doc_id lifecycle_state failure_reason }} }}"#,
                    escape_graphql_string(&resumed.doc_id),
                )).await?;
                let rows = response["data"]["AgentRequest"].as_array().context("User cannot read continuation")?;
                if let [row] = rows.as_slice() {
                    match row["lifecycle_state"].as_str() {
                        Some("claimed" | "processing") => break row.clone(),
                        Some("completed" | "failed" | "interrupted" | "superseded") => bail!("continuation became terminal before the Goal could be paused: {row}"),
                        _ => {}
                    }
                }
                anyhow::ensure!(Instant::now() < deadline, "User-signed Goal continuation was not claimed: {response}");
                sleep(Duration::from_millis(50)).await;
            };
            anyhow::ensure!(claimed["request_id"] == resumed.request_id && claimed["node_did"] == node_did
                && claimed["requester_did"] == user_did && claimed["agent_did"] == binding.agent_did
                && claimed["session_id"] == session && claimed["session_doc_id"] == session_doc && claimed["agent_doc_id"] == agent_doc
                && claimed["admission_kind"] == "client" && claimed["admission_signer_did"] == user_did
                && claimed["relationships_pending"] == false && claimed["input"]["queue"]["source"] == "goal"
                && claimed["caused_by_trigger_kind"] == "goal" && claimed["caused_by_trigger_id"] == goal.goal_id
                && claimed["caused_by_parent_request_id"] == predecessor.request_id
                && claimed["caused_by_parent_request_doc_id"] == parent_doc,
                "claimed continuation lost its signed physical provenance: {claimed}");
            let paused = set_goal_from_access(&access, &node_did, &session, None, Some(GoalStatus::Paused), None, None).await?;
            anyhow::ensure!(paused.status == "paused" && paused.assignment_root_request_doc_id.as_deref() == Some(resumed.doc_id.as_str()), "User pause lost its Goal assignment: {paused:?}");

            let reply = wait_for_complete_agent_response_with_access(&access, &resumed.request_id, &session, Duration::from_secs(240)).await?;
            wait_for_client_complete_response(&core, &session, &node_did, &resumed.request_id, &reply).await?;
            let evidence = access.execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID lifecycle_state terminal_output }} goal_children: AgentRequest(filter: {{ session_id: {{ _eq: "{}" }}, caused_by_trigger_kind: {{ _eq: "goal" }} }}) {{ _docID }} InferenceCall(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID request_id agent_did model_name call_state }} AgentMessage(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID request_doc_id session_doc_id agent_did creator_did parent_linked }} Goal(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID status assignment_root_request_doc_id }} }}"#,
                escape_graphql_string(&resumed.doc_id), escape_graphql_string(&session), escape_graphql_string(&resumed.request_id),
                escape_graphql_string(&reply.header_doc_id), escape_graphql_string(&goal.doc_id),
            )).await?;
            let calls = evidence["data"]["InferenceCall"].as_array().context("User cannot see provider evidence")?;
            anyhow::ensure!(calls.iter().any(|call| call["request_id"] == resumed.request_id && call["agent_did"] == binding.agent_did && call["model_name"] == model && call["call_state"] == "completed"), "no completed selected GLM inference: {evidence}");
            let rows = evidence["data"]["AgentRequest"].as_array().context("User cannot read completed continuation")?;
            let children = evidence["data"]["goal_children"].as_array().context("User cannot read Goal requests")?;
            let messages = evidence["data"]["AgentMessage"].as_array().context("User cannot read canonical response")?;
            let goals = evidence["data"]["Goal"].as_array().context("User cannot read paused Goal")?;
            anyhow::ensure!(rows.len() == 1 && rows[0]["lifecycle_state"] == "completed" && children.len() == 1 && children[0]["_docID"] == resumed.doc_id && messages.len() == 1
                && messages[0]["request_doc_id"] == resumed.doc_id && messages[0]["session_doc_id"] == session_doc
                && messages[0]["agent_did"] == binding.agent_did && messages[0]["creator_did"] == binding.agent_did
                && messages[0]["parent_linked"] == true && goals.len() == 1 && goals[0]["status"] == "paused"
                && goals[0]["assignment_root_request_doc_id"] == resumed.doc_id,
                "claimed turn did not complete under its paused Goal: {evidence}");
            let owner = access.transact_readonly("test.live_goal_continuation_owner", |txn| {
                let doc = &resumed.doc_id;
                Box::pin(async move { txn.document_owners("AgentRequest", std::slice::from_ref(doc)).await.map_err(anyhow::Error::from) })
            }).await?;
            anyhow::ensure!(owner.len() == 1 && owner[0].owner.as_ref().map(|did| did.as_str()) == Some(user_did.as_str()), "continuation was not User-owned");
            Ok(())
        }.await;
        let shutdown = Arc::new(core).shutdown().await;
        outcome?;
        shutdown?;
        Ok(())
    }).await;

    let signal = tokio::process::Command::new("kill")
        .arg("-TERM")
        .arg(server.child.id().to_string())
        .status()
        .await?;
    anyhow::ensure!(signal.success(), "could not signal live runtime shutdown");
    let stopped = Instant::now() + Duration::from_secs(30);
    while server.child.try_wait()?.is_none() {
        anyhow::ensure!(
            Instant::now() < stopped,
            "live runtime did not stop after SIGTERM"
        );
        sleep(Duration::from_millis(50)).await;
    }
    result
}
