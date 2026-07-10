use super::*;

#[test]
fn default_policy() {
    let policy = RetryPolicy::default();
    assert_eq!(policy.max_retries, 3);
    assert_eq!(policy.base_delay_ms, 1000);
    assert!(policy.has_retries());
}

#[test]
fn no_retry_policy() {
    let policy = RetryPolicy {
        max_retries: 0,
        ..Default::default()
    };
    assert!(!policy.has_retries());
}

#[test]
fn exponential_backoff_increases() {
    let policy = RetryPolicy {
        max_retries: 5,
        base_delay_ms: 1000,
        max_delay_ms: 60_000,
    };

    // Run multiple times to average out jitter
    let mut avg_delays = Vec::new();
    for attempt in 0..4 {
        let mut total = 0u128;
        let runs = 100;
        for _ in 0..runs {
            total += policy.delay_for_attempt(attempt).as_millis();
        }
        avg_delays.push(total / runs);
    }

    // Each attempt should roughly double (within jitter bounds)
    assert!(avg_delays[1] > avg_delays[0], "attempt 1 > attempt 0");
    assert!(avg_delays[2] > avg_delays[1], "attempt 2 > attempt 1");
}

#[test]
fn delay_respects_max() {
    let policy = RetryPolicy {
        max_retries: 3,
        base_delay_ms: 1000,
        max_delay_ms: 5000,
    };

    // Even at high attempt count, should not exceed max + jitter
    for _ in 0..50 {
        let delay = policy.delay_for_attempt(20);
        // max_delay + 25% jitter ceiling
        assert!(delay.as_millis() <= 6250);
    }
}

#[test]
fn delay_has_minimum_floor() {
    let policy = RetryPolicy {
        max_retries: 1,
        base_delay_ms: 10,
        max_delay_ms: 10,
    };

    for _ in 0..50 {
        assert!(policy.delay_for_attempt(0).as_millis() >= 100);
    }
}

#[test]
fn defradb_transaction_conflict_classifier_matches_prescribed_retry_error() {
    assert!(is_defradb_transaction_conflict_text(
        "commit error: datastore error: storage error: transaction conflict. Please retry"
    ));
    assert!(is_defradb_transaction_conflict_text(
        "TRANSACTION CONFLICT. PLEASE RETRY"
    ));
    assert!(!is_defradb_transaction_conflict_text(
        "unique constraint violation"
    ));
}

#[test]
fn defradb_conflict_backoff_is_exponential() {
    assert_eq!(defradb_conflict_retry_backoff(0).as_millis(), 100);
    assert_eq!(defradb_conflict_retry_backoff(1).as_millis(), 200);
    assert_eq!(defradb_conflict_retry_backoff(2).as_millis(), 400);
}

#[tokio::test]
async fn failure_reason_persistence_retries_non_conflict_transient_failures() {
    let mut attempts = 0u32;
    let value = retry_terminal_persistence_operation(
        "record_request_failure_reason",
        3,
        Duration::ZERO,
        || {
            attempts += 1;
            let current = attempts;
            async move {
                if current < 3 {
                    anyhow::bail!("temporary disk unavailable");
                }
                Ok("persisted")
            }
        },
    )
    .await
    .unwrap();

    assert_eq!(value, "persisted");
    assert_eq!(attempts, 3);
}

#[tokio::test]
async fn terminal_lifecycle_persistence_exhaustion_is_bounded() {
    let mut attempts = 0u32;
    let error = retry_terminal_persistence_operation::<(), _, _>(
        "transition_request_terminal_status",
        3,
        Duration::ZERO,
        || {
            attempts += 1;
            async { anyhow::bail!("storage remains unavailable") }
        },
    )
    .await
    .expect_err("the bounded retry must exhaust");

    assert!(error.to_string().contains("storage remains unavailable"));
    assert_eq!(attempts, 4, "initial attempt plus three retries");
}
