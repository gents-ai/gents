use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;

use crate::toolset::{
    default_read_only_command_policy, CommandExecutionMode, CommandExecutionPolicy, ToolSet,
    ToolSetBuilder,
};

use super::modes::{BashMode, FileToolMode, ToolCeiling};

pub(super) fn downgrade_file_tools(
    behavior_name: &str,
    requested: FileToolMode,
    ceiling: FileToolMode,
) -> FileToolMode {
    if requested.rank() <= ceiling.rank() {
        return requested;
    }

    tracing::warn!(
        behavior_id = %behavior_name,
        requested = ?requested,
        ceiling = ?ceiling,
        "downgrading file tool mode to fit tool ceiling"
    );
    ceiling
}

pub(super) fn downgrade_bash(
    behavior_name: &str,
    requested: BashMode,
    ceiling: BashMode,
) -> BashMode {
    if requested.rank() <= ceiling.rank() {
        return requested;
    }

    tracing::warn!(
        behavior_id = %behavior_name,
        requested = ?requested,
        ceiling = ?ceiling,
        "downgrading bash mode to fit tool ceiling"
    );
    ceiling
}

pub(super) fn build_host_tools(
    behavior_name: &str,
    file_tools: FileToolMode,
    bash: BashMode,
    command_policy: Option<CommandExecutionPolicy>,
    file_tool_root: Option<&Path>,
    cli_tool_names: &[String],
    ceiling: &ToolCeiling,
) -> Result<ToolSet> {
    let mut builder = ToolSetBuilder::default();
    let needs_file_tool_root =
        !matches!(file_tools, FileToolMode::Off) || !matches!(bash, BashMode::Off);
    let effective_root = if needs_file_tool_root {
        resolve_effective_tool_root(behavior_name, file_tool_root, ceiling.root())?
    } else {
        None
    };
    if let Some(root) = effective_root.clone() {
        builder = builder.read_root(root.clone());
    }

    if !matches!(file_tools, FileToolMode::Off) {
        builder = builder.list_files().read_file().glob().grep();
    }

    if matches!(file_tools, FileToolMode::ReadWrite) {
        let root = effective_root
            .clone()
            .ok_or_else(|| anyhow!("readwrite file tools require a configured tool root"))?;
        builder = builder.write_file(root.clone()).edit_file(root);
    }

    let command_policy = constrain_command_policy_to_effective_bash(command_policy, bash);
    match bash {
        BashMode::Off => {}
        BashMode::ReadOnly => {
            builder = match command_policy.clone() {
                Some(policy) => builder.bash_read_only_with_policy(policy),
                None => builder.bash_read_only(),
            };
        }
        BashMode::Unrestricted => {
            let root = effective_root
                .clone()
                .ok_or_else(|| anyhow!("unrestricted bash requires a configured tool root"))?;
            builder = match command_policy.clone() {
                Some(policy) => builder.bash_unrestricted_with_policy(root, policy),
                None => builder.bash_unrestricted(root),
            };
        }
    }

    let cli_tools = ceiling
        .cli_tools()
        .iter()
        .map(|tool| (tool.name.clone(), tool.clone()))
        .collect::<HashMap<_, _>>();
    for tool_name in dedupe_strings(cli_tool_names.to_vec()) {
        match cli_tools.get(&tool_name) {
            Some(tool) => builder = builder.cli_tool(tool.clone()),
            None => tracing::warn!(
                behavior_id = %behavior_name,
                cli_tool = %tool_name,
                "dropping CLI tool not present in tool ceiling"
            ),
        }
    }

    Ok(builder.build())
}

fn constrain_command_policy_to_effective_bash(
    command_policy: Option<CommandExecutionPolicy>,
    bash: BashMode,
) -> Option<CommandExecutionPolicy> {
    match (command_policy, bash) {
        (_, BashMode::Off) | (None, _) => None,
        (Some(policy), BashMode::Unrestricted) => Some(policy),
        (Some(policy), BashMode::ReadOnly)
            if matches!(policy.mode, CommandExecutionMode::ReadOnly) =>
        {
            Some(policy)
        }
        (Some(policy), BashMode::ReadOnly) => Some(
            default_read_only_command_policy()
                .with_allowed_argv_prefixes(policy.allowed_argv_prefixes)
                .with_forbidden_argv_prefixes(policy.forbidden_argv_prefixes)
                .with_network_mode(policy.network_mode),
        ),
    }
}

pub(super) fn resolve_effective_tool_root(
    behavior_name: &str,
    selection_root: Option<&Path>,
    ceiling_root: Option<&Path>,
) -> Result<Option<PathBuf>> {
    let selection_root = selection_root
        .map(resolve_configured_tool_root)
        .transpose()?;
    let ceiling_root = ceiling_root.map(resolve_configured_tool_root).transpose()?;

    match (selection_root, ceiling_root) {
        (Some(selection_root), Some(ceiling_root)) => {
            if selection_root.starts_with(&ceiling_root) {
                Ok(Some(selection_root))
            } else {
                bail!(
                    "behavior {behavior_name} file tool root {} escapes operator tool root {}",
                    selection_root.display(),
                    ceiling_root.display()
                );
            }
        }
        (Some(selection_root), None) => Ok(Some(selection_root)),
        (None, Some(ceiling_root)) => Ok(Some(ceiling_root)),
        (None, None) => Ok(None),
    }
}

pub(super) fn resolve_configured_tool_root(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .with_context(|| format!("resolving relative tool root {}", path.display()))?
            .join(path)
    };

    resolve_path_with_canonical_prefix(&absolute)
}

pub(super) fn resolve_path_with_canonical_prefix(path: &Path) -> Result<PathBuf> {
    let mut resolved = PathBuf::new();
    let mut missing_tail = false;

    for component in path.components() {
        match component {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                resolved.push(component.as_os_str());
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            std::path::Component::Normal(name) => {
                let candidate = resolved.join(name);
                if !missing_tail && candidate.exists() {
                    resolved = std::fs::canonicalize(&candidate).with_context(|| {
                        format!("canonicalizing tool root {}", candidate.display())
                    })?;
                } else {
                    missing_tail = true;
                    resolved.push(name);
                }
            }
        }
    }

    Ok(resolved)
}

pub(super) fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let mut deduped = Vec::with_capacity(values.len());
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            deduped.push(trimmed.to_string());
        }
    }
    deduped
}

pub(super) async fn has_registered_mcp_services(node: &EmbeddedNode) -> Result<bool> {
    let query = r#"{
  ToolServiceRegistry(
    filter: { status: { _eq: "online" } }
    limit: 1
  ) {
    service_id
  }
}"#;

    let response = node.execute(query).await;
    if response.has_errors() {
        bail!(
            "query ToolServiceRegistry for tool-surface resolution failed: {:?}",
            response.errors
        );
    }

    let services = response
        .data
        .as_ref()
        .and_then(|data| data.get("ToolServiceRegistry"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(!services.is_empty())
}
