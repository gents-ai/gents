use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::Node as EmbeddedNode;
use gents::{
    ensure_node, AgentToolSurfaceConfig, BackendProviderKind, KeyIdentity, ResolvedAgent,
    RuntimeNode,
};

pub fn fixture_pack_names() -> Vec<String> {
    const KNOWN: [&str; 8] = [
        "assets_fixture",
        "documents_fixture",
        "slot_fixture",
        "dependent_fixture",
        "dependee_fixture",
        "bind_plugin_fixture",
        "job_plugin_fixture",
        "plugin_documents_fixture",
    ];
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packs");
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("reading {}: {error}", root.display()))
    {
        let path = entry
            .unwrap_or_else(|error| panic!("reading an entry of {}: {error}", root.display()))
            .path();
        if !path.is_dir() {
            continue;
        }
        let manifest = path.join("manifest.json");
        let bytes = std::fs::read(&manifest)
            .unwrap_or_else(|error| panic!("reading {}: {error}", manifest.display()));
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .unwrap_or_else(|error| panic!("parsing {}: {error}", manifest.display()));
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| panic!("{} is not a UTF-8 name", path.display()));
        names.push(name.to_owned());
    }
    names.sort();
    for known in KNOWN {
        assert!(
            names.iter().any(|name| name == known),
            "fixture {known} is missing from {}",
            root.display()
        );
    }
    names
}

pub fn test_identity(name: &str) -> KeyIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    KeyIdentity::load_or_create(path, None).unwrap()
}

pub fn test_node_for(
    identity: Arc<dyn gents::NodeIdentity>,
    default_agent_did: impl Into<String>,
) -> Arc<RuntimeNode> {
    Arc::new(RuntimeNode {
        node_did: identity.did().to_string(),
        identity,
        default_agent_did: default_agent_did.into(),
        display_name: None,
        enabled: true,
    })
}

pub fn test_agent(
    name: &str,
    backend_id: &str,
    backend_api_key_env_var: Option<&str>,
) -> ResolvedAgent {
    let identity: Arc<dyn gents::NodeIdentity> = Arc::new(test_identity(name));
    let principal = test_node_for(identity, name);
    let mut behavior = test_agent_for_node(name, principal);
    behavior.backend_id = Some(backend_id.to_owned());
    behavior.backend_auth = match backend_api_key_env_var {
        Some(variable) => gents::documents::BackendAuth::Environment {
            variable: variable.into(),
        },
        None => gents::documents::BackendAuth::Unauthenticated,
    };
    behavior
}

pub fn test_agent_for_node(
    agent_did: impl Into<String>,
    principal: Arc<RuntimeNode>,
) -> ResolvedAgent {
    let _display_name = agent_did.into();
    let binding =
        gents::store::identity::provision_agent_identity(principal.identity.as_ref()).unwrap();
    let agent_identity =
        gents::store::identity::resolve_agent_identity(principal.identity.as_ref(), &binding)
            .unwrap();
    test_agent_with_identity(principal, agent_identity)
}

pub fn test_agent_with_identity(
    principal: Arc<RuntimeNode>,
    agent_identity: Arc<dyn gents::NodeIdentity>,
) -> ResolvedAgent {
    let agent_did = agent_identity.did().to_owned();
    ResolvedAgent {
        configuration: gents::inference::resolved::ResolvedConfiguration::Static,
        profile_id: None,
        skills: Vec::new(),
        oauth_pool: Vec::new(),
        agent_identity,
        agent_did,
        node: principal,
        backend_id: None,
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        openai_wire_api: gents::OpenAiWireApi::ChatCompletions,
        backend_endpoint: "http://localhost:8000/v1".to_string(),
        claude_platform: None,
        backend_auth: gents::documents::BackendAuth::Unauthenticated,
        model_name: gents::inference::resolved::DEFAULT_MODEL_NAME.to_string(),
        service_tier: None,
        resolved_reasoning_efforts: None,
        context_window: gents::inference::resolved::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: gents::inference::resolved::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: gents::inference::resolved::DEFAULT_MAX_TURNS,
        max_turns_provenance: gents::inference::resolved::MaxTurnsProvenance::Default,
        system_prompt: String::new(),
        tools: AgentToolSurfaceConfig::default(),
        compaction: None,
        compaction_inference: None,
        max_total_tokens: None,
        stream_batch_ms: gents::inference::resolved::DEFAULT_STREAM_BATCH_MS,
        stream_liveness_timeout: Duration::from_secs(
            gents::inference::resolved::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
        ),
        deadline_duration: Duration::from_secs(
            gents::inference::resolved::DEFAULT_DEADLINE_DURATION_SECS,
        ),
        provider_idle_timeout: Duration::from_secs(
            gents::inference::resolved::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
        ),
        completion_retry:
            gents::runtime::agent::completion_retry::CompletionRetryProfileFields::default(),
        sampling: gents::inference::resolved::SamplingConfig::default(),
    }
}

pub async fn session_command_agent_snapshot(
    db: &super::TestDb,
) -> tokio::sync::watch::Receiver<
    Arc<gents::runtime::agent::p2p_reconcile::session_commands::SessionCommandAgentSnapshot>,
> {
    let access =
        gents::ConfigAccess::local_as(db.node.clone(), db.node_identity.did().parse().unwrap());
    let response = access
        .execute("{ Agent { agent_did creation_nonce node_did } }")
        .await
        .unwrap();
    let mut agents = rapidhash::RapidHashMap::default();
    for row in response["data"]["Agent"].as_array().expect("Agent rows") {
        if row["node_did"].as_str() != Some(db.node_identity.did()) {
            continue;
        }
        let binding = gents_protocol::agent_identity::AgentIdentityBinding {
            agent_did: row["agent_did"].as_str().expect("Agent DID").into(),
            creation_nonce: row["creation_nonce"]
                .as_str()
                .expect("Agent creation nonce")
                .into(),
        };
        let signer =
            gents::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
                .expect("held Agent identity");
        let node = test_node_for(db.node_identity.clone(), &binding.agent_did);
        let agent = Arc::new(test_agent_with_identity(node, signer));
        agents.insert(binding.agent_did, agent);
    }
    assert!(
        !agents.is_empty(),
        "session command tests require a held Agent"
    );
    let default_agent_did = agents.keys().next().unwrap().clone();
    let runtime_node = test_node_for(db.node_identity.clone(), &default_agent_did);
    let snapshot =
        gents::runtime::agent::p2p_reconcile::session_commands::SessionCommandAgentSnapshot {
            generation: 1,
            node: Some(runtime_node),
            local_did: db.node_identity.did().into(),
            default_agent_did,
            agents,
            tool_surfaces: Default::default(),
            backend_admission_configs: Default::default(),
            unavailable_agents: Default::default(),
            active_schedules: Default::default(),
            unavailable_schedules: Default::default(),
            active_event_triggers: Default::default(),
            unavailable_event_triggers: Default::default(),
            active_tasks: Default::default(),
            dispatchers: Default::default(),
            agent_executor_capacities: Default::default(),
            agent_executor_queue_capacities: Default::default(),
        };
    let (_sender, receiver) = tokio::sync::watch::channel(Arc::new(snapshot));
    receiver
}

pub async fn configure_child_agent(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_did: &str,
    tools_id: &str,
    targets: Vec<gents::AgentTargetDocument>,
    enabled: bool,
) -> String {
    use gents::documents::{AgentContext, AgentTools, Tools};
    use gents::store::config_client::{
        read_desired_state_record_in_txn as read, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    use gents::Collection;

    let root =
        gents::store::identity::RegisteredIdentity::from_registered_did(node_did, None).unwrap();
    let binding = gents::store::identity::provision_agent_identity(&root).unwrap();
    gents::ConfigAccess::transact_local_write(node, None, "test.configure_child_agent", |txn| {
        let targets = targets.clone();
        let binding = &binding;
        Box::pin(async move {
            let mut documents = Vec::new();
            let mut behavior = read(txn, Collection::Agent, node_did, agent_did)
                .await?
                .map(|(_, value)| serde_json::from_value::<gents::AgentDocument>(value))
                .transpose()?
                .unwrap_or_else(|| gents::AgentDocument {
                    agent_did: binding.agent_did.clone(),
                    creation_nonce: binding.creation_nonce.clone(),
                    node_did: node_did.to_string(),
                    display_name: Some(agent_did.to_string()),
                    description: None,
                    context_id: None,
                    inference_profile_id: format!("{agent_did}-inference"),
                    enabled: true,
                    tags: Vec::new(),
                    created_at: Some("2026-05-12T00:00:00Z".to_string()),
                });

            if read(
                txn,
                Collection::InferenceProfile,
                node_did,
                &behavior.inference_profile_id,
            )
            .await?
            .is_none()
            {
                let backend_id = format!("{agent_did}-test-backend");
                if read(txn, Collection::InferenceBackend, node_did, &backend_id)
                    .await?
                    .is_none()
                {
                    documents.push((
                        Collection::InferenceBackend,
                        serde_json::json!({
                            "node_did": node_did,
                            "backend_id": backend_id,
                            "name": format!("{agent_did} test backend"),
                            "provider_kind": "OpenAiCompatible",
                            "openai_wire_api": "chat_completions",
                            "endpoint": "http://127.0.0.1:1/v1",
                            "auth": {"kind": "unauthenticated"}
                        }),
                    ));
                }
                documents.push((
                    Collection::InferenceProfile,
                    serde_json::json!({
                        "node_did": node_did,
                        "profile_id": behavior.inference_profile_id,
                        "backend_id": backend_id,
                        "model_name": "test-model"
                    }),
                ));
            }

            let context_id = behavior
                .context_id
                .clone()
                .unwrap_or_else(|| format!("{agent_did}-test-context"));
            let mut context = read(txn, Collection::AgentContext, node_did, &context_id)
                .await?
                .map(|(_, value)| serde_json::from_value::<AgentContext>(value))
                .transpose()?
                .unwrap_or_else(|| AgentContext {
                    context_id: context_id.clone(),
                    node_did: node_did.to_string(),
                    display_name: None,
                    description: None,
                    system_prompt: None,
                    tools_id: None,
                    compaction_id: None,
                    skill_ids: Vec::new(),
                    tags: Vec::new(),
                });
            context.tools_id = Some(tools_id.to_string());

            let mut tools = read(txn, Collection::Tools, node_did, tools_id)
                .await?
                .map(|(_, value)| serde_json::from_value::<Tools>(value))
                .transpose()?
                .unwrap_or_else(|| Tools {
                    tools_id: tools_id.to_string(),
                    node_did: node_did.to_string(),
                    ..Default::default()
                });
            tools.agents = Some(AgentTools {
                target_ids: targets
                    .iter()
                    .map(|target| target.target_id.clone())
                    .collect(),
                enabled: Some(enabled),
            });
            behavior.context_id = Some(context_id);

            let selected_agent = behavior.agent_did.clone();
            documents.extend(targets.into_iter().map(|target| {
                (
                    Collection::AgentTarget,
                    serde_json::to_value(target).expect("serialize target fixture"),
                )
            }));
            documents.extend([
                (
                    Collection::Tools,
                    serde_json::to_value(tools).expect("serialize tools fixture"),
                ),
                (
                    Collection::AgentContext,
                    serde_json::to_value(context).expect("serialize context fixture"),
                ),
                (
                    Collection::Agent,
                    serde_json::to_value(behavior).expect("serialize behavior fixture"),
                ),
            ]);
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
            gents::store::config_client::apply_desired_state_plan(txn, &plan)
                .await
                .map(|_| selected_agent)
        })
    })
    .await
    .unwrap()
}

pub async fn configure_agent_tools(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_did: &str,
    system_prompt: Option<String>,
    tools: gents::documents::Tools,
    referenced_documents: Vec<(gents::Collection, serde_json::Value)>,
) {
    configure_agent_tools_as(
        &gents::ConfigAccess::local(node.clone()),
        node_did,
        agent_did,
        system_prompt,
        tools,
        referenced_documents,
    )
    .await;
}

pub async fn configure_agent_tools_as(
    access: &gents::ConfigAccess,
    node_did: &str,
    agent_did: &str,
    system_prompt: Option<String>,
    tools: gents::documents::Tools,
    referenced_documents: Vec<(gents::Collection, serde_json::Value)>,
) {
    use gents::documents::AgentContext;
    use gents::store::config_client::{
        read_desired_state_record_in_txn as read, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    use gents::Collection;

    assert_eq!(tools.node_did, node_did);
    access
        .transact_write("test.configure_agent_tools", |txn| {
            let tools = tools.clone();
            let referenced_documents = referenced_documents.clone();
            let system_prompt = system_prompt.clone();
            Box::pin(async move {
                let (_, behavior) = read(txn, Collection::Agent, node_did, agent_did)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("behavior {agent_did} is missing"))?;
                let mut behavior: gents::AgentDocument = serde_json::from_value(behavior)?;
                let context_id = behavior
                    .context_id
                    .clone()
                    .unwrap_or_else(|| format!("{agent_did}:context"));
                let mut context = read(txn, Collection::AgentContext, node_did, &context_id)
                    .await?
                    .map(|(_, value)| serde_json::from_value::<AgentContext>(value))
                    .transpose()?
                    .unwrap_or_else(|| AgentContext {
                        context_id: context_id.clone(),
                        node_did: node_did.to_string(),
                        display_name: None,
                        description: None,
                        system_prompt: None,
                        tools_id: None,
                        compaction_id: None,
                        skill_ids: Vec::new(),
                        tags: Vec::new(),
                    });
                context.tools_id = Some(tools.tools_id.clone());
                if let Some(system_prompt) = system_prompt {
                    context.system_prompt = Some(system_prompt);
                }
                behavior.context_id = Some(context_id);

                let mut documents = referenced_documents;
                documents.extend([
                    (Collection::Tools, serde_json::to_value(tools)?),
                    (Collection::AgentContext, serde_json::to_value(context)?),
                    (Collection::Agent, serde_json::to_value(behavior)?),
                ]);
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
                gents::store::config_client::apply_desired_state_plan(txn, &plan)
                    .await
                    .map(|_| ())
            })
        })
        .await
        .unwrap();
}

pub fn agent_target(
    owner_did: &str,
    name: impl Into<String>,
    target_node_did: impl Into<String>,
    agent_did: impl Into<String>,
) -> gents::AgentTargetDocument {
    let name = name.into();
    gents::AgentTargetDocument {
        target_id: name.clone(),
        node_did: owner_did.to_string(),
        target_node_did: target_node_did.into(),
        agent_did: agent_did.into(),
        name,
        description: None,
        tags: Vec::new(),
    }
}

pub async fn bind_default_agent_backend(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    backend_id: &str,
    endpoint: &str,
) {
    bind_agent_backend_chain(node, node_did, None, backend_id, endpoint, "default").await;
}

pub async fn bind_managed_agent_backend(
    db: &super::TestDb,
    fixture: &super::ManagedAgentFixture,
    backend_id: &str,
    endpoint: &str,
    model_name: &str,
) {
    use gents::store::config_client::{
        apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
        DesiredStateApplyPlan,
    };
    use gents::Collection;

    let owner = db.node_identity.did();
    let agent_did = &fixture.binding.agent_did;
    fixture
        .access
        .transact_write("test.bind_managed_agent_backend", |txn| {
            Box::pin(async move {
                let (_, mut node) =
                    read_desired_state_record_in_txn(txn, Collection::Node, owner, owner)
                        .await?
                        .expect("held user must own the test Node");
                let (_, agent) =
                    read_desired_state_record_in_txn(txn, Collection::Agent, owner, agent_did)
                        .await?
                        .expect("managed test Agent must exist");
                let profile_id = agent["inference_profile_id"]
                    .as_str()
                    .expect("managed Agent inference profile");
                node["default_agent_did"] = agent_did.clone().into();
                let profile = serde_json::json!({
                    "node_did": owner,
                    "profile_id": profile_id,
                    "backend_id": backend_id,
                    "model_name": model_name,
                });
                let backend = serde_json::json!({
                    "node_did": owner,
                    "backend_id": backend_id,
                    "name": backend_id,
                    "provider_kind": "OpenAiCompatible",
                    "openai_wire_api": "chat_completions",
                    "auth": {"kind": "unauthenticated"},
                    "endpoint": endpoint,
                    "max_concurrent": 1,
                    "max_queue_depth": 100,
                    "enabled": true,
                });
                let plan = DesiredStateApplyPlan::new(
                    [
                        (Collection::Node, node),
                        (Collection::InferenceBackend, backend),
                        (Collection::InferenceProfile, profile),
                    ]
                    .into_iter()
                    .map(|(collection, value)| DesiredStateApplyDocument {
                        collection,
                        add: value.clone(),
                        update: value,
                    })
                    .collect(),
                )?;
                apply_desired_state_plan(txn, &plan).await
            })
        })
        .await
        .expect("bind managed Agent backend as held user");
    gents::inference::backends::registry::set_backend_probe_status_as(
        &fixture.access,
        owner,
        backend_id,
        "healthy",
    )
    .await
    .expect("mark test backend healthy");
}

pub async fn bind_agent_backend(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_did: &str,
    backend_id: &str,
    endpoint: &str,
    model_name: &str,
) {
    bind_agent_backend_chain(
        node,
        node_did,
        Some(agent_did),
        backend_id,
        endpoint,
        model_name,
    )
    .await;
}

async fn bind_agent_backend_chain(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_did: Option<&str>,
    backend_id: &str,
    endpoint: &str,
    model_name: &str,
) {
    ensure_node(node, node_did).await.unwrap();
    let root =
        gents::store::identity::RegisteredIdentity::from_registered_did(node_did, None).unwrap();
    let binding = gents::store::identity::provision_agent_identity(&root).unwrap();
    gents::store::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "test.bind_behavior_inference",
        |txn| {
            let binding = &binding;
            Box::pin(async move {
                use gents::store::config_client::{
                    read_desired_state_record_in_txn as read, DesiredStateApplyDocument,
                    DesiredStateApplyPlan,
                };
                use gents::Collection;

                let (_, mut principal) = read(txn, Collection::Node, node_did, node_did)
                    .await?
                    .expect("principal");
                let agent_did = agent_did
                    .map(str::to_owned)
                    .or_else(|| principal["default_agent_did"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| binding.agent_did.clone());

                let mut behavior = read(txn, Collection::Agent, node_did, &agent_did)
                    .await?
                    .map(|(_, value)| value)
                    .unwrap_or_else(|| {
                        serde_json::json!({
                            "node_did": node_did,
                            "agent_did": binding.agent_did,
                            "creation_nonce": binding.creation_nonce,
                            "enabled": true
                        })
                    });
                let agent_did = behavior["agent_did"]
                    .as_str()
                    .expect("agent DID")
                    .to_owned();
                principal["default_agent_did"] = agent_did.clone().into();
                let profile_id = behavior["inference_profile_id"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{agent_did}-inference"));
                behavior["inference_profile_id"] = profile_id.clone().into();
                behavior["enabled"] = true.into();

                let mut profile = read(txn, Collection::InferenceProfile, node_did, &profile_id)
                    .await?
                    .map(|(_, value)| value)
                    .unwrap_or_else(|| {
                        serde_json::json!({
                            "node_did": node_did,
                            "profile_id": profile_id
                        })
                    });
                profile["backend_id"] = backend_id.into();
                profile["model_name"] = model_name.into();

                let mut backend = read(txn, Collection::InferenceBackend, node_did, backend_id)
                    .await?
                    .map(|(_, value)| value)
                    .unwrap_or_else(|| {
                        serde_json::json!({
                            "node_did": node_did,
                            "backend_id": backend_id,
                            "name": backend_id,
                            "provider_kind": "OpenAiCompatible",
                            "openai_wire_api": "chat_completions",
                            "auth": {"kind": "unauthenticated"}
                        })
                    });
                backend["endpoint"] = endpoint.into();
                backend["max_concurrent"] = 1.into();
                backend["max_queue_depth"] = 100.into();
                backend["enabled"] = true.into();

                let plan = DesiredStateApplyPlan::new(
                    [
                        (Collection::Node, principal),
                        (Collection::InferenceBackend, backend),
                        (Collection::InferenceProfile, profile),
                        (Collection::Agent, behavior),
                    ]
                    .into_iter()
                    .map(|(collection, value)| DesiredStateApplyDocument {
                        collection,
                        add: value.clone(),
                        update: value,
                    })
                    .collect(),
                )?;
                gents::store::config_client::apply_desired_state_plan(txn, &plan).await
            })
        },
    )
    .await
    .unwrap();
    gents::inference::backends::registry::set_backend_probe_status(
        node, node_did, backend_id, "healthy",
    )
    .await
    .unwrap();
}
