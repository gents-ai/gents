use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use defra_agent::{
    cli_tool, graphql::escape_graphql_string, BackendProviderKind, BashMode, FileToolMode,
};

use crate::cli::args::{BackendPresetArg, ToolCeilingArg};
use crate::shared::ResolvedBackendConfig;
use crate::{
    first_graphql_row, normalize_optional_string, post_graphql, require_non_empty,
    DEFAULT_INIT_ENDPOINT,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BackendResolutionMode {
    Init,
    ConfigWrite,
}

pub(crate) fn default_backend_max_queue_depth() -> i64 {
    100
}

pub(crate) fn resolve_backend_config_with_preset(
    preset: Option<BackendPresetArg>,
    explicit_endpoint: Option<&str>,
    explicit_provider_kind: Option<&str>,
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
    let api_key_env_var =
        resolve_backend_api_key_env_var(explicit_api_key_env_var, api_key.is_some(), preset);

    Ok(ResolvedBackendConfig {
        provider_kind,
        endpoint,
        api_key,
        api_key_env_var,
    })
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
                "an inference endpoint is required\nNext:\n  1. Pass it explicitly: `defra-agent init http://HOST:PORT/v1 --model-name MODEL`\n  2. Or choose a preset with a default endpoint: `defra-agent init --backend-preset openrouter --model-name MODEL`\n  3. Or set INFERENCE_ENDPOINT before running `defra-agent init`"
            ),
            BackendResolutionMode::ConfigWrite => anyhow::anyhow!(
                "an inference endpoint is required\nNext:\n  1. Pass --endpoint explicitly\n  2. Or choose a preset with a default endpoint, such as --backend-preset openrouter"
            ),
        })
}

fn resolve_backend_provider_kind(
    explicit: Option<&str>,
    preset: Option<BackendPresetArg>,
) -> Result<BackendProviderKind> {
    match normalize_optional_string(explicit) {
        Some(value) => BackendProviderKind::parse_optional(Some(&value)),
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

pub(crate) fn resolve_task_prompt(
    prompt: Option<&str>,
    prompt_file: Option<&Path>,
) -> Result<String> {
    match (prompt, prompt_file) {
        (Some(_), Some(path)) => anyhow::bail!(
            "provide either --prompt or --prompt-file, not both ({})",
            path.display()
        ),
        (Some(prompt), None) => Ok(require_non_empty("prompt", prompt)?.to_string()),
        (None, Some(path)) => {
            let prompt = fs::read_to_string(path)
                .with_context(|| format!("reading task prompt from {}", path.display()))?;
            Ok(require_non_empty("prompt-file", &prompt)?.to_string())
        }
        (None, None) => anyhow::bail!("a task prompt is required; pass --prompt or --prompt-file"),
    }
}

pub(crate) async fn resolve_scheduled_task_behavior_id(
    graphql: &str,
    agent_did: &str,
    explicit_behavior_id: Option<&str>,
) -> Result<String> {
    let behavior_id = match explicit_behavior_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(behavior_id) => behavior_id.to_string(),
        None => load_default_behavior_id_for_agent(graphql, agent_did).await?,
    };

    ensure_behavior_belongs_to_agent(graphql, agent_did, &behavior_id).await?;
    Ok(behavior_id)
}

async fn load_default_behavior_id_for_agent(graphql: &str, agent_did: &str) -> Result<String> {
    let query = format!(
        r#"{{
            AgentPrincipal(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                limit: 1
            ) {{
                default_behavior_id
            }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
    );
    let response = post_graphql(graphql, &query).await?;
    let principal = first_graphql_row(&response, "AgentPrincipal")
        .with_context(|| format!("loading AgentPrincipal for {agent_did}"))?;
    principal
        .get("default_behavior_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("AgentPrincipal {agent_did} has no default_behavior_id"))
}

async fn ensure_behavior_belongs_to_agent(
    graphql: &str,
    agent_did: &str,
    behavior_id: &str,
) -> Result<()> {
    let query = format!(
        r#"{{
            AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{behavior_id}" }} }},
                limit: 1
            ) {{
                behavior_id
                agent_did
            }}
        }}"#,
        behavior_id = escape_graphql_string(behavior_id),
    );
    let response = post_graphql(graphql, &query).await?;
    let behavior = first_graphql_row(&response, "AgentBehavior")
        .with_context(|| format!("loading AgentBehavior {behavior_id}"))?;
    let owner = behavior
        .get("agent_did")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("AgentBehavior {behavior_id} is missing agent_did"))?;
    if owner != agent_did {
        anyhow::bail!(
            "AgentBehavior {} belongs to {} not {}",
            behavior_id,
            owner,
            agent_did
        );
    }
    Ok(())
}

pub(crate) fn parse_cli_tool_arg(value: &str) -> Result<defra_agent::CliToolConfig> {
    let (name, path) = value
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("--cli-tool must be NAME=/absolute/path"))?;
    let name = name.trim();
    let path = path.trim();
    if name.is_empty() || path.is_empty() {
        anyhow::bail!("--cli-tool must be NAME=/absolute/path");
    }

    Ok(cli_tool(
        name,
        PathBuf::from(path),
        format!("Run the approved {name} CLI."),
    ))
}

pub(crate) fn normalize_file_tools_mode(enabled: bool, explicit: Option<&str>) -> Result<String> {
    let value = if enabled {
        explicit
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("ReadOnly")
    } else {
        "Off"
    };
    FileToolMode::parse(value)?;
    Ok(value.to_string())
}

pub(crate) fn normalize_bash_mode(enabled: bool, explicit: Option<&str>) -> Result<String> {
    let value = if enabled {
        explicit
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("ReadOnly")
    } else {
        "Off"
    };
    BashMode::parse(value)?;
    Ok(value.to_string())
}

pub(crate) fn format_tool_ceiling(value: ToolCeilingArg) -> &'static str {
    match value {
        ToolCeilingArg::MetaOnly => "meta-only",
        ToolCeilingArg::Readonly => "readonly",
        ToolCeilingArg::Readwrite => "readwrite",
    }
}
