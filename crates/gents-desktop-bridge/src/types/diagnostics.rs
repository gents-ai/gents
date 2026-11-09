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
    pub agent_did: Option<String>,
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
