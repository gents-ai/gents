use anyhow::Result;
use defra_agent::{CommandExecutionMode, CommandNetworkMode, ToolSelectionDocument};
use serde_json::json;

use crate::cli::*;
use crate::config_writes::{write_tool_selection_document, ConfigAccess};
use crate::print_json;
use crate::{normalize_bash_mode, normalize_file_tools_mode, normalize_optional_string};

pub(super) async fn tool_selection_set(args: ToolSelectionUpsertArgs) -> Result<()> {
    let file_tools_mode =
        normalize_file_tools_mode(args.enable_file_tools, args.file_tools_mode.as_deref())?;
    let bash_mode = normalize_bash_mode(args.enable_bash, args.bash_mode.as_deref())?;
    let command_execution_policy =
        normalize_optional_string(args.command_execution_policy.as_deref());
    if let Some(mode) = command_execution_policy.as_deref() {
        CommandExecutionMode::parse(mode)?;
    }
    let command_network_mode = normalize_optional_string(args.command_network_mode.as_deref());
    if let Some(mode) = command_network_mode.as_deref() {
        CommandNetworkMode::parse(mode)?;
    }
    let file_tool_root = args
        .file_tool_root
        .as_ref()
        .map(|path| path.to_string_lossy().to_string());
    let access = ConfigAccess::Graphql(args.graphql.clone());
    let selection = ToolSelectionDocument {
        selection_id: args.selection_id.clone(),
        agent_did: args.agent_did.clone(),
        display_name: args.display_name.clone(),
        enable_file_tools: Some(args.enable_file_tools),
        file_tools_mode: Some(file_tools_mode.clone()),
        file_tool_root: file_tool_root.clone(),
        enable_bash: Some(args.enable_bash),
        bash_mode: Some(bash_mode.clone()),
        command_execution_policy: command_execution_policy.clone(),
        command_allowed_argv_prefixes: Some(args.command_allowed_argv_prefixes.clone()),
        command_forbidden_argv_prefixes: Some(args.command_forbidden_argv_prefixes.clone()),
        command_network_mode: command_network_mode.clone(),
        cli_tool_names: Some(args.cli_tool_names.clone()),
        enable_meta_tools: Some(args.enable_meta_tools),
        allowed_mcp_service_ids: Some(args.allowed_mcp_service_ids.clone()),
        backgroundable_tool_names: Some(args.backgroundable_tool_names.clone()),
        subagent_targets: Some(Vec::new()),
        subagent_spawn_enabled: Some(false),
        subagent_steering_enabled: Some(false),
        subagent_background_enabled: Some(false),
        subagent_allow_cross_deployment: Some(false),
        cross_deployment_spawn_timeout_seconds: None,
        enable_defra_query: args.enable_defra_query,
        defra_query_collections: Some(args.defra_query_collections.clone()),
    };
    let doc_id = write_tool_selection_document(&access, &selection).await?;
    let output = json!({
        "doc_id": doc_id,
        "selection_id": args.selection_id,
        "agent_did": args.agent_did,
        "enable_file_tools": args.enable_file_tools,
        "file_tools_mode": file_tools_mode,
        "file_tool_root": file_tool_root,
        "enable_bash": args.enable_bash,
        "bash_mode": bash_mode,
        "command_execution_policy": command_execution_policy,
        "command_allowed_argv_prefixes": args.command_allowed_argv_prefixes,
        "command_forbidden_argv_prefixes": args.command_forbidden_argv_prefixes,
        "command_network_mode": command_network_mode,
        "cli_tool_names": args.cli_tool_names,
        "enable_meta_tools": args.enable_meta_tools,
        "allowed_mcp_service_ids": args.allowed_mcp_service_ids,
        "backgroundable_tool_names": args.backgroundable_tool_names,
        "enable_defra_query": args.enable_defra_query,
        "defra_query_collections": args.defra_query_collections,
    });
    print_json(&output)?;
    Ok(())
}
