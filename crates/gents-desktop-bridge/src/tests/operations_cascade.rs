use crate::cascade::{build_cascade_preview, CascadeClassification, CascadeWalkRequest};
use crate::types::DesktopPreviewInterruptCascadeRequest;
use gents_protocol::request_lifecycle::RequestLifecycleState;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn walk_returns_classified_descendants_for_five_child_fixture() {
    let (core, _tmp) = super::support::seed_cascade_fixture().await;
    let req = CascadeWalkRequest {
        root_request_id: "req_root".into(),
        agent_did: Some("did:test:operator".into()),
        include_terminal: true,
    };
    let result = crate::cascade::walk(&core, &req).await.expect("walk ok");
    let mut classified: Vec<_> = result
        .rows
        .iter()
        .map(|row| (row.request_id.as_str(), row.classification))
        .collect();
    classified.sort_by_key(|(id, _)| *id);
    assert_eq!(
        classified,
        [
            ("req_a17_old", CascadeClassification::AlreadyTerminal),
            ("req_b91", CascadeClassification::WillInterrupt),
            ("req_b92", CascadeClassification::WillInterrupt),
            ("req_b93", CascadeClassification::WillDetach),
            ("req_c01", CascadeClassification::WillInterrupt),
            ("req_c02", CascadeClassification::UnknownPolicy),
        ]
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
    for (group, expected) in [
        (
            &preview.will_interrupt,
            &["req_b91", "req_b92", "req_c01"][..],
        ),
        (&preview.will_detach, &["req_b93"][..]),
        (&preview.already_terminal, &["req_a17_old"][..]),
        (&preview.unknown_policy, &["req_c02"][..]),
    ] {
        let mut ids: Vec<_> = group.iter().map(|row| row.request_id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, expected);
        for row in group {
            assert_eq!(row.parent_request_id.as_deref(), Some("req_root"));
            assert!(row.parent_tool_call_id.is_some());
            assert!(row.tool_name.is_some());
        }
    }
    assert_eq!(
        preview.already_terminal[0].lifecycle_state.as_deref(),
        Some("completed")
    );
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
