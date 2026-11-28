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
fn header_observations_and_tiers() {
    let empty = obs(&[]);
    assert!(empty.is_empty() && empty.limits(NOW).is_empty());

    let normal = obs(&[("status", "allowed"), ("reset", "19000")]);
    assert!(
        !normal.is_empty() && normal.limits(NOW).is_empty() && normal.overage == Overage::Absent
    );
    assert_eq!(tier_after(&normal, Family::Other, false), Some(Tier::Clear));

    let allowed_window = obs(&[("5h-status", "allowed"), ("5h-utilization", "0.1")]);
    assert!(allowed_window.limits(NOW).is_empty());

    let overage_meta = obs(&[
        ("overage-disabled-reason", "out_of_credits"),
        ("overage-in-use", "true"),
    ]);
    assert_eq!(
        overage_meta.overage_disabled_reason.as_deref(),
        Some("out_of_credits")
    );
    assert_eq!(overage_meta.overage_in_use, Some(true));

    type HeaderCase<'a> = (
        &'static str,
        &'a [(&'a str, &'a str)],
        &'a [Limit],
        Family,
        bool,
        Option<Tier>,
    );
    let cases: &[HeaderCase<'_>] = &[
        (
            "session_limit_reached",
            &[
                ("status", "rejected"),
                ("representative-claim", "five_hour"),
                ("reset", "5000"),
            ],
            &[lim(Claim::FiveHour, Severity::Rejected, 5000)],
            Family::Sonnet,
            true,
            None,
        ),
        (
            "approaching_weekly_limit",
            &[
                ("status", "allowed_warning"),
                ("representative-claim", "seven_day"),
                ("reset", "9000"),
            ],
            &[lim(Claim::SevenDay, Severity::Warning, 9000)],
            Family::Other,
            false,
            Some(Tier::Warned),
        ),
        (
            "weekly_limit_defaults_reset",
            &[
                ("status", "rejected"),
                ("representative-claim", "seven_day"),
            ],
            &[lim(Claim::SevenDay, Severity::Rejected, NOW + 604_800)],
            Family::Other,
            false,
            None,
        ),
        (
            "sonnet_warning",
            &[
                ("status", "allowed_warning"),
                ("representative-claim", "seven_day_sonnet"),
                ("reset", "5000"),
            ],
            &[lim(Claim::SevenDaySonnet, Severity::Warning, 5000)],
            Family::Sonnet,
            false,
            Some(Tier::Warned),
        ),
        (
            "unknown_claim_defaults_five_hours",
            &[("status", "rejected"), ("representative-claim", "mystery")],
            &[lim(Claim::Unknown, Severity::Rejected, RESET)],
            Family::Opus,
            false,
            None,
        ),
        (
            "missing_claim_defaults_unknown",
            &[("status", "rejected")],
            &[lim(Claim::Unknown, Severity::Rejected, RESET)],
            Family::Opus,
            false,
            None,
        ),
        (
            "duplicates_keep_later_reset",
            &[
                ("status", "allowed_warning"),
                ("representative-claim", "five_hour"),
                ("reset", "3000"),
                ("5h-status", "allowed_warning"),
                ("5h-reset", "4000"),
            ],
            &[lim(Claim::FiveHour, Severity::Warning, 4000)],
            Family::Sonnet,
            false,
            Some(Tier::Warned),
        ),
    ];
    for &(name, headers, expected_limits, family, allow_overage, expected_tier) in cases {
        let o = obs(headers);
        assert_eq!(o.limits(NOW), expected_limits, "{name}");
        assert_eq!(
            tier_after(&o, family, allow_overage),
            expected_tier,
            "{name}"
        );
    }

    let opus_only = obs(&[
        ("status", "rejected"),
        ("representative-claim", "seven_day_opus"),
        ("reset", "5000"),
    ]);
    assert_eq!(tier_after(&opus_only, Family::Opus, false), None);
    let mut a = fresh();
    observe(
        Family::Opus,
        &opus_only.limits(NOW),
        opus_only.overage,
        &mut a,
    );
    assert_eq!(
        tier(&Policy::default(), NOW, Family::Sonnet, &a),
        Some(Tier::Clear)
    );
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
fn per_window_warning_threshold_and_malformed_numbers() {
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

    let bad = obs(&[
        ("status", "rejected"),
        ("representative-claim", "five_hour"),
        ("reset", "soon"),
        ("5h-utilization", "NaN"),
        ("7d-surpassed-threshold", "high"),
        ("overage-reset", "-1"),
    ]);
    assert_eq!(bad.reset, None);
    assert_eq!(bad.overage_reset, None);
    assert!(bad.windows.is_empty());
    assert_eq!(
        bad.limits(NOW),
        [lim(Claim::FiveHour, Severity::Rejected, RESET)]
    );
}
