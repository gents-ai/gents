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
    let report = headers(&[
        ("content-type", "text/event-stream"),
        ("request-id", "req-1"),
    ]);
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

fn rfc(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

#[test]
fn endpoint_codex_windows_plan_and_credits() {
    let body = json!({
        "plan_type": "plus",
        "rate_limit": {
            "allowed": true,
            "limit_reached": false,
            "primary_window": {
                "used_percent": 12,
                "limit_window_seconds": 18000,
                "reset_after_seconds": 600,
                "reset_at": 1790000000
            },
            "secondary_window": {
                "used_percent": 40,
                "limit_window_seconds": 604800,
                "reset_at": 1790500000
            }
        },
        "credits": { "has_credits": true, "unlimited": false, "balance": "3.00" }
    });
    let report = codex_usage(&body, now()).expect("well-formed");
    let primary = find(&report, "primary");
    assert_eq!(primary.used_pct, 12.0);
    assert_eq!(primary.window_minutes, Some(300));
    assert_eq!(primary.resets_at, Some(at(1_790_000_000)));
    assert_eq!(primary.source, UsageSource::Endpoint);
    let secondary = find(&report, "secondary");
    assert_eq!(secondary.window_minutes, Some(10080));
    assert_eq!(
        report.plan.as_ref().map(|plan| plan.name.as_str()),
        Some("plus")
    );
    let credits = report.credits.expect("credits");
    assert_eq!(credits.has_credits, Some(true));
    assert_eq!(credits.unlimited, Some(false));
    assert_eq!(credits.balance.as_deref(), Some("3.00"));
}

#[test]
fn endpoint_codex_unknown_fields_are_ignored() {
    let body = json!({
        "rate_limit": {
            "primary_window": { "used_percent": 7, "future_field": [1, 2] },
            "secondary_window": null,
            "luna_reserve": {}
        },
        "spend_control": { "anything": true },
        "additional_rate_limits": [{ "limit_name": "extra" }]
    });
    let report = codex_usage(&body, now()).expect("well-formed");
    assert_eq!(report.windows.len(), 1, "{report:?}");
    assert_eq!(find(&report, "primary").used_pct, 7.0);
    assert!(report.plan.is_none());
    assert!(report.credits.is_none());
}

#[test]
fn endpoint_codex_without_rate_limit_is_malformed() {
    assert_eq!(codex_usage(&json!({ "plan_type": "plus" }), now()), None);
    assert_eq!(codex_usage(&json!("not an object"), now()), None);
}

#[test]
fn endpoint_grok_percent_and_period_end() {
    let body = json!({
        "config": {
            "creditUsagePercent": 33.5,
            "currentPeriod": {
                "type": "USAGE_PERIOD_TYPE_WEEKLY",
                "start": "2026-09-28T00:00:00Z",
                "end": "2026-10-05T00:00:00Z"
            }
        },
        "subscriptionTier": "tier-a"
    });
    let report = grok_billing(&body, now()).expect("well-formed");
    let period = find(&report, "period");
    assert_eq!(period.used_pct, 33.5);
    assert_eq!(period.resets_at, Some(rfc("2026-10-05T00:00:00Z")));
    assert_eq!(period.window_minutes, Some(7 * 24 * 60));
    assert_eq!(report.plan.map(|plan| plan.name), Some("tier-a".into()));
}

#[test]
fn endpoint_grok_period_without_percent_is_zero_used() {
    // Hypothesis until a live Grok read: proto3 JSON omits a zero percent.
    let body = json!({ "config": { "currentPeriod": {
        "start": "2026-09-28T00:00:00Z",
        "end": "2026-10-05T00:00:00Z"
    }}});
    let report = grok_billing(&body, now()).expect("well-formed");
    assert_eq!(find(&report, "period").used_pct, 0.0);
}

#[test]
fn endpoint_grok_legacy_shape_is_malformed() {
    let body = json!({ "monthlyLimit": 1000, "used": 250 });
    assert_eq!(grok_billing(&body, now()), None);
}

#[test]
fn endpoint_openrouter_capped_key_window_and_next_reset() {
    // 2026-09-30 is a Wednesday.
    let cases = [
        ("daily", rfc("2026-10-01T00:00:00Z")),
        ("weekly", rfc("2026-10-05T00:00:00Z")),
        ("monthly", rfc("2026-10-01T00:00:00Z")),
    ];
    for (reset, expected) in cases {
        let body = json!({ "data": {
            "label": "sk-or-TEST",
            "limit": 20.0,
            "limit_remaining": 15.0,
            "limit_reset": reset,
            "usage": 5.0
        }});
        let report = openrouter_key(&body, now()).expect("well-formed");
        let window = find(&report, reset);
        assert_eq!(window.used_pct, 25.0);
        assert_eq!(window.resets_at, Some(expected), "{reset}");
    }
    let body = json!({ "data": { "limit": 10, "limit_remaining": 10, "limit_reset": null } });
    let report = openrouter_key(&body, now()).expect("well-formed");
    let total = find(&report, "total");
    assert_eq!(total.used_pct, 0.0);
    assert_eq!(total.resets_at, None);
}

#[test]
fn endpoint_openrouter_uncapped_key_is_empty_not_unlimited() {
    let body = json!({ "data": { "limit": null, "limit_remaining": null, "usage": 5.0 } });
    let report = openrouter_key(&body, now()).expect("an uncapped key is well-formed");
    assert!(report.is_empty(), "{report:?}");
    assert_eq!(openrouter_key(&json!({ "error": "x" }), now()), None);
}

#[test]
fn endpoint_claude_percent_windows_labels_match_headers_and_nulls_skipped() {
    let body = json!({
        "five_hour": { "utilization": 12.5, "resets_at": "2026-09-30T15:00:00.000Z" },
        "seven_day": { "utilization": 40.0, "resets_at": null },
        "seven_day_opus": { "utilization": 5.0, "resets_at": "2026-10-03T00:00:00Z" },
        "seven_day_sonnet": null,
        "extra_usage": { "is_enabled": false, "utilization": 99.0 }
    });
    let report = claude_oauth_usage(&body, now()).expect("well-formed");
    assert_eq!(report.windows.len(), 3, "{report:?}");
    let five = find(&report, "5h");
    assert_eq!(five.used_pct, 12.5);
    assert_eq!(five.resets_at, Some(rfc("2026-09-30T15:00:00Z")));
    assert_eq!(find(&report, "7d").resets_at, None);
    assert_eq!(find(&report, "7d opus").used_pct, 5.0);

    let from_headers = headers(&[("anthropic-ratelimit-unified-7d_opus-utilization", "0.05")]);
    assert_eq!(from_headers.windows[0].label, "7d opus");
}

fn window(label: &str, used_pct: f64, observed_at: DateTime<Utc>) -> UsageWindow {
    UsageWindow {
        label: label.to_string(),
        window_minutes: None,
        used_pct,
        resets_at: None,
        source: UsageSource::Header,
        observed_at,
    }
}

fn report(windows: Vec<UsageWindow>) -> UsageReport {
    UsageReport {
        windows,
        ..UsageReport::default()
    }
}

fn plan_at(name: &str, observed_at: DateTime<Utc>) -> Option<UsagePlan> {
    Some(UsagePlan {
        name: name.to_string(),
        observed_at,
    })
}

#[test]
fn view_merge_newer_window_per_label_wins() {
    let earlier = now() - Duration::minutes(5);
    let stored = report(vec![
        window("primary", 10.0, earlier),
        window("secondary", 30.0, earlier),
    ]);
    let merged = stored.merge(report(vec![window("primary", 20.0, now())]));
    assert_eq!(merged.windows.len(), 2, "{merged:?}");
    assert_eq!(find(&merged, "primary").used_pct, 20.0);
    assert_eq!(find(&merged, "primary").observed_at, now());
    assert_eq!(find(&merged, "secondary").used_pct, 30.0);
}

#[test]
fn view_merge_older_report_never_replaces_newer() {
    let stored = report(vec![window("5h", 50.0, now())]);
    let late = report(vec![window("5h", 10.0, now() - Duration::minutes(1))]);
    let merged = stored.merge(late);
    assert_eq!(find(&merged, "5h").used_pct, 50.0);
    assert_eq!(merged.windows.len(), 1);
}

#[test]
fn view_merge_plan_and_credits_by_their_own_time() {
    let credits = |balance: &str, observed_at| {
        Some(UsageCredits {
            has_credits: Some(true),
            unlimited: None,
            balance: Some(balance.to_string()),
            observed_at,
        })
    };
    let newer = UsageReport {
        windows: vec![window("primary", 10.0, now() - Duration::minutes(10))],
        credits: credits("5.00", now()),
        plan: plan_at("pro", now()),
    };
    let older_plan_newer_window = UsageReport {
        windows: vec![window("primary", 20.0, now())],
        credits: credits("9.00", now() - Duration::minutes(3)),
        plan: plan_at("plus", now() - Duration::minutes(3)),
    };
    let merged = newer.clone().merge(older_plan_newer_window);
    assert_eq!(find(&merged, "primary").used_pct, 20.0);
    assert_eq!(merged.plan.as_ref().unwrap().name, "pro");
    assert_eq!(
        merged.credits.as_ref().unwrap().balance.as_deref(),
        Some("5.00")
    );

    let merged = newer.merge(report(vec![window("primary", 30.0, now())]));
    assert_eq!(
        merged.plan.unwrap().name,
        "pro",
        "a missing plan never clears one"
    );
    assert!(merged.credits.is_some(), "missing credits never clear them");
}

#[test]
fn view_header_and_endpoint_same_window_merge_into_one() {
    let codex_headers = headers(&[
        ("x-codex-primary-used-percent", "10"),
        ("x-codex-secondary-used-percent", "20"),
    ]);
    let codex_endpoint = codex_usage(
        &json!({ "rate_limit": { "primary_window": { "used_percent": 15 } } }),
        now() + Duration::seconds(1),
    )
    .unwrap();
    let merged = codex_headers.merge(codex_endpoint);
    assert_eq!(merged.windows.len(), 2, "{merged:?}");
    assert_eq!(find(&merged, "primary").used_pct, 15.0);
    assert_eq!(find(&merged, "primary").source, UsageSource::Endpoint);

    let claude_headers = headers(&[("anthropic-ratelimit-unified-5h-utilization", "0.1")]);
    let claude_endpoint = claude_oauth_usage(
        &json!({ "five_hour": { "utilization": 30.0, "resets_at": null } }),
        now() + Duration::seconds(1),
    )
    .unwrap();
    let merged = claude_headers.merge(claude_endpoint);
    assert_eq!(merged.windows.len(), 1, "{merged:?}");
    assert_eq!(find(&merged, "5h").used_pct, 30.0);
}

#[test]
fn view_fresh_until_fifteen_minutes() {
    let usage = report(vec![
        window("a", 1.0, now() - Duration::minutes(15)),
        window("b", 2.0, now()),
    ]);
    let visible = visible_windows(&usage, now());
    assert_eq!(visible.len(), 2);
    assert!(visible
        .iter()
        .all(|(_, freshness)| *freshness == Freshness::Fresh));
}

#[test]
fn view_stale_until_sixty_minutes() {
    let usage = report(vec![window("a", 1.0, now() - Duration::minutes(16))]);
    let visible = visible_windows(&usage, now());
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].1, Freshness::Stale);
}

#[test]
fn view_dropped_after_sixty_minutes() {
    let usage = report(vec![
        window("kept", 1.0, now() - Duration::minutes(59)),
        window("dropped", 2.0, now() - Duration::minutes(61)),
    ]);
    let visible = visible_windows(&usage, now());
    assert_eq!(visible.len(), 1, "{visible:?}");
    assert_eq!(visible[0].0.label, "kept");
    assert_eq!(visible[0].1, Freshness::Stale);
}

#[test]
fn view_never_past_resets_at() {
    let mut future = window("future", 1.0, now());
    future.resets_at = Some(now() + Duration::minutes(1));
    let mut past = window("past", 2.0, now());
    past.resets_at = Some(now() - Duration::seconds(1));
    let usage = report(vec![future, past]);
    let visible = visible_windows(&usage, now());
    assert_eq!(visible.len(), 1, "{visible:?}");
    assert_eq!(visible[0].0.label, "future");
}

#[test]
fn view_same_values_ignores_observed_at() {
    let first = UsageReport {
        windows: vec![window("primary", 10.0, now())],
        credits: None,
        plan: plan_at("plus", now()),
    };
    let mut later = first.clone();
    later.windows[0].observed_at = now() + Duration::seconds(30);
    later.plan = plan_at("plus", now() + Duration::seconds(30));
    assert!(first.same_values(&later));

    let mut changed = later.clone();
    changed.windows[0].used_pct = 11.0;
    assert!(!first.same_values(&changed));
    let mut changed = later.clone();
    changed.plan = plan_at("pro", now());
    assert!(!first.same_values(&changed));
    let mut changed = later;
    changed.credits = Some(UsageCredits {
        has_credits: Some(false),
        unlimited: None,
        balance: None,
        observed_at: now(),
    });
    assert!(!first.same_values(&changed));
}
