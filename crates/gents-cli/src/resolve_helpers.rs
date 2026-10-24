use std::path::{Path, PathBuf};

use anyhow::Result;
use gents::{cli_tool, BackendProviderKind, OpenAiWireApi};

use crate::cli::args::{BackendPresetArg, OpenAiWireApiArg, ToolCeilingArg, ToolPackageArg};
use crate::shared::ResolvedBackendConfig;
use crate::{normalize_optional_string, DEFAULT_INIT_ENDPOINT};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ResolveHelpersError {
    #[error("an inference endpoint is required\nNext:\n  1. Pass it explicitly: `gents init --inference-url http://HOST:PORT/v1 --model-name MODEL`\n  2. Or choose a preset with a default endpoint: `gents init --backend-preset openrouter --model-name MODEL`\n  3. Or set INFERENCE_ENDPOINT before running `gents init`")]
    InitEndpointRequired,
    #[error("an inference endpoint is required\nNext:\n  1. Pass --inference-url explicitly\n  2. Or choose a preset with a default endpoint, such as --backend-preset openrouter")]
    ConfigWriteEndpointRequired,
    #[error("--cli-tool must be NAME=/absolute/path")]
    CliToolArgument,
    #[error("--tool-root is required when --tool-ceiling readwrite")]
    ReadwriteRequiresRoot,
    #[error("provide either --api-key or --api-key-env-var, not both")]
    ApiKeyConflict,
}

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
        return Err(ResolveHelpersError::ApiKeyConflict.into());
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
) -> Result<String, ResolveHelpersError> {
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
        .ok_or(match mode {
            BackendResolutionMode::Init => ResolveHelpersError::InitEndpointRequired,
            BackendResolutionMode::ConfigWrite => ResolveHelpersError::ConfigWriteEndpointRequired,
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

pub(crate) fn parse_cli_tool_arg(value: &str) -> Result<gents::CliToolConfig, ResolveHelpersError> {
    let (name, path) = value
        .split_once('=')
        .ok_or(ResolveHelpersError::CliToolArgument)?;
    let name = name.trim();
    let path = path.trim();
    if name.is_empty() || path.is_empty() {
        return Err(ResolveHelpersError::CliToolArgument);
    }

    Ok(cli_tool(
        name,
        PathBuf::from(path),
        format!("Run the approved {name} CLI."),
    ))
}

/// The host ceiling a `gents server` enforces for this mode and root. Serve
/// resolves the readonly default root before calling; a readonly ceiling
/// without one leaves file tools unrooted.
pub(crate) fn host_tool_ceiling(
    mode: ToolCeilingArg,
    root: Option<&Path>,
) -> Result<gents::ToolCeiling, ResolveHelpersError> {
    Ok(match (mode, root) {
        (ToolCeilingArg::MetaOnly, _) => gents::ToolCeiling::meta_only(),
        (ToolCeilingArg::Readonly, Some(root)) => gents::ToolCeiling::readonly_at(root),
        (ToolCeilingArg::Readonly, None) => gents::ToolCeiling::readonly(),
        (ToolCeilingArg::Readwrite, Some(root)) => gents::ToolCeiling::readwrite(root),
        (ToolCeilingArg::Readwrite, None) => {
            return Err(ResolveHelpersError::ReadwriteRequiresRoot)
        }
    })
}

pub(crate) fn format_tool_ceiling(value: ToolCeilingArg) -> &'static str {
    match value {
        ToolCeilingArg::MetaOnly => "meta-only",
        ToolCeilingArg::Readonly => "readonly",
        ToolCeilingArg::Readwrite => "readwrite",
    }
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
