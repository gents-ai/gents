use crate::support::builder_runtime::submit_request;
use crate::support::live_inference::*;
use crate::support::test_db;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: set GENTS_EVAL_TARGET and pass --ignored"]
async fn target_backend_probes_healthy_and_completes() {
    let target = live_target();
    target.assert_reachable().await;

    let db = test_db("steward-loop-live-smoke").await;
    let actors = bind_target(&db, &target).await;
    let reader = &actors.fixture.access;

    let agent = boot_live_agent(&db, db.node_identity.clone())
        .await
        .expect("boot live agent");

    let request_id = "req-live-smoke";
    let session_id = "session-live-smoke";
    let started = std::time::Instant::now();
    submit_request(
        &actors,
        request_id,
        session_id,
        "interactive",
        "Reply with the single word: ok",
    )
    .await;

    let terminal = wait_for_request_terminal(reader, request_id, Duration::from_secs(120)).await;
    let elapsed = started.elapsed();
    eprintln!("[live-smoke] request terminal state = {terminal} (latency {elapsed:?})");
    assert_eq!(
        terminal, "completed",
        "target {} request must complete; got {terminal}",
        target.name
    );

    let answer = wait_for_assistant_answer(reader, request_id, Duration::from_secs(30)).await;
    eprintln!("[live-smoke] assistant answer = {answer:?}");
    assert!(
        !answer.trim().is_empty(),
        "target {} must produce a non-empty assistant response; got empty",
        target.name
    );
    if answer.to_lowercase().contains("ok") {
        eprintln!("[live-smoke] OK: answer contains 'ok'");
    } else {
        eprintln!("[live-smoke] SOFT-WARN: answer did not contain 'ok': {answer:?}");
    }

    agent.shutdown().await;
}
