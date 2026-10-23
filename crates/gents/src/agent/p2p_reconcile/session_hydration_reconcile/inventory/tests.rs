use super::*;
use crate::tool_call_lifecycle::admission_fixture::{
    ensure_fixture_session, published_admission_with_owner, PublishedAdmissionOptions,
};

#[tokio::test]
async fn uncaptured_terminal_is_unknown_and_an_empty_closure_is_known() {
    let (fixture, _request) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "inventory-source-unknown".into(),
        real_identity: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = fixture.node;
    let mut tool = fixture.tool;
    let requester = tool.requester_did().unwrap().to_owned();
    let session_id = tool.session_id().to_owned();
    let session =
        ConfigAccess::transact_local_readonly(&node, None, "test.inventory.session", |txn| {
            let (did, session_id, requester) = (&fixture.node_did, &session_id, &requester);
            Box::pin(async move {
                crate::session::load_agent_session_row_in_txn(txn, did, session_id, Some(requester))
                    .await
            })
        })
        .await
        .unwrap()
        .unwrap();
    let mut admission = FileInventoryAdmission {
        session_doc_id: session.doc_id,
        request: HydrationRequest {
            request_key: "inventory-source-unknown".into(),
            peer_id: "fixture-peer".into(),
            requester_did: requester.clone(),
            node_did: fixture.node_did.clone(),
            session_id,
        },
    };
    let running = FileInventorySnapshot::load(&node, &admission)
        .await
        .unwrap();
    assert_eq!(running.binding.coverage.pending, 1);
    assert_eq!(running.binding.coverage.unknown, 0);
    tool.complete("no retained capture").await.unwrap();
    let terminal = FileInventorySnapshot::load(&node, &admission)
        .await
        .unwrap();
    assert_eq!(terminal.binding.coverage.pending, 0);
    assert_eq!(terminal.binding.coverage.unknown, 1);
    assert!(terminal.entries.is_empty());
    assert_ne!(terminal.binding.digest, running.binding.digest);
    let repeated = FileInventorySnapshot::load(&node, &admission)
        .await
        .unwrap();
    assert_eq!(repeated.binding, terminal.binding);
    ensure_fixture_session(
        &node,
        "empty-inventory-session",
        &fixture.node_did,
        Some(&requester),
        "2026-10-24T00:00:00Z",
    )
    .await;
    admission.request.session_id = "empty-inventory-session".into();
    let empty_session =
        ConfigAccess::transact_local_readonly(&node, None, "test.inventory.empty_session", |txn| {
            let admission = &admission;
            Box::pin(async move {
                crate::session::load_agent_session_row_in_txn(
                    txn,
                    &admission.request.node_did,
                    &admission.request.session_id,
                    Some(&admission.request.requester_did),
                )
                .await
            })
        })
        .await
        .unwrap()
        .unwrap();
    admission.session_doc_id = empty_session.doc_id;
    let empty = FileInventorySnapshot::load(&node, &admission)
        .await
        .unwrap();
    assert_eq!(
        empty.binding.coverage,
        FileInventoryCoverage {
            entries: 0,
            pending: 0,
            unknown: 0
        }
    );
    assert!(empty.entries.is_empty());
    assert_ne!(empty.binding.digest, terminal.binding.digest);
    node.shutdown().await;
    tokio::fs::remove_dir_all(fixture.path).await.unwrap();
}
