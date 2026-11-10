//! Failed background wake redrive scheduling invariants.
//!
//! Regression coverage for the production owners in
//! `crates/gents/src/request/lifecycle/background_wake_recovery.rs`:
//!
//! - `background_wake_retry_delay` must stay bounded: exponential growth caps
//!   at `BACKGROUND_WAKE_RETRY_MAX_SECONDS` (60s) and a negative persisted
//!   retry count clamps to the 5s base instead of panicking or inverting.
//! - `background_wake_next_retry_at` returns `None` for an absent or malformed
//!   `terminalized_at` (no panic, no invented delay). How `None` maps to
//!   redrive due-ness is decided by the private `retry_is_due` and is not
//!   exercised here.
//!
//! These tests exercise the public production functions directly (no copied
//! logic). They pin scheduling invariants only; they make no claim about
//! detecting wedged or unhealthy busy sessions.

use gents::request::lifecycle::{background_wake_next_retry_at, background_wake_retry_delay};

#[test]
fn background_wake_backoff_uses_exponential_delay_up_to_the_cap() {
    assert_eq!(background_wake_retry_delay(0), chrono::Duration::seconds(5));
    assert_eq!(
        background_wake_retry_delay(1),
        chrono::Duration::seconds(10)
    );
    assert_eq!(
        background_wake_retry_delay(2),
        chrono::Duration::seconds(20)
    );
    assert_eq!(
        background_wake_retry_delay(3),
        chrono::Duration::seconds(40)
    );
    // From the fourth attempt on, the delay saturates so the recovery sweep
    // keeps a bounded cadence instead of backing off unboundedly.
    assert_eq!(
        background_wake_retry_delay(4),
        chrono::Duration::seconds(60)
    );
}

#[test]
fn background_wake_backoff_stays_bounded_for_extreme_retry_counts() {
    for retry_count in [0, 1, 2, 3, 4, 10, 30, 31, 63, 64, i64::MAX] {
        let delay = background_wake_retry_delay(retry_count);
        assert!(
            delay <= chrono::Duration::seconds(60),
            "retry_count={retry_count} exceeded the 60s cap: {delay:?}"
        );
        assert!(
            delay >= chrono::Duration::seconds(5),
            "retry_count={retry_count} fell below the 5s base: {delay:?}"
        );
    }
    // A negative persisted retry count must clamp to the base delay.
    assert_eq!(
        background_wake_retry_delay(-7),
        chrono::Duration::seconds(5)
    );
}

#[test]
fn absent_or_malformed_terminal_timestamp_returns_none() {
    // No persisted terminal timestamp yields no backoff anchor.
    assert_eq!(background_wake_next_retry_at(None, 1), None);
    // A malformed timestamp yields None: no panic, no invented delay.
    assert_eq!(
        background_wake_next_retry_at(Some("not-a-timestamp"), 1),
        None
    );
    assert_eq!(
        background_wake_next_retry_at(Some("2026-13-45T99:99:99Z"), 1),
        None
    );
    // A valid anchor yields terminalized_at + the retry delay.
    let anchored = background_wake_next_retry_at(Some("2026-08-12T22:00:00Z"), 1)
        .expect("valid rfc3339 terminal timestamp yields a backoff anchor");
    assert_eq!(
        anchored,
        chrono::DateTime::parse_from_rfc3339("2026-08-12T22:00:10Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    );
}
