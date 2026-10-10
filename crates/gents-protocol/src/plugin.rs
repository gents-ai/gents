use serde::{Deserialize, Serialize};

/// Host-authored evidence for an installed plugin attempt. The enclosing tool
/// record supplies request identity and generation; guests cannot supply it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginExecutionReceipt {
    pub coordinate: String,
    pub artifact_digest: String,
    pub input_digest: String,
    pub output_digest: Option<String>,
    pub authority: Option<PluginExecutionAuthority>,
    pub limits: Option<PluginExecutionLimits>,
    pub verdict: PluginExecutionVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginExecutionLimits {
    pub fuel: Option<u64>,
    pub memory_bytes: u64,
    pub wall_ms: u64,
    pub max_output_bytes: u64,
}

/// The bounded WASI guest receives filesystem/environment grants. Network is
/// served separately by the host; the guest receives no socket or listen grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginExecutionAuthority {
    pub filesystem: PluginFilesystemGrant,
    pub environment: PluginEnvironmentGrant,
    pub host_http: Option<PluginHttpGrant>,
    pub host_model: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "access", content = "paths", rename_all = "snake_case")]
pub enum PluginFilesystemGrant {
    None,
    ReadOnly(Vec<String>),
    ReadWrite(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "access", content = "keys", rename_all = "snake_case")]
pub enum PluginEnvironmentGrant {
    None,
    AllowList(Vec<String>),
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginHttpGrant {
    /// None permits any public host; Some restricts requests to these entries.
    pub hosts: Option<Vec<String>>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginExecutionVerdict {
    AdmissionRefused,
    ExecutionError,
    Success,
    Refused,
    OutOfFuel,
    OutOfMemory,
    Timeout,
    BadOutput,
    Failed,
}
