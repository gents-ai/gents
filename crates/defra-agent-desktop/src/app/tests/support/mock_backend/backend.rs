use super::*;

#[derive(Debug, Clone)]
pub(crate) struct AgentBackendConfig {
    pub(crate) endpoint: String,
    pub(crate) model_name: String,
    pub(crate) provider_kind: BackendProviderKind,
    pub(crate) api_key: Option<String>,
    pub(crate) api_key_env_var: Option<String>,
}

impl AgentBackendConfig {
    pub(crate) fn openai_compatible(endpoint: &str, model_name: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            model_name: model_name.to_string(),
            provider_kind: BackendProviderKind::OpenAiCompatible,
            api_key: None,
            api_key_env_var: None,
        }
    }

    pub(crate) fn mock(endpoint: &str) -> Self {
        Self::openai_compatible(endpoint, "default")
    }

    pub(crate) fn live_from_env() -> Result<Self> {
        let endpoint = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT");
        let model_name = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_MODEL")
            .or_else(|| optional_env("DEFRA_AGENT_TEST_OPENROUTER_MODEL"))
            .unwrap_or_else(|| "openai/gpt-4o-mini".to_string());
        let provider_kind = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_PROVIDER");
        let api_key = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_API_KEY");
        let api_key_env_var = optional_env("DEFRA_AGENT_DESKTOP_LIVE_BACKEND_API_KEY_ENV_VAR");

        if endpoint.is_some()
            || provider_kind.is_some()
            || api_key.is_some()
            || api_key_env_var.is_some()
        {
            if let Some(env_var_name) = api_key_env_var.as_deref() {
                std::env::var(env_var_name).with_context(|| {
                    format!(
                        "set {env_var_name} because DEFRA_AGENT_DESKTOP_LIVE_BACKEND_API_KEY_ENV_VAR points at it"
                    )
                })?;
            }

            return Ok(Self {
                endpoint: endpoint.context(
                    "set DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT for the live desktop smoke test",
                )?,
                model_name,
                provider_kind: BackendProviderKind::parse_optional(provider_kind.as_deref())?,
                api_key,
                api_key_env_var,
            });
        }

        if std::env::var("OPENROUTER_API_KEY").is_ok() {
            return Ok(Self {
                endpoint: "https://openrouter.ai/api/v1".to_string(),
                model_name,
                provider_kind: BackendProviderKind::OpenRouter,
                api_key: None,
                api_key_env_var: Some("OPENROUTER_API_KEY".to_string()),
            });
        }

        anyhow::bail!(
            "set DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT or OPENROUTER_API_KEY to run the live desktop smoke test"
        );
    }
}

pub(crate) fn test_runtime() -> Result<Arc<Runtime>> {
    Ok(Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(4)
            .build()?,
    ))
}

pub(crate) fn shutdown_core(runtime: &Runtime, core: ClientCore) -> Result<()> {
    runtime.block_on(core.shutdown())
}

pub(crate) async fn bind_default_behavior_backend(
    node: &EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
    backend: &AgentBackendConfig,
) -> Result<()> {
    let bootstrap = ensure_agent_principal(node, agent_did).await?;
    let escaped_backend_id = escape_graphql_string(backend_id);
    let escaped_endpoint = escape_graphql_string(&backend.endpoint);
    let escaped_provider_kind = escape_graphql_string(backend.provider_kind.as_str());
    let escaped_model_name = escape_graphql_string(&backend.model_name);
    let api_key_field = graphql_optional_string_field("api_key", backend.api_key.as_deref());
    let api_key_env_var_field =
        graphql_optional_string_field("api_key_env_var", backend.api_key_env_var.as_deref());
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                add: {{
                    backend_id: "{escaped_backend_id}",
                    name: "{escaped_backend_id}",
                    provider_kind: "{escaped_provider_kind}",
                    endpoint: "{escaped_endpoint}",
                    {api_key_field}
                    {api_key_env_var_field}
                    max_concurrent: 2,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model_name}"],
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{escaped_backend_id}",
                    provider_kind: "{escaped_provider_kind}",
                    endpoint: "{escaped_endpoint}",
                    {api_key_field}
                    {api_key_env_var_field}
                    max_concurrent: 2,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model_name}"],
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!("upsert inference backend failed: {:?}", response.errors);
    }

    let mut default_behavior = load_agent_behavior(node, &bootstrap.default_behavior.behavior_id)
        .await?
        .expect("default behavior document");
    default_behavior.backend_id = Some(backend_id.to_string());
    default_behavior.model_name = Some(backend.model_name.clone());
    upsert_agent_behavior(node, &default_behavior).await?;
    Ok(())
}
