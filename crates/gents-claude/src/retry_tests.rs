use super::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

fn b(attempt: u32, retry_after: Option<&str>, random: f64) -> u64 {
    backoff_ms(attempt, retry_after, MAX_DELAY_MS, BASE_DELAY_MS, random)
}

#[test]
fn backoff_table_without_jitter() {
    let want = [250, 500, 1000, 2000, 4000, 8000, 16000, 32000, 32000];
    for (attempt, want) in want.into_iter().enumerate() {
        assert_eq!(b(attempt as u32, None, 0.0), want, "attempt {attempt}");
    }
}

#[test]
fn backoff_table_with_maximal_jitter() {
    let want = [312, 625, 1250, 2500, 5000, 10000, 20000, 40000, 40000];
    for (attempt, want) in want.into_iter().enumerate() {
        assert_eq!(b(attempt as u32, None, 0.999999), want, "attempt {attempt}");
    }
}

#[test]
fn backoff_rounds_half_up() {
    // jitter of 0.25 rounds down, of 0.75 rounds up.
    assert_eq!(b(1, None, 0.002), 500);
    assert_eq!(b(1, None, 0.006), 501);
}

#[test]
fn backoff_cap_and_custom_base() {
    assert_eq!(backoff_ms(30, None, 1000, 500, 0.0), 1000);
    assert_eq!(backoff_ms(3, None, u64::MAX, 100, 0.0), 400);
    assert_eq!(b(u32::MAX, None, 0.0), 32000);
}

#[test]
fn retry_after_header_table() {
    let cases = [
        ("12", 12000),
        ("12.9", 12000),
        (" 7s", 7000),
        ("\u{a0}\u{feff}9", 9000),
        ("+5", 5000),
        ("0", 500),
        ("-3", 500),
        ("abc", 500),
        ("", 500),
        ("Wed, 21 Oct 2026 07:28:00 GMT", 500),
        ("0x10", 500),
    ];
    for (header, want) in cases {
        assert_eq!(b(1, Some(header), 0.0), want, "{header:?}");
    }
    assert_eq!(b(1, Some("1"), 0.0), 1000);
    assert_eq!(
        b(1, Some("99999999999999999999999999999999999999999"), 0.0),
        u64::MAX
    );
}

#[test]
fn js_parse_int_matches_ecmascript() {
    assert_eq!(js_parse_int("  42abc"), Some(42.0));
    assert_eq!(js_parse_int("-7"), Some(-7.0));
    assert_eq!(js_parse_int("1e3"), Some(1.0));
    assert_eq!(js_parse_int("."), None);
    assert_eq!(js_parse_int("-"), None);
    assert_eq!(js_parse_int("\u{85}1"), None);
    assert_eq!(js_parse_int(&"9".repeat(400)), Some(f64::INFINITY));
}

#[test]
fn backoff_is_monotonic_in_attempts() {
    let mut rng = StdRng::seed_from_u64(7);
    for _ in 0..500 {
        let random: f64 = rng.random();
        let mut last = 0;
        for attempt in 0..64 {
            let v = b(attempt, None, random);
            assert!(v >= last, "attempt {attempt} random {random}");
            last = v;
        }
    }
}

#[test]
fn header_override_is_never_below_the_computed_delay() {
    let mut rng = StdRng::seed_from_u64(11);
    for _ in 0..2000 {
        let attempt = rng.random_range(0..12);
        let random: f64 = rng.random();
        let header = rng.random_range(-100i64..100).to_string();
        let computed = b(attempt, None, random);
        assert!(b(attempt, Some(&header), random) >= computed);
    }
}

#[test]
fn interactive_cliff_boundaries() {
    assert!(!exceeds_interactive_cliff(60_000, None));
    assert!(exceeds_interactive_cliff(60_001, None));
    assert!(exceeds_interactive_cliff(60_001, Some(10_000)));
    assert!(!exceeds_interactive_cliff(80_000, Some(70_000)));
    assert!(!exceeds_interactive_cliff(75_000, Some(60_000)));
    assert!(exceeds_interactive_cliff(75_001, Some(60_000)));
    assert!(!exceeds_interactive_cliff(0, Some(u64::MAX)));
}

const OVERFLOW: &str = "prompt is too long: input length and `max_tokens` exceed context limit: 188240 + 20000 > 200000, decrease input length or `max_tokens` and try again";

#[test]
fn context_overflow_shrinks_max_tokens() {
    assert_eq!(
        adjust_for_context_overflow(OVERFLOW, None),
        Ok(Some(10_760))
    );
    assert_eq!(
        adjust_for_context_overflow(OVERFLOW, Some(20_000)),
        Ok(Some(10_760))
    );
}

#[test]
fn context_overflow_ignores_other_messages() {
    for message in [
        "",
        "invalid request",
        "input length and `max_tokens` exceed context limit: 1 + 2",
        "input length and `max_tokens` exceed context limit: x + 2 > 3",
    ] {
        assert_eq!(
            adjust_for_context_overflow(message, None),
            Ok(None),
            "{message}"
        );
    }
}

#[test]
fn context_overflow_below_floor_and_no_progress() {
    let low = "input length and `max_tokens` exceed context limit: 197000 + 8000 > 200000";
    assert_eq!(
        adjust_for_context_overflow(low, None),
        Err(ContextOverflowError::BelowFloor { available: 2000 })
    );
    let edge = "input length and `max_tokens` exceed context limit: 195000 + 8000 > 200000";
    assert_eq!(adjust_for_context_overflow(edge, None), Ok(Some(4000)));
    assert_eq!(
        adjust_for_context_overflow(edge, Some(4000)),
        Err(ContextOverflowError::NoProgress {
            available: 4000,
            previous: 4000
        })
    );
    assert_eq!(
        adjust_for_context_overflow(edge, Some(4001)),
        Ok(Some(4000))
    );
}

#[test]
fn context_overflow_huge_numbers_saturate() {
    let huge =
        "input length and `max_tokens` exceed context limit: 99999999999999999999999 + 1 > 5";
    assert_eq!(
        adjust_for_context_overflow(huge, None),
        Err(ContextOverflowError::BelowFloor { available: 0 })
    );
    let big_limit =
        "input length and `max_tokens` exceed context limit: 1 + 1 > 99999999999999999999999";
    assert_eq!(
        adjust_for_context_overflow(big_limit, None),
        Ok(Some(u64::MAX - 1001))
    );
}

#[test]
fn retryable_status_table() {
    assert!(retryable_status(400, Some("true")));
    assert!(!retryable_status(500, Some("false")));
    for s in [408, 409, 429, 500, 529, 599] {
        assert!(retryable_status(s, None), "{s}");
        assert!(retryable_status(s, Some("TRUE")), "{s}");
    }
    for s in [200, 400, 401, 403, 404, 422] {
        assert!(!retryable_status(s, None), "{s}");
    }
}
