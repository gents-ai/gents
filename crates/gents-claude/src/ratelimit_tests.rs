use super::*;
use crate::rotation::{observe, tier, Account, Family, Policy, Tier};
use rapidhash::{HashMapExt, RapidHashMap};

const NOW: u64 = 1000;
const RESET: u64 = 1000 + 18_000;

fn obs(pairs: &[(&str, &str)]) -> HeaderObservation {
    let mut map: RapidHashMap<String, String> = RapidHashMap::new();
    for (k, v) in pairs {
        map.insert(format!("anthropic-ratelimit-unified-{k}"), (*v).to_owned());
    }
    observe_headers(|name| map.get(name).map(String::as_str), NOW)
}

fn fresh() -> Account {
    Account {
        enabled: true,
        limits: vec![],
        overage: Overage::Absent,
        blocked_until: 0,
    }
}

fn tier_after(o: &HeaderObservation, family: Family, allow_overage: bool) -> Option<Tier> {
    let mut a = fresh();
    observe(family, &o.limits(NOW), o.overage, &mut a);
    tier(&Policy { allow_overage }, NOW, family, &a)
}

fn lim(claim: Claim, severity: Severity, reset_at: u64) -> Limit {
    Limit {
        claim,
        severity,
        reset_at,
    }
}

#[test]
fn normal_has_no_limits() {
    let o = obs(&[("status", "allowed"), ("reset", "19000")]);
    assert!(!o.is_empty());
    assert!(o.limits(NOW).is_empty());
    assert_eq!(o.overage, Overage::Absent);
    assert_eq!(tier_after(&o, Family::Other, false), Some(Tier::Clear));
}

#[test]
fn no_headers_is_empty_and_inert() {
    let o = obs(&[]);
    assert!(o.is_empty());
    assert!(o.limits(NOW).is_empty());
}

#[test]
fn session_limit_reached() {
    let o = obs(&[
        ("status", "rejected"),
        ("representative-claim", "five_hour"),
        ("reset", "5000"),
    ]);
    assert_eq!(
        o.limits(NOW),
        [lim(Claim::FiveHour, Severity::Rejected, 5000)]
    );
    assert_eq!(tier_after(&o, Family::Sonnet, true), None);
}

#[test]
fn approaching_weekly_limit() {
    let o = obs(&[
        ("status", "allowed_warning"),
        ("representative-claim", "seven_day"),
        ("reset", "9000"),
    ]);
    assert_eq!(
        o.limits(NOW),
        [lim(Claim::SevenDay, Severity::Warning, 9000)]
    );
    assert_eq!(tier_after(&o, Family::Other, false), Some(Tier::Warned));
}

#[test]
fn weekly_limit_reached_defaults_reset_to_the_period() {
    let o = obs(&[
        ("status", "rejected"),
        ("representative-claim", "seven_day"),
    ]);
    assert_eq!(
        o.limits(NOW),
        [lim(Claim::SevenDay, Severity::Rejected, NOW + 604_800)]
    );
    assert_eq!(tier_after(&o, Family::Other, false), None);
}

#[test]
fn overage_scenarios() {
    for (status, overage, serves) in [
        ("allowed", Overage::Allowed, true),
        ("allowed_warning", Overage::Warning, true),
        ("rejected", Overage::Rejected, false),
    ] {
        let o = obs(&[
            ("status", "rejected"),
            ("representative-claim", "five_hour"),
            ("reset", "5000"),
            ("overage-status", status),
            ("overage-reset", "7000"),
        ]);
        assert_eq!(o.overage, overage);
        assert_eq!(o.overage_reset, Some(7000));
        assert_eq!(tier_after(&o, Family::Other, false), None);
        let expect = serves.then_some(Tier::Overage);
        assert_eq!(tier_after(&o, Family::Other, true), expect, "{status}");
    }
}

#[test]
fn overage_reason_and_in_use_are_read() {
    let o = obs(&[
        ("overage-disabled-reason", "out_of_credits"),
        ("overage-in-use", "true"),
    ]);
    assert_eq!(o.overage_disabled_reason.as_deref(), Some("out_of_credits"));
    assert_eq!(o.overage_in_use, Some(true));
}

#[test]
fn opus_limit_binds_only_opus() {
    let o = obs(&[
        ("status", "rejected"),
        ("representative-claim", "seven_day_opus"),
        ("reset", "5000"),
    ]);
    assert_eq!(tier_after(&o, Family::Opus, false), None);
    let mut a = fresh();
    observe(Family::Opus, &o.limits(NOW), o.overage, &mut a);
    assert_eq!(
        tier(&Policy::default(), NOW, Family::Sonnet, &a),
        Some(Tier::Clear)
    );
}

#[test]
fn sonnet_warning() {
    let o = obs(&[
        ("status", "allowed_warning"),
        ("representative-claim", "seven_day_sonnet"),
        ("reset", "5000"),
    ]);
    assert_eq!(tier_after(&o, Family::Sonnet, false), Some(Tier::Warned));
}

#[test]
fn per_window_warning_and_threshold() {
    let o = obs(&[
        ("5h-status", "allowed_warning"),
        ("5h-reset", "4000"),
        ("5h-utilization", "0.9"),
        ("5h-surpassed-threshold", "0.9"),
        ("7d-surpassed-threshold", "0.75"),
        ("7d_opus-status", "rejected"),
    ]);
    assert_eq!(o.windows.len(), 3);
    assert_eq!(o.windows[0].utilization, Some(0.9));
    assert_eq!(
        o.limits(NOW),
        [
            lim(Claim::FiveHour, Severity::Warning, 4000),
            lim(Claim::SevenDay, Severity::Warning, NOW + 604_800),
            lim(Claim::SevenDayOpus, Severity::Rejected, NOW + 604_800),
        ]
    );
    assert_eq!(tier_after(&o, Family::Sonnet, false), Some(Tier::Warned));
    assert_eq!(tier_after(&o, Family::Opus, false), None);
}

#[test]
fn allowed_window_without_threshold_adds_nothing() {
    let o = obs(&[("5h-status", "allowed"), ("5h-utilization", "0.1")]);
    assert!(o.limits(NOW).is_empty());
}

#[test]
fn malformed_numbers_are_ignored() {
    let o = obs(&[
        ("status", "rejected"),
        ("representative-claim", "five_hour"),
        ("reset", "soon"),
        ("5h-utilization", "NaN"),
        ("7d-surpassed-threshold", "high"),
        ("overage-reset", "-1"),
    ]);
    assert_eq!(o.reset, None);
    assert_eq!(o.overage_reset, None);
    assert!(o.windows.is_empty());
    assert_eq!(
        o.limits(NOW),
        [lim(Claim::FiveHour, Severity::Rejected, RESET)]
    );
}

#[test]
fn unknown_claim_binds_every_family_and_defaults_to_five_hours() {
    let o = obs(&[("status", "rejected"), ("representative-claim", "mystery")]);
    assert_eq!(
        o.limits(NOW),
        [lim(Claim::Unknown, Severity::Rejected, RESET)]
    );
    assert_eq!(tier_after(&o, Family::Opus, false), None);
    let o = obs(&[("status", "rejected")]);
    assert_eq!(o.claim, None);
    assert_eq!(
        o.limits(NOW),
        [lim(Claim::Unknown, Severity::Rejected, RESET)]
    );
}

#[test]
fn duplicates_keep_the_later_reset() {
    let o = obs(&[
        ("status", "allowed_warning"),
        ("representative-claim", "five_hour"),
        ("reset", "3000"),
        ("5h-status", "allowed_warning"),
        ("5h-reset", "4000"),
    ]);
    assert_eq!(
        o.limits(NOW),
        [lim(Claim::FiveHour, Severity::Warning, 4000)]
    );
}
