use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The desktop's managed-server preference file in its client state root.
pub const MANAGED_SERVER_CONFIG: &str = "managed-server.json";

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(rename_all = "kebab-case")]
pub enum ManagedServerToolCeiling {
    MetaOnly,
    Readonly,
    Readwrite,
}

/// The desktop's managed-server preference, `managed-server.json`. Every
/// reader decodes it here, so reset ownership and the desktop agree on what
/// a preference is.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredManagedServer {
    pub node_name: String,
    #[serde(default)]
    pub tool_ceiling: Option<ManagedServerToolCeiling>,
    #[serde(default)]
    pub tool_root: Option<String>,
    /// The canonical home and init identity the ceiling and root were
    /// reviewed for. A remembered grant applies only to that home.
    #[serde(default)]
    pub reviewed_for: Option<ReviewedHome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewedHome {
    pub node_home: String,
    pub node_did: String,
}
