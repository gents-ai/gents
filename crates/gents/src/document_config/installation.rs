use serde::{Deserialize, Serialize};

/// MCP connection configuration; discovered tools and health are observations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolServiceRegistry {
    /// Logical name within agent_did; clients/pools must not resolve globally.
    pub service_id: String,
    pub agent_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tailscale_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lan_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_port: Option<i64>,
    /// Absent/empty uses the endpoint root (empty path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_path: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "super::serde_helpers::is_disabled"
    )]
    pub send_agent_did: bool,
    /// Operator availability gate, distinct from observed connectivity/health.
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    pub enabled: bool,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
}

/// Principal-local checkout used by existing workspace provisioning callbacks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepositoryPlacement {
    pub repository_id: String,
    pub agent_did: String,
    pub host_path: String,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    pub enabled: bool,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
}

/// References existing DefraDB ACP policies; this is not a second ACL system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionAcpBinding {
    pub binding_id: String,
    pub agent_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behavior_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection_id: Option<String>,
    pub policy_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staged_policy_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_policy_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_map_json: Option<String>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    pub enabled: bool,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectionAcpObservation {
    pub binding_id: String,
    pub agent_did: String,
    pub publication_status: Option<String>,
    pub published_at: Option<String>,
}
