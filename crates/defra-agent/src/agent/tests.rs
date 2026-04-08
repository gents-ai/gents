use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use super::supervision::supervise_profiles_with_runner;
use super::*;
use crate::identity::SimpleIdentity;

async fn test_node() -> Arc<EmbeddedNode> {
    Arc::new(EmbeddedNode::builder().build().await.unwrap())
}

fn test_identity(name: &str) -> SimpleIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    SimpleIdentity::new(name, path, None)
}

#[tokio::test]
async fn profile_builder_rejects_missing_identity() {
    let node = test_node().await;
    let error = match DefraAgent::builder()
        .node(node)
        .profile("amy-general")
        .done()
        .build()
    {
        Ok(_) => panic!("expected missing identity error"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("missing identity"));
}

#[tokio::test]
async fn profile_builder_rejects_duplicate_names() {
    let node = test_node().await;
    let error = match DefraAgent::builder()
        .node(node)
        .profile("amy-general")
        .identity(test_identity("amy-general-a"))
        .done()
        .profile("amy-general")
        .identity(test_identity("amy-general-b"))
        .done()
        .build()
    {
        Ok(_) => panic!("expected duplicate profile error"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("duplicate profile name"));
}

#[tokio::test]
async fn supervision_restarts_panicking_profile_while_sibling_continues() {
    let panic_attempts = Arc::new(AtomicUsize::new(0));
    let sibling_ticks = Arc::new(AtomicUsize::new(0));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let profiles = vec![
        Arc::new(
            PendingProfileConfig::new("panic-profile")
                .build_with_identity_for_test(test_identity("panic-profile")),
        ),
        Arc::new(
            PendingProfileConfig::new("steady-profile")
                .build_with_identity_for_test(test_identity("steady-profile")),
        ),
    ];

    let runner = {
        let panic_attempts = panic_attempts.clone();
        let sibling_ticks = sibling_ticks.clone();
        move |profile: Arc<crate::config::ProfileConfig>, mut shutdown: watch::Receiver<bool>| {
            let panic_attempts = panic_attempts.clone();
            let sibling_ticks = sibling_ticks.clone();
            async move {
                if profile.name == "panic-profile" {
                    let attempt = panic_attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt < 2 {
                        panic!("boom");
                    }
                }

                loop {
                    sibling_ticks.fetch_add(1, Ordering::SeqCst);
                    tokio::select! {
                        _ = shutdown.changed() => return Ok(()),
                        _ = tokio::time::sleep(Duration::from_millis(25)) => {}
                    }
                }
            }
        }
    };

    let task = tokio::spawn(supervise_profiles_with_runner(
        profiles,
        shutdown_rx,
        crate::retry::RetryPolicy {
            max_retries: 3,
            base_delay_ms: 10,
            max_delay_ms: 25,
        },
        runner,
    ));

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if panic_attempts.load(Ordering::SeqCst) >= 3
                && sibling_ticks.load(Ordering::SeqCst) > 3
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("profiles should restart and continue");
    assert!(panic_attempts.load(Ordering::SeqCst) >= 3);
    assert!(sibling_ticks.load(Ordering::SeqCst) > 3);

    let _ = shutdown_tx.send(true);
    task.await.unwrap().unwrap();
}
