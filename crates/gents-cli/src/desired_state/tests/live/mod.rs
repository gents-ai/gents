use anyhow::Result;
use gents::defra_node::{EmbeddedNode, StorageBackend};
use gents::{ensure_runtime_schemas, SubagentTarget};

use super::super::validate::validate_manifest_against_live;
use super::super::*;
use crate::config_writes::ConfigAccess;

mod apply;
mod prune;
mod validation;

fn manifest_with_subagent_targets(targets: Vec<SubagentTarget>) -> DesiredStateManifest {
    use super::super::{DesiredAgentPrincipal, DesiredStateManifest, DesiredToolSelection};
    let targets: Vec<String> = targets.iter().map(SubagentTarget::to_entry).collect();
    DesiredStateManifest {
        agent_principal: DesiredAgentPrincipal {
            agent_did: "did:key:test-live-validate".to_string(),
            display_name: None,
            default_behavior_id: None,
            enabled: true,
        },
        agent_behaviors: Vec::new(),
        skills: Vec::new(),
        datastore_tool_surfaces: Vec::new(),
        tool_selections: vec![DesiredToolSelection {
            selection_id: "live-test-sel".to_string(),
            agent_did: "did:key:test-live-validate".to_string(),
            display_name: None,
            tool_policy_version: None,
            enable_file_tools: false,
            file_tools_mode: "ReadOnly".to_string(),
            file_tool_root: None,
            enable_bash: false,
            bash_mode: "ReadOnly".to_string(),
            command_execution_policy: None,
            command_allowed_argv_prefixes: Vec::new(),
            command_forbidden_argv_prefixes: Vec::new(),
            read_only_command_allowlist: Vec::new(),
            command_network_mode: None,
            cli_tool_names: Vec::new(),
            enable_meta_tools: false,
            allowed_mcp_service_ids: Vec::new(),
            delegate_to: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_memory: false,
            enable_session_history_tool: false,
            enable_context_budget: true,
            enable_defra_query: true,
            defra_query_collections: Vec::new(),
            subagent_targets: targets,
            subagent_spawn_enabled: true,
            subagent_steering_enabled: false,
            subagent_background_enabled: false,
            subagent_default_await_mode: None,
            subagent_allow_cross_deployment: false,
            cross_deployment_spawn_timeout_seconds: None,
            write_tools: Vec::new(),
            datastore_tool_surface_ids: Vec::new(),
            enable_self_config: false,
            self_config_categories: Vec::new(),
            self_config_no_lockout: false,
            self_config_dry_run: false,
            enable_lsp: false,
            lsp_config: None,
        }],
        inference_backends: Vec::new(),
        inference_profiles: Vec::new(),
        tool_service_registries: Vec::new(),
        projection_acp_bindings: Vec::new(),
        peer_pairings: Vec::new(),
        tasks: Vec::new(),
        schedules: Vec::new(),
        event_triggers: Vec::new(),
        callback_bindings: Vec::new(),
        repository_placements: Vec::new(),
    }
}
