use anyhow::{Context, Result};
use gents_inference::backends::provider::BackendProviderKind;

#[derive(Debug, Clone, Default)]
pub(crate) struct LiveBackendOverride {
    pub(crate) inference_url: Option<String>,
    pub(crate) model_name: Option<String>,
    pub(crate) fallback_model_name: Option<String>,
    pub(crate) provider: Option<String>,
    pub(crate) api_key: Option<String>,
    pub(crate) api_key_env_var: Option<String>,
    pub(crate) inference_settings: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct LiveBackendConfig {
    pub(crate) endpoint: String,
    pub(crate) model_name: String,
    pub(crate) provider_kind: BackendProviderKind,
    pub(crate) api_key: Option<String>,
    pub(crate) api_key_env_var: Option<String>,
    pub(crate) inference_settings: Option<String>,
}

impl LiveBackendConfig {
    pub(crate) fn resolve(override_config: Option<&LiveBackendOverride>) -> Result<Self> {
        let field = |pick: fn(&LiveBackendOverride) -> Option<&String>| {
            override_config.and_then(|config| normalize_optional_owned(pick(config)))
        };
        let endpoint = field(|config| config.inference_url.as_ref());
        let model_name = field(|config| config.model_name.as_ref())
            .or_else(|| field(|config| config.fallback_model_name.as_ref()))
            .unwrap_or_else(|| "openai/gpt-4o-mini".to_string());
        let provider_kind = field(|config| config.provider.as_ref());
        let api_key = field(|config| config.api_key.as_ref());
        let api_key_env_var = field(|config| config.api_key_env_var.as_ref());
        let inference_settings =
            override_config.and_then(|config| config.inference_settings.clone());

        if endpoint.is_some()
            || provider_kind.is_some()
            || api_key.is_some()
            || api_key_env_var.is_some()
        {
            if let Some(env_var_name) = api_key_env_var.as_deref() {
                require_key_variable(env_var_name, "--api-key-env-var")?;
            }

            return Ok(Self {
                endpoint: endpoint.context(
                    "pass --inference-url or set OPENROUTER_API_KEY to run the live Tauri bridge runner",
                )?,
                model_name,
                provider_kind: BackendProviderKind::parse_optional(provider_kind.as_deref())?,
                api_key,
                api_key_env_var,
                inference_settings,
            });
        }

        require_key_variable(OPENROUTER_KEY_VARIABLE, "the OpenRouter fallback").context(
            "pass --inference-url or set OPENROUTER_API_KEY to run the live Tauri bridge runner",
        )?;
        Ok(Self {
            endpoint: "https://openrouter.ai/api/v1".to_string(),
            model_name,
            provider_kind: BackendProviderKind::OpenRouter,
            api_key: None,
            api_key_env_var: Some(OPENROUTER_KEY_VARIABLE.to_string()),
            inference_settings,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct LiveTargetBackendOverride {
    pub(crate) inference_url: Option<String>,
    pub(crate) model_name: Option<String>,
    pub(crate) provider: Option<String>,
    pub(crate) api_key: Option<String>,
    pub(crate) api_key_env_var: Option<String>,
}

impl LiveBackendConfig {
    pub(crate) fn resolve_target(
        override_config: Option<&LiveTargetBackendOverride>,
        primary: &LiveBackendConfig,
    ) -> Result<Option<Self>> {
        let field = |pick: fn(&LiveTargetBackendOverride) -> Option<&String>| {
            override_config.and_then(|config| normalize_optional_owned(pick(config)))
        };
        let Some(endpoint) = field(|config| config.inference_url.as_ref()) else {
            return Ok(None);
        };
        let model_name = field(|config| config.model_name.as_ref())
            .unwrap_or_else(|| primary.model_name.clone());
        let provider_kind = field(|config| config.provider.as_ref());
        let api_key = field(|config| config.api_key.as_ref());
        let api_key_env_var = field(|config| config.api_key_env_var.as_ref());
        if let Some(env_var_name) = api_key_env_var.as_deref() {
            require_key_variable(env_var_name, "--agent-target-api-key-env-var")?;
        }
        Ok(Some(Self {
            endpoint,
            model_name,
            provider_kind: BackendProviderKind::parse_optional(provider_kind.as_deref())?,
            api_key,
            api_key_env_var,
            inference_settings: None,
        }))
    }
}

/// The variable the OpenRouter fallback backend names as its key source.
const OPENROUTER_KEY_VARIABLE: &str = "OPENROUTER_API_KEY";

/// The runtime reads a backend's key from the variable it names, so the
/// runner refuses to seed one whose variable is unset.
fn require_key_variable(name: &str, source: &str) -> Result<()> {
    std::env::var(name)
        .map(drop)
        .with_context(|| format!("set {name} because {source} points at it"))
}

fn normalize_optional_owned(value: Option<&String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
