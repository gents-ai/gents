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
    pub surface: gents::tool_surface::ToolSurfaceExplanation,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_view_preserves_the_native_report_and_event_payloads() {
        let request = gents::run_timeline::TimelineRequestRow {
            request_id: "request".into(),
            session_id: Some("session".into()),
            created_at: Some("2026-10-14T12:00:00Z".into()),
            ..Default::default()
        };
        let native =
            gents::run_timeline::build_run_timeline(gents::run_timeline::RunTimelineRows {
                request: request.clone(),
                requests: vec![request],
                ..Default::default()
            });
        let report = serde_json::to_value(native).unwrap();
        let view: RequestTimelineView = serde_json::from_value(report.clone()).unwrap();
        assert!(!view.events.is_empty());
        assert_eq!(serde_json::to_value(view).unwrap(), report);
    }
}
