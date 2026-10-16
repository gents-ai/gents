use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::types::ManagedServerToolCeiling;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SavedPeerView {
    pub peer_id: String,
    pub label: String,
    pub node_did: String,
    pub addr: String,
    pub source: Option<String>,
    pub graphql: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DesktopBootstrapSummary {
    pub default_node_home: String,
    pub init_node_name: Option<String>,
    pub init_node_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub init_config_status: Option<InitConfigStatus>,
    pub init_tool_ceiling: Option<String>,
    pub init_tool_root: Option<String>,
    pub desktop_home: String,
    pub peer_directory_path: String,
    pub node_data_dir: String,
    pub diagnostics_hint: String,
    pub node_home_exists: bool,
    pub desktop_home_exists: bool,
    pub peer_directory_exists: bool,
    pub client_state_exists: bool,
    pub saved_peers: Vec<SavedPeerView>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum InitConfigStatus {
    Absent,
    Configured,
    Unreadable,
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
    pub node_name: Option<String>,
    pub node_did: Option<String>,
    pub graphql: Option<String>,
    pub effective_tool_ceiling: Option<ManagedServerToolCeiling>,
    pub effective_tool_root: Option<String>,
    pub suggested_tool_root: Option<String>,
    pub pairing_ready: bool,
    pub approval_required: bool,
    /// Not a failure: the runtime answers but has not reported ready; it is
    /// waited on without a bound and never restarted for taking long.
    pub runtime_booting: bool,
    pub error: Option<String>,
    #[ts(optional = nullable)]
    pub error_code: Option<crate::error::BridgeErrorCode>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ManagedServerRootValidation {
    pub canonical_path: String,
}

pub use gents_desktop_core::home_reset::{
    HomeResetDisposition, IncompatibleStoreScope, IncompatibleStoreView, ManagedServerResetResult,
};
