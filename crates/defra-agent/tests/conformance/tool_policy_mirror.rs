//! Adapter from Lean's compact JSON SurfaceView into the production Rust
//! tool-surface policy resolver. This file intentionally does not reimplement
//! the meet: it only performs vocabulary conversion around `ToolPolicySurface`.

use std::collections::{BTreeMap, BTreeSet};

use defra_agent::tool_surface::ToolPolicySurface;
use defra_agent::tool_surface::{BashMode, EndpointScope, FileToolMode, ToolPolicyBash};
use defra_agent::toolset::{CommandExecutionMode, CommandNetworkMode};

use crate::lean_vocab_test::{
    LeanToolPolicySurfaceView as View, LeanToolPolicyWriteGrant as WriteGrant,
};

fn file_from_rank(rank: u8) -> FileToolMode {
    match rank {
        0 => FileToolMode::Off,
        1 => FileToolMode::ReadOnly,
        2 => FileToolMode::ReadWrite,
        other => panic!("unknown file rank {other}"),
    }
}

fn file_rank(mode: FileToolMode) -> u8 {
    match mode {
        FileToolMode::Off => 0,
        FileToolMode::ReadOnly => 1,
        FileToolMode::ReadWrite => 2,
    }
}

fn bash_tool_from_exec_rank(rank: u8) -> BashMode {
    match rank {
        0 => BashMode::Off,
        1 => BashMode::ReadOnly,
        2 => BashMode::Unrestricted,
        other => panic!("unknown bash execution rank {other}"),
    }
}

fn exec_from_rank(rank: u8) -> CommandExecutionMode {
    match rank {
        0 => CommandExecutionMode::ReadOnly,
        1 => CommandExecutionMode::WorkspaceWrite,
        2 => CommandExecutionMode::Unrestricted,
        other => panic!("unknown command execution rank {other}"),
    }
}

fn exec_rank(mode: CommandExecutionMode) -> u8 {
    match mode {
        CommandExecutionMode::ReadOnly => 0,
        CommandExecutionMode::WorkspaceWrite => 1,
        CommandExecutionMode::Unrestricted => 2,
    }
}

fn net_from_rank(rank: u8) -> CommandNetworkMode {
    match rank {
        0 => CommandNetworkMode::Disabled,
        1 => CommandNetworkMode::Inherit,
        2 => CommandNetworkMode::Enabled,
        other => panic!("unknown command network rank {other}"),
    }
}

fn net_rank(mode: CommandNetworkMode) -> u8 {
    match mode {
        CommandNetworkMode::Disabled => 0,
        CommandNetworkMode::Inherit => 1,
        CommandNetworkMode::Enabled => 2,
    }
}

fn unit_scope_from_strings(kind: &str, keys: &[String]) -> EndpointScope<String, ()> {
    match kind {
        "none" => EndpointScope::None,
        "all" => EndpointScope::All,
        "only" => EndpointScope::<String, ()>::only_units(keys.iter().cloned()),
        other => panic!("unknown string scope kind {other:?}"),
    }
}

fn unit_scope_from_prefixes(kind: &str, keys: &[Vec<String>]) -> EndpointScope<Vec<String>, ()> {
    match kind {
        "none" => EndpointScope::None,
        "all" => EndpointScope::All,
        "only" => EndpointScope::<Vec<String>, ()>::only_units(keys.iter().cloned()),
        other => panic!("unknown prefix scope kind {other:?}"),
    }
}

fn write_scope_from_grants(
    kind: &str,
    grants: &[WriteGrant],
) -> EndpointScope<(String, String), BTreeSet<String>> {
    match kind {
        "none" => EndpointScope::None,
        "all" => EndpointScope::All,
        "only" => EndpointScope::Only(
            grants
                .iter()
                .map(|grant| {
                    (
                        (grant.tool.clone(), grant.collection.clone()),
                        grant.fields.iter().cloned().collect(),
                    )
                })
                .collect::<BTreeMap<_, _>>(),
        ),
        other => panic!("unknown write scope kind {other:?}"),
    }
}

fn grants_from_write_scope(
    scope: &EndpointScope<(String, String), BTreeSet<String>>,
) -> Vec<WriteGrant> {
    match scope {
        EndpointScope::Only(grants) => grants
            .iter()
            .map(|((tool, collection), fields)| WriteGrant {
                tool: tool.clone(),
                collection: collection.clone(),
                fields: fields.iter().cloned().collect(),
            })
            .collect(),
        EndpointScope::None | EndpointScope::All => Vec::new(),
    }
}

fn surface_from_view(view: &View) -> ToolPolicySurface {
    ToolPolicySurface {
        file: file_from_rank(view.file_rank),
        bash: ToolPolicyBash {
            tool: bash_tool_from_exec_rank(view.bash_mode),
            execution_mode: exec_from_rank(view.bash_mode),
            network_mode: net_from_rank(view.bash_net),
            sandbox: view.bash_sandbox,
            allowed_argv_prefixes: unit_scope_from_prefixes(
                &view.bash_allowed_kind,
                &view.bash_allowed_prefixes,
            ),
        },
        meta: view.meta,
        defra_query: view.defra_query,
        memory: view.meta,
        session_history: view.meta,
        context_budget: view.meta,
        spawn: view.spawn,
        steering: view.meta,
        background: view.spawn,
        orchestration: view.spawn,
        cross_deployment: view.spawn,
        skills: view.meta,
        cli_tools: EndpointScope::All,
        mcp_services: unit_scope_from_strings(&view.mcp_scope_kind, &view.mcp_services),
        defra_collections: EndpointScope::All,
        subagent_targets: EndpointScope::All,
        background_tools: EndpointScope::All,
        write_tools: write_scope_from_grants(&view.write_scope_kind, &view.write_grants),
    }
}

fn view_from_surface(
    surface: &ToolPolicySurface,
    mcp_probe: String,
    write_probe_tool: String,
    write_probe_collection: String,
) -> View {
    let write_probe = (write_probe_tool.clone(), write_probe_collection.clone());
    let write_fields = surface
        .write_tools
        .lookup(&write_probe)
        .map(|fields| fields.iter().cloned().collect())
        .unwrap_or_default();

    View {
        file_rank: file_rank(surface.file),
        meta: surface.meta,
        defra_query: surface.defra_query,
        spawn: surface.spawn,
        bash_mode: exec_rank(surface.bash.execution_mode),
        bash_net: net_rank(surface.bash.network_mode),
        bash_sandbox: surface.bash.sandbox,
        bash_allowed_kind: surface.bash.allowed_argv_prefixes.kind().to_string(),
        bash_allowed_prefixes: surface.bash.allowed_argv_prefixes.keys(),
        mcp_permits: surface.mcp_services.permits(&mcp_probe),
        mcp_probe,
        mcp_scope_kind: surface.mcp_services.kind().to_string(),
        mcp_services: surface.mcp_services.keys(),
        write_probe_tool,
        write_probe_collection,
        write_scope_kind: surface.write_tools.kind().to_string(),
        write_grants: grants_from_write_scope(&surface.write_tools),
        write_fields,
    }
}

pub(super) fn rederive(behavior: &View, ceiling: &View, runtime: &View) -> View {
    let behavior_policy = surface_from_view(behavior);
    let ceiling_policy = surface_from_view(ceiling);
    let runtime_policy = surface_from_view(runtime);
    let effective =
        ToolPolicySurface::effective(&behavior_policy, &ceiling_policy, &runtime_policy);
    view_from_surface(
        &effective,
        behavior.mcp_probe.clone(),
        behavior.write_probe_tool.clone(),
        behavior.write_probe_collection.clone(),
    )
}
