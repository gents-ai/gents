use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanToolPolicyWriteGrant {
    pub(crate) tool: String,
    pub(crate) collection: String,
    pub(crate) fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanToolPolicySurfaceView {
    pub(crate) file_rank: u8,
    pub(crate) meta: bool,
    pub(crate) defra_query: bool,
    pub(crate) self_config: bool,
    pub(crate) memory: bool,
    pub(crate) session_history: bool,
    pub(crate) context_budget: bool,
    pub(crate) spawn: bool,
    pub(crate) steering: bool,
    pub(crate) background: bool,
    pub(crate) orchestration: bool,
    pub(crate) cross_deployment: bool,
    pub(crate) skills: bool,
    pub(crate) bash_mode: u8,
    pub(crate) bash_net: u8,
    pub(crate) bash_sandbox: bool,
    pub(crate) bash_allowed_kind: String,
    pub(crate) bash_allowed_prefixes: Vec<Vec<String>>,
    pub(crate) bash_forbidden: Vec<Vec<String>>,
    pub(crate) bash_read_only_kind: String,
    pub(crate) bash_read_only_keys: Vec<String>,
    pub(crate) cli_scope_kind: String,
    pub(crate) cli_keys: Vec<String>,
    pub(crate) mcp_probe: String,
    pub(crate) mcp_scope_kind: String,
    pub(crate) mcp_services: Vec<String>,
    pub(crate) mcp_permits: bool,
    pub(crate) defra_collections_scope_kind: String,
    pub(crate) defra_collections_keys: Vec<String>,
    pub(crate) self_config_categories_scope_kind: String,
    pub(crate) self_config_categories_keys: Vec<String>,
    pub(crate) subagent_targets_scope_kind: String,
    pub(crate) subagent_targets_keys: Vec<String>,
    pub(crate) background_tools_scope_kind: String,
    pub(crate) background_tools_keys: Vec<String>,
    pub(crate) write_probe_tool: String,
    pub(crate) write_probe_collection: String,
    pub(crate) write_scope_kind: String,
    pub(crate) write_grants: Vec<LeanToolPolicyWriteGrant>,
    pub(crate) write_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanToolPolicyCase {
    pub(crate) name: String,
    pub(crate) behavior: LeanToolPolicySurfaceView,
    pub(crate) ceiling: LeanToolPolicySurfaceView,
    pub(crate) runtime: LeanToolPolicySurfaceView,
    pub(crate) expected: LeanToolPolicySurfaceView,
}
