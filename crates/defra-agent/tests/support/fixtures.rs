use std::sync::Arc;
use std::time::Duration;

use defra_agent::compaction::CompactionStrategy;
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{
    ensure_agent_principal, load_agent_behavior, upsert_agent_behavior, AgentBehavior,
    AgentPrincipal, BackendProviderKind, BehaviorToolConfig, KeyIdentity,
};

pub fn test_identity(name: &str) -> KeyIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    KeyIdentity::load_or_create(path, None).unwrap()
}

pub fn test_principal_for(
    identity: Arc<dyn defra_agent::AgentIdentity>,
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
    let identity: Arc<dyn defra_agent::AgentIdentity> = Arc::new(test_identity(name));
    let principal = test_principal_for(identity, name);
    AgentBehavior {
        skills: Vec::new(),
        behavior_id: name.to_string(),
        principal,
        backend_id: Some(backend_id.to_string()),
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        backend_endpoint: "http://localhost:8000/v1".to_string(),
        backend_api_key: None,
        backend_api_key_env_var: backend_api_key_env_var.map(ToOwned::to_owned),
        model_name: defra_agent::config::DEFAULT_MODEL_NAME.to_string(),
        context_window: defra_agent::config::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: defra_agent::config::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: defra_agent::config::DEFAULT_MAX_TURNS,
        system_prompt: String::new(),
        tools: BehaviorToolConfig::default(),
        compaction_threshold: defra_agent::config::DEFAULT_COMPACTION_THRESHOLD,
        compaction_strategy: CompactionStrategy::StripThenSummarize,
        stream_batch_ms: defra_agent::config::DEFAULT_STREAM_BATCH_MS,
        deadline_duration: Duration::from_secs(defra_agent::config::DEFAULT_DEADLINE_DURATION_SECS),
        sampling: defra_agent::config::SamplingConfig::default(),
    }
}

/// Construct a test-only `AgentBehavior` that shares the provided
/// `Arc<AgentPrincipal>`. Use this in tests that build multiple
/// behaviors on a single snapshot — passing the same principal Arc
/// to each call preserves the single-principal-per-snapshot
/// invariant.
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
        backend_endpoint: "http://localhost:8000/v1".to_string(),
        backend_api_key: None,
        backend_api_key_env_var: None,
        model_name: defra_agent::config::DEFAULT_MODEL_NAME.to_string(),
        context_window: defra_agent::config::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: defra_agent::config::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: defra_agent::config::DEFAULT_MAX_TURNS,
        system_prompt: String::new(),
        tools: BehaviorToolConfig::default(),
        compaction_threshold: defra_agent::config::DEFAULT_COMPACTION_THRESHOLD,
        compaction_strategy: CompactionStrategy::StripThenSummarize,
        stream_batch_ms: defra_agent::config::DEFAULT_STREAM_BATCH_MS,
        deadline_duration: Duration::from_secs(defra_agent::config::DEFAULT_DEADLINE_DURATION_SECS),
        sampling: defra_agent::config::SamplingConfig::default(),
    }
}

pub async fn bind_default_behavior_backend(
    node: &EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
    endpoint: &str,
) {
    let bootstrap = ensure_agent_principal(node, agent_did).await.unwrap();
    let escaped_backend_id = escape_graphql_string(backend_id);
    let escaped_endpoint = escape_graphql_string(endpoint);
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                add: {{
                    backend_id: "{escaped_backend_id}",
                    name: "{escaped_backend_id}",
                    endpoint: "{escaped_endpoint}",
                    max_concurrent: 1,
                    enabled: true,
                    models: ["default"],
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{escaped_backend_id}",
                    endpoint: "{escaped_endpoint}",
                    max_concurrent: 1,
                    enabled: true,
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let mut default_behavior = load_agent_behavior(node, &bootstrap.default_behavior.behavior_id)
        .await
        .unwrap()
        .expect("default behavior document");
    default_behavior.backend_id = Some(backend_id.to_string());
    upsert_agent_behavior(node, &default_behavior)
        .await
        .unwrap();
}
