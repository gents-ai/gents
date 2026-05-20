use crate::bridge::cascade::{CascadeClassification, CascadeWalkRequest, CascadeWalkRow};

#[test]
fn cascade_request_default_shape() {
    let req = CascadeWalkRequest {
        root_request_id: "req_root".into(),
        agent_did: None,
        include_terminal: false,
    };
    assert_eq!(req.root_request_id, "req_root");
}

#[test]
fn cascade_classification_variant_names() {
    let v = CascadeClassification::WillInterrupt;
    assert!(matches!(v, CascadeClassification::WillInterrupt));
    let _ = CascadeClassification::WillDetach;
    let _ = CascadeClassification::AlreadyTerminal;
    let _ = CascadeClassification::UnknownPolicy;
}

#[test]
fn cascade_row_carries_lineage() {
    let row = CascadeWalkRow {
        request_id: "req_b91".into(),
        session_id: Some("sess_1".into()),
        behavior_id: Some("amy-general".into()),
        lifecycle_state: Some("processing".into()),
        parent_request_id: Some("req_root".into()),
        parent_tool_call_id: Some("tc_42".into()),
        tool_name: Some("summarize".into()),
        await_mode: Some("background".into()),
        cancel_policy: Some("cascade".into()),
        classification: CascadeClassification::WillInterrupt,
    };
    assert_eq!(row.parent_request_id.as_deref(), Some("req_root"));
}
