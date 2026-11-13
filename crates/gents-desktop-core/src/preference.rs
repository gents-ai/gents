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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeIdentity {
    pub reviewed: ReviewedHome,
    pub tool_ceiling: Option<ManagedServerToolCeiling>,
    pub tool_root: Option<String>,
}
pub fn parse_tool_ceiling(value: &str) -> Option<ManagedServerToolCeiling> {
    match value.trim().to_ascii_lowercase().as_str() {
        "meta-only" | "metaonly" | "meta_only" => Some(ManagedServerToolCeiling::MetaOnly),
        "readonly" => Some(ManagedServerToolCeiling::Readonly),
        "readwrite" => Some(ManagedServerToolCeiling::Readwrite),
        _ => None,
    }
}
pub async fn read_home_identity(node_home: &std::path::Path) -> Option<HomeIdentity> {
    let home = tokio::fs::canonicalize(node_home).await.ok()?;
    let bytes = tokio::fs::read(node_home.join("init.json")).await.ok()?;
    let init: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let text = |key: &str| {
        init.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Some(HomeIdentity {
        reviewed: ReviewedHome {
            node_home: home.to_string_lossy().into_owned(),
            node_did: text("node_did")?,
        },
        tool_ceiling: text("tool_ceiling").as_deref().and_then(parse_tool_ceiling),
        tool_root: text("tool_root"),
    })
}
