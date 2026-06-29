use anyhow::Result;
use defra_agent::{CommandExecutionMode, CommandNetworkMode, ToolSelectionDocument};
use serde_json::json;

use crate::cli::*;
use crate::config_writes::{write_tool_selection_document_with_clear_fields, ConfigAccess};
use crate::normalize_optional_string;
use crate::print_json;

pub(super) async fn tool_selection_set(args: ToolSelectionUpsertArgs) -> Result<()> {
    let plan = tool_selection_command_plan(&args)?;
    let access = ConfigAccess::Graphql(args.graphql.clone());
    let doc_id = write_tool_selection_document_with_clear_fields(
        &access,
        &plan.selection,
        &plan.clear_update_fields,
    )
    .await?;
    let output = json!({
        "doc_id": doc_id,
        "selection_id": args.selection_id,
        "agent_did": args.agent_did,
        "cleared_fields": plan.clear_update_fields,
        "display_name": plan.selection.display_name,
        "enable_file_tools": plan.selection.enable_file_tools,
        "file_tools_mode": plan.file_tools_mode,
        "file_tool_root": plan.file_tool_root,
        "enable_bash": plan.selection.enable_bash,
        "bash_mode": plan.bash_mode,
        "command_execution_policy": plan.command_execution_policy,
        "command_allowed_argv_prefixes": plan.selection.command_allowed_argv_prefixes,
        "command_forbidden_argv_prefixes": plan.selection.command_forbidden_argv_prefixes,
        "command_network_mode": plan.command_network_mode,
        "cli_tool_names": plan.selection.cli_tool_names,
        "enable_meta_tools": plan.selection.enable_meta_tools,
        "allowed_mcp_service_ids": plan.selection.allowed_mcp_service_ids,
        "backgroundable_tool_names": plan.selection.backgroundable_tool_names,
        "enable_memory": args.enable_memory,
        "enable_session_history_tool": args.enable_session_history_tool,
        "enable_defra_query": args.enable_defra_query,
        "defra_query_collections": plan.selection.defra_query_collections,
        "subagent_targets": plan.selection.subagent_targets,
        "subagent_spawn_enabled": args.subagent_spawn_enabled,
        "orchestration_enabled": args.orchestration_enabled,
        "subagent_steering_enabled": args.subagent_steering_enabled,
        "subagent_background_enabled": args.subagent_background_enabled,
        "subagent_allow_cross_deployment": args.subagent_allow_cross_deployment,
        "cross_deployment_spawn_timeout_seconds": args.cross_deployment_spawn_timeout_seconds,
    });
    print_json(&output)?;
    Ok(())
}

#[derive(Debug)]
struct ToolSelectionCommandPlan {
    selection: ToolSelectionDocument,
    file_tools_mode: Option<String>,
    file_tool_root: Option<String>,
    bash_mode: Option<String>,
    command_execution_policy: Option<String>,
    command_network_mode: Option<String>,
    clear_update_fields: Vec<&'static str>,
}

fn tool_selection_command_plan(args: &ToolSelectionUpsertArgs) -> Result<ToolSelectionCommandPlan> {
    let mut clear_update_fields = Vec::new();
    let (enable_file_tools, file_tools_mode) = file_tools_update(args, &mut clear_update_fields)?;
    let (enable_bash, bash_mode) = bash_update(args, &mut clear_update_fields)?;
    let subagent_targets = subagent_targets_update(&args)?;
    let command_execution_policy = nullable_string_update(
        args.command_execution_policy.as_deref(),
        args.clear_command_execution_policy,
        "--command-execution-policy",
        "--clear-command-execution-policy",
        "command_execution_policy",
        &mut clear_update_fields,
    )?;
    if let Some(mode) = command_execution_policy.as_deref() {
        CommandExecutionMode::parse(mode)?;
    }
    let command_network_mode = nullable_string_update(
        args.command_network_mode.as_deref(),
        args.clear_command_network_mode,
        "--command-network-mode",
        "--clear-command-network-mode",
        "command_network_mode",
        &mut clear_update_fields,
    )?;
    if let Some(mode) = command_network_mode.as_deref() {
        CommandNetworkMode::parse(mode)?;
    }
    let display_name = nullable_string_update(
        args.display_name.as_deref(),
        args.clear_display_name,
        "--display-name",
        "--clear-display-name",
        "display_name",
        &mut clear_update_fields,
    )?;
    let file_tool_root = nullable_path_update(
        args.file_tool_root.as_ref(),
        args.clear_file_tool_root,
        "--file-tool-root",
        "--clear-file-tool-root",
        "file_tool_root",
        &mut clear_update_fields,
    )?;
    let command_allowed_argv_prefixes = string_list_update(
        &args.command_allowed_argv_prefixes,
        args.clear_command_allowed_argv_prefixes,
        "--command-allowed-argv-prefix",
        "--clear-command-allowed-argv-prefixes",
    )?;
    let command_forbidden_argv_prefixes = string_list_update(
        &args.command_forbidden_argv_prefixes,
        args.clear_command_forbidden_argv_prefixes,
        "--command-forbidden-argv-prefix",
        "--clear-command-forbidden-argv-prefixes",
    )?;
    let cli_tool_names = string_list_update(
        &args.cli_tool_names,
        args.clear_cli_tool_names,
        "--cli-tool-name",
        "--clear-cli-tool-names",
    )?;
    let allowed_mcp_service_ids = string_list_update(
        &args.allowed_mcp_service_ids,
        args.clear_allowed_mcp_service_ids,
        "--allowed-mcp-service-id",
        "--clear-allowed-mcp-service-ids",
    )?;
    let backgroundable_tool_names = string_list_update(
        &args.backgroundable_tool_names,
        args.clear_backgroundable_tool_names,
        "--backgroundable-tool-name",
        "--clear-backgroundable-tool-names",
    )?;
    let defra_query_collections = string_list_update(
        &args.defra_query_collections,
        args.clear_defra_query_collections,
        "--defra-query-collection",
        "--clear-defra-query-collections",
    )?;
    let cross_deployment_spawn_timeout_seconds = timeout_update(args, &mut clear_update_fields)?;
    let selection = ToolSelectionDocument {
        selection_id: args.selection_id.clone(),
        agent_did: args.agent_did.clone(),
        display_name,
        tool_policy_version: None,
        enable_file_tools,
        file_tools_mode: file_tools_mode.clone(),
        file_tool_root: file_tool_root.clone(),
        enable_bash,
        bash_mode: bash_mode.clone(),
        command_execution_policy: command_execution_policy.clone(),
        command_allowed_argv_prefixes,
        command_forbidden_argv_prefixes,
        command_network_mode: command_network_mode.clone(),
        cli_tool_names,
        enable_meta_tools: args.enable_meta_tools,
        allowed_mcp_service_ids,
        backgroundable_tool_names,
        subagent_targets: subagent_targets.clone(),
        subagent_spawn_enabled: args.subagent_spawn_enabled,
        orchestration_enabled: args.orchestration_enabled,
        subagent_steering_enabled: args.subagent_steering_enabled,
        subagent_background_enabled: args.subagent_background_enabled,
        subagent_default_await_mode: None,
        subagent_allow_cross_deployment: args.subagent_allow_cross_deployment,
        cross_deployment_spawn_timeout_seconds,
        enable_memory: args.enable_memory,
        enable_session_history_tool: args.enable_session_history_tool,
        enable_defra_query: args.enable_defra_query,
        defra_query_collections,
        // `write_tools` is apply-managed (declared in `config apply` manifests);
        // the imperative command exposes no flag, so leave it `None` to preserve
        // any apply-managed decls on update (same rationale as subagent_targets).
        write_tools: None,
    };
    selection.validate()?;
    Ok(ToolSelectionCommandPlan {
        selection,
        file_tools_mode,
        file_tool_root,
        bash_mode,
        command_execution_policy,
        command_network_mode,
        clear_update_fields,
    })
}

fn file_tools_update(
    args: &ToolSelectionUpsertArgs,
    clear_update_fields: &mut Vec<&'static str>,
) -> Result<(Option<bool>, Option<String>)> {
    if args.clear_file_tools_mode {
        if args.file_tools_mode.is_some() || args.enable_file_tools.is_some() {
            anyhow::bail!(
                "--clear-file-tools-mode cannot be combined with --file-tools-mode or --enable-file-tools"
            );
        }
        clear_update_fields.push("file_tools_mode");
        return Ok((None, None));
    }

    let Some(enabled) = args.enable_file_tools else {
        let mode = normalize_optional_string(args.file_tools_mode.as_deref());
        if let Some(mode) = mode.as_deref() {
            defra_agent::FileToolMode::parse(mode)?;
        }
        return Ok((None, mode));
    };

    let mode = if enabled {
        normalize_optional_string(args.file_tools_mode.as_deref())
            .unwrap_or_else(|| "ReadOnly".to_string())
    } else {
        "Off".to_string()
    };
    defra_agent::FileToolMode::parse(&mode)?;
    Ok((Some(enabled), Some(mode)))
}

fn bash_update(
    args: &ToolSelectionUpsertArgs,
    clear_update_fields: &mut Vec<&'static str>,
) -> Result<(Option<bool>, Option<String>)> {
    if args.clear_bash_mode {
        if args.bash_mode.is_some() || args.enable_bash.is_some() {
            anyhow::bail!("--clear-bash-mode cannot be combined with --bash-mode or --enable-bash");
        }
        clear_update_fields.push("bash_mode");
        return Ok((None, None));
    }

    let Some(enabled) = args.enable_bash else {
        let mode = normalize_optional_string(args.bash_mode.as_deref());
        if let Some(mode) = mode.as_deref() {
            defra_agent::BashMode::parse(mode)?;
        }
        return Ok((None, mode));
    };

    let mode = if enabled {
        normalize_optional_string(args.bash_mode.as_deref())
            .unwrap_or_else(|| "ReadOnly".to_string())
    } else {
        "Off".to_string()
    };
    defra_agent::BashMode::parse(&mode)?;
    Ok((Some(enabled), Some(mode)))
}

fn nullable_string_update(
    value: Option<&str>,
    clear: bool,
    value_flag: &str,
    clear_flag: &str,
    field_name: &'static str,
    clear_update_fields: &mut Vec<&'static str>,
) -> Result<Option<String>> {
    if clear {
        if normalize_optional_string(value).is_some() {
            anyhow::bail!("{clear_flag} cannot be combined with {value_flag}");
        }
        clear_update_fields.push(field_name);
        return Ok(None);
    }
    Ok(normalize_optional_string(value))
}

fn nullable_path_update(
    value: Option<&std::path::PathBuf>,
    clear: bool,
    value_flag: &str,
    clear_flag: &str,
    field_name: &'static str,
    clear_update_fields: &mut Vec<&'static str>,
) -> Result<Option<String>> {
    if clear {
        if value.is_some() {
            anyhow::bail!("{clear_flag} cannot be combined with {value_flag}");
        }
        clear_update_fields.push(field_name);
        return Ok(None);
    }
    Ok(value.map(|path| path.to_string_lossy().to_string()))
}

fn string_list_update(
    values: &[String],
    clear: bool,
    value_flag: &str,
    clear_flag: &str,
) -> Result<Option<Vec<String>>> {
    if clear {
        if !values.is_empty() {
            anyhow::bail!("{clear_flag} cannot be combined with {value_flag}");
        }
        Ok(Some(Vec::new()))
    } else if values.is_empty() {
        Ok(None)
    } else {
        Ok(Some(values.to_vec()))
    }
}

fn timeout_update(
    args: &ToolSelectionUpsertArgs,
    clear_update_fields: &mut Vec<&'static str>,
) -> Result<Option<i64>> {
    if args.clear_cross_deployment_spawn_timeout_seconds {
        if args.cross_deployment_spawn_timeout_seconds.is_some() {
            anyhow::bail!(
                "--clear-cross-deployment-spawn-timeout-seconds cannot be combined with --cross-deployment-spawn-timeout-seconds"
            );
        }
        clear_update_fields.push("cross_deployment_spawn_timeout_seconds");
        return Ok(None);
    }

    if let Some(timeout) = args.cross_deployment_spawn_timeout_seconds {
        if timeout <= 0 {
            anyhow::bail!("--cross-deployment-spawn-timeout-seconds must be greater than zero");
        }
        return Ok(Some(timeout));
    }
    Ok(None)
}

fn subagent_targets_update(args: &ToolSelectionUpsertArgs) -> Result<Option<Vec<String>>> {
    if args.clear_subagent_targets && !args.subagent_targets.is_empty() {
        anyhow::bail!("--clear-subagent-targets cannot be combined with --subagent-target");
    }
    if args.clear_subagent_targets {
        Ok(Some(Vec::new()))
    } else if args.subagent_targets.is_empty() {
        Ok(None)
    } else {
        Ok(Some(args.subagent_targets.clone()))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn default_args() -> ToolSelectionUpsertArgs {
        ToolSelectionUpsertArgs {
            graphql: "http://127.0.0.1:9191/api/v0/graphql".to_string(),
            agent_did: "did:key:z-test".to_string(),
            selection_id: "default-tools".to_string(),
            display_name: None,
            clear_display_name: false,
            enable_file_tools: None,
            file_tools_mode: None,
            clear_file_tools_mode: false,
            file_tool_root: None::<PathBuf>,
            clear_file_tool_root: false,
            enable_bash: None,
            bash_mode: None,
            clear_bash_mode: false,
            command_execution_policy: None,
            clear_command_execution_policy: false,
            command_network_mode: None,
            clear_command_network_mode: false,
            command_allowed_argv_prefixes: Vec::new(),
            clear_command_allowed_argv_prefixes: false,
            command_forbidden_argv_prefixes: Vec::new(),
            clear_command_forbidden_argv_prefixes: false,
            cli_tool_names: Vec::new(),
            clear_cli_tool_names: false,
            enable_meta_tools: None,
            allowed_mcp_service_ids: Vec::new(),
            clear_allowed_mcp_service_ids: false,
            backgroundable_tool_names: Vec::new(),
            clear_backgroundable_tool_names: false,
            enable_memory: None,
            enable_session_history_tool: None,
            enable_defra_query: None,
            defra_query_collections: Vec::new(),
            clear_defra_query_collections: false,
            subagent_targets: Vec::new(),
            clear_subagent_targets: false,
            subagent_spawn_enabled: None,
            orchestration_enabled: None,
            subagent_steering_enabled: None,
            subagent_background_enabled: None,
            subagent_allow_cross_deployment: None,
            cross_deployment_spawn_timeout_seconds: None,
            clear_cross_deployment_spawn_timeout_seconds: false,
        }
    }

    #[test]
    fn subagent_flags_build_tool_selection_document() {
        let target = defra_agent::subagent_target_entry(
            "worker",
            "did:key:z-test",
            "worker",
            Some("worker behavior".to_string()),
        );
        let mut args = default_args();
        args.subagent_targets = vec![target.clone()];
        args.subagent_spawn_enabled = Some(true);
        args.orchestration_enabled = Some(true);
        args.subagent_steering_enabled = Some(true);
        args.subagent_background_enabled = Some(false);
        args.subagent_allow_cross_deployment = Some(true);
        args.cross_deployment_spawn_timeout_seconds = Some(90);

        let plan = tool_selection_command_plan(&args).unwrap();

        assert_eq!(plan.selection.subagent_targets, Some(vec![target]));
        assert_eq!(plan.selection.subagent_spawn_enabled, Some(true));
        assert_eq!(plan.selection.orchestration_enabled, Some(true));
        assert_eq!(plan.selection.subagent_steering_enabled, Some(true));
        assert_eq!(plan.selection.subagent_background_enabled, Some(false));
        assert_eq!(plan.selection.subagent_allow_cross_deployment, Some(true));
        assert_eq!(
            plan.selection.cross_deployment_spawn_timeout_seconds,
            Some(90)
        );
    }

    #[test]
    fn omitted_subagent_flags_preserve_and_clear_is_explicit() {
        let args = default_args();
        let plan = tool_selection_command_plan(&args).unwrap();
        assert_eq!(plan.selection.display_name, None);
        assert_eq!(plan.selection.enable_file_tools, None);
        assert_eq!(plan.selection.file_tools_mode, None);
        assert_eq!(plan.selection.file_tool_root, None);
        assert_eq!(plan.selection.enable_bash, None);
        assert_eq!(plan.selection.bash_mode, None);
        assert_eq!(plan.selection.command_execution_policy, None);
        assert_eq!(plan.selection.command_allowed_argv_prefixes, None);
        assert_eq!(plan.selection.command_forbidden_argv_prefixes, None);
        assert_eq!(plan.selection.command_network_mode, None);
        assert_eq!(plan.selection.cli_tool_names, None);
        assert_eq!(plan.selection.enable_meta_tools, None);
        assert_eq!(plan.selection.allowed_mcp_service_ids, None);
        assert_eq!(plan.selection.backgroundable_tool_names, None);
        assert_eq!(plan.selection.defra_query_collections, None);
        assert_eq!(plan.selection.subagent_targets, None);
        assert_eq!(plan.selection.subagent_spawn_enabled, None);
        assert_eq!(plan.selection.orchestration_enabled, None);
        assert_eq!(plan.selection.subagent_steering_enabled, None);
        assert_eq!(plan.selection.subagent_background_enabled, None);
        assert_eq!(plan.selection.subagent_allow_cross_deployment, None);
        assert_eq!(plan.selection.cross_deployment_spawn_timeout_seconds, None);
        assert!(plan.clear_update_fields.is_empty());

        let mut clear_args = default_args();
        clear_args.clear_subagent_targets = true;
        let plan = tool_selection_command_plan(&clear_args).unwrap();
        assert_eq!(plan.selection.subagent_targets, Some(Vec::new()));
    }

    #[test]
    fn explicit_tool_fields_update_and_clear_with_patch_semantics() {
        let mut args = default_args();
        args.display_name = Some("Ops".to_string());
        args.enable_file_tools = Some(true);
        args.file_tool_root = Some(PathBuf::from("/tmp/workspace"));
        args.enable_bash = Some(false);
        args.command_allowed_argv_prefixes = vec!["git status".to_string()];
        args.cli_tool_names = vec!["rg".to_string()];
        args.enable_meta_tools = Some(false);
        args.allowed_mcp_service_ids = vec!["observability".to_string()];
        args.backgroundable_tool_names = vec!["bash".to_string()];
        args.defra_query_collections = vec!["AgentRequest".to_string()];

        let plan = tool_selection_command_plan(&args).unwrap();

        assert_eq!(plan.selection.display_name.as_deref(), Some("Ops"));
        assert_eq!(plan.selection.enable_file_tools, Some(true));
        assert_eq!(plan.selection.file_tools_mode.as_deref(), Some("ReadOnly"));
        assert_eq!(
            plan.selection.file_tool_root.as_deref(),
            Some("/tmp/workspace")
        );
        assert_eq!(plan.selection.enable_bash, Some(false));
        assert_eq!(plan.selection.bash_mode.as_deref(), Some("Off"));
        assert_eq!(
            plan.selection.command_allowed_argv_prefixes,
            Some(vec!["git status".to_string()])
        );
        assert_eq!(plan.selection.cli_tool_names, Some(vec!["rg".to_string()]));
        assert_eq!(plan.selection.enable_meta_tools, Some(false));
        assert_eq!(
            plan.selection.allowed_mcp_service_ids,
            Some(vec!["observability".to_string()])
        );
        assert_eq!(
            plan.selection.backgroundable_tool_names,
            Some(vec!["bash".to_string()])
        );
        assert_eq!(
            plan.selection.defra_query_collections,
            Some(vec!["AgentRequest".to_string()])
        );

        let mut clear_args = default_args();
        clear_args.clear_display_name = true;
        clear_args.clear_file_tool_root = true;
        clear_args.clear_command_execution_policy = true;
        clear_args.clear_command_network_mode = true;
        clear_args.clear_command_allowed_argv_prefixes = true;
        clear_args.clear_cli_tool_names = true;
        clear_args.clear_allowed_mcp_service_ids = true;
        clear_args.clear_backgroundable_tool_names = true;
        clear_args.clear_defra_query_collections = true;
        clear_args.clear_cross_deployment_spawn_timeout_seconds = true;
        let plan = tool_selection_command_plan(&clear_args).unwrap();
        assert_eq!(
            plan.clear_update_fields,
            vec![
                "command_execution_policy",
                "command_network_mode",
                "display_name",
                "file_tool_root",
                "cross_deployment_spawn_timeout_seconds"
            ]
        );
        assert_eq!(
            plan.selection.command_allowed_argv_prefixes,
            Some(Vec::new())
        );
        assert_eq!(plan.selection.cli_tool_names, Some(Vec::new()));
        assert_eq!(plan.selection.allowed_mcp_service_ids, Some(Vec::new()));
        assert_eq!(plan.selection.backgroundable_tool_names, Some(Vec::new()));
        assert_eq!(plan.selection.defra_query_collections, Some(Vec::new()));
    }

    #[test]
    fn invalid_subagent_combinations_are_rejected() {
        let target = defra_agent::subagent_target_entry(
            "worker",
            "did:key:z-test",
            "worker",
            Some("worker behavior".to_string()),
        );
        let mut args = default_args();
        args.clear_subagent_targets = true;
        args.subagent_targets = vec![target];
        assert!(tool_selection_command_plan(&args)
            .unwrap_err()
            .to_string()
            .contains("--clear-subagent-targets"));

        let mut timeout_args = default_args();
        timeout_args.cross_deployment_spawn_timeout_seconds = Some(0);
        assert!(tool_selection_command_plan(&timeout_args)
            .unwrap_err()
            .to_string()
            .contains("must be greater than zero"));

        let mut clear_conflict_args = default_args();
        clear_conflict_args.clear_allowed_mcp_service_ids = true;
        clear_conflict_args.allowed_mcp_service_ids = vec!["observability".to_string()];
        assert!(tool_selection_command_plan(&clear_conflict_args)
            .unwrap_err()
            .to_string()
            .contains("--clear-allowed-mcp-service-ids"));
    }
}
