use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::types::ManagedServerToolCeiling;

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
    pub diagnostics_hint: String,
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
    pub effective_tool_ceiling: Option<ManagedServerToolCeiling>,
    pub effective_tool_root: Option<String>,
    pub suggested_tool_root: Option<String>,
    pub pairing_ready: bool,
    pub approval_required: bool,
    /// This home's runtime answers but has not reported ready, typically while
    /// it migrates its data after an update. Not a failure: it is waited on
    /// without a bound and is never restarted for taking long.
    pub runtime_booting: bool,
    pub error: Option<String>,
    /// Typed classification of `error`: `incompatibleLocalStore` when the
    /// runtime refused this home's store.
    #[ts(optional = nullable)]
    pub error_code: Option<crate::error::BridgeErrorCode>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ManagedServerRootValidation {
    pub canonical_path: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ManagedServerResetResult {
    /// The managed runtime home whose own entries are in scope, if any.
    pub managed_home: Option<String>,
    /// The desktop client state root whose client entries are in scope, if any.
    pub desktop_home: Option<String>,
    /// The stores this version refused, and why.
    pub stores: Vec<IncompatibleStoreView>,
    /// Confirmation text for archiving.
    pub confirmation: String,
    /// Confirmation text for permanent deletion; absent when deletion is not
    /// offered (a store another, possibly newer, version wrote).
    pub delete_confirmation: Option<String>,
    pub consequence: String,
    pub completed: bool,
    /// What a completed reset did.
    pub disposition: Option<HomeResetDisposition>,
    /// Where an archive moved the retired entries.
    pub backup_path: Option<String>,
    /// Entries an archive moves, exactly.
    pub planned_paths: Vec<String>,
    /// Entries a deletion removes, exactly. Narrower than the archive: of
    /// `keys/` only the managed home's own key file is deleted.
    pub delete_paths: Vec<String>,
    /// Entries a completed reset archived or deleted.
    pub retired_paths: Vec<String>,
    /// Entries of the managed home left untouched: other agents' homes,
    /// backups, user files and anything outside the runtime's inventory.
    pub retained_paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum HomeResetDisposition {
    Archive,
    Delete,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum IncompatibleStoreScope {
    /// The managed runtime's store in its home.
    Runtime,
    /// The desktop client's own store.
    Client,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct IncompatibleStoreView {
    pub scope: IncompatibleStoreScope,
    pub path: String,
    pub detail: String,
    /// The store is known to come from an earlier release. A store another
    /// (possibly newer) build wrote is not offered for deletion by default.
    pub older: bool,
    /// An identity key an older build wrote with unsafe permissions; `path`
    /// is the key (or the home's key directory).
    pub unsafe_key: bool,
}
