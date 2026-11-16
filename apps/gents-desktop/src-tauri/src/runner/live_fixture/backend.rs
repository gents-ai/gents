use anyhow::{Context, Result};
use gents_inference::backends::provider::BackendProviderKind;

#[derive(Debug, Clone, Default)]
pub(crate) struct LiveBackendOverride {
    pub(crate) inference_url: Option<String>,
    pub(crate) model_name: Option<String>,
    pub(crate) provider: Option<String>,
    pub(crate) api_key: Option<String>,
    pub(crate) api_key_env_var: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct LiveBackendConfig {
    pub(crate) endpoint: String,
    pub(crate) model_name: String,
    pub(crate) provider_kind: BackendProviderKind,
    pub(crate) api_key: Option<String>,
    pub(crate) api_key_env_var: Option<String>,
}

impl LiveBackendConfig {
    pub(crate) fn resolve(override_config: Option<&LiveBackendOverride>) -> Result<Self> {
        let endpoint = override_config
            .and_then(|config| normalize_optional_owned(config.inference_url.as_ref()))
            .context("pass --inference-url to run the live Tauri bridge runner")?;
        let model_name = override_config
            .and_then(|config| normalize_optional_owned(config.model_name.as_ref()))
            .unwrap_or_else(|| "openai/gpt-4o-mini".to_string());
        let provider_kind =
            override_config.and_then(|config| normalize_optional_owned(config.provider.as_ref()));
        let api_key =
            override_config.and_then(|config| normalize_optional_owned(config.api_key.as_ref()));
        let api_key_env_var = override_config
            .and_then(|config| normalize_optional_owned(config.api_key_env_var.as_ref()));
        Ok(Self {
            endpoint,
            model_name,
            provider_kind: BackendProviderKind::parse_optional(provider_kind.as_deref())?,
            api_key,
            api_key_env_var,
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
        let endpoint =
            override_config.and_then(|c| normalize_optional_owned(c.inference_url.as_ref()));
        let Some(endpoint) = endpoint else {
            return Ok(None);
        };
        let model_name = override_config
            .and_then(|c| normalize_optional_owned(c.model_name.as_ref()))
            .unwrap_or_else(|| primary.model_name.clone());
        let provider_kind =
            override_config.and_then(|c| normalize_optional_owned(c.provider.as_ref()));
        let api_key = override_config.and_then(|c| normalize_optional_owned(c.api_key.as_ref()));
        let api_key_env_var =
            override_config.and_then(|c| normalize_optional_owned(c.api_key_env_var.as_ref()));
        Ok(Some(Self {
            endpoint,
            model_name,
            provider_kind: BackendProviderKind::parse_optional(provider_kind.as_deref())?,
            api_key,
            api_key_env_var,
        }))
    }
}

fn normalize_optional_owned(value: Option<&String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
