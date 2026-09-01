use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SavedPeerView {
    pub peer_id: String,
    pub label: String,
    pub agent_did: String,
    pub addr: String,
    pub source: Option<String>,
    pub graphql: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DesktopBootstrapSummary {
    pub default_agent_home: String,
    pub init_agent_name: Option<String>,
    pub init_agent_did: Option<String>,
    pub init_tool_ceiling: Option<String>,
    pub init_tool_root: Option<String>,
    pub desktop_home: String,
    pub peer_directory_path: String,
    pub node_data_dir: String,
    pub log_file_path: String,
    pub agent_home_exists: bool,
    pub desktop_home_exists: bool,
    pub peer_directory_exists: bool,
    pub client_state_exists: bool,
    pub saved_peers: Vec<SavedPeerView>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum ManagedServerState {
    Disabled,
    Starting,
    Running,
    External,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ManagedServerStatus {
    pub state: ManagedServerState,
    pub auto_start: bool,
    pub agent_name: Option<String>,
    pub agent_did: Option<String>,
    pub graphql: Option<String>,
    pub error: Option<String>,
}
