use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use gents::__test_internals::run_subagent_source_for_test;
use gents::defra_node::EmbeddedNode;
use gents::{
    ActiveRuntimeSnapshot, AgentBehavior, AgentIdentity, AgentPrincipal, BackendProviderKind,
    BehaviorToolConfig, KeyIdentity, ensure_agent_principal,
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
    child.principal = Arc::new(AgentPrincipal {
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
) -> Arc<AgentPrincipal> {
    Arc::new(AgentPrincipal {
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
) -> AgentBehavior {
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
    principal: Arc<AgentPrincipal>,
) -> AgentBehavior {
    let behavior_id = behavior_id.into();
    AgentBehavior {
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

pub async fn bind_default_behavior_backend(
    node: &EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
    endpoint: &str,
) {
    ensure_agent_principal(node, agent_did).await.unwrap();
    gents::config_client::ConfigAccess::transact_local(node, None, "test.bind_default_inference", |txn| {
        Box::pin(async move {
            use gents::config_client::{read_desired_state_record_in_txn as read, DesiredStateApplyDocument, DesiredStateApplyPlan};
            use gents::Collection;
            let (_, mut principal) = read(txn, Collection::AgentPrincipal, agent_did, agent_did)
                .await?.expect("principal");
            let behavior_id = principal["default_behavior_id"].as_str().unwrap_or("default").to_owned();
            principal["default_behavior_id"] = behavior_id.clone().into();
            let mut behavior = read(txn, Collection::AgentBehavior, agent_did, &behavior_id).await?
                .map(|(_, value)| value).unwrap_or_else(|| serde_json::json!({
                    "agent_did": agent_did, "behavior_id": behavior_id
                }));
            let profile_id = behavior["inference_profile_id"].as_str().map(str::to_owned)
                .unwrap_or_else(|| format!("{behavior_id}-inference"));
            behavior["inference_profile_id"] = profile_id.clone().into();
            let mut profile = read(txn, Collection::InferenceProfile, agent_did, &profile_id).await?
                .map(|(_, value)| value).unwrap_or_else(|| serde_json::json!({
                    "agent_did": agent_did, "profile_id": profile_id, "model_name": "default"
                }));
            profile["backend_id"] = backend_id.into();
            let mut backend = read(txn, Collection::InferenceBackend, agent_did, backend_id).await?
                .map(|(_, value)| value).unwrap_or_else(|| serde_json::json!({
                    "agent_did": agent_did, "backend_id": backend_id, "name": backend_id,
                    "provider_kind": "OpenAiCompatible", "openai_wire_api": "chat_completions",
                    "auth": {"kind": "unauthenticated"}
                }));
            backend["endpoint"] = endpoint.into();
            backend["max_concurrent"] = 1.into();
            backend["enabled"] = true.into();
            let plan = DesiredStateApplyPlan::new([
                (Collection::AgentPrincipal, principal),
                (Collection::InferenceBackend, backend),
                (Collection::InferenceProfile, profile),
                (Collection::AgentBehavior, behavior),
            ].into_iter().map(|(collection, value)| DesiredStateApplyDocument {
                collection, add: value.clone(), update: value,
            }).collect())?;
            gents::config_client::apply_desired_state_plan(txn, &plan).await
        })
    }).await.unwrap();
    gents::backend_registry::set_backend_probe_status(node, agent_did, backend_id, "healthy")
        .await
        .unwrap();
}
