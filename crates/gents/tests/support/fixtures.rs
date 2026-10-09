use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::{
    ensure_node, AgentToolSurfaceConfig, BackendProviderKind, KeyIdentity, ResolvedAgent,
    RuntimeNode,
};

/// Every pack fixture directory under `tests/fixtures/packs`, sorted by
/// name. Shared by every test that must cover "every fixture pack" so a new
/// fixture is picked up by all of them without each keeping its own copy of
/// the list. Panics on any unreadable directory or manifest, and when a
/// known fixture is missing, so a broken fixture cannot drop out of the
/// coverage unnoticed.
pub fn fixture_pack_names() -> Vec<String> {
    const KNOWN: [&str; 7] = [
        "review_graph",
        "prepared_graph",
        "assets_fixture",
        "documents_fixture",
        "slot_fixture",
        "dependent_fixture",
        "bind_plugin_fixture",
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
    default_agent_id: impl Into<String>,
) -> Arc<RuntimeNode> {
    Arc::new(RuntimeNode {
        node_did: identity.did().to_string(),
        identity,
        default_agent_id: default_agent_id.into(),
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
        Some(variable) => gents::document_config::BackendAuth::Environment {
            variable: variable.into(),
        },
        None => gents::document_config::BackendAuth::Unauthenticated,
    };
    behavior
}

pub fn test_agent_for_node(
    agent_id: impl Into<String>,
    principal: Arc<RuntimeNode>,
) -> ResolvedAgent {
    let agent_id = agent_id.into();
    ResolvedAgent {
        skills: Vec::new(),
        agent_id,
        node: principal,
        backend_id: None,
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        openai_wire_api: gents::OpenAiWireApi::ChatCompletions,
        backend_endpoint: "http://localhost:8000/v1".to_string(),
        backend_auth: gents::document_config::BackendAuth::Unauthenticated,
        model_name: gents::config::DEFAULT_MODEL_NAME.to_string(),
        resolved_reasoning_efforts: None,
        context_window: gents::config::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: gents::config::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: gents::config::DEFAULT_MAX_TURNS,
        max_turns_provenance: gents::config::MaxTurnsProvenance::Default,
        system_prompt: String::new(),
        tools: AgentToolSurfaceConfig::default(),
        compaction: None,
        compaction_inference: None,
        max_total_tokens: None,
        stream_batch_ms: gents::config::DEFAULT_STREAM_BATCH_MS,
        stream_liveness_timeout: Duration::from_secs(
            gents::config::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
        ),
        deadline_duration: Duration::from_secs(gents::config::DEFAULT_DEADLINE_DURATION_SECS),
        provider_idle_timeout: Duration::from_secs(
            gents::config::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
        ),
        completion_retry: gents::agent::completion_retry::CompletionRetryProfileFields::default(),
        sampling: gents::config::SamplingConfig::default(),
    }
}

/// Publish the canonical document chain that grants one behavior the
/// session-message tools over `targets`.
///
/// Publish the complete behavior-owned tool graph used by integration tests.
/// Targets are standalone documents, `Tools` selects them, `AgentContext`
/// selects the tools, and the behavior selects the context.
pub async fn configure_child_agent(
    node: &EmbeddedNode,
    node_did: &str,
    agent_id: &str,
    tools_id: &str,
    targets: Vec<gents::AgentTargetDocument>,
    enabled: bool,
) {
    use gents::config_client::{
        read_desired_state_record_in_txn as read, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    use gents::document_config::{AgentContext, AgentTools, Tools};
    use gents::Collection;

    gents::ConfigAccess::transact_local(node, None, "test.configure_child_agent", |txn| {
        let targets = targets.clone();
        Box::pin(async move {
            let mut documents = Vec::new();
            let mut behavior = read(txn, Collection::Agent, node_did, agent_id)
                .await?
                .map(|(_, value)| serde_json::from_value::<gents::AgentDocument>(value))
                .transpose()?
                .unwrap_or_else(|| gents::AgentDocument {
                    agent_id: agent_id.to_string(),
                    node_did: node_did.to_string(),
                    display_name: Some(agent_id.to_string()),
                    description: None,
                    context_id: None,
                    inference_profile_id: format!("{agent_id}-inference"),
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
                let backend_id = format!("{agent_id}-test-backend");
                if read(txn, Collection::InferenceBackend, node_did, &backend_id)
                    .await?
                    .is_none()
                {
                    documents.push((
                        Collection::InferenceBackend,
                        serde_json::json!({
                            "node_did": node_did,
                            "backend_id": backend_id,
                            "name": format!("{agent_id} test backend"),
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
                .unwrap_or_else(|| format!("{agent_id}-test-context"));
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
            gents::config_client::apply_desired_state_plan(txn, &plan)
                .await
                .map(|_| ())
        })
    })
    .await
    .unwrap();
}

/// Publish one canonical `Tools -> AgentContext -> Agent` chain and
/// any standalone documents referenced by the tools document.
pub async fn configure_agent_tools(
    node: &EmbeddedNode,
    node_did: &str,
    agent_id: &str,
    system_prompt: Option<String>,
    tools: gents::document_config::Tools,
    referenced_documents: Vec<(gents::Collection, serde_json::Value)>,
) {
    use gents::config_client::{
        read_desired_state_record_in_txn as read, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    use gents::document_config::AgentContext;
    use gents::Collection;

    assert_eq!(tools.node_did, node_did);
    gents::ConfigAccess::transact_local(node, None, "test.configure_agent_tools", |txn| {
        let tools = tools.clone();
        let referenced_documents = referenced_documents.clone();
        let system_prompt = system_prompt.clone();
        Box::pin(async move {
            let (_, behavior) = read(txn, Collection::Agent, node_did, agent_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("behavior {agent_id} is missing"))?;
            let mut behavior: gents::AgentDocument = serde_json::from_value(behavior)?;
            let context_id = behavior
                .context_id
                .clone()
                .unwrap_or_else(|| format!("{agent_id}:context"));
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
            gents::config_client::apply_desired_state_plan(txn, &plan)
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
    agent_id: impl Into<String>,
) -> gents::AgentTargetDocument {
    let name = name.into();
    gents::AgentTargetDocument {
        target_id: name.clone(),
        node_did: owner_did.to_string(),
        target_node_did: target_node_did.into(),
        agent_id: agent_id.into(),
        name,
        description: None,
        tags: Vec::new(),
    }
}

pub async fn bind_default_agent_backend(
    node: &EmbeddedNode,
    node_did: &str,
    backend_id: &str,
    endpoint: &str,
) {
    bind_agent_backend_chain(node, node_did, None, backend_id, endpoint, "default").await;
}

/// Publish the canonical principal → behavior → inference profile → backend
/// chain used by daemon integration fixtures with an explicit behavior id.
pub async fn bind_agent_backend(
    node: &EmbeddedNode,
    node_did: &str,
    agent_id: &str,
    backend_id: &str,
    endpoint: &str,
    model_name: &str,
) {
    bind_agent_backend_chain(
        node,
        node_did,
        Some(agent_id),
        backend_id,
        endpoint,
        model_name,
    )
    .await;
}

async fn bind_agent_backend_chain(
    node: &EmbeddedNode,
    node_did: &str,
    agent_id: Option<&str>,
    backend_id: &str,
    endpoint: &str,
    model_name: &str,
) {
    ensure_node(node, node_did).await.unwrap();
    gents::config_client::ConfigAccess::transact_local(
        node,
        None,
        "test.bind_behavior_inference",
        |txn| {
            Box::pin(async move {
                use gents::config_client::{
                    read_desired_state_record_in_txn as read, DesiredStateApplyDocument,
                    DesiredStateApplyPlan,
                };
                use gents::Collection;

                let (_, mut principal) = read(txn, Collection::Node, node_did, node_did)
                    .await?
                    .expect("principal");
                let agent_id = agent_id
                    .map(str::to_owned)
                    .or_else(|| principal["default_agent_id"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| gents::default_agent_id_for_node(node_did));
                principal["default_agent_id"] = agent_id.clone().into();

                let mut behavior = read(txn, Collection::Agent, node_did, &agent_id)
                    .await?
                    .map(|(_, value)| value)
                    .unwrap_or_else(|| {
                        serde_json::json!({
                            "node_did": node_did,
                            "agent_id": agent_id,
                            "enabled": true
                        })
                    });
                let profile_id = behavior["inference_profile_id"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{agent_id}-inference"));
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
                gents::config_client::apply_desired_state_plan(txn, &plan).await
            })
        },
    )
    .await
    .unwrap();
    gents::backend_registry::set_backend_probe_status(node, node_did, backend_id, "healthy")
        .await
        .unwrap();
}
