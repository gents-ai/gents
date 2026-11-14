use anyhow::Result;
use gents_inference::backends::provider::BackendProviderKind;
use gents_loop::openai_wire::OpenAiWireApi;

use crate::cli::args::{BackendPresetArg, OpenAiWireApiArg};
use crate::shared::ResolvedBackendConfig;
use crate::DEFAULT_INIT_ENDPOINT;
use gents_host::http_client::normalize_optional_string;
use gents_service::args::ToolPackageArg;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BackendResolutionMode {
    Init,
    ConfigWrite,
}

pub(crate) fn resolve_backend_config_with_preset(
    preset: Option<BackendPresetArg>,
    explicit_endpoint: Option<&str>,
    explicit_provider_kind: Option<&str>,
    explicit_openai_wire_api: Option<OpenAiWireApiArg>,
    explicit_api_key: Option<&str>,
    explicit_api_key_env_var: Option<&str>,
    mode: BackendResolutionMode,
) -> Result<ResolvedBackendConfig> {
    let api_key = normalize_optional_string(explicit_api_key);
    let explicit_api_key_env_var = normalize_optional_string(explicit_api_key_env_var);
    if api_key.is_some() && explicit_api_key_env_var.is_some() {
        anyhow::bail!("provide either --api-key or --api-key-env-var, not both");
    }

    let endpoint = resolve_backend_endpoint(explicit_endpoint, preset, mode)?;
    let provider_kind = resolve_backend_provider_kind(explicit_provider_kind, preset)?;
    let openai_wire_api = if provider_kind.uses_messages_wire() {
        // Claude is not an OpenAI-wire provider. Never persist a sticky wire
        // value when migrating an existing OpenAiCompatible backend.
        None
    } else {
        resolve_openai_wire_api(explicit_openai_wire_api, preset)
    };
    let api_key_env_var =
        resolve_backend_api_key_env_var(explicit_api_key_env_var, api_key.is_some(), preset);

    Ok(ResolvedBackendConfig {
        provider_kind,
        openai_wire_api,
        endpoint,
        api_key,
        api_key_env_var,
    })
}

fn resolve_openai_wire_api(
    explicit: Option<OpenAiWireApiArg>,
    preset: Option<BackendPresetArg>,
) -> Option<OpenAiWireApi> {
    explicit
        .map(OpenAiWireApiArg::to_config)
        .or_else(|| preset.and_then(BackendPresetArg::default_openai_wire_api))
}

fn resolve_backend_endpoint(
    explicit: Option<&str>,
    preset: Option<BackendPresetArg>,
    mode: BackendResolutionMode,
) -> Result<String> {
    normalize_optional_string(explicit)
        .or_else(|| preset.and_then(|candidate| candidate.default_endpoint().map(str::to_string)))
        .or_else(|| {
            (mode == BackendResolutionMode::Init)
                .then(|| std::env::var("INFERENCE_ENDPOINT").ok())
                .flatten()
                .and_then(|value| {
                    let trimmed = value.trim();
                    (!trimmed.is_empty()).then(|| trimmed.to_string())
                })
        })
        .or_else(|| {
            (mode == BackendResolutionMode::Init).then(|| DEFAULT_INIT_ENDPOINT.to_string())
        })
        .ok_or_else(|| match mode {
            BackendResolutionMode::Init => anyhow::anyhow!(
                "an inference endpoint is required\nNext:\n  1. Pass it explicitly: `gents init --inference-url http://HOST:PORT/v1 --model-name MODEL`\n  2. Or choose a preset with a default endpoint: `gents init --backend-preset openrouter --model-name MODEL`\n  3. Or set INFERENCE_ENDPOINT before running `gents init`"
            ),
            BackendResolutionMode::ConfigWrite => anyhow::anyhow!(
                "an inference endpoint is required\nNext:\n  1. Pass --inference-url explicitly\n  2. Or choose a preset with a default endpoint, such as --backend-preset openrouter"
            ),
        })
}

fn resolve_backend_provider_kind(
    explicit: Option<&str>,
    preset: Option<BackendPresetArg>,
) -> Result<BackendProviderKind> {
    match normalize_optional_string(explicit) {
        Some(value) => Ok(BackendProviderKind::parse_optional(Some(&value))?),
        None => Ok(
            preset.map_or_else(BackendProviderKind::default, |candidate| {
                candidate.provider_kind()
            }),
        ),
    }
}

fn resolve_backend_api_key_env_var(
    explicit: Option<String>,
    raw_api_key_present: bool,
    preset: Option<BackendPresetArg>,
) -> Option<String> {
    explicit.or_else(|| {
        (!raw_api_key_present)
            .then(|| preset.and_then(|candidate| candidate.default_api_key_env_var()))
            .flatten()
            .map(ToOwned::to_owned)
    })
}

pub(crate) fn format_tool_package(value: ToolPackageArg) -> &'static str {
    match value {
        ToolPackageArg::Minimal => "minimal",
        ToolPackageArg::Introspection => "introspection",
        ToolPackageArg::Readonly => "readonly",
        ToolPackageArg::Write => "write",
        ToolPackageArg::Yolo => "yolo",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_cli_subscription_drops_sticky_openai_wire_api() {
        let resolved = resolve_backend_config_with_preset(
            Some(BackendPresetArg::ClaudeCliSubscription),
            None,
            None,
            Some(OpenAiWireApiArg::ChatCompletions),
            None,
            None,
            BackendResolutionMode::ConfigWrite,
        )
        .expect("resolve claude preset");

        assert_eq!(
            resolved.provider_kind,
            BackendProviderKind::ClaudeCliSubscription
        );
        assert_eq!(
            resolved.openai_wire_api, None,
            "Claude migration must not persist sticky openai_wire_api"
        );
    }
}
