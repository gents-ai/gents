//! Explicitly gated live inference smoke; shared fixtures live in support.

use crate::support::fixtures::test_identity;
use crate::support::interrupt::create_runtime_request;
use crate::support::live_inference::*;
use crate::support::test_db;
use gents::AgentIdentity;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: set GENTS_EVAL_TARGET and pass --ignored"]
async fn target_backend_probes_healthy_and_completes() {
    let target = live_target();
    target.assert_reachable().await;

    let db = test_db("steward-loop-live-smoke").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("steward-loop-live-smoke"));

    let (agent_did, behavior_id) = bind_target(db.node.as_ref(), identity.as_ref(), &target).await;

    let agent = boot_live_agent(&db, identity)
        .await
        .expect("boot live agent");

    let request_id = "req-live-smoke";
    let session_id = "session-live-smoke";
    let started = std::time::Instant::now();
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        request_id,
        session_id,
        "Reply with the single word: ok",
    )
    .await;

    let terminal =
        wait_for_request_terminal(db.node.as_ref(), request_id, Duration::from_secs(120)).await;
    let elapsed = started.elapsed();
    eprintln!("[live-smoke] request terminal state = {terminal} (latency {elapsed:?})");
    assert_eq!(
        terminal, "completed",
        "target {} request must complete; got {terminal}",
        target.name
    );

    let answer =
        wait_for_assistant_answer(db.node.as_ref(), request_id, Duration::from_secs(30)).await;
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
