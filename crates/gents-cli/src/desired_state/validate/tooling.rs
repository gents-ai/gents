use std::collections::{BTreeMap, BTreeSet, HashSet};

use gents::{
    is_reserved_builtin_tool_name, CommandExecutionMode, CommandNetworkMode, SubagentTarget,
    SurfaceToolDecl, ToolSelectionDocument, WriteToolDecl, KEY_BACKEND_KEYRING,
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
        if let Err(error) = gents::validate_query_methods(&tool.query_methods) {
            errors.push(format!(
                "EthTool {} has invalid query_methods: {error}",
                tool.tool_id
            ));
        }
        if let Err(error) = gents::validate_eth_call_declarations(
            &tool.calls,
            tool.key_binding_id.as_deref(),
            (tool.chain_id > 0).then_some(tool.chain_id as u64),
        ) {
            errors.push(format!(
                "EthTool {} has invalid calls: {error}",
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

/// Decode a manifest tool selection into the document type
/// `ToolSelectionDocument::validate` owns. `write_tools` is deliberately left
/// `None`: its field-level checks only make sense over the inline ∪
/// surface-expanded list, which only `validate_tool_selection_surface_links`
/// (a cross-document, manifest-only concern — `DatastoreToolSurface` doesn't
/// exist as a concept inside one `ToolSelectionDocument`) can assemble.
fn to_document_tool_selection(selection: &DesiredToolSelection) -> ToolSelectionDocument {
    fn some(values: &[String]) -> Option<Vec<String>> {
        Some(values.to_vec())
    }
    ToolSelectionDocument {
        selection_id: selection.selection_id.clone(),
        agent_did: selection.agent_did.clone(),
        display_name: selection.display_name.clone(),
        tool_policy_version: Some(selection.tool_policy_version.clone()),
        enable_file_tools: Some(selection.enable_file_tools),
        file_tools_mode: Some(selection.file_tools_mode.clone()),
        file_tool_root: selection.file_tool_root.clone(),
        enable_bash: Some(selection.enable_bash),
        bash_mode: Some(selection.bash_mode.clone()),
        command_execution_policy: selection.command_execution_policy.clone(),
        command_allowed_argv_prefixes: some(&selection.command_allowed_argv_prefixes),
        command_forbidden_argv_prefixes: some(&selection.command_forbidden_argv_prefixes),
        read_only_command_allowlist: some(&selection.read_only_command_allowlist),
        command_network_mode: selection.command_network_mode.clone(),
        cli_tool_names: some(&selection.cli_tool_names),
        enable_meta_tools: Some(selection.enable_meta_tools),
        enable_goal_tools: selection.enable_goal_tools.flatten(),
        enable_goal_creation: selection.enable_goal_creation.flatten(),
        allowed_mcp_service_ids: some(&selection.allowed_mcp_service_ids),
        required_mcp_service_ids: some(&selection.required_mcp_service_ids),
        backgroundable_tool_names: some(&selection.backgroundable_tool_names),
        approval_required_tools: None,
        subagent_targets: some(&selection.subagent_targets),
        subagent_spawn_enabled: Some(selection.subagent_spawn_enabled),
        subagent_steering_enabled: Some(selection.subagent_steering_enabled),
        subagent_background_enabled: Some(selection.subagent_background_enabled),
        subagent_default_await_mode: selection.subagent_default_await_mode.clone(),
        subagent_allow_cross_deployment: Some(selection.subagent_allow_cross_deployment),
        cross_deployment_spawn_timeout_seconds: selection.cross_deployment_spawn_timeout_seconds,
        enable_memory: Some(selection.enable_memory),
        enable_session_history_tool: Some(selection.enable_session_history_tool),
        enable_context_budget: Some(selection.enable_context_budget),
        enable_defra_query: Some(selection.enable_defra_query),
        defra_query_collections: some(&selection.defra_query_collections),
        write_tools: None,
        datastore_tool_surface_ids: some(&selection.datastore_tool_surface_ids),
        eth_tool_ids: some(&selection.eth_tool_ids),
        enable_self_config: Some(selection.enable_self_config),
        self_config_categories: some(&selection.self_config_categories),
        self_config_no_lockout: Some(selection.self_config_no_lockout),
        self_config_dry_run: Some(selection.self_config_dry_run),
        enable_lsp: Some(selection.enable_lsp),
        lsp_config: selection.lsp_config.clone(),
    }
}

pub(super) fn validate_tool_selections(
    manifest: &DesiredStateManifest,
    principal_agent_did: &str,
    errors: &mut Vec<String>,
) -> BTreeSet<String> {
    let mut tool_selection_ids = BTreeSet::new();
    for selection in &manifest.tool_selections {
        // Manifest-shape: empty/duplicate selection_id and principal
        // ownership have no document equivalent (a document validator sees
        // one document at a time and doesn't know the current principal).
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

        // Document rules (subagent_default_await_mode vs
        // subagent_background_enabled, backgroundable_tool_names emptiness,
        // subagent_targets structural validity, required_mcp_service_ids
        // needing enable_meta_tools and staying inside
        // allowed_mcp_service_ids, self_config_categories vocabulary) are
        // owned by `ToolSelectionDocument::validate`.
        errors.extend(
            to_document_tool_selection(selection)
                .validation_violations()
                .into_iter()
                .map(|error| format!("tool selection {}: {error}", selection.selection_id)),
        );

        // Manifest-shape: command_execution_policy/command_network_mode
        // parsing and the two argv-prefix JSON shape checks have no document
        // equivalent (the document stores them as plain strings/lists; the
        // richer enum/JSON-array shape is a manifest-authoring concern).
        if let Some(mode) = selection.command_execution_policy.as_deref() {
            if let Err(error) = CommandExecutionMode::parse(mode) {
                errors.push(format!(
                    "tool selection {} has invalid command_execution_policy: {error}",
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
        // Manifest-shape: `allowed_mcp_service_ids` entry emptiness has no
        // document equivalent (`ToolSelectionDocument::validate` only checks
        // `required_mcp_service_ids` entries, since only those are
        // referenced by the no-lockout-adjacent callable-dependency rule).
        validate_tool_selection_non_empty_entries(
            &selection.selection_id,
            "allowed_mcp_service_ids",
            &selection.allowed_mcp_service_ids,
            errors,
        );
        // Manifest-shape: duplicate subagent target names and the
        // cross-deployment permission gate have no document equivalent
        // (`ToolSelectionDocument::validate` checks each entry's own
        // structural validity; this pass skips malformed entries and handles
        // only the cross-entry/cross-document rules below).
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
        if selection.subagent_spawn_enabled && selection.subagent_targets.is_empty() {
            errors.push(format!(
                "tool selection {} sets subagent_spawn_enabled but has no subagent_targets; the tools would be inert",
                selection.selection_id
            ));
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
        // Malformed/structurally-invalid entries are already reported by
        // `ToolSelectionDocument::validate` (#1331, the single owner —
        // called earlier in `validate_tool_selections`); skip them here
        // without a second error message. Parsing still has to run because
        // the checks below need the decoded fields.
        let Ok(target) = SubagentTarget::parse(entry) else {
            continue;
        };
        if !target.is_structurally_valid() {
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
