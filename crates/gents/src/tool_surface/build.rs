use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::tool_service_health::{ToolServiceHealthProjection, ToolServiceHealthState};

use crate::toolset::{
    default_read_only_command_policy, CommandExecutionMode, CommandExecutionPolicy, ToolSet,
    ToolSetBuilder,
};

use super::modes::{BashMode, FileToolMode, ToolCeiling};
use super::policy::{meet_execution_mode, meet_network_mode, EndpointScope, ToolPolicyBash};

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

#[allow(clippy::too_many_arguments)]
pub(super) fn build_host_tools(
    behavior_name: &str,
    file_tools: FileToolMode,
    bash: BashMode,
    command_policy: Option<CommandExecutionPolicy>,
    effective_bash: &ToolPolicyBash,
    file_tool_root: Option<&Path>,
    cli_tool_names: &[String],
    ceiling: &ToolCeiling,
) -> Result<ToolSet> {
    // Per-request IsolatedWorkspace roots overlay into TOOL_RUNTIME_SCOPE
    // at claim time. Do not bake workspace_id paths into this ToolSet.
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

    let command_policy =
        constrain_command_policy_to_effective_bash(command_policy, effective_bash, bash);
    match bash {
        BashMode::Off => {}
        BashMode::ReadOnly => {
            builder = match command_policy.clone() {
                Some(policy) => builder.bash_read_only_with_policy_and_timeouts(
                    policy,
                    ceiling.command_timeout(),
                    ceiling.command_timeout_max(),
                ),
                None => builder.bash_read_only_with_timeouts(
                    ceiling.command_timeout(),
                    ceiling.command_timeout_max(),
                ),
            };
        }
        BashMode::Unrestricted => {
            let root = effective_root
                .clone()
                .ok_or_else(|| anyhow!("unrestricted bash requires a configured tool root"))?;
            builder = match command_policy.clone() {
                Some(policy) => builder.bash_unrestricted_with_policy_and_timeouts(
                    root,
                    policy,
                    ceiling.command_timeout(),
                    ceiling.command_timeout_max(),
                ),
                None => builder.bash_unrestricted_with_timeouts(
                    root,
                    ceiling.command_timeout(),
                    ceiling.command_timeout_max(),
                ),
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
    effective_bash: &ToolPolicyBash,
    bash: BashMode,
) -> Option<CommandExecutionPolicy> {
    let base: Option<CommandExecutionPolicy> = match (command_policy, bash) {
        (_, BashMode::Off) => return None,
        (None, _) => None,
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
    };

    let (allowed_override, deny_all) =
        project_allowed_prefixes(&effective_bash.allowed_argv_prefixes);
    let forbidden: Vec<Vec<String>> = effective_bash
        .forbidden_argv_prefixes
        .iter()
        .cloned()
        .collect();
    let read_only_override = project_read_only_allowlist(&effective_bash.read_only_allowlist);

    let imposes_constraint = deny_all
        || matches!(effective_bash.allowed_argv_prefixes, EndpointScope::Only(_))
        || !forbidden.is_empty()
        || read_only_override.is_some();

    let base = match base {
        Some(policy) => policy,
        None if imposes_constraint => match bash {
            BashMode::ReadOnly => default_read_only_command_policy(),
            _ => CommandExecutionPolicy::write_capable(),
        },
        None => return None,
    };

    Some(apply_effective_bash(
        base,
        effective_bash,
        allowed_override,
        deny_all,
        forbidden,
        read_only_override,
    ))
}

fn project_allowed_prefixes(
    scope: &EndpointScope<Vec<String>, ()>,
) -> (Option<Vec<Vec<String>>>, bool) {
    match scope {
        EndpointScope::All => (None, false),
        EndpointScope::None => (None, true),
        EndpointScope::Only(keys) if keys.is_empty() => (None, true),
        EndpointScope::Only(_) => (Some(scope.keys()), false),
    }
}

fn project_read_only_allowlist(scope: &EndpointScope<String, ()>) -> Option<Vec<String>> {
    match scope {
        EndpointScope::All => None,
        EndpointScope::None => Some(Vec::new()),
        EndpointScope::Only(keys) if keys.is_empty() => Some(Vec::new()),
        EndpointScope::Only(_) => Some(scope.keys()),
    }
}

fn apply_effective_bash(
    mut policy: CommandExecutionPolicy,
    effective_bash: &ToolPolicyBash,
    allowed_override: Option<Vec<Vec<String>>>,
    deny_all: bool,
    forbidden: Vec<Vec<String>>,
    read_only_override: Option<Vec<String>>,
) -> CommandExecutionPolicy {
    if deny_all {
        policy.allowed_argv_prefixes = Vec::new();
        policy = policy.with_deny_all_argv(true);
    } else if let Some(allowed) = allowed_override {
        policy.allowed_argv_prefixes = allowed;
    }
    if !forbidden.is_empty() {
        policy.forbidden_argv_prefixes = forbidden;
    }
    if let Some(read_only) = read_only_override {
        policy = policy.with_read_only_allowlist(read_only);
    }
    policy.mode = meet_execution_mode(policy.mode, effective_bash.execution_mode);
    policy.network_mode = meet_network_mode(policy.network_mode, effective_bash.network_mode);
    if effective_bash.deny_git_metadata_writes {
        policy = policy.with_git_worktree_diff();
    }
    policy
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

pub(crate) fn resolve_configured_tool_root(path: &Path) -> Result<PathBuf> {
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

pub(super) fn dedupe_subagent_targets(
    values: Vec<crate::document_config::SubagentTargetDocument>,
) -> Vec<crate::document_config::SubagentTargetDocument> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let mut deduped = Vec::with_capacity(values.len());
    for target in values {
        if target.name.trim().is_empty()
            || target.target_agent_did.trim().is_empty()
            || target.behavior_id.trim().is_empty()
        {
            continue;
        }
        if seen.insert(target.name.trim().to_string()) {
            deduped.push(target);
        }
    }
    deduped
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn top_bash() -> ToolPolicyBash {
        ToolPolicyBash::unrestricted()
    }

    #[test]
    fn no_command_policy_and_unconstrained_ceiling_stays_none() {
        // Preserve today's behavior: a behavior with no command policy and an
        // unconstrained effective bash resolves to no executable policy.
        let out =
            constrain_command_policy_to_effective_bash(None, &top_bash(), BashMode::Unrestricted);
        assert!(out.is_none());
    }

    #[test]
    fn ceiling_allowed_only_narrows_the_executable_gate() {
        let mut eb = top_bash();
        eb.allowed_argv_prefixes =
            EndpointScope::<Vec<String>, ()>::only_units([vec!["git".to_string()]]);
        let out = constrain_command_policy_to_effective_bash(None, &eb, BashMode::Unrestricted)
            .expect("a narrowing ceiling must synthesize an executable policy");
        assert_eq!(out.allowed_argv_prefixes, vec![vec!["git".to_string()]]);
        assert!(!out.deny_all_argv());
    }

    #[test]
    fn effective_only_empty_allowed_projects_to_deny_all_not_allow_all() {
        // The headline trap: Only(∅) must NOT become an empty allowed list
        // (which validates as allow-all) — it must set the deny-all sentinel.
        for scope in [
            EndpointScope::<Vec<String>, ()>::None,
            EndpointScope::<Vec<String>, ()>::only_units(Vec::<Vec<String>>::new()),
        ] {
            let mut eb = top_bash();
            eb.allowed_argv_prefixes = scope;
            let out = constrain_command_policy_to_effective_bash(None, &eb, BashMode::Unrestricted)
                .expect("deny-all must synthesize a policy");
            assert!(out.deny_all_argv(), "Only(empty)/None must be deny-all");
            assert!(out.allowed_argv_prefixes.is_empty());
        }
    }

    #[test]
    fn ceiling_forbidden_union_reaches_the_executable_policy() {
        let mut eb = top_bash();
        eb.forbidden_argv_prefixes =
            BTreeSet::from([vec!["curl".to_string()], vec!["rm".to_string()]]);
        let out = constrain_command_policy_to_effective_bash(None, &eb, BashMode::Unrestricted)
            .expect("a forbidden ceiling must synthesize a policy");
        assert!(out
            .forbidden_argv_prefixes
            .contains(&vec!["curl".to_string()]));
        assert!(out
            .forbidden_argv_prefixes
            .contains(&vec!["rm".to_string()]));
    }

    #[test]
    fn effective_all_allowed_keeps_the_behavior_base_gate() {
        // effective allowed = All ⇒ no ceiling narrowing ⇒ keep the behavior's
        // own allowed list unchanged.
        let base = CommandExecutionPolicy::write_capable()
            .with_allowed_argv_prefixes(vec![vec!["git".to_string()]]);
        let out = constrain_command_policy_to_effective_bash(
            Some(base),
            &top_bash(),
            BashMode::Unrestricted,
        )
        .expect("a Some base is preserved");
        assert_eq!(out.allowed_argv_prefixes, vec![vec!["git".to_string()]]);
        assert!(!out.deny_all_argv());
    }

    #[test]
    fn ceiling_read_only_only_narrows_the_executable_allowlist() {
        let mut eb = top_bash();
        eb.read_only_allowlist = EndpointScope::<String, ()>::only_units([String::from("cat")]);
        let out = constrain_command_policy_to_effective_bash(None, &eb, BashMode::ReadOnly)
            .expect("a read-only-narrowing ceiling must synthesize a policy");
        assert_eq!(out.read_only_allowlist(), ["cat".to_string()]);
    }
}

pub(super) async fn enabled_mcp_service_ids(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Vec<String>> {
    Ok(crate::registry::configured_mcp_services(node, agent_did)
        .await?
        .into_iter()
        .filter(|service| service.enabled)
        .map(|service| service.service_id)
        .collect())
}

/// MCP services whose principal registry row is enabled and whose agent-scoped
/// measured health permits calls, per `ToolServiceHealthState::project`
/// (the single owner of the classification): `Healthy` and `Stale` remain
/// callable (the normal health gate warns but proceeds on `Stale`);
/// `Unreachable` and missing/unrecognized measurements do not satisfy a
/// required-service dependency.
pub(crate) async fn measured_available_mcp_service_ids(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Vec<String>> {
    let services = crate::registry::configured_mcp_services(node, agent_did).await?;
    if services.is_empty() {
        return Ok(Vec::new());
    }
    let query = mcp_health_query(agent_did);
    let response = crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "tools.measured_availability",
        |txn| {
            let query = query.clone();
            Box::pin(async move { txn.execute(&query).await })
        },
    )
    .await?;
    let hostname = hostname::get()
        .ok()
        .and_then(|value| value.into_string().ok());
    project_measured_mcp_services(agent_did, &services, &response, hostname.as_deref())
}

/// Explain the same measured availability for local or HTTP control-plane
/// access. A remote server's loopback endpoint is never inferred from the CLI host.
pub async fn measured_mcp_services_for_access(
    access: &crate::config_client::ConfigAccess,
    agent_did: &str,
    services: &[crate::document_config::ToolServiceRegistry],
) -> Result<Vec<String>> {
    if services.is_empty() {
        return Ok(Vec::new());
    }
    let response = access.execute(&mcp_health_query(agent_did)).await?;
    let hostname = match access {
        crate::config_client::ConfigAccess::Local(_) => hostname::get()
            .ok()
            .and_then(|value| value.into_string().ok()),
        crate::config_client::ConfigAccess::Graphql(_) => None,
    };
    project_measured_mcp_services(agent_did, services, &response, hostname.as_deref())
}

fn mcp_health_query(agent_did: &str) -> String {
    format!(
        r#"{{ ToolServiceHealthState(filter: {{agent_did: {{_eq: "{}"}}}}) {{ service_id endpoint status }} }}"#,
        crate::graphql::escape_graphql_string(agent_did)
    )
}

fn project_measured_mcp_services(
    agent_did: &str,
    services: &[crate::document_config::ToolServiceRegistry],
    response: &serde_json::Value,
    local_hostname: Option<&str>,
) -> Result<Vec<String>> {
    anyhow::ensure!(
        !agent_did.trim().is_empty(),
        "MCP availability requires principal scope"
    );
    let mut expected_endpoints = HashMap::<String, HashSet<String>>::new();
    let mut seen = HashSet::new();
    for service in services {
        anyhow::ensure!(
            service.agent_did == agent_did,
            "MCP availability received foreign service"
        );
        anyhow::ensure!(
            seen.insert(&service.service_id),
            "duplicate scoped MCP service"
        );
        if !service.enabled {
            continue;
        }
        let Some(port) = service
            .mcp_port
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port > 0)
        else {
            continue;
        };
        let raw_path = service.mcp_path.as_deref().unwrap_or_default().trim();
        let path = if raw_path.is_empty() || raw_path.starts_with('/') {
            raw_path.to_owned()
        } else {
            format!("/{raw_path}")
        };
        let endpoints = expected_endpoints
            .entry(service.service_id.clone())
            .or_default();
        for host in [&service.hostname, &service.tailscale_ip, &service.lan_ip]
            .into_iter()
            .flatten()
            .filter(|host| !host.trim().is_empty())
        {
            endpoints.insert(format!("http://{host}:{port}{path}"));
            if local_hostname == Some(host.as_str()) {
                endpoints.insert(format!("http://127.0.0.1:{port}{path}"));
            }
        }
    }
    let rows = response
        .get("data")
        .and_then(|data| data.get("ToolServiceHealthState"))
        .and_then(serde_json::Value::as_array)
        .context("MCP health query returned no rows array")?;
    let mut available = rows
        .iter()
        .filter_map(|row| {
            let service_id = row.get("service_id")?.as_str()?;
            let endpoint = row.get("endpoint")?.as_str()?;
            let status = row.get("status")?.as_str()?;
            let callable = matches!(
                ToolServiceHealthState::parse_opt(Some(status))
                    .map(ToolServiceHealthState::project),
                Some(ToolServiceHealthProjection::Healthy | ToolServiceHealthProjection::Stale)
            );
            (callable
                && expected_endpoints
                    .get(service_id)
                    .is_some_and(|endpoints| endpoints.contains(endpoint)))
            .then(|| service_id.to_owned())
        })
        .collect::<Vec<_>>();
    available.sort();
    available.dedup();
    Ok(available)
}
