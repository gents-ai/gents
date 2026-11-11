use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanToolPolicyWriteGrant {
    pub tool: String,
    pub collection: String,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanToolPolicySurfaceView {
    pub file_rank: u8,
    pub goal_tools: bool,
    pub goal_create: bool,
    pub defra_query: bool,
    pub self_config: bool,
    pub schema_management: bool,
    pub p2p_read: bool,
    pub p2p_mutate: bool,
    pub p2p_mutation_allowed: bool,
    pub session_history: bool,
    pub context_budget: bool,
    pub session_messages: bool,
    pub skills: bool,
    pub lsp: bool,
    pub bash_mode: u8,
    pub bash_net: u8,
    pub bash_sandbox: bool,
    pub bash_allowed_kind: String,
    pub bash_allowed_prefixes: Vec<Vec<String>>,
    pub bash_forbidden: Vec<Vec<String>>,
    pub bash_read_only_kind: String,
    pub bash_read_only_keys: Vec<String>,
    pub cli_scope_kind: String,
    pub cli_keys: Vec<String>,
    pub mcp_probe: String,
    pub mcp_scope_kind: String,
    pub mcp_services: Vec<String>,
    pub mcp_permits: bool,
    pub defra_collections_scope_kind: String,
    pub defra_collections_keys: Vec<String>,
    pub self_config_categories_scope_kind: String,
    pub self_config_categories_keys: Vec<String>,
    pub agent_targets_scope_kind: String,
    pub agent_targets_keys: Vec<String>,
    pub background_tools_scope_kind: String,
    pub background_tools_keys: Vec<String>,
    pub write_probe_tool: String,
    pub write_probe_collection: String,
    pub write_scope_kind: String,
    pub write_grants: Vec<LeanToolPolicyWriteGrant>,
    pub write_fields: Vec<String>,
    pub query_probe_tool: String,
    pub query_probe_collection: String,
    pub query_scope_kind: String,
    pub query_grants: Vec<LeanToolPolicyWriteGrant>,
    pub query_fields: Vec<String>,
    pub plugin_tools_kind: String,
    pub plugin_tools_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanToolPolicyCase {
    pub name: String,
    pub agent: LeanToolPolicySurfaceView,
    pub ceiling: LeanToolPolicySurfaceView,
    pub runtime: LeanToolPolicySurfaceView,
    pub expected: LeanToolPolicySurfaceView,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanGoalCapabilityResolutionCase {
    pub name: String,
    pub explicit_goal_tools: Option<bool>,
    pub explicit_goal_create: Option<bool>,
    pub expected_goal_tools: bool,
    pub expected_goal_create: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanLspActionCase {
    pub name: String,
    pub lsp: bool,
    pub file_rank: u8,
    pub action: String,
    pub mutates: bool,
    pub source: String,
    pub advertised: bool,
    pub action_authorized: bool,
    pub apply_authorized: bool,
}
