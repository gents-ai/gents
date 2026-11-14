use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_session::request::lifecycle::test_support::{
    agent_access, agent_identity, claimed_owner_caused_by, request_row, test_node,
};
use gents_session::request::lifecycle::TriggerLineage;
use std::time::Duration;

#[tokio::test]
async fn latest_only_supersession_settles_parked_and_resuming_fires() {
    use gents_session::request::lifecycle::backend_wait::{wake_parked_execution, WakeResult};
    for resuming in [false, true] {
        let (node, _db, identity, agent) = test_node().await;
        let access = agent_access(&node, &identity, &agent);
        let mut lifecycle = claimed_owner_caused_by(
            &node,
            &identity,
            &agent,
            TriggerLineage {
                trigger_id: Some("latest".to_owned()),
                ..Default::default()
            },
        )
        .await;
        let writer = gents_session::request::streaming::DefraStreamWriter::new_managed(
            node.clone(),
            agent_identity(&identity, &agent).as_ref(),
            Duration::ZERO,
        )
        .unwrap();
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        let doc_id = lifecycle.request().doc_id.clone();
        let node_did = lifecycle.request().node_did.clone();
        assert!(lifecycle.park_for_backend().await.unwrap());
        if resuming {
            assert!(matches!(
                wake_parked_execution(
                    &access,
                    &doc_id,
                    "lease-test",
                    Some(chrono::Utc::now()),
                    chrono::Utc::now(),
                )
                .await
                .unwrap(),
                WakeResult::Resumed { .. }
            ));
        }
        let owner = &node_did;
        let superseded = access
            .transact_write("test.supersede_parked_fire", move |txn| {
                Box::pin(async move {
                    crate::triggers::engine::durable::supersede_trigger_requests_in_txn(
                        txn, owner, "latest", None,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        assert_eq!(superseded, 1, "resuming={resuming}");
        let row = request_row(&access, &doc_id).await;
        assert_eq!(
            row.lifecycle_state,
            Some(RequestLifecycleState::Superseded),
            "resuming={resuming}"
        );
        assert!(row.terminalized_at.is_some());
        assert_eq!(
            wake_parked_execution(
                &access,
                &doc_id,
                "lease-test",
                Some(chrono::Utc::now()),
                chrono::Utc::now(),
            )
            .await
            .unwrap(),
            WakeResult::Lost,
            "a superseded fire must not resume"
        );
    }
}
