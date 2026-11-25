use super::*;
use crate::runner::embedded::executor::install::install_schema;
use crate::runner::embedded::executor::tests::seed_trigger;
use crate::runner::embedded::executor::{close, EventSourcesReady};
use crate::runner::executor::FixtureDocument;
use gents_automation::triggers::snapshot_observer::RuntimeSnapshotObserver;
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
        session_readers: Vec::new(),
        session_agent_owned: false,
        expired_client_request: false,
    };
    let stage = StageSpec {
        stage_id: "stage".into(),
        prompt: String::new(),
        seed: Some(seed),
        expect_fire: true,
        deadline_secs: Some(2),
        settle: false,
        continuation: None,
        review_previous: false,
        captures: Vec::new(),
    };
    let error = {
        let runtime = running_runtime();
        let waiting =
            seed_and_await_fire(&home, "did:test:unselected", &ready, &runtime, &stage, None);
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
async fn a_seed_observation_reads_its_fired_request_beside_a_failed_fire() {
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
        None,
    )
    .await
    .unwrap();
    let since = "2026-06-01T00:00:00Z";
    let mut quiet = Quiet::default();
    let unfired = observe_seed(&access, &home, since, &doc_id, false, &mut quiet)
        .await
        .unwrap();
    assert_eq!((unfired.fired, unfired.fires.failures), (None, vec![]));

    create_document(
        &access,
        "eval.test.seed_race_lineage",
        "AgentRequest",
        &json!({
            "request_id": "fired",
            "node_did": home.did(),
            "requester_did": home.did(),
            "agent_did": "did:test:new-agent",
            "session_id": "s-1",
            "content": "x",
            "execution_origin": "event",
            "lifecycle_state": "completed",
            "created_at": "2026-01-01T00:00:00Z",
            "caused_by_trigger_id": "seed-trigger",
            "caused_by_source_doc_id": doc_id,
        }),
        None,
    )
    .await
    .unwrap();
    seed_trigger(
        &home,
        "seed-trigger",
        "error",
        Some("earlier attempt"),
        since,
    )
    .await;
    gents_automation::test_support::update_trigger_runtime_fields(
        &home.node,
        home.did(),
        "seed-trigger",
        gents_automation::test_support::TriggerRuntimeUpdate {
            last_fired_source_doc_id: Some(doc_id.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let seen = observe_seed(&access, &home, since, &doc_id, true, &mut quiet)
        .await
        .unwrap();
    home.node.shutdown().await.expect("shutdown node");
    assert_eq!(
        seen.fired,
        Some(("fired".to_owned(), Some("seed-trigger".to_owned())))
    );
    assert_eq!(seen.fires.failures, [FireFailure::Error]);
    assert_eq!(
        seen.fires.detail.as_deref(),
        Some("trigger seed-trigger error: earlier attempt")
    );
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum SeedAction {
    Wait,
    Observe,
    Pass,
    Fail,
}

#[test]
fn generated_seed_steps_match_lean() {
    gents_lean_vocab::families::eval_seed_steps::replay::<FireFailure, OutcomeKind, SeedAction>(
        |input| {
            let (action, kind) = match seed_step(&SeedObservation {
                expect_fire: input.expect_fire,
                observable: input.observable,
                fired: input.fired,
                failures: input.failures.clone(),
                quiet: input.quiet,
                window_ended: input.window_ended,
            }) {
                SeedStep::Wait => (SeedAction::Wait, None),
                SeedStep::Observe => (SeedAction::Observe, None),
                SeedStep::Pass => (SeedAction::Pass, None),
                SeedStep::Fail(kind) => (SeedAction::Fail, Some(kind)),
            };
            gents_lean_vocab::families::eval_seed_steps::SeedStep { action, kind }
        },
    );
}

fn running_runtime() -> RunningRuntime {
    RunningRuntime {
        shutdown: watch::channel(false).0,
        handle: tokio::spawn(std::future::pending::<anyhow::Result<()>>()),
        node_did: String::new(),
    }
}
