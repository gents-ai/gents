use crate::provenance::session_provenance;
use crate::tests::support::seed_provenance_fixture;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sent_lists_every_request_this_session_caused_on_any_agent() {
    let (core, _tmp, parent_doc_id) = seed_provenance_fixture().await;
    let view = session_provenance(&core, "did:test:operator", "sess_parent")
        .await
        .expect("provenance");

    assert_eq!(view.session_id, "sess_parent");
    assert!(view.received.is_empty());
    assert!(!view.truncated);
    let mut sent: Vec<_> = view
        .sent
        .iter()
        .map(|row| {
            (
                row.request_id.as_str(),
                row.session_id.as_deref(),
                row.agent_did.as_deref(),
                row.caused_by_tool_call_id.as_deref(),
            )
        })
        .collect();
    sent.sort_unstable();
    assert_eq!(
        sent,
        [
            (
                "req_child",
                Some("sess_child"),
                Some("did:test:operator"),
                Some("tc_start")
            ),
            (
                "req_child_2",
                Some("sess_child"),
                Some("did:test:operator"),
                Some("tc_message")
            ),
            (
                "req_peer",
                Some("sess_peer"),
                Some("did:test:other"),
                Some("tc_peer")
            ),
        ]
    );
    for row in &view.sent {
        assert_eq!(row.caused_by_request_id.as_deref(), Some("req_parent"));
        assert_eq!(
            row.caused_by_request_doc_id.as_deref(),
            Some(parent_doc_id.as_str())
        );
        assert_eq!(row.caused_by_session_id.as_deref(), Some("sess_parent"));
        assert_eq!(row.hop, Some(1));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn received_names_the_causing_session() {
    let (core, _tmp, _) = seed_provenance_fixture().await;
    let view = session_provenance(&core, "did:test:operator", "sess_child")
        .await
        .expect("provenance");

    assert!(view.sent.is_empty());
    let mut received: Vec<_> = view
        .received
        .iter()
        .map(|row| {
            (
                row.request_id.as_str(),
                row.caused_by_session_id.as_deref(),
                row.lifecycle_state.as_deref(),
            )
        })
        .collect();
    received.sort_unstable();
    assert_eq!(
        received,
        [
            ("req_child", Some("sess_parent"), Some("completed")),
            ("req_child_2", Some("sess_parent"), Some("processing")),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_root_session_has_no_provenance() {
    let (core, _tmp, _) = seed_provenance_fixture().await;
    let view = session_provenance(&core, "did:test:operator", "sess_unrelated")
        .await
        .expect("provenance");
    assert!(view.received.is_empty());
    assert!(view.sent.is_empty());
}
