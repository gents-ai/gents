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
        // The imperative `config tools set` exposes no flags for subagent
        // enablement (those are managed via `config apply` manifests). Emitting
        // `Some(false)`/`Some(empty)` here would write those fields into the
        // update mutation and silently clobber an apply-managed subagent
        // config. Leave them `None` so the writer omits them: on update the
        // stored values are preserved, on create the schema defaults apply.
        subagent_targets: None,
        subagent_spawn_enabled: None,
        subagent_steering_enabled: None,
        subagent_background_enabled: None,
        subagent_allow_cross_deployment: None,
        cross_deployment_spawn_timeout_seconds: None,
        enable_memory: args.enable_memory,
        enable_session_history_tool: args.enable_session_history_tool,
        enable_defra_query: args.enable_defra_query,
        defra_query_collections: Some(args.defra_query_collections.clone()),
        // `write_tools` is apply-managed (declared in `config apply` manifests);
        // the imperative command exposes no flag, so leave it `None` to preserve
        // any apply-managed decls on update (same rationale as subagent_targets).
        write_tools: None,
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
        "enable_memory": args.enable_memory,
        "enable_session_history_tool": args.enable_session_history_tool,
        "enable_defra_query": args.enable_defra_query,
        "defra_query_collections": args.defra_query_collections,
    });
    print_json(&output)?;
    Ok(())
}
