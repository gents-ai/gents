use super::*;
use crate::eval::runner::embedded::executor::install::install_schema;
use crate::eval::runner::embedded::executor::tests::seed_trigger;
use crate::eval::runner::embedded::executor::{close, EventSourcesReady};
use crate::RuntimeSnapshotObserver;
use serde_json::json;

#[tokio::test]
async fn a_seed_wait_resolves_as_soon_as_reconciliation_reports_an_error() {
    let home = EmbeddedHome::create_temp("seed-reconcile-error")
        .await
        .unwrap();
    let (sender, ready) = watch::channel(None);
    let observer = EventSourcesReady(sender);
    let seed = FixtureDocument {
        collection: "MissingSeedCollection".into(),
        document: json!({"label": "not submitted"}),
    };
    let error = {
        let waiting = seed_and_await_fire(
            &home,
            &ready,
            &seed,
            Duration::from_secs(2),
            "trial",
            "stage",
        );
        tokio::pin!(waiting);
        assert!(futures::poll!(waiting.as_mut()).is_pending());
        observer.on_event_sources_reconciled(1, "fixture", Err("missing seed collection"));
        let std::task::Poll::Ready(result) = futures::poll!(waiting.as_mut()) else {
            panic!("the reported reconciliation error did not end the seed wait");
        };
        result.unwrap_err()
    };
    assert_eq!(
        error.to_string(),
        "the trial runtime's event sources did not reconcile: missing seed collection"
    );
    close(home).await;
}

#[tokio::test]
async fn a_request_published_between_seed_observations_takes_precedence() {
    let home = EmbeddedHome::create_temp("seed-observation-race")
        .await
        .unwrap();
    let access = ConfigAccess::local(home.node.clone());
    install_schema(&access, "type RaceSeed { label: String }")
        .await
        .unwrap();
    let doc_id = create_document(
        &access,
        "eval.test.seed_race",
        "RaceSeed",
        &json!({"label": "race"}),
    )
    .await
    .unwrap();
    assert_eq!(fired_request(&home.node, &doc_id).await.unwrap(), None);

    create_document(
        &access,
        "eval.test.seed_race_lineage",
        "AgentRequest",
        &json!({
            "request_id": "fired",
            "node_did": home.did(),
            "requester_did": home.did(),
            "agent_id": "seeded-agent",
            "session_id": "s-1",
            "content": "x",
            "execution_origin": "event",
            "lifecycle_state": "completed",
            "created_at": "2026-01-01T00:00:00Z",
            "caused_by_source_doc_id": doc_id,
        }),
    )
    .await
    .unwrap();
    let since = "2026-06-01T00:00:00Z";
    seed_trigger(
        &home,
        "seed-trigger",
        "error",
        Some("earlier attempt"),
        since,
    )
    .await;
    crate::document_config::update_trigger_runtime_fields(
        &home.node,
        home.did(),
        "seed-trigger",
        crate::document_config::TriggerRuntimeUpdate {
            last_fired_source_doc_id: Some(doc_id.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let failure = trigger_failure(&home.node, home.did(), since, Some(&doc_id), "t1", "seed")
        .await
        .expect("the second observation sees the delayed trigger failure");
    let resolved = resolve_seed_failure(
        &home.node,
        &doc_id,
        failure,
        Duration::from_secs(600),
        Instant::now(),
    )
    .await
    .unwrap();
    home.node.shutdown().await;
    let StageSubmission::Submitted {
        request_id,
        remaining,
    } = resolved
    else {
        panic!("the committed request must take precedence: {resolved:?}");
    };
    assert_eq!(request_id, "fired");
    assert!(!remaining.is_zero());
}
