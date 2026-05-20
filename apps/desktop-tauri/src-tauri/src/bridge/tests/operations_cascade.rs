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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn walk_returns_classified_descendants_for_five_child_fixture() {
    let (core, _tmp) = super::support::seed_cascade_fixture().await;
    let req = CascadeWalkRequest {
        root_request_id: "req_root".into(),
        agent_did: Some("did:test:operator".into()),
        include_terminal: true,
    };
    let result = crate::bridge::cascade::walk(&core, &req)
        .await
        .expect("walk ok");
    let kinds: Vec<_> = result.rows.iter().map(|r| r.classification).collect();
    assert_eq!(
        kinds
            .iter()
            .filter(|c| **c == CascadeClassification::WillInterrupt)
            .count(),
        3,
        "expected 3 WillInterrupt, got: {:?}",
        kinds
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|c| **c == CascadeClassification::WillDetach)
            .count(),
        1,
        "expected 1 WillDetach, got: {:?}",
        kinds
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|c| **c == CascadeClassification::UnknownPolicy)
            .count(),
        1,
        "expected 1 UnknownPolicy, got: {:?}",
        kinds
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|c| **c == CascadeClassification::AlreadyTerminal)
            .count(),
        1,
        "expected 1 AlreadyTerminal, got: {:?}",
        kinds
    );
    assert_eq!(
        result.root_state.as_deref(),
        Some("processing"),
        "root_state mismatch"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn walk_returns_no_rows_for_standalone_root() {
    let (core, _tmp) = super::support::seed_standalone_fixture().await;
    let req = CascadeWalkRequest {
        root_request_id: "req_solo".into(),
        agent_did: Some("did:test:operator".into()),
        include_terminal: false,
    };
    let result = crate::bridge::cascade::walk(&core, &req)
        .await
        .expect("walk ok");
    assert!(
        result.rows.is_empty(),
        "expected empty rows for standalone root, got: {:?}",
        result.rows
    );
}
