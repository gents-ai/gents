use super::*;
use gents::documents::{IntegrationTools, PluginToolRef, Tools};
use gents::store::config_client::{
    apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
    NodePermission,
};
use gents::{Collection, NodeIdentity};
use serde_json::json;
use std::sync::Arc;

const PARCEL: &str = "LivePluginDeniedParcel";
const NOTE: &str = "FixtureNote";
const PLUGIN: &str = "fixture/job_fixture";

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
        .transact_write("test.live_plugin_config", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await?;
    Ok(())
}

/// Prepare a foreign-owned document and prove the selected managed Agent can
/// read it but cannot update it. The first plugin write has an independently
/// verified successful baseline using that same held Agent signer.
async fn prepare_data(home: &std::path::Path, init: &Value, agent_did: &str) -> Result<String> {
    let identity = identity_from_init(init)?;
    let node_did = identity.did().to_owned();
    let data = home.join("data");
    let node = Arc::new(
        gents::store::local::store_key::open_home_store_key(home, &data)
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
    let policy = gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID;
    node.add_collection(&format!(
        "type {PARCEL} @policy(id: \"{policy}\", resource: \"configuration\") {{ status: String }}"
    ))
    .await?;
    let note_schema = fs::read_to_string(
        fixture_pack_dir("job_plugin_fixture").join("schemas/fixture_note.graphql"),
    )?;
    node.add_collection(&note_schema).await?;
    let admin = ConfigAccess::local_as(node.clone(), node_did.parse()?);
    for permission in [
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
        NodePermission::CollectionGet,
    ] {
        admin
            .grant_node_permission(&agent_did.parse()?, permission)
            .await?;
    }
    let rows = admin
        .execute(&format!(
            "{{ Agent(filter: {{agent_did: {{_eq: \"{}\"}}}}) {{agent_did creation_nonce}} }}",
            escape_graphql_string(agent_did)
        ))
        .await?;
    let agent = &rows["data"]["Agent"][0];
    let binding = gents_protocol::agent_identity::AgentIdentityBinding {
        agent_did: agent_did.into(),
        creation_nonce: agent["creation_nonce"]
            .as_str()
            .context("Agent identity binding missing")?
            .into(),
    };
    let signer = gents::store::identity::resolve_agent_identity(&identity, &binding)?;
    let actor =
        ConfigAccess::local_managed_agent(node.clone(), agent_did.parse()?, signer.as_ref())?;
    let created = admin
        .write(
            "test.live_plugin_parcel",
            &format!("mutation {{ create_{PARCEL}(input: {{status: \"original\"}}) {{_docID}} }}"),
        )
        .await?;
    let parcel = gents::store::graphql::created_doc_id(&created, PARCEL)?;
    admin
        .add_actor_relationship(PARCEL, &parcel, "reader", &agent_did.parse()?)
        .await?;
    let actor_id = gents::defra_node::AcpIdentity::Authenticated(agent_did.parse()?);
    let acp = node.document_acp();
    anyhow::ensure!(
        acp.check_doc_access(
            &actor_id,
            gents::defra_node::DocumentPermission::Read,
            policy,
            "configuration",
            &parcel
        )
        .await?,
        "Agent must read fixture parcel"
    );
    anyhow::ensure!(
        !acp.check_doc_access(
            &actor_id,
            gents::defra_node::DocumentPermission::Update,
            policy,
            "configuration",
            &parcel
        )
        .await?,
        "Agent must not update fixture parcel"
    );
    actor
        .write(
            "test.live_plugin_baseline",
            &format!("mutation {{ create_{NOTE}(input: {{text: \"baseline\"}}) {{_docID}} }}"),
        )
        .await?;
    drop(actor);
    drop(admin);
    node.shutdown().await?;
    Ok(parcel)
}

fn prepare_pack(root: &std::path::Path, parcel: &str) -> Result<Value> {
    copy_dir_all(&fixture_pack_dir("job_plugin_fixture"), root)?;
    let path = root.join("manifest.json");
    let mut manifest: Value = read_json_file(&path)?;
    let plugin = &mut manifest["plugins"][0];
    plugin["job"] = json!(false);
    plugin["data"] = json!({"collections":[NOTE,PARCEL],"ops":["insert","update"],"stamped":{}});
    let args = json!({"args":{"plan":[{"when":{},"output":{"response":"fixture writes emitted","writes":[
        {"op":"insert","collection":NOTE,"fields":{"text":"must-roll-back"}},
        {"op":"update","collection":PARCEL,"doc_id":parcel,"fields":{"status":"must-not-change"}}
    ]}}]}});
    plugin["input_schema"] = json!({"type":"object", "properties":{"args":{"type":"object","properties":{"plan":{"type":"array","items":{"type":"object","properties":{"when":{"type":"object"},"output":{"type":"object"}},"required":["when","output"]}}},"required":["plan"]}},"required":["args"]});
    manifest["assets"]
        .as_array_mut()
        .context("pack assets")?
        .push(json!("schemas/live_parcel.graphql"));
    manifest["schemas"]
        .as_array_mut()
        .context("pack schemas")?
        .push(json!("schemas/live_parcel.graphql"));
    write_json_file(&path, &manifest)?;
    fs::write(
        root.join("schemas/live_parcel.graphql"),
        format!(
            "type {PARCEL} @policy(id: \"{}\", resource: \"configuration\") {{ status: String }}",
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID
        ),
    )?;
    Ok(args)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live GLM: set GENTS_CLI_E2E_MODEL_ENDPOINT and GENTS_CLI_E2E_MODEL_NAME"]
async fn live_glm_plugin_denial_rolls_back_data_and_terminalizes_tool() -> Result<()> {
    let _guard = enrollment_e2e_lock().lock().await;
    let (endpoint, model) = resolve_live_inference().await?;
    anyhow::ensure!(
        model.to_ascii_lowercase().contains("glm"),
        "select a real GLM model, got {model}"
    );
    let temp = tempfile::Builder::new()
        .prefix("gents-live-plugin-denial-")
        .tempdir()?
        .keep();
    tracing::info!(path = %temp.display(), "retained live plugin fixture");
    let home = temp.join("runtime");
    let client = temp.join("client");
    let owner = temp.join("owner");
    fs::create_dir_all(&home)?;
    let home_arg = home.to_str().context("runtime home UTF-8")?;
    let owner_env = [(
        "GENTS_DESKTOP_HOME",
        owner.to_str().context("owner home UTF-8")?,
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
            "live-plugin-denial",
            "--model-name",
            &model,
            "--inference-url",
            &endpoint,
        ],
        &owner_env,
    )?;
    let node_did = node_did_from_init(&init)?;
    let agent_did = agent_did_from_init(&init)?;
    let parcel = prepare_data(&home, &init, &agent_did).await?;
    let pack = temp.join("pack");
    let args = prepare_pack(&pack, &parcel)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let (mut server, _) =
        spawn_server_with_ready_json(&home, port, &["--home", home_arg], &owner_env)?;
    wait_for_port(port, &mut server)?;
    server.capturing(async {
        wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
        run_cli_json_with_env(&home, &["pack", "install", pack.to_str().context("pack path UTF-8")?, "--home", home_arg, "--graphql", &graphql], &owner_env)?;
        let core = ClientCore::start_with_paths_and_options(DesktopPaths::from_root(&client), ClientCoreOptions::local_only()).await?;
        let (status, _) = wait_for_enrollment_token(&format!("http://127.0.0.1:{port}")).await?;
        let pending = core.request_status_enrollment_with_label(&status, Some("plugin denial")).await?;
        wait_for_runtime_enrollment_request(&graphql, &pending.request_id).await?;
        run_cli_json_with_env(&home, &["p2p", "enrollment", "grant", &pending.request_id, "--home", home_arg], &owner_env)?;
        run_cli_json_with_env(&home, &["p2p", "enrollment", "approve", &pending.request_id, "--home", home_arg], &owner_env)?;
        wait_for_chat_ready_enrollment(&core, &node_did).await?;
        wait_for_client_node_readiness(&core, &node_did).await?;
        let snapshot = core.store().snapshot();
        let mut agent = snapshot.agents.iter().find(|row| row.node_did == node_did && row.agent_did == agent_did).cloned().context("selected Agent unavailable")?;
        let mut context = snapshot.contexts.iter().find(|row| Some(&row.context_id) == agent.context_id.as_ref() && row.node_did == node_did).cloned().context("selected AgentContext unavailable")?;
        let tools = Tools { tools_id: "live-plugin-denial-tools".into(), node_did: node_did.clone(), integrations: Some(IntegrationTools {
            plugins: Some(vec![PluginToolRef { plugin: PLUGIN.into(), digest: None, tool_calls: false, input_fields: Vec::new(), job: None }]), ..Default::default()
        }), ..Default::default() };
        context.context_id = "live-plugin-denial-context".into();
        context.tools_id = Some(tools.tools_id.clone());
        context.skill_ids.clear();
        context.compaction_id = None;
        context.system_prompt = Some("Call the requested fixture tool exactly once with the supplied JSON arguments. After the tool returns, report its result without retrying.".into());
        let access = core.operator_access(&node_did)?;
        publish(&access, vec![(Collection::Tools, serde_json::to_value(&tools)?), (Collection::AgentContext, serde_json::to_value(&context)?)]).await?;
        agent.context_id = Some(context.context_id.clone());
        core.save_agent(&agent).await?;
        let session = Uuid::new_v4().to_string();
        let prompt = format!("Call job_fixture exactly once with these exact JSON arguments: {}. Do not change any value or retry. Report the result.", args);
        let receipt = core.submit_request(&session, &node_did, &prompt, Some(&agent_did)).await?;
        let _reply = wait_for_complete_agent_response_with_access(&access, &receipt.request_id, &session, Duration::from_secs(240)).await?;
        let observed = access.execute(&format!("{{ InferenceCall(filter: {{request_id: {{_eq: \"{}\"}}}}) {{model_name agent_did call_state}} AgentToolCall(filter: {{request_id: {{_eq: \"{}\"}}}}) {{_docID node_did requester_did session_id request_doc_id tool_name lifecycle_state tool_failure_class}} }}", escape_graphql_string(&receipt.request_id), escape_graphql_string(&receipt.request_id))).await?;
        let calls = observed["data"]["InferenceCall"].as_array().context("missing inference evidence")?;
        anyhow::ensure!(calls.iter().any(|call| call["model_name"] == model && call["agent_did"] == agent_did && call["call_state"] == "completed"), "missing completed real model call: {calls:?}");
        let tools = observed["data"]["AgentToolCall"].as_array().context("missing tool calls")?;
        let selected: Vec<_> = tools.iter().filter(|call| call["tool_name"] == "job_fixture").collect();
        anyhow::ensure!(selected.len() == 1, "expected one plugin call: {tools:?}");
        let call = selected[0];
        anyhow::ensure!(call["lifecycle_state"] == "failed" && call["tool_failure_class"] == "toolReturnedError", "plugin did not settle failed: {call}");
        let actual_args = gents::tools::call::lifecycle::load_tool_call_arguments(&access, call["_docID"].as_str().context("tool doc")?, &node_did, &session, call["requester_did"].as_str()).await?;
        let actual_args: Value = serde_json::from_str(&actual_args).context("plugin argument JSON")?;
        anyhow::ensure!(actual_args == args, "model changed the fixture writes: {actual_args}");
        let tool_result = gents::tools::call::lifecycle::load_tool_call_result(&access, call["_docID"].as_str().context("tool doc")?, &node_did, &session, call["requester_did"].as_str()).await?;
        let text = gents::tools::call::lifecycle::render_tool_result(&tool_result)?;
        anyhow::ensure!(text.contains("plugin data writes were refused and nothing was committed"), "failure did not reach the deferred write owner: {text}");
        Arc::new(core).shutdown().await?;
        Ok::<(), anyhow::Error>(())
    }).await?;
    drop(server);
    let identity = identity_from_init(&init)?;
    let data = home.join("data");
    let node = Arc::new(
        gents::store::local::store_key::open_home_store_key(&home, &data)
            .await?
            .encrypt(
                gents::defra_node::Node::builder()
                    .data_path(&data)
                    .with_storage_backend(gents::defra_node::StorageBackend::Regolith),
            )
            .with_node_identity_did(identity.did())
            .with_node_acp_enabled()
            .build()
            .await?,
    );
    let admin = ConfigAccess::local_as(node.clone(), identity.did().parse()?);
    let stored = admin
        .execute(&format!("{{ {NOTE} {{text}} {PARCEL} {{status}} }}"))
        .await?;
    anyhow::ensure!(
        stored["data"][NOTE] == json!([{"text":"baseline"}]),
        "permitted write escaped rollback: {stored}"
    );
    anyhow::ensure!(
        stored["data"][PARCEL] == json!([{"status":"original"}]),
        "denied write changed parcel: {stored}"
    );
    drop(admin);
    node.shutdown().await?;
    Ok(())
}
