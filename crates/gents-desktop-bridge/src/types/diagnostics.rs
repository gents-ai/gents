use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct RequestTimelineView {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub node_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub agent_id: Option<String>,
    pub child_request_ids: Vec<String>,
    pub events: Vec<RunTimelineEventView>,
    #[serde(flatten)]
    details: super::json::WireObject,
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct RunTimelineEventView {
    pub kind: String,
    #[serde(flatten)]
    payload: super::json::WireObject,
}

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ToolSurfaceExplanationView {
    pub agent_id: String,
    pub enabled: bool,
    pub context_id: Option<String>,
    pub tools_id: Option<String>,
    pub tools_source: String,
    pub ceiling_source: String,
    pub mcp_services_online: bool,
    pub surface: gents::tools::surface::ToolSurfaceExplanation,
}
