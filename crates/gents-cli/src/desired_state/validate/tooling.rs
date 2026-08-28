use std::collections::{BTreeMap, BTreeSet, HashSet};

use gents::{
    is_reserved_builtin_tool_name, CommandExecutionMode, CommandNetworkMode, SubagentTarget,
    SurfaceToolDecl, WriteToolDecl, KEY_BACKEND_KEYRING,
};

use super::super::{
    DesiredDatastoreToolSurface, DesiredEthTool, DesiredStateManifest, DesiredToolSelection,
};
use super::storage::non_empty;

pub(super) fn validate_surfaces(
    manifest: &DesiredStateManifest,
    principal_agent_did: &str,
    errors: &mut Vec<String>,
) {
    let mut surface_ids = BTreeSet::new();
    for surface in &manifest.datastore_tool_surfaces {
        let surface_id = surface.surface_id.trim();
        if surface_id.is_empty() {
            errors.push("DatastoreToolSurface has empty surface_id".to_string());
        } else if !surface_ids.insert(surface_id.to_string()) {
            errors.push(format!(
                "duplicate DatastoreToolSurface surface_id {surface_id}"
            ));
        }
        if !principal_agent_did.is_empty() && surface.agent_did.trim() != principal_agent_did {
            errors.push(format!(
                "DatastoreToolSurface {} agent_did does not match principal",
                surface.surface_id
            ));
        }
        validate_datastore_surface_entries(
            &format!("surface:{}", surface.surface_id),
            &surface.entries,
            errors,
        );
    }
}

pub(super) fn validate_eth_tools(
    manifest: &DesiredStateManifest,
    principal_agent_did: &str,
    errors: &mut Vec<String>,
) {
    let mut binding_ids = BTreeSet::new();
    for binding in &manifest.chain_key_bindings {
        let binding_id = binding.binding_id.trim();
        if binding_id.is_empty() {
            errors.push("ChainKeyBinding has empty binding_id".to_string());
        } else if !binding_ids.insert(binding_id.to_string()) {
            errors.push(format!("duplicate ChainKeyBinding binding_id {binding_id}"));
        }
        if !principal_agent_did.is_empty() && binding.principal_did.trim() != principal_agent_did {
            errors.push(format!(
                "ChainKeyBinding {} principal_did does not match principal",
                binding.binding_id
            ));
        }
        if !is_eth_address(&binding.address) {
            errors.push(format!(
                "ChainKeyBinding {} has an invalid Ethereum address",
                binding.binding_id
            ));
        }
        if binding.key_backend.trim() != KEY_BACKEND_KEYRING {
            errors.push(format!(
                "ChainKeyBinding {} has unsupported key_backend {:?}",
                binding.binding_id, binding.key_backend
            ));
        }
        if binding
            .attestation
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        {
            errors.push(format!(
                "ChainKeyBinding {} has no principal attestation",
                binding.binding_id
            ));
        }
        if binding
            .created_at
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        {
            errors.push(format!(
                "ChainKeyBinding {} has no created_at timestamp",
                binding.binding_id
            ));
        }
    }

    let mut tool_ids = BTreeSet::new();
    for tool in &manifest.eth_tools {
        let tool_id = tool.tool_id.trim();
        if tool_id.is_empty() {
            errors.push("EthTool has empty tool_id".to_string());
        } else if !tool_ids.insert(tool_id.to_string()) {
            errors.push(format!("duplicate EthTool tool_id {tool_id}"));
        }
        if !principal_agent_did.is_empty() && tool.agent_did.trim() != principal_agent_did {
            errors.push(format!(
                "EthTool {} agent_did does not match principal",
                tool.tool_id
            ));
        }
        if tool.rpc_url.trim().is_empty() {
            errors.push(format!("EthTool {} has empty rpc_url", tool.tool_id));
        }
        if tool.chain_id <= 0 {
            errors.push(format!(
                "EthTool {} must have a positive chain_id",
                tool.tool_id
            ));
        }
        if let Some(binding_id) = tool.key_binding_id.as_deref().map(str::trim) {
            if binding_id.is_empty() {
                errors.push(format!(
                    "EthTool {} has an empty key_binding_id",
                    tool.tool_id
                ));
                continue;
            }
            if !binding_ids.contains(binding_id) {
                errors.push(format!(
                    "EthTool {} references missing ChainKeyBinding {}",
                    tool.tool_id, binding_id
                ));
            }
        }
    }
}

fn is_eth_address(value: &str) -> bool {
    let value = value.trim();
    value.len() == 42
        && value.starts_with("0x")
        && value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn validate_tool_selections(
    manifest: &DesiredStateManifest,
    principal_agent_did: &str,
    errors: &mut Vec<String>,
) -> BTreeSet<String> {
    let mut tool_selection_ids = BTreeSet::new();
    for selection in &manifest.tool_selections {
        let selection_id = selection.selection_id.trim();
        if selection_id.is_empty() {
            errors.push(
                "tool-selections.json contains a tool selection with an empty selection_id"
                    .to_string(),
            );
        } else if !tool_selection_ids.insert(selection_id.to_string()) {
            errors.push(format!(
                "duplicate selection_id in tool-selections.json: {selection_id}"
            ));
        }

        if !principal_agent_did.is_empty() && selection.agent_did.trim() != principal_agent_did {
            errors.push(format!(
                "tool selection {} belongs to {} not {}",
                selection.selection_id, selection.agent_did, manifest.agent_principal.agent_did
            ));
        }

        if let Some(mode) = selection
            .subagent_default_await_mode
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            match mode {
                "foreground" => {}
                "background" if selection.subagent_background_enabled => {}
                "background" => errors.push(format!(
                    "tool selection {} sets subagent_default_await_mode=background but subagent_background_enabled is false",
                    selection.selection_id
                )),
                other => errors.push(format!(
                    "tool selection {} has invalid subagent_default_await_mode {other:?}; expected foreground or background",
                    selection.selection_id
                )),
            }
        }

        if let Some(mode) = selection.command_execution_policy.as_deref() {
            if let Err(error) = CommandExecutionMode::parse(mode) {
                errors.push(format!(
                    "tool selection {} has invalid command_execution_policy: {error}",
                    selection.selection_id
                ));
            }
        }

        for (index, tool_name) in selection.backgroundable_tool_names.iter().enumerate() {
            if tool_name.trim().is_empty() {
                errors.push(format!(
                    "tool selection {} has empty backgroundable_tool_names[{index}]",
                    selection.selection_id
                ));
            }
        }
        for (index, target) in selection.subagent_targets.iter().enumerate() {
            if target.trim().is_empty() {
                errors.push(format!(
                    "tool selection {} has empty subagent_targets[{index}]",
                    selection.selection_id
                ));
            }
        }
        if let Some(mode) = selection.command_network_mode.as_deref() {
            if let Err(error) = CommandNetworkMode::parse(mode) {
                errors.push(format!(
                    "tool selection {} has invalid command_network_mode: {error}",
                    selection.selection_id
                ));
            }
        }
        validate_command_argv_prefixes(
            &selection.selection_id,
            "command_allowed_argv_prefixes",
            &selection.command_allowed_argv_prefixes,
            errors,
        );
        validate_command_argv_prefixes(
            &selection.selection_id,
            "command_forbidden_argv_prefixes",
            &selection.command_forbidden_argv_prefixes,
            errors,
        );
        validate_tool_selection_non_empty_entries(
            &selection.selection_id,
            "allowed_mcp_service_ids",
            &selection.allowed_mcp_service_ids,
            errors,
        );
        validate_tool_selection_non_empty_entries(
            &selection.selection_id,
            "required_mcp_service_ids",
            &selection.required_mcp_service_ids,
            errors,
        );
        if !selection.required_mcp_service_ids.is_empty() && !selection.enable_meta_tools {
            errors.push(format!(
                "tool selection {} requires MCP services but enable_meta_tools is false",
                selection.selection_id
            ));
        }
        if !selection.allowed_mcp_service_ids.is_empty() {
            for service_id in &selection.required_mcp_service_ids {
                if !selection.allowed_mcp_service_ids.contains(service_id) {
                    errors.push(format!(
                        "tool selection {} requires MCP service {} outside allowed_mcp_service_ids",
                        selection.selection_id, service_id
                    ));
                }
            }
        }
        validate_subagent_targets(
            &selection.selection_id,
            selection.agent_did.trim(),
            selection.subagent_allow_cross_deployment,
            &selection.subagent_targets,
            errors,
        );
        // Field-level write-tool checks run once, inside the link validation,
        // over the merged inline ∪ surface list (which equals the inline list
        // when no surfaces are linked).
        validate_tool_selection_surface_links(manifest, selection, errors);
        validate_eth_tool_links(manifest, selection, errors);
        if selection.subagent_spawn_enabled {
            if selection.subagent_targets.is_empty() {
                errors.push(format!(
                    "tool selection {} sets subagent_spawn_enabled but has no subagent_targets; the tools would be inert",
                    selection.selection_id
                ));
            }
        }
    }
    tool_selection_ids
}

fn validate_eth_tool_links(
    manifest: &DesiredStateManifest,
    selection: &DesiredToolSelection,
    errors: &mut Vec<String>,
) {
    let tools: BTreeMap<&str, &DesiredEthTool> = manifest
        .eth_tools
        .iter()
        .map(|tool| (tool.tool_id.trim(), tool))
        .collect();
    let mut linked_ids = BTreeSet::new();
    for tool_id in &selection.eth_tool_ids {
        let tool_id = tool_id.trim();
        if tool_id.is_empty() {
            errors.push(format!(
                "tool selection {} has an empty eth_tool_ids entry",
                selection.selection_id
            ));
            continue;
        }
        if !linked_ids.insert(tool_id) {
            errors.push(format!(
                "tool selection {} lists EthTool {} more than once",
                selection.selection_id, tool_id
            ));
            continue;
        }
        let Some(tool) = tools.get(tool_id) else {
            errors.push(format!(
                "tool selection {} references missing EthTool {}",
                selection.selection_id, tool_id
            ));
            continue;
        };
        if tool.agent_did.trim() != selection.agent_did.trim() {
            errors.push(format!(
                "tool selection {} references EthTool {} owned by a different agent",
                selection.selection_id, tool_id
            ));
        } else if !tool.enabled {
            errors.push(format!(
                "tool selection {} references disabled EthTool {}",
                selection.selection_id, tool_id
            ));
        }
    }
}

pub(super) fn validate_tool_service_registries(
    manifest: &DesiredStateManifest,
    errors: &mut Vec<String>,
) {
    let mut service_ids = BTreeSet::new();
    for service in &manifest.tool_service_registries {
        let service_id = service.service_id.trim();
        if service_id.is_empty() {
            errors.push(
                "tool-services manifest contains a service with an empty service_id".to_string(),
            );
        } else if !service_ids.insert(service_id.to_string()) {
            errors.push(format!(
                "duplicate service_id in tool-services manifest: {service_id}"
            ));
        }

        if service.mcp_port.unwrap_or_default() <= 0 {
            errors.push(format!(
                "service {} in tool-services manifest must contain a positive mcp_port",
                service.service_id
            ));
        }

        if non_empty(&service.hostname).is_none()
            && non_empty(&service.tailscale_ip).is_none()
            && non_empty(&service.lan_ip).is_none()
        {
            errors.push(format!(
                "service {} in tool-services manifest must contain at least one of hostname, tailscale_ip, or lan_ip",
                service.service_id
            ));
        }
    }
}

fn validate_command_argv_prefixes(
    selection_id: &str,
    field: &str,
    prefixes: &[String],
    errors: &mut Vec<String>,
) {
    for prefix in prefixes {
        let trimmed = prefix.trim();
        if trimmed.is_empty() {
            errors.push(format!(
                "tool selection {selection_id} has an empty {field} entry"
            ));
            continue;
        }

        if trimmed.starts_with('[') {
            match serde_json::from_str::<Vec<String>>(trimmed) {
            Ok(tokens)
                if !tokens.is_empty() && tokens.iter().all(|token| !token.trim().is_empty()) => {}
            Ok(_) => errors.push(format!(
                "tool selection {selection_id} {field} JSON entry must contain non-empty argv tokens"
            )),
            Err(error) => errors.push(format!(
                "tool selection {selection_id} {field} JSON entry is invalid: {error}"
            )),
        }
        }
    }
}

fn validate_subagent_targets(
    selection_id: &str,
    selection_agent_did: &str,
    allow_cross_deployment: bool,
    entries: &[String],
    errors: &mut Vec<String>,
) {
    let mut seen_names: HashSet<String> = HashSet::new();
    for entry in entries {
        let target = match SubagentTarget::parse(entry) {
            Ok(target) => target,
            Err(error) => {
                errors.push(format!(
                "tool selection {selection_id} subagent_targets entry {entry:?} is not valid SubagentTarget JSON: {error}"
            ));
                continue;
            }
        };
        if !target.is_structurally_valid() {
            errors.push(format!(
            "tool selection {selection_id} subagent_targets entry {entry:?} must have non-empty name, agent_did, and behavior_id"
        ));
            continue;
        }
        if !seen_names.insert(target.name.trim().to_string()) {
            errors.push(format!(
                "tool selection {selection_id} has a duplicate subagent target name {:?}",
                target.name
            ));
        }
        if !allow_cross_deployment
            && !selection_agent_did.is_empty()
            && target.agent_did.trim() != selection_agent_did
        {
            errors.push(format!(
            "cross-deployment subagent delegation is deferred; remote target {} requires subagent_allow_cross_deployment=true (trusted-fleet only).",
            target.name
        ));
        }
    }
}

fn validate_tool_selection_surface_links(
    manifest: &DesiredStateManifest,
    selection: &DesiredToolSelection,
    errors: &mut Vec<String>,
) {
    use gents::{is_reserved_builtin_tool_name, SurfaceToolDecl};
    use std::collections::{BTreeMap, BTreeSet};

    // Trimmed to match the uniqueness check above and the lookup below.
    let surfaces: BTreeMap<&str, &DesiredDatastoreToolSurface> = manifest
        .datastore_tool_surfaces
        .iter()
        .map(|s| (s.surface_id.trim(), s))
        .collect();

    let mut merged: Vec<String> = selection.write_tools.clone();
    let mut seen_names: BTreeSet<String> = BTreeSet::new();
    for entry in &selection.write_tools {
        if let Ok(decl) = serde_json::from_str::<WriteToolDecl>(entry) {
            seen_names.insert(decl.tool_name);
        }
    }

    let mut linked_ids: BTreeSet<&str> = BTreeSet::new();
    for surface_id in &selection.datastore_tool_surface_ids {
        let surface_id = surface_id.trim();
        if surface_id.is_empty() {
            errors.push(format!(
                "tool selection {} has an empty datastore_tool_surface_ids entry",
                selection.selection_id
            ));
            continue;
        }
        if !linked_ids.insert(surface_id) {
            // Expanding twice would trip the tool_name collision check and
            // blame the wrong thing.
            errors.push(format!(
                "tool selection {} lists DatastoreToolSurface {} more than once",
                selection.selection_id, surface_id
            ));
            continue;
        }
        let Some(surface) = surfaces.get(surface_id) else {
            errors.push(format!(
                "tool selection {} references missing DatastoreToolSurface {}",
                selection.selection_id, surface_id
            ));
            continue;
        };
        if surface.agent_did.trim() != selection.agent_did.trim() {
            errors.push(format!(
                "tool selection {} references DatastoreToolSurface {} owned by a different agent",
                selection.selection_id, surface_id
            ));
            continue;
        }
        if !surface.enabled {
            errors.push(format!(
                "tool selection {} references disabled DatastoreToolSurface {}",
                selection.selection_id, surface_id
            ));
            continue;
        }
        for entry in &surface.entries {
            match serde_json::from_str::<SurfaceToolDecl>(entry) {
                Ok(decl) => {
                    if let Err(error) = decl.validate() {
                        errors.push(format!(
                            "DatastoreToolSurface {surface_id} has a malformed entry: {error}"
                        ));
                        continue;
                    }
                    if !seen_names.insert(decl.tool_name().to_string()) {
                        errors.push(format!(
                        "duplicate tool_name {:?} after expanding DatastoreToolSurface {} for tool selection {}",
                        decl.tool_name(),
                        surface_id,
                        selection.selection_id
                    ));
                    }
                    match decl {
                        SurfaceToolDecl::Create(_) => merged.push(entry.clone()),
                        SurfaceToolDecl::Query(_) => {
                            // Creates are re-checked by `validate_write_tools`
                            // below; query entries never enter that list.
                            if is_reserved_builtin_tool_name(decl.tool_name()) {
                                errors.push(format!(
                                "DatastoreToolSurface {} tool_name {:?} collides with a built-in tool",
                                surface_id,
                                decl.tool_name()
                            ));
                            }
                            if selection
                                .cli_tool_names
                                .iter()
                                .any(|name| name.trim() == decl.tool_name())
                            {
                                errors.push(format!(
                                "DatastoreToolSurface {} tool_name {:?} collides with a cli_tool_names entry in tool selection {}",
                                surface_id,
                                decl.tool_name(),
                                selection.selection_id
                            ));
                            }
                        }
                    }
                }
                Err(error) => errors.push(format!(
                    "DatastoreToolSurface {} entry is not valid create/query tool JSON: {error}",
                    surface_id
                )),
            }
        }
    }

    // Re-run field-level checks over the merged create list.
    validate_write_tools(
        &selection.selection_id,
        &merged,
        &selection.cli_tool_names,
        errors,
    );
}

fn validate_datastore_surface_entries(label: &str, entries: &[String], errors: &mut Vec<String>) {
    let mut seen_tool_names: HashSet<String> = HashSet::new();
    for entry in entries {
        let decl: SurfaceToolDecl = match serde_json::from_str(entry) {
            Ok(decl) => decl,
            Err(error) => {
                errors.push(format!(
                    "{label} entry {entry:?} is not valid create/query tool JSON: {error}"
                ));
                continue;
            }
        };
        if !decl.is_well_formed() {
            errors.push(format!(
            "{label} entry {entry:?} is malformed (tool_name/collection required; query entries also need a projection)"
        ));
            continue;
        }
        if is_reserved_builtin_tool_name(decl.tool_name()) {
            errors.push(format!(
                "{label} tool_name {:?} collides with a built-in tool",
                decl.tool_name()
            ));
        }
        if !seen_tool_names.insert(decl.tool_name().to_string()) {
            errors.push(format!(
                "{label} has a duplicate tool_name {:?}",
                decl.tool_name()
            ));
        }
        if let SurfaceToolDecl::Create(create) = decl {
            if !create.output_obligation_is_well_formed() {
                errors.push(format!(
                "{label} tool {:?} output_obligation.minimum_writes must be greater than zero and output_obligation.expected_count_field, when present, must name a required model-provided field",
                create.tool_name
            ));
            }
        }
    }
}

fn validate_write_tools(
    selection_id: &str,
    entries: &[String],
    cli_tool_names: &[String],
    errors: &mut Vec<String>,
) {
    let cli_tool_names: HashSet<&str> = cli_tool_names.iter().map(|name| name.trim()).collect();
    let mut seen_tool_names: HashSet<String> = HashSet::new();
    for entry in entries {
        let decl: WriteToolDecl = match serde_json::from_str(entry) {
            Ok(decl) => decl,
            Err(error) => {
                errors.push(format!(
                "tool selection {selection_id} write_tools entry {entry:?} is not valid WriteToolDecl JSON: {error}"
            ));
                continue;
            }
        };
        if let Err(error) = decl.validate() {
            errors.push(format!(
            "tool selection {selection_id} write_tools entry for tool {:?} is malformed: {error}",
            decl.tool_name
        ));
        }
        if !decl.output_obligation_is_well_formed() {
            errors.push(format!(
            "tool selection {selection_id} write_tools tool {:?} output_obligation.minimum_writes must be greater than zero and output_obligation.expected_count_field, when present, must name a required model-provided field",
            decl.tool_name
        ));
        }
        if is_reserved_builtin_tool_name(&decl.tool_name) {
            errors.push(format!(
                "tool selection {selection_id} write_tools tool_name {:?} collides with a \
             built-in tool; declared write tools must use a name not already provided by the \
             native, meta, subagent, or built-in (defra_query, context_budget, sessions, \
             memory) tool surface",
                decl.tool_name.trim()
            ));
        }
        if cli_tool_names.contains(decl.tool_name.trim()) {
            errors.push(format!(
                "tool selection {selection_id} write_tools tool_name {:?} collides with a \
             cli_tool_names entry in the same tool selection; each tool must have a unique name",
                decl.tool_name.trim()
            ));
        }
        let mut seen_field_names: HashSet<String> = HashSet::new();
        for field in &decl.fields {
            if !seen_field_names.insert(field.name.trim().to_string()) {
                errors.push(format!(
                "tool selection {selection_id} write_tools tool {:?} has a duplicate field name {:?}",
                decl.tool_name,
                field.name.trim()
            ));
            }
        }
        if !decl.tool_name.trim().is_empty()
            && !seen_tool_names.insert(decl.tool_name.trim().to_string())
        {
            errors.push(format!(
                "tool selection {selection_id} has a duplicate write_tools tool_name {:?}",
                decl.tool_name.trim()
            ));
        }
    }
}

fn validate_tool_selection_non_empty_entries(
    selection_id: &str,
    field: &str,
    values: &[String],
    errors: &mut Vec<String>,
) {
    for value in values {
        if value.trim().is_empty() {
            errors.push(format!(
                "tool selection {selection_id} has an empty {field} entry"
            ));
        }
    }
}
