use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const MANAGED_SERVER_CONFIG: &str = "managed-server.json";

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(rename_all = "kebab-case")]
pub enum ManagedServerToolCeiling {
    MetaOnly,
    Readonly,
    Readwrite,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredManagedServer {
    pub node_name: String,
    #[serde(default)]
    pub tool_ceiling: Option<ManagedServerToolCeiling>,
    #[serde(default)]
    pub tool_root: Option<String>,
    #[serde(default)]
    pub reviewed_for: Option<ReviewedHome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewedHome {
    pub node_home: String,
    pub node_did: String,
}
