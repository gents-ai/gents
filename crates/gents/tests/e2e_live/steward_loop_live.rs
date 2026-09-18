//! Explicitly gated live inference smoke; shared fixtures live in support.

use crate::support::fixtures::test_identity;
use crate::support::interrupt::create_runtime_request;
use crate::support::live_inference::*;
use crate::support::test_db;
use gents::AgentIdentity;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: set GENTS_D4F_LIVE=1 and pass --ignored"]
async fn d4f_backend_probes_healthy_and_completes() {
    assert!(
        d4f_enabled(),
        "set GENTS_D4F_LIVE=1 and pass --ignored to run the d4f live smoke test"
    );

    assert_d4f_reachable().await;

    let db = test_db("steward-loop-d4f-smoke").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("steward-loop-d4f-smoke"));

    let (agent_did, behavior_id) = bind_d4f_backend(db.node.as_ref(), identity.as_ref()).await;

    let agent = boot_d4f_agent(&db, identity).await.expect("boot d4f agent");

    let request_id = "req-d4f-smoke";
    let session_id = "session-d4f-smoke";
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
    eprintln!("[d4f-smoke] request terminal state = {terminal} (latency {elapsed:?})");
    assert_eq!(
        terminal, "completed",
        "d4f-backed request must complete; got {terminal}"
    );

    let answer =
        wait_for_assistant_answer(db.node.as_ref(), request_id, Duration::from_secs(30)).await;
    eprintln!("[d4f-smoke] assistant answer = {answer:?}");
    assert!(
        !answer.trim().is_empty(),
        "d4f must produce a non-empty assistant response; got empty"
    );
    if answer.to_lowercase().contains("ok") {
        eprintln!("[d4f-smoke] OK: answer contains 'ok'");
    } else {
        eprintln!("[d4f-smoke] SOFT-WARN: answer did not contain 'ok': {answer:?}");
    }

    agent.shutdown().await;
}
