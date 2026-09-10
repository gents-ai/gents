//! Deserialization types for the Lean tool-policy contract rows
//! (`Proofs/ToolPolicy.ContractCases`). Field names and types mirror the
//! emitted JSON exactly and decode strictly: contract drift must fail loudly
//! here instead of being masked by serde defaults.

use serde::Deserialize;

/// One `(tool, collection) → fields` write/query grant in the emitted view.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanToolPolicyWriteGrant {
    pub(crate) tool: String,
    pub(crate) collection: String,
    pub(crate) fields: Vec<String>,
}

/// Lean `ToolPolicy.ContractCases.SurfaceView`: the JSON projection of one
/// resolved surface under fixed probes and known key universes.
///
/// `cross_principal` is the Lean and canonical-config name
/// (`SubagentTools.allow_cross_principal`). Production `ToolPolicySurface`
/// still carries the field as `cross_deployment` until the runtime rename
/// lands, so the conformance codec maps between the two names explicitly
/// instead of hiding the difference behind a serde alias.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanToolPolicySurfaceView {
    pub(crate) file_rank: u8,
    pub(crate) goal_tools: bool,
    pub(crate) goal_create: bool,
    pub(crate) defra_query: bool,
    pub(crate) self_config: bool,
    pub(crate) memory: bool,
    pub(crate) session_history: bool,
    pub(crate) context_budget: bool,
    pub(crate) spawn: bool,
    pub(crate) steering: bool,
    pub(crate) background: bool,
    pub(crate) cross_principal: bool,
    pub(crate) skills: bool,
    pub(crate) lsp: bool,
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
    pub(crate) query_probe_tool: String,
    pub(crate) query_probe_collection: String,
    pub(crate) query_scope_kind: String,
    pub(crate) query_grants: Vec<LeanToolPolicyWriteGrant>,
    pub(crate) query_fields: Vec<String>,
    pub(crate) eth_query_methods_kind: String,
    pub(crate) eth_query_methods_keys: Vec<String>,
    pub(crate) eth_call_tools_kind: String,
    pub(crate) eth_call_tools_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanToolPolicyCase {
    pub(crate) name: String,
    pub(crate) behavior: LeanToolPolicySurfaceView,
    pub(crate) ceiling: LeanToolPolicySurfaceView,
    pub(crate) runtime: LeanToolPolicySurfaceView,
    pub(crate) expected: LeanToolPolicySurfaceView,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanGoalCapabilityResolutionCase {
    pub(crate) name: String,
    pub(crate) explicit_goal_tools: Option<bool>,
    pub(crate) explicit_goal_create: Option<bool>,
    pub(crate) expected_goal_tools: bool,
    pub(crate) expected_goal_create: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanLspActionCase {
    pub(crate) name: String,
    pub(crate) lsp: bool,
    pub(crate) file_rank: u8,
    pub(crate) action: String,
    pub(crate) mutates: bool,
    pub(crate) source: String,
    pub(crate) advertised: bool,
    pub(crate) action_authorized: bool,
    pub(crate) apply_authorized: bool,
}
