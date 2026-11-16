use std::time::Duration;

use crate::support::agent_requests::submit_user_request;
use crate::support::live_inference::{
    bind_target, boot_live_agent, live_target, terminal_assistant_answer, wait_for_request_terminal,
};
use crate::support::test_db;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn target_backend_probes_healthy_and_completes() {
    let target = live_target().await;
    let db = test_db("live-client-request").await;
    let actors = bind_target(&db, &target).await;
    let runtime = boot_live_agent(&db, db.node_identity.clone())
        .await
        .unwrap();
    submit_user_request(
        actors.node_did(),
        &actors.fixture,
        "session-live-client",
        "req-live-client",
        "Briefly confirm that you received this message.",
        Default::default(),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(
            &actors.fixture.access,
            "req-live-client",
            Duration::from_secs(120)
        )
        .await,
        "completed"
    );
    assert!(
        !terminal_assistant_answer(&actors.fixture.access, "req-live-client")
            .await
            .trim()
            .is_empty()
    );
    runtime.shutdown().await;
}
