use super::*;
use crate::rotation::Claim;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

const NOW: u64 = 100;
const FAR: u64 = 1_000_000;

fn keys(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("a{i}")).collect()
}

fn pool() -> AccountPool<String> {
    AccountPool::new(DEFAULT_SESSION_CAP)
}

fn limit(claim: Claim, severity: Severity, reset_at: u64) -> Limit {
    Limit {
        claim,
        severity,
        reset_at,
    }
}

fn pick(p: &AccountPool<String>, a: &[String], s: &str) -> Choice<String> {
    p.choose(a, s, Family::Sonnet, NOW, &Policy::default())
        .expect("eligible")
}

#[test]
fn pool_is_send_sync() {
    assert_send_sync::<AccountPool<String>>();
}

#[test]
fn round_robin_and_stickiness() {
    let (p, a) = (pool(), keys(3));
    let got: Vec<usize> = ["s1", "s2", "s3", "s4"]
        .iter()
        .map(|s| pick(&p, &a, s).index)
        .collect();
    assert_eq!(got, [0, 1, 2, 0]);
    for (s, i) in [("s1", 0), ("s2", 1), ("s3", 2), ("s4", 0)] {
        let c = pick(&p, &a, s);
        assert_eq!((c.index, c.switched), (i, false));
    }
}

#[test]
fn warning_rotates_only_that_session() {
    let (p, a) = (pool(), keys(3));
    for s in ["s1", "s2", "s3"] {
        pick(&p, &a, s);
    }
    p.observe(
        &a[0],
        Family::Sonnet,
        &[limit(Claim::FiveHour, Severity::Warning, FAR)],
        Overage::Absent,
    );
    let c = pick(&p, &a, "s1");
    assert_eq!((c.index, c.switched), (1, true));
    assert!(!pick(&p, &a, "s2").switched);
    assert!(!pick(&p, &a, "s3").switched);
}

#[test]
fn rejection_hops_then_exhausts() {
    let (p, a) = (pool(), keys(3));
    let c = pick(&p, &a, "s");
    p.reject(&c.key, NOW, 500);
    let c2 = pick(&p, &a, "s");
    assert!(c2.switched && c2.key != c.key);
    p.reject(&c2.key, NOW, 300);
    let c3 = pick(&p, &a, "s");
    p.reject(&c3.key, NOW, 700);
    assert!(p
        .choose(&a, "s", Family::Sonnet, NOW, &Policy::default())
        .is_none());
    assert_eq!(
        p.earliest_reset(&a, Family::Sonnet, NOW, &Policy::default()),
        Some(300)
    );
    assert_eq!(
        p.earliest_reset(&a, Family::Sonnet, 300, &Policy::default()),
        Some(300)
    );
    assert!(p
        .choose(&a, "s", Family::Sonnet, 300, &Policy::default())
        .is_some());
}

#[test]
fn earliest_reset_uses_rejected_limits_and_now_when_free() {
    let (p, a) = (pool(), keys(2));
    assert_eq!(
        p.earliest_reset(&a, Family::Sonnet, NOW, &Policy::default()),
        Some(NOW)
    );
    assert_eq!(
        p.earliest_reset(&[], Family::Sonnet, NOW, &Policy::default()),
        None
    );
    p.observe(
        &a[0],
        Family::Sonnet,
        &[limit(Claim::FiveHour, Severity::Rejected, 900)],
        Overage::Absent,
    );
    p.observe(
        &a[1],
        Family::Sonnet,
        &[limit(Claim::SevenDay, Severity::Rejected, 800)],
        Overage::Absent,
    );
    assert_eq!(
        p.earliest_reset(&a, Family::Sonnet, NOW, &Policy::default()),
        Some(800)
    );
}

#[test]
fn overage_only_when_policy_allows() {
    let (p, a) = (pool(), keys(2));
    for k in &a {
        p.observe(
            k,
            Family::Sonnet,
            &[limit(Claim::FiveHour, Severity::Rejected, FAR)],
            Overage::Allowed,
        );
    }
    let allow = Policy {
        allow_overage: true,
    };
    assert!(p
        .choose(&a, "s", Family::Sonnet, NOW, &Policy::default())
        .is_none());
    assert!(p.choose(&a, "s", Family::Sonnet, NOW, &allow).is_some());
    assert_eq!(p.earliest_reset(&a, Family::Sonnet, NOW, &allow), Some(NOW));
    assert_eq!(
        p.earliest_reset(&a, Family::Sonnet, NOW, &Policy::default()),
        Some(FAR)
    );
}

#[test]
fn default_and_debug_report_counts_only() {
    let p = AccountPool::<String>::default();
    pick(&p, &keys(2), "s");
    assert_eq!(format!("{p:?}"), "AccountPool { accounts: 0, sessions: 1 }");
}

#[test]
fn limits_are_family_scoped() {
    let (p, a) = (pool(), keys(2));
    let c = pick(&p, &a, "s");
    p.observe(
        &c.key,
        Family::Opus,
        &[limit(Claim::SevenDayOpus, Severity::Rejected, FAR)],
        Overage::Absent,
    );
    let sonnet = p
        .choose(&a, "s", Family::Sonnet, NOW, &Policy::default())
        .expect("eligible");
    assert!(!sonnet.switched);
    let opus = p
        .choose(&a, "s", Family::Opus, NOW, &Policy::default())
        .expect("eligible");
    assert!(opus.switched && opus.key != c.key);
}

#[test]
fn removed_account_gets_fresh_choice() {
    let (p, a) = (pool(), keys(3));
    let c = pick(&p, &a, "s");
    let rest: Vec<String> = a.iter().filter(|k| **k != c.key).cloned().collect();
    let c2 = pick(&p, &rest, "s");
    assert!(c2.switched && rest.contains(&c2.key));
}

#[test]
fn session_cap_clears_and_still_answers() {
    let (p, a) = (AccountPool::<String>::new(4), keys(3));
    for i in 0..50 {
        let c = pick(&p, &a, &format!("s{i}"));
        assert!(a.contains(&c.key));
        assert!(p.sessions.len() <= 4);
    }
}

#[test]
fn headerless_response_changes_nothing_and_headers_apply() {
    let (p, a) = (pool(), keys(1));
    p.observe_headers(&a[0], Family::Sonnet, |_| None, NOW);
    assert!(p.account_state(&a[0]).is_none());
    p.observe_headers(
        &a[0],
        Family::Sonnet,
        |n| match n {
            "anthropic-ratelimit-unified-status" => Some("rejected"),
            "anthropic-ratelimit-unified-reset" => Some("900"),
            "anthropic-ratelimit-unified-representative-claim" => Some("five_hour"),
            _ => None,
        },
        NOW,
    );
    assert!(p.account_state(&a[0]).is_some());
    assert_eq!(
        p.earliest_reset(&a, Family::Sonnet, NOW, &Policy::default()),
        Some(900)
    );
}

#[test]
fn forget_drops_state() {
    let (p, a) = (pool(), keys(2));
    let c = pick(&p, &a, "s");
    p.reject(&c.key, NOW, 500);
    p.forget_account(&c.key);
    assert!(p.account_state(&c.key).is_none());
    p.forget_session("s");
    assert_eq!(
        pick(&p, &a, "s").key,
        a[p.cursor.load(Ordering::Relaxed) - 1]
    );
}

#[test]
fn concurrent_choices_never_return_dead_accounts() {
    let (p, a) = (pool(), keys(8));
    let dead = &a[..3];
    for k in dead {
        p.reject(k, 0, u64::MAX);
    }
    std::thread::scope(|scope| {
        for t in 0..8u64 {
            let (p, a) = (&p, &a);
            scope.spawn(move || {
                let mut rng = StdRng::seed_from_u64(t);
                for _ in 0..2000 {
                    let k = &a[3 + rng.random_range(0..5)];
                    match rng.random_range(0..3) {
                        0 => p.observe(
                            k,
                            Family::Sonnet,
                            &[limit(Claim::FiveHour, Severity::Warning, FAR)],
                            Overage::Absent,
                        ),
                        1 => p.reject(k, 0, 0),
                        _ => {}
                    }
                    let s = format!("s{}", rng.random_range(0..64));
                    let c = p
                        .choose(a, &s, Family::Sonnet, NOW, &Policy::default())
                        .expect("live accounts");
                    assert!(a.contains(&c.key));
                    assert!(c.index >= 3, "returned permanently rejected {}", c.key);
                }
            });
        }
    });
}

#[test]
fn random_sequences_match_policy() {
    let mut rng = StdRng::seed_from_u64(7);
    let (p, a) = (pool(), keys(4));
    for _ in 0..2000 {
        let k = &a[rng.random_range(0..4)];
        if rng.random_bool(0.3) {
            let sev = if rng.random_bool(0.5) {
                Severity::Warning
            } else {
                Severity::Rejected
            };
            p.observe(
                k,
                Family::Sonnet,
                &[limit(Claim::FiveHour, sev, NOW + rng.random_range(0..200))],
                Overage::Absent,
            );
        }
        let now = NOW + rng.random_range(0..200);
        let got = p.choose(&a, "s", Family::Sonnet, now, &Policy::default());
        let tiers: Vec<_> = a
            .iter()
            .map(|k| {
                let acc = p
                    .account_state(k)
                    .map_or_else(fresh, |x| Account::clone(&x));
                rotation::tier(&Policy::default(), now, Family::Sonnet, &acc)
            })
            .collect();
        match got {
            None => assert!(tiers.iter().all(Option::is_none)),
            Some(c) => assert_eq!(
                tiers[c.index].map(rotation::Tier::rank),
                tiers.iter().flatten().map(|t| t.rank()).min()
            ),
        }
    }
}
