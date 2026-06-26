use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanToolPolicySurfaceView {
    pub(crate) file_rank: u8,
    pub(crate) meta: bool,
    pub(crate) defra_query: bool,
    pub(crate) spawn: bool,
    pub(crate) bash_mode: u8,
    pub(crate) bash_net: u8,
    pub(crate) bash_sandbox: bool,
    pub(crate) bash_allowed_kind: String,
    pub(crate) mcp_probe: String,
    pub(crate) mcp_permits: bool,
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
