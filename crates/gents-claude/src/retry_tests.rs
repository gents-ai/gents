use super::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

fn b(attempt: u32, retry_after: Option<&str>, random: f64) -> u64 {
    backoff_ms(attempt, retry_after, MAX_DELAY_MS, BASE_DELAY_MS, random)
}

#[test]
fn backoff_schedule_and_bounds() {
    for (random, expected) in [
        (0.0, [250, 500, 1000, 2000, 4000, 8000, 16000, 32000, 32000]),
        (
            0.999999,
            [312, 625, 1250, 2500, 5000, 10000, 20000, 40000, 40000],
        ),
    ] {
        for (attempt, want) in expected.into_iter().enumerate() {
            assert_eq!(b(attempt as u32, None, random), want, "attempt {attempt}");
        }
    }
    assert_eq!(b(1, None, 0.002), 500);
    assert_eq!(b(1, None, 0.006), 501);
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
