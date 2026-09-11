use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use gents::__test_internals::run_subagent_source_for_test;
use gents::defra_node::EmbeddedNode;
use gents::{
    ensure_agent_principal, ActiveRuntimeSnapshot, AgentIdentity, BackendProviderKind,
    BehaviorToolConfig, KeyIdentity, ResolvedBehavior, RuntimePrincipal,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub struct SubagentSourceGuard {
    cancel: CancellationToken,
    handle: Option<tokio::task::JoinHandle<()>>,
    _snapshot_tx: watch::Sender<Arc<ActiveRuntimeSnapshot>>,
}

impl Drop for SubagentSourceGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

pub fn spawn_subagent_source(
    node: Arc<EmbeddedNode>,
    agent_did: &str,
    parent_behavior_id: &str,
    child_behavior_id: &str,
) -> SubagentSourceGuard {
    spawn_subagent_source_with_authorized_peers(
        node,
        agent_did,
        parent_behavior_id,
        child_behavior_id,
        HashSet::new(),
    )
}

pub fn spawn_subagent_source_with_authorized_peers(
    node: Arc<EmbeddedNode>,
    agent_did: &str,
    parent_behavior_id: &str,
    child_behavior_id: &str,
    authorized_peer_dids: HashSet<String>,
) -> SubagentSourceGuard {
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("subagent-source-principal"));
    let principal = test_principal_for(identity, parent_behavior_id);
    let mut child = test_behavior_for_principal(child_behavior_id, principal.clone());
    child.principal = Arc::new(RuntimePrincipal {
        agent_did: agent_did.to_string(),
        identity: principal.identity.clone(),
        default_behavior_id: parent_behavior_id.to_string(),
        display_name: None,
        enabled: true,
    });
    let mut behaviors = HashMap::new();
    behaviors.insert(child_behavior_id.to_string(), Arc::new(child));
    let snapshot = ActiveRuntimeSnapshot {
        generation: 1,
        principal: None,
        local_did: agent_did.to_string(),
        default_behavior_id: parent_behavior_id.to_string(),
        behaviors,
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: HashSet::new(),
        active_tasks: HashMap::new(),
        dispatchers: HashMap::new(),
        behavior_executor_capacities: HashMap::new(),
        behavior_executor_queue_capacities: HashMap::new(),
    };
    let (snapshot_tx, snapshot_rx) = watch::channel(Arc::new(snapshot));
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(run_subagent_source_for_test(
        node,
        snapshot_rx,
        authorized_peer_dids,
        cancel.clone(),
    ));
    SubagentSourceGuard {
        cancel,
        handle: Some(handle),
        _snapshot_tx: snapshot_tx,
    }
}

pub fn test_identity(name: &str) -> KeyIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    KeyIdentity::load_or_create(path, None).unwrap()
}

pub fn test_principal_for(
    identity: Arc<dyn gents::AgentIdentity>,
    default_behavior_id: impl Into<String>,
) -> Arc<RuntimePrincipal> {
    Arc::new(RuntimePrincipal {
        agent_did: identity.did().to_string(),
        identity,
        default_behavior_id: default_behavior_id.into(),
        display_name: None,
        enabled: true,
    })
}

pub fn test_behavior(
    name: &str,
    backend_id: &str,
    backend_api_key_env_var: Option<&str>,
) -> ResolvedBehavior {
    let identity: Arc<dyn gents::AgentIdentity> = Arc::new(test_identity(name));
    let principal = test_principal_for(identity, name);
    let mut behavior = test_behavior_for_principal(name, principal);
    behavior.backend_id = Some(backend_id.to_owned());
    behavior.backend_auth = match backend_api_key_env_var {
        Some(variable) => gents::document_config::BackendAuth::Environment {
            variable: variable.into(),
        },
        None => gents::document_config::BackendAuth::Unauthenticated,
    };
    behavior
}

pub fn test_behavior_for_principal(
    behavior_id: impl Into<String>,
    principal: Arc<RuntimePrincipal>,
) -> ResolvedBehavior {
    let behavior_id = behavior_id.into();
    ResolvedBehavior {
        skills: Vec::new(),
        behavior_id,
        principal,
        backend_id: None,
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        openai_wire_api: gents::OpenAiWireApi::ChatCompletions,
        backend_endpoint: "http://localhost:8000/v1".to_string(),
        backend_auth: gents::document_config::BackendAuth::Unauthenticated,
        model_name: gents::config::DEFAULT_MODEL_NAME.to_string(),
        context_window: gents::config::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: gents::config::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: gents::config::DEFAULT_MAX_TURNS,
        system_prompt: String::new(),
        tools: BehaviorToolConfig::default(),
        compaction: None,
        compaction_inference: None,
        max_total_tokens: None,
        stream_batch_ms: gents::config::DEFAULT_STREAM_BATCH_MS,
        stream_liveness_timeout: Duration::from_secs(
            gents::config::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
        ),
        deadline_duration: Duration::from_secs(gents::config::DEFAULT_DEADLINE_DURATION_SECS),
        completion_retry: gents::agent::completion_retry::CompletionRetryProfileFields::default(),
        sampling: gents::config::SamplingConfig::default(),
    }
}

/// Publish the canonical document chain that grants one behavior subagent tools.
///
/// Publish the complete behavior-owned tool graph used by integration tests.
/// Targets are standalone documents, `Tools` selects them, `AgentContext`
/// selects the tools, and the behavior selects the context.
pub async fn configure_subagent_behavior(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    tools_id: &str,
    targets: Vec<gents::SubagentTargetDocument>,
    spawn_enabled: bool,
    background_enabled: bool,
    allow_cross_principal: Option<bool>,
) {
    use gents::config_client::{
        read_desired_state_record_in_txn as read, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    use gents::document_config::{AgentContext, SubagentTools, Tools};
    use gents::Collection;

    gents::ConfigAccess::transact_local(node, None, "test.configure_subagent_behavior", |txn| {
        let targets = targets.clone();
        Box::pin(async move {
            let mut documents = Vec::new();
            let mut behavior = read(txn, Collection::AgentBehavior, agent_did, behavior_id)
                .await?
                .map(|(_, value)| serde_json::from_value::<gents::AgentBehaviorDocument>(value))
                .transpose()?
                .unwrap_or_else(|| gents::AgentBehaviorDocument {
                    behavior_id: behavior_id.to_string(),
                    agent_did: agent_did.to_string(),
                    display_name: Some(behavior_id.to_string()),
                    description: None,
                    context_id: None,
                    inference_profile_id: format!("{behavior_id}-inference"),
                    enabled: true,
                    tags: Vec::new(),
                    created_at: Some("2026-05-12T00:00:00Z".to_string()),
                });

            if read(
                txn,
                Collection::InferenceProfile,
                agent_did,
                &behavior.inference_profile_id,
            )
            .await?
            .is_none()
            {
                let backend_id = format!("{behavior_id}-test-backend");
                if read(txn, Collection::InferenceBackend, agent_did, &backend_id)
                    .await?
                    .is_none()
                {
                    documents.push((
                        Collection::InferenceBackend,
                        serde_json::json!({
                            "agent_did": agent_did,
                            "backend_id": backend_id,
                            "name": format!("{behavior_id} test backend"),
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
                        "agent_did": agent_did,
                        "profile_id": behavior.inference_profile_id,
                        "backend_id": backend_id,
                        "model_name": "test-model"
                    }),
                ));
            }

            let context_id = behavior
                .context_id
                .clone()
                .unwrap_or_else(|| format!("{behavior_id}-test-context"));
            let mut context = read(txn, Collection::AgentContext, agent_did, &context_id)
                .await?
                .map(|(_, value)| serde_json::from_value::<AgentContext>(value))
                .transpose()?
                .unwrap_or_else(|| AgentContext {
                    context_id: context_id.clone(),
                    agent_did: agent_did.to_string(),
                    display_name: None,
                    description: None,
                    system_prompt: None,
                    tools_id: None,
                    compaction_id: None,
                    skill_ids: Vec::new(),
                    tags: Vec::new(),
                });
            context.tools_id = Some(tools_id.to_string());

            let mut tools = read(txn, Collection::Tools, agent_did, tools_id)
                .await?
                .map(|(_, value)| serde_json::from_value::<Tools>(value))
                .transpose()?
                .unwrap_or_else(|| Tools {
                    tools_id: tools_id.to_string(),
                    agent_did: agent_did.to_string(),
                    ..Default::default()
                });
            tools.subagents = Some(SubagentTools {
                target_ids: targets
                    .iter()
                    .map(|target| target.target_id.clone())
                    .collect(),
                spawn_enabled: Some(spawn_enabled),
                steering_enabled: Some(true),
                background_enabled: Some(background_enabled),
                allow_cross_principal,
                ..Default::default()
            });
            behavior.context_id = Some(context_id);

            documents.extend(targets.into_iter().map(|target| {
                (
                    Collection::SubagentTarget,
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
                    Collection::AgentBehavior,
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

/// Publish one canonical `Tools -> AgentContext -> AgentBehavior` chain and
/// any standalone documents referenced by the tools document.
pub async fn configure_behavior_tools(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    system_prompt: Option<String>,
    tools: gents::document_config::Tools,
    referenced_documents: Vec<(gents::Collection, serde_json::Value)>,
) {
    use gents::config_client::{
        read_desired_state_record_in_txn as read, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    use gents::document_config::AgentContext;
    use gents::Collection;

    assert_eq!(tools.agent_did, agent_did);
    gents::ConfigAccess::transact_local(node, None, "test.configure_behavior_tools", |txn| {
        let tools = tools.clone();
        let referenced_documents = referenced_documents.clone();
        let system_prompt = system_prompt.clone();
        Box::pin(async move {
            let (_, behavior) = read(txn, Collection::AgentBehavior, agent_did, behavior_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("behavior {behavior_id} is missing"))?;
            let mut behavior: gents::AgentBehaviorDocument = serde_json::from_value(behavior)?;
            let context_id = behavior
                .context_id
                .clone()
                .unwrap_or_else(|| format!("{behavior_id}:context"));
            let mut context = read(txn, Collection::AgentContext, agent_did, &context_id)
                .await?
                .map(|(_, value)| serde_json::from_value::<AgentContext>(value))
                .transpose()?
                .unwrap_or_else(|| AgentContext {
                    context_id: context_id.clone(),
                    agent_did: agent_did.to_string(),
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
                (Collection::AgentBehavior, serde_json::to_value(behavior)?),
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

pub fn subagent_target(
    owner_did: &str,
    name: impl Into<String>,
    target_agent_did: impl Into<String>,
    behavior_id: impl Into<String>,
) -> gents::SubagentTargetDocument {
    let name = name.into();
    gents::SubagentTargetDocument {
        target_id: name.clone(),
        agent_did: owner_did.to_string(),
        target_agent_did: target_agent_did.into(),
        behavior_id: behavior_id.into(),
        name,
        description: None,
        tags: Vec::new(),
    }
}

pub async fn bind_default_behavior_backend(
    node: &EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
    endpoint: &str,
) {
    bind_behavior_backend_chain(node, agent_did, None, backend_id, endpoint, "default").await;
}

/// Publish the canonical principal → behavior → inference profile → backend
/// chain used by daemon integration fixtures with an explicit behavior id.
pub async fn bind_behavior_backend(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    backend_id: &str,
    endpoint: &str,
    model_name: &str,
) {
    bind_behavior_backend_chain(
        node,
        agent_did,
        Some(behavior_id),
        backend_id,
        endpoint,
        model_name,
    )
    .await;
}

async fn bind_behavior_backend_chain(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: Option<&str>,
    backend_id: &str,
    endpoint: &str,
    model_name: &str,
) {
    ensure_agent_principal(node, agent_did).await.unwrap();
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

                let (_, mut principal) =
                    read(txn, Collection::AgentPrincipal, agent_did, agent_did)
                        .await?
                        .expect("principal");
                let behavior_id = behavior_id
                    .map(str::to_owned)
                    .or_else(|| principal["default_behavior_id"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| gents::default_behavior_id_for_agent(agent_did));
                principal["default_behavior_id"] = behavior_id.clone().into();

                let mut behavior = read(txn, Collection::AgentBehavior, agent_did, &behavior_id)
                    .await?
                    .map(|(_, value)| value)
                    .unwrap_or_else(|| {
                        serde_json::json!({
                            "agent_did": agent_did,
                            "behavior_id": behavior_id,
                            "enabled": true
                        })
                    });
                let profile_id = behavior["inference_profile_id"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{behavior_id}-inference"));
                behavior["inference_profile_id"] = profile_id.clone().into();
                behavior["enabled"] = true.into();

                let mut profile = read(txn, Collection::InferenceProfile, agent_did, &profile_id)
                    .await?
                    .map(|(_, value)| value)
                    .unwrap_or_else(|| {
                        serde_json::json!({
                            "agent_did": agent_did,
                            "profile_id": profile_id
                        })
                    });
                profile["backend_id"] = backend_id.into();
                profile["model_name"] = model_name.into();

                let mut backend = read(txn, Collection::InferenceBackend, agent_did, backend_id)
                    .await?
                    .map(|(_, value)| value)
                    .unwrap_or_else(|| {
                        serde_json::json!({
                            "agent_did": agent_did,
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
                        (Collection::AgentPrincipal, principal),
                        (Collection::InferenceBackend, backend),
                        (Collection::InferenceProfile, profile),
                        (Collection::AgentBehavior, behavior),
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
    gents::backend_registry::set_backend_probe_status(node, agent_did, backend_id, "healthy")
        .await
        .unwrap();
}
