use super::*;
use crate::graph_pipeline::runtime::{
    attribution_test_fixture, graph_test_owner, seed_signed_graph_request,
};
use crate::watcher::DocumentChangeWakeup;

const RECOVERY: Duration = Duration::from_secs(3600);
const READ_CADENCE: Duration = GRAPH_RUN_READ_INTERVAL;

fn graph_run_wakeup(node: &EmbeddedNode) -> DocumentChangeWakeup {
    DocumentChangeWakeup::new(node, Some(&["GraphRun"]), RECOVERY)
}

#[tokio::test]
async fn active_graph_results_wake_the_native_sweep_before_recovery() {
    let (node, run, trigger) = attribution_test_fixture(1).await;
    seed_signed_graph_request(&node, &run, &trigger, "request", "processing", "").await;
    let sweep = sweep_owned_graph_runs(&node, graph_test_owner())
        .await
        .unwrap();
    assert_eq!(sweep.running, 1);
    assert_eq!(sweep.terminalized, 0);
    let mut wakeup = graph_run_wakeup(&node);
    let mut cancelled_wakeup = graph_run_wakeup(&node);
    for observer in [&mut wakeup, &mut cancelled_wakeup] {
        assert!(
            prepare_graph_run_wait(&node, observer, &sweep, tokio::time::Instant::now()).is_some()
        );
    }
    let access = ConfigAccess::local(node.clone());
    access
        .write(
            "graph_reconciler_test.result",
            &format!(
                r#"mutation {{ create_PipelineResult(input: {{
                    graph_run_id: "{}", report: "ready"
                }}) {{ _docID }} }}"#,
                escape_graphql_string(&run.correlation),
            ),
        )
        .await
        .unwrap();

    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let minimum_read_at = started + READ_CADENCE;
    let next_deadline = prepare_graph_run_wait(&node, &mut wakeup, &sweep, minimum_read_at);
    let cancel = CancellationToken::new();
    {
        let wait = wait_for_graph_run_change(&mut cancelled_wakeup, None, minimum_read_at, &cancel);
        tokio::pin!(wait);
        tokio::select! {
            biased;
            _ = &mut wait => panic!("a queued change must retain the sweep-start read cadence"),
            _ = tokio::task::yield_now() => {},
        }
        tokio::time::advance(READ_CADENCE / 2).await;
        cancel.cancel();
        assert!(!wait.await);
        assert!(tokio::time::Instant::now() < minimum_read_at);
    }
    let changed = wait_for_graph_run_change(
        &mut wakeup,
        next_deadline,
        minimum_read_at,
        &CancellationToken::new(),
    )
    .await;
    let elapsed = started.elapsed();
    tokio::time::resume();
    assert!(changed);
    assert!(elapsed >= READ_CADENCE && elapsed < READ_CADENCE * 2);
    let view = load_graph_run_view(&node, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(view.status, "running");
    assert_eq!(view.results[0].observed_count, 1);

    access
        .write(
            "graph_reconciler_test.complete_request",
            r#"mutation { update_AgentRequest(
                filter: {request_id: {_eq: "request"}},
                input: {lifecycle_state: "completed"}
            ) { _docID } }"#,
        )
        .await
        .unwrap();
    let completed = sweep_owned_graph_runs(&node, graph_test_owner())
        .await
        .unwrap();
    assert_eq!(completed.running, 0);
    assert_eq!(completed.terminalized, 1);
    assert_eq!(
        load_graph_run_view(&node, graph_test_owner(), &run.run_id)
            .await
            .unwrap()
            .status,
        "succeeded"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn known_graph_deadlines_and_cancellation_precede_recovery() {
    let (node, run, trigger) = attribution_test_fixture(1).await;
    seed_signed_graph_request(&node, &run, &trigger, "request", "processing", "").await;
    let sweep = sweep_owned_graph_runs(&node, graph_test_owner())
        .await
        .unwrap();
    assert_eq!(sweep.running, 1);
    assert!(sweep.next_deadline.is_some());
    let mut wakeup = graph_run_wakeup(&node);
    let cancel = CancellationToken::new();

    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let minimum_read_at = started + READ_CADENCE;
    let deadline = prepare_graph_run_wait(&node, &mut wakeup, &sweep, minimum_read_at).unwrap();
    assert!(deadline > started);
    assert!(deadline - started < RECOVERY);
    assert!(wait_for_graph_run_change(&mut wakeup, Some(deadline), minimum_read_at, &cancel).await);
    let woke = tokio::time::Instant::now();
    assert!(woke >= deadline && woke - deadline < Duration::from_millis(5));
    cancel.cancel();
    assert!(!wait_for_graph_run_change(&mut wakeup, None, minimum_read_at, &cancel).await);
    assert_eq!(tokio::time::Instant::now(), woke);
    tokio::time::resume();

    node.shutdown().await;
}

#[tokio::test]
async fn expired_graph_deadlines_keep_the_sweep_start_cadence() {
    let (node, run, trigger) = attribution_test_fixture(1).await;
    seed_signed_graph_request(&node, &run, &trigger, "request", "processing", "").await;
    let started_at = (chrono::Utc::now() - chrono::TimeDelta::hours(1)).to_rfc3339();
    ConfigAccess::local(node.clone())
        .write(
            "graph_reconciler_test.expired_run",
            &format!(
                r#"mutation {{ update_GraphRun(
                    filter: {{run_id: {{_eq: "{}"}}}}, input: {{started_at: "{}"}}
                ) {{ _docID }} }}"#,
                escape_graphql_string(&run.run_id),
                escape_graphql_string(&started_at),
            ),
        )
        .await
        .unwrap();
    let sweep = sweep_owned_graph_runs(&node, graph_test_owner())
        .await
        .unwrap();
    assert_eq!(sweep.running, 1);
    assert!(!sweep.retry_required);
    assert!(sweep.next_deadline.unwrap() < chrono::Utc::now());
    let mut wakeup = graph_run_wakeup(&node);
    let cancel = CancellationToken::new();

    tokio::time::pause();
    for sweep_duration in [READ_CADENCE * 4 / 5, READ_CADENCE * 2] {
        let started = tokio::time::Instant::now();
        let minimum_read_at = started + READ_CADENCE;
        tokio::time::advance(sweep_duration).await;
        let wait_started = tokio::time::Instant::now();
        let deadline = prepare_graph_run_wait(&node, &mut wakeup, &sweep, minimum_read_at).unwrap();
        let expected = wait_started.max(minimum_read_at);
        assert_eq!(deadline, expected);
        assert!(
            wait_for_graph_run_change(&mut wakeup, Some(deadline), minimum_read_at, &cancel).await
        );
        let woke = tokio::time::Instant::now();
        assert!(woke >= expected && woke - expected < Duration::from_millis(5));
    }
    tokio::time::resume();

    node.shutdown().await;
}

#[tokio::test]
async fn individual_graph_errors_retry_before_other_runs_later_deadlines() {
    let (node, broken, _) = attribution_test_fixture(1).await;
    let healthy = crate::graph_pipeline::start_graph_run(
        &node,
        None,
        graph_test_owner(),
        "result-pipeline",
        None,
        "input",
        json!({"payload": "healthy run"}),
        crate::graph_pipeline::EntryInputOrigin::Operator,
    )
    .await
    .unwrap();
    assert_ne!(broken.run_id, healthy.run_id);
    ConfigAccess::local(node.clone())
        .write(
            "graph_reconciler_test.invalid_failure",
            &format!(
                r#"mutation {{ update_GraphRun(
                    filter: {{run_id: {{_eq: "{}"}}}},
                    input: {{error: "{}"}}
                ) {{ _docID }} }}"#,
                escape_graphql_string(&broken.run_id),
                escape_graphql_string("{}"),
            ),
        )
        .await
        .unwrap();
    let error = reconcile_graph_run(&node, None, graph_test_owner(), &broken.run_id)
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<GraphRunUnobservable>(),
        Some(GraphRunUnobservable::InvalidFailureEvidence)
    ));
    let sweep = sweep_owned_graph_runs(&node, graph_test_owner())
        .await
        .unwrap();
    assert_eq!(sweep.running, 2);
    assert!(sweep.retry_required);
    assert!(sweep.next_deadline.is_some());
    let mut wakeup = graph_run_wakeup(&node);

    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let minimum_read_at = started + READ_CADENCE;
    let deadline = prepare_graph_run_wait(&node, &mut wakeup, &sweep, minimum_read_at).unwrap();
    assert_eq!(deadline, minimum_read_at);
    assert!(
        wait_for_graph_run_change(
            &mut wakeup,
            Some(deadline),
            minimum_read_at,
            &CancellationToken::new()
        )
        .await
    );
    let elapsed = started.elapsed();
    assert!(elapsed >= READ_CADENCE && elapsed < READ_CADENCE * 2);
    tokio::time::resume();

    node.shutdown().await;
}
