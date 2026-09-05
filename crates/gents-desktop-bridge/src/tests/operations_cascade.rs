use crate::cascade::{
    build_cascade_preview, CascadeClassification, CascadeWalkRequest, CascadeWalkRow,
};
use crate::types::DesktopPreviewInterruptCascadeRequest;
use gents_protocol::request_lifecycle::RequestLifecycleState;

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
        lifecycle_state: Some(RequestLifecycleState::Processing),
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
    let result = crate::cascade::walk(&core, &req).await.expect("walk ok");
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
        result.root_state,
        Some(RequestLifecycleState::Processing),
        "root_state mismatch"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn preview_returns_four_classified_groups_and_a_signature() {
    let (core, _tmp) = super::support::seed_cascade_fixture().await;
    let preview = build_cascade_preview(
        &core,
        &DesktopPreviewInterruptCascadeRequest {
            request_id: "req_root".into(),
            agent_did: Some("did:test:operator".into()),
            include_terminal: Some(true),
        },
    )
    .await
    .expect("preview ok");

    assert_eq!(preview.root_request_id, "req_root");
    assert_eq!(preview.root_state.as_deref(), Some("processing"));
    assert_eq!(preview.will_interrupt.len(), 3);
    assert_eq!(preview.will_detach.len(), 1);
    assert_eq!(preview.already_terminal.len(), 1);
    assert_eq!(preview.unknown_policy.len(), 1);
    assert_eq!(preview.preview_signature.len(), 64);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn walk_returns_no_rows_for_standalone_root() {
    let (core, _tmp) = super::support::seed_standalone_fixture().await;
    let req = CascadeWalkRequest {
        root_request_id: "req_solo".into(),
        agent_did: Some("did:test:operator".into()),
        include_terminal: false,
    };
    let result = crate::cascade::walk(&core, &req).await.expect("walk ok");
    assert!(
        result.rows.is_empty(),
        "expected empty rows for standalone root, got: {:?}",
        result.rows
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn walk_rejects_an_unknown_request_lifecycle_state() {
    let (core, _tmp) = super::support::seed_standalone_fixture().await;
    let response = core
        .node()
        .execute(
            r#"mutation {
                update_AgentRequest(
                    filter: { request_id: { _eq: "req_solo" } },
                    input: { lifecycle_state: "notARequestState" }
                ) { _docID }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "mutation failed: {:?}",
        response.errors
    );

    let error = crate::cascade::walk(
        &core,
        &CascadeWalkRequest {
            root_request_id: "req_solo".into(),
            agent_did: Some("did:test:operator".into()),
            include_terminal: false,
        },
    )
    .await
    .expect_err("unknown lifecycle state must fail at the row boundary");

    assert!(error.contains("notARequestState"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn walk_excludes_unlinked_rows_owned_by_different_agent_did() {
    let (core, _tmp) = super::support::seed_cascade_fixture_with_foreign_request().await;
    let req = CascadeWalkRequest {
        root_request_id: "req_root".into(),
        agent_did: Some("did:test:operator".into()),
        include_terminal: true,
    };
    let result = crate::cascade::walk(&core, &req).await.expect("walk ok");

    let has_foreign = result.rows.iter().any(|r| r.request_id == "req_foreign");
    assert!(
        !has_foreign,
        "walk should not include unlinked foreign request rows"
    );

    let operator_count = result
        .rows
        .iter()
        .filter(|r| r.request_id != "req_foreign")
        .count();
    assert!(
        operator_count > 0,
        "expected operator-owned rows to be present"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn walk_includes_bridge_linked_child_owned_by_different_agent_did() {
    let (core, _tmp) = super::support::seed_cascade_fixture_with_foreign_linked_child().await;
    let preview = build_cascade_preview(
        &core,
        &DesktopPreviewInterruptCascadeRequest {
            request_id: "req_root".into(),
            agent_did: Some("did:test:operator".into()),
            include_terminal: Some(true),
        },
    )
    .await
    .expect("preview ok");

    let linked = preview
        .will_interrupt
        .iter()
        .find(|row| row.request_id == "req_foreign_linked");
    assert!(
        linked.is_some(),
        "cascade preview should include cross-DID children reached by bridge edge"
    );
}
