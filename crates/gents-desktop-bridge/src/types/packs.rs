use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct InstalledPacksView {
    pub packs: Vec<InstalledPackView>,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct InstalledPackView {
    pub pack: String,
    pub installed: String,
    pub latest: Option<String>,
    pub outdated: Option<bool>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct PackSearchView {
    pub packs: Vec<FoundPackView>,
    pub has_more: bool,
    #[serde(flatten)]
    pagination: super::json::WireObject,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct FoundPackView {
    pub namespace: String,
    pub name: String,
    #[serde(
        default,
        deserialize_with = "super::json::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional = nullable)]
    pub description: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "super::json::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional = nullable)]
    pub latest: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "super::json::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional = nullable)]
    pub kind: Option<Option<String>>,
    #[serde(flatten)]
    metadata: super::json::WireObject,
}

/// Registry reports may carry additional metadata. Retain it across the wire;
/// the registry, rather than the desktop bridge, owns its nested schema.
#[derive(Debug, Serialize, Deserialize, TS)]
pub struct PackInfoView {
    #[serde(
        default,
        deserialize_with = "super::json::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional = nullable)]
    pub namespace: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "super::json::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional = nullable)]
    pub name: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "super::json::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional = nullable)]
    pub latest: Option<Option<String>>,
    #[serde(flatten)]
    metadata: super::json::WireObject,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct PackInstallView {
    pub pack: String,
    pub digest: String,
    pub source: String,
    #[serde(
        default,
        deserialize_with = "super::json::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional = nullable)]
    pub owner: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "super::json::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional = nullable)]
    pub would_write: Option<Option<bool>>,
    #[serde(flatten)]
    details: super::json::WireObject,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct PackUpdateView {
    pub updated: Vec<String>,
    pub failed: Vec<PackUpdateFailureView>,
    pub current: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct PackUpdateFailureView {
    pub pack: String,
    pub error: String,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct PackRemoveView {
    pub pack: String,
    pub removed: BTreeMap<String, Value>,
    #[serde(
        default,
        deserialize_with = "super::json::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional = nullable)]
    pub owner: Option<Option<String>>,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct PackLoginView {
    pub registry: String,
    pub signed_in_as: String,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct PackLogoutView {
    pub registry: String,
    pub signed_out: bool,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct PackAccountView {
    pub registry: String,
    pub account: PackAccountIdentityView,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct PackAccountIdentityView {
    pub username: String,
    #[serde(flatten)]
    metadata: super::json::WireObject,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct PackPluginSlotsView {
    pub plugins: Vec<PackPluginSlotView>,
    pub profiles: Vec<gents::pack::PackInferenceProfileOption>,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct PackPluginSlotView {
    pub plugin: String,
    pub slot: String,
    pub profile: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct PackPluginBindingView {
    pub plugin: String,
    pub bound: Option<gents::plugin::model_calls::ModelBinding>,
}

/// The JSON `gents job promote` reports for a promoted dynamic run.
#[derive(Debug, Serialize, Deserialize, TS)]
pub struct WorkflowPromoteView {
    pub tool: String,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct DynamicWorkflowsView {
    pub workflows: Vec<DynamicWorkflowSummary>,
    pub installed: Vec<InstalledWorkflowSummary>,
}

/// One installed job plugin: a workflow that is already a pack.
#[derive(Debug, Serialize, Deserialize, TS)]
pub struct InstalledWorkflowSummary {
    pub plugin: String,
    pub version: String,
    pub description: String,
    pub pack: Option<String>,
}

/// One dynamic script that ran, with its runs and the tools it became.
#[derive(Debug, Serialize, Deserialize, TS)]
pub struct DynamicWorkflowSummary {
    pub script_id: String,
    pub handle: String,
    #[ts(type = "number")]
    pub runs: u64,
    #[ts(type = "number")]
    pub last_run_at: u64,
    pub agent_id: Option<String>,
    pub latest_address: String,
    pub promoted_as: Vec<String>,
}

/// One dynamic run: its job view, script source and promotions.
#[derive(Debug, Serialize, Deserialize, TS)]
pub struct DynamicWorkflowView {
    pub view: gents_protocol::job::JobView,
    pub script: String,
    pub promoted_as: Vec<String>,
}
