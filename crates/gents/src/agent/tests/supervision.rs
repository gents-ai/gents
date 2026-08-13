use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use super::super::supervision::supervise_behaviors_with_runner;
use super::super::*;
use super::support::*;

#[tokio::test]
async fn supervision_restarts_panicking_behavior_while_sibling_continues() {
    let panic_attempts = Arc::new(AtomicUsize::new(0));
    let sibling_ticks = Arc::new(AtomicUsize::new(0));
    let (panic_attempt_tx, mut panic_attempt_rx) = watch::channel(0usize);
    let (sibling_tick_tx, mut sibling_tick_rx) = watch::channel(0usize);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let behaviors = vec![
        Arc::new(
            PendingAgentBehavior::new("panic-profile")
                .build_with_identity_for_test(test_identity("panic-profile")),
        ),
        Arc::new(
            PendingAgentBehavior::new("steady-profile")
                .build_with_identity_for_test(test_identity("steady-profile")),
        ),
    ];

    let runner = {
        let panic_attempts = panic_attempts.clone();
        let sibling_ticks = sibling_ticks.clone();
        let panic_attempt_tx = panic_attempt_tx.clone();
        let sibling_tick_tx = sibling_tick_tx.clone();
        move |behavior: Arc<crate::config::AgentBehavior>, mut shutdown: watch::Receiver<bool>| {
            let panic_attempts = panic_attempts.clone();
            let sibling_ticks = sibling_ticks.clone();
            let panic_attempt_tx = panic_attempt_tx.clone();
            let sibling_tick_tx = sibling_tick_tx.clone();
            async move {
                if behavior.behavior_id == "panic-profile" {
                    let attempt = panic_attempts.fetch_add(1, Ordering::SeqCst);
                    panic_attempt_tx.send_replace(attempt + 1);
                    if attempt < 2 {
                        panic!("boom");
                    }
                }

                loop {
                    let ticks = sibling_ticks.fetch_add(1, Ordering::SeqCst) + 1;
                    sibling_tick_tx.send_replace(ticks);
                    tokio::select! {
                        _ = shutdown.changed() => return Ok(()),
                        _ = tokio::time::sleep(Duration::from_millis(25)) => {}
                    }
                }
            }
        }
    };

    let task = tokio::spawn(supervise_behaviors_with_runner(
        behaviors,
        shutdown_rx,
        crate::retry::RetryPolicy {
            max_retries: 3,
            base_delay_ms: 10,
            max_delay_ms: 25,
        },
        runner,
    ));

    tokio::time::timeout(Duration::from_secs(30), async {
        panic_attempt_rx
            .wait_for(|attempts| *attempts >= 3)
            .await
            .expect("panic-attempt observer should remain open");
        sibling_tick_rx
            .wait_for(|ticks| *ticks > 3)
            .await
            .expect("sibling-tick observer should remain open");
    })
    .await
    .expect("behaviors should restart and continue");
    assert!(panic_attempts.load(Ordering::SeqCst) >= 3);
    assert!(sibling_ticks.load(Ordering::SeqCst) > 3);

    let _ = shutdown_tx.send(true);
    task.await.unwrap().unwrap();
}
