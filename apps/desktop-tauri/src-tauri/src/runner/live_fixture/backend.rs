use anyhow::{Context, Result};
use defra_agent::BackendProviderKind;

const LIVE_BACKEND_PREFIX: &str = "DEFRA_AGENT_DESKTOP_LIVE_BACKEND";

#[derive(Debug, Clone, Default)]
pub(crate) struct LiveBackendOverride {
    pub(crate) inference_url: Option<String>,
    pub(crate) model_name: Option<String>,
    pub(crate) provider: Option<String>,
    pub(crate) api_key: Option<String>,
    pub(crate) api_key_env_var: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentBackendConfig {
    pub(crate) endpoint: String,
    pub(crate) model_name: String,
    pub(crate) provider_kind: BackendProviderKind,
    pub(crate) api_key: Option<String>,
    pub(crate) api_key_env_var: Option<String>,
}

impl AgentBackendConfig {
    pub(crate) fn resolve(override_config: Option<&LiveBackendOverride>) -> Result<Self> {
        let endpoint = override_config
            .and_then(|config| normalize_optional_owned(config.inference_url.as_ref()))
            .or_else(|| optional_env(&format!("{LIVE_BACKEND_PREFIX}_ENDPOINT")));
        let model_name = override_config
            .and_then(|config| normalize_optional_owned(config.model_name.as_ref()))
            .or_else(|| optional_env(&format!("{LIVE_BACKEND_PREFIX}_MODEL")))
            .or_else(|| optional_env("DEFRA_AGENT_TEST_OPENROUTER_MODEL"))
            .unwrap_or_else(|| "openai/gpt-4o-mini".to_string());
        let provider_kind = override_config
            .and_then(|config| normalize_optional_owned(config.provider.as_ref()))
            .or_else(|| optional_env(&format!("{LIVE_BACKEND_PREFIX}_PROVIDER")));
        let api_key = override_config
            .and_then(|config| normalize_optional_owned(config.api_key.as_ref()))
            .or_else(|| optional_env(&format!("{LIVE_BACKEND_PREFIX}_API_KEY")));
        let api_key_env_var = override_config
            .and_then(|config| normalize_optional_owned(config.api_key_env_var.as_ref()))
            .or_else(|| optional_env(&format!("{LIVE_BACKEND_PREFIX}_API_KEY_ENV_VAR")));

        if endpoint.is_some()
            || provider_kind.is_some()
            || api_key.is_some()
            || api_key_env_var.is_some()
        {
            if let Some(env_var_name) = api_key_env_var.as_deref() {
                std::env::var(env_var_name).with_context(|| {
                    format!(
                        "set {env_var_name} because {LIVE_BACKEND_PREFIX}_API_KEY_ENV_VAR points at it"
                    )
                })?;
            }

            return Ok(Self {
                endpoint: endpoint.context(format!(
                    "set {LIVE_BACKEND_PREFIX}_ENDPOINT or OPENROUTER_API_KEY to run the live Tauri bridge runner"
                ))?,
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
            "set {LIVE_BACKEND_PREFIX}_ENDPOINT or OPENROUTER_API_KEY to run the live Tauri bridge runner"
        );
    }
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_optional_owned(value: Option<&String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
