use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::json;

use super::*;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 30, 12, 0, 0).unwrap()
}

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).unwrap()
}

fn find<'r>(report: &'r UsageReport, label: &str) -> &'r UsageWindow {
    report
        .windows
        .iter()
        .find(|window| window.label == label)
        .unwrap_or_else(|| panic!("no {label} window in {report:?}"))
}

fn headers(report_headers: &[(&str, &str)]) -> UsageReport {
    usage_from_headers(report_headers.iter().copied(), UsageSource::Header, now())
}

#[test]
fn headers_claude_unified_fraction_becomes_percent_windows() {
    let report = headers(&[
        ("anthropic-ratelimit-unified-5h-utilization", "0.25"),
        ("anthropic-ratelimit-unified-5h-reset", "1790000000"),
        ("Anthropic-Ratelimit-Unified-7d-Utilization", "0.5"),
        ("anthropic-ratelimit-unified-7d-reset", "1790500000"),
        ("anthropic-ratelimit-unified-7d_opus-utilization", "1"),
        ("anthropic-ratelimit-unified-overage-utilization", "0.9"),
        ("anthropic-ratelimit-unified-fallback-percentage", "0.5"),
    ]);
    assert_eq!(report.windows.len(), 3, "{report:?}");
    let five = find(&report, "5h");
    assert_eq!(five.used_pct, 25.0);
    assert_eq!(five.resets_at, Some(at(1_790_000_000)));
    let week = find(&report, "7d");
    assert_eq!(week.used_pct, 50.0);
    assert_eq!(week.resets_at, Some(at(1_790_500_000)));
    let opus = find(&report, "7d opus");
    assert_eq!(opus.used_pct, 100.0);
    assert_eq!(opus.resets_at, None);
}

#[test]
fn headers_codex_slot_labels_minutes_and_credits() {
    let report = headers(&[
        ("x-codex-primary-used-percent", "12.5"),
        ("x-codex-primary-window-minutes", "300"),
        ("x-codex-primary-reset-at", "1790000000"),
        ("x-codex-secondary-used-percent", "40"),
        ("x-codex-secondary-window-minutes", "10080"),
        ("x-codex-credits-has-credits", "true"),
        ("x-codex-credits-unlimited", "false"),
        ("x-codex-credits-balance", "12.50"),
    ]);
    let primary = find(&report, "primary");
    assert_eq!(primary.used_pct, 12.5);
    assert_eq!(primary.window_minutes, Some(300));
    assert_eq!(primary.resets_at, Some(at(1_790_000_000)));
    let secondary = find(&report, "secondary");
    assert_eq!(secondary.used_pct, 40.0);
    assert_eq!(secondary.window_minutes, Some(10080));
    assert_eq!(
        report.credits,
        Some(UsageCredits {
            has_credits: Some(true),
            unlimited: Some(false),
            balance: Some("12.50".into()),
            observed_at: now(),
        })
    );
}

#[test]
fn headers_codex_empty_window_is_skipped() {
    let report = headers(&[
        ("x-codex-primary-used-percent", "0"),
        ("x-codex-primary-window-minutes", "0"),
        ("x-codex-secondary-used-percent", "0"),
        ("x-codex-secondary-window-minutes", "10080"),
    ]);
    assert_eq!(report.windows.len(), 1, "{report:?}");
    assert_eq!(find(&report, "secondary").used_pct, 0.0);
}

#[test]
fn headers_openai_key_headroom_with_duration_reset() {
    let report = headers(&[
        ("x-ratelimit-limit-requests", "100"),
        ("x-ratelimit-remaining-requests", "75"),
        ("x-ratelimit-reset-requests", "6m0s"),
        ("x-ratelimit-limit-tokens", "1000"),
        ("x-ratelimit-remaining-tokens", "1000"),
        ("x-ratelimit-reset-tokens", "20ms"),
    ]);
    let requests = find(&report, "requests");
    assert_eq!(requests.used_pct, 25.0);
    assert_eq!(requests.window_minutes, None);
    assert_eq!(requests.resets_at, Some(now() + Duration::minutes(6)));
    let tokens = find(&report, "tokens");
    assert_eq!(tokens.used_pct, 0.0);
    assert_eq!(tokens.resets_at, Some(now() + Duration::milliseconds(20)));
}

#[test]
fn headers_none_gives_an_empty_report() {
    let report = headers(&[("content-type", "text/event-stream"), ("request-id", "req-1")]);
    assert!(report.is_empty(), "{report:?}");
    assert_eq!(report, UsageReport::default());
}

#[test]
fn headers_malformed_values_drop_the_window_never_default() {
    let report = headers(&[
        ("anthropic-ratelimit-unified-5h-utilization", "NaN"),
        ("anthropic-ratelimit-unified-7d-utilization", "-0.1"),
        ("anthropic-ratelimit-unified-7d_sonnet-utilization", "lots"),
        ("x-codex-primary-used-percent", ""),
        ("x-codex-primary-window-minutes", "300"),
        ("x-codex-secondary-used-percent", "250"),
        ("x-ratelimit-limit-requests", "0"),
        ("x-ratelimit-remaining-requests", "0"),
        ("x-ratelimit-limit-tokens", "100"),
        ("x-ratelimit-remaining-tokens", "many"),
    ]);
    assert_eq!(report.windows.len(), 1, "{report:?}");
    let secondary = find(&report, "secondary");
    assert_eq!(secondary.used_pct, 100.0, "above 100 is clamped");
    assert!(report.credits.is_none());
}

#[test]
fn headers_source_and_observed_at_are_kept() {
    let report = usage_from_headers(
        [("x-codex-primary-used-percent", "5")],
        UsageSource::Error,
        now(),
    );
    let primary = find(&report, "primary");
    assert_eq!(primary.source, UsageSource::Error);
    assert_eq!(primary.observed_at, now());
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["windows"][0]["source"], json!("error"));
}
