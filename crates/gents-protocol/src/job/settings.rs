use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::DynamicMode;

/// Operator-owned job ceiling of one plugin: what a job it runs may start and
/// how long it may live. An absent block means non-job verbs only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct PluginJobSettings {
    /// AgentTarget ids the job may start children on; at least one.
    #[serde(default)]
    pub agent_targets: Vec<String>,
    /// Children alive at once across the job.
    #[serde(default = "PluginJobSettings::default_max_live")]
    pub max_live: u32,
    /// Children the job may start in total.
    #[serde(default = "PluginJobSettings::default_max_spawns")]
    pub max_spawns: u32,
    /// Messages one child slot may receive per attempt.
    #[serde(default = "PluginJobSettings::default_max_messages_per_slot")]
    pub max_messages_per_slot: u32,
    /// Bytes of the job's result.
    #[serde(default = "PluginJobSettings::default_result_bytes")]
    pub result_bytes: u64,
    /// Seconds the job may run.
    #[serde(default = "PluginJobSettings::default_max_wall_secs")]
    pub max_wall_secs: u64,
    /// Operator-only rules for dynamic scripts and trusted packs.
    #[serde(default, deserialize_with = "policy_without_project_roots")]
    pub policy: JobPolicy,
}

/// Reads a policy, dropping the `project_roots` a 0.21 Tools document may
/// still carry. Accepted for this release only.
fn policy_without_project_roots<'de, D>(deserializer: D) -> Result<JobPolicy, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let mut policy = serde_json::Value::deserialize(deserializer)?;
    if let Some(fields) = policy.as_object_mut() {
        fields.remove("project_roots");
    }
    serde_json::from_value(policy).map_err(serde::de::Error::custom)
}

impl PluginJobSettings {
    pub const DEFAULT_MAX_LIVE: u32 = 8;
    pub const DEFAULT_MAX_SPAWNS: u32 = 128;
    pub const DEFAULT_MAX_MESSAGES_PER_SLOT: u32 = 4;
    pub const DEFAULT_RESULT_BYTES: u64 = 65_536;
    pub const DEFAULT_MAX_WALL_SECS: u64 = 7_200;
    pub const MAX_WALL_SECS_CAP: u64 = 86_400;

    fn default_max_live() -> u32 {
        Self::DEFAULT_MAX_LIVE
    }

    fn default_max_spawns() -> u32 {
        Self::DEFAULT_MAX_SPAWNS
    }

    fn default_max_messages_per_slot() -> u32 {
        Self::DEFAULT_MAX_MESSAGES_PER_SLOT
    }

    fn default_result_bytes() -> u64 {
        Self::DEFAULT_RESULT_BYTES
    }

    fn default_max_wall_secs() -> u64 {
        Self::DEFAULT_MAX_WALL_SECS
    }
}

/// Operator-only job policy; self-config may not edit it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobPolicy {
    #[serde(default = "JobPolicy::default_dynamic")]
    pub dynamic: DynamicMode,
    #[serde(default)]
    pub dynamic_targets: Vec<String>,
    #[serde(default)]
    pub trusted_packs: Vec<String>,
}

impl JobPolicy {
    pub const DEFAULT_DYNAMIC: DynamicMode = DynamicMode::Check;

    fn default_dynamic() -> DynamicMode {
        Self::DEFAULT_DYNAMIC
    }
}

impl Default for JobPolicy {
    fn default() -> Self {
        Self {
            dynamic: Self::DEFAULT_DYNAMIC,
            dynamic_targets: Vec::new(),
            trusted_packs: Vec::new(),
        }
    }
}

/// Manifest `job`: the plugin runs job steps.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct PluginJobDeclaration(pub bool);

/// The context value a stamped field is host-filled from.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum StampedField {
    NodeDid,
    SessionId,
    RequestDocId,
    ToolCallDocId,
    JobRowDocId,
    ChainRootDocId,
}

/// Manifest `data`: `ops` are `query`, `insert`, `update` or `delete`; `stamped`
/// maps a written field to the context value that fills it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct PluginDataDeclaration {
    pub collections: Vec<String>,
    pub ops: Vec<String>,
    #[serde(default)]
    pub stamped: BTreeMap<String, StampedField>,
}
