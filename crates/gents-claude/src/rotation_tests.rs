use super::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::Value;

fn family(v: &Value) -> Family {
    match v.as_str().unwrap() {
        "opus" => Family::Opus,
        "sonnet" => Family::Sonnet,
        "other" => Family::Other,
        other => panic!("family {other}"),
    }
}

fn overage(v: &Value) -> Overage {
    match v.as_str().unwrap() {
        "absent" => Overage::Absent,
        "allowed" => Overage::Allowed,
        "allowed_warning" => Overage::Warning,
        "rejected" => Overage::Rejected,
        other => panic!("overage {other}"),
    }
}

fn limit(v: &Value) -> Limit {
    let claim = Claim::parse(v["claim"].as_str().unwrap());
    assert_ne!(claim, Claim::Unknown);
    Limit {
        claim,
        severity: match v["severity"].as_str().unwrap() {
            "allowed_warning" => Severity::Warning,
            "rejected" => Severity::Rejected,
            other => panic!("severity {other}"),
        },
        reset_at: v["reset_at"].as_u64().unwrap(),
    }
}

fn account(v: &Value) -> Account {
    Account {
        enabled: v["enabled"].as_bool().unwrap(),
        limits: v["limits"].as_array().unwrap().iter().map(limit).collect(),
        overage: overage(&v["overage"]),
        blocked_until: v["blocked_until"].as_u64().unwrap(),
    }
}

fn pool(v: &Value) -> Vec<Account> {
    v["pool"].as_array().unwrap().iter().map(account).collect()
}

fn current(v: &Value) -> Option<usize> {
    v["current"].as_u64().map(|c| c as usize)
}

fn cases(key: &str) -> Vec<Value> {
    let contract: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let list = contract[key].as_array().unwrap().clone();
    assert!(!list.is_empty(), "{key} is empty");
    list
}

#[test]
fn generated_rotation_select_cases_choose_the_account() {
    for c in cases("account_rotation_select_cases") {
        let policy = Policy {
            allow_overage: c["allow_overage"].as_bool().unwrap(),
        };
        let got = select(
            &policy,
            c["now"].as_u64().unwrap(),
            family(&c["family"]),
            &pool(&c),
            current(&c),
            c["cursor"].as_u64().unwrap() as usize,
        );
        assert_eq!(
            got,
            c["expected"].as_u64().map(|e| e as usize),
            "{}",
            c["name"]
        );
    }
}

#[test]
fn generated_rotation_attempt_cases_walk_the_pool() {
    for c in cases("account_rotation_attempt_cases") {
        let policy = Policy {
            allow_overage: c["allow_overage"].as_bool().unwrap(),
        };
        let rejects: Vec<(usize, u64)> = c["rejects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["account"].as_u64().unwrap() as usize,
                    r["reset_at"].as_u64().unwrap(),
                )
            })
            .collect();
        let mut accounts = pool(&c);
        let got = attempt(
            &policy,
            c["now"].as_u64().unwrap(),
            family(&c["family"]),
            &mut accounts,
            current(&c),
            c["cursor"].as_u64().unwrap() as usize,
            |i| rejects.iter().find(|(a, _)| *a == i).map(|(_, r)| *r),
        );
        let expected: Vec<usize> = c["expected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_u64().unwrap() as usize)
            .collect();
        assert_eq!(got, expected, "{}", c["name"]);
    }
}

#[test]
fn generated_rotation_observe_cases_fold_limits() {
    for c in cases("account_rotation_observe_cases") {
        let mut acc = account(&c["account"]);
        let reported: Vec<Limit> = c["reported"]
            .as_array()
            .unwrap()
            .iter()
            .map(limit)
            .collect();
        observe(
            family(&c["family"]),
            &reported,
            overage(&c["overage"]),
            &mut acc,
        );
        assert_eq!(acc, account(&c["expected"]), "{}", c["name"]);
    }
}

const NOW: u64 = 1000;

fn random_account(rng: &mut StdRng) -> Account {
    let claims = [
        Claim::FiveHour,
        Claim::SevenDay,
        Claim::SevenDayOpus,
        Claim::SevenDaySonnet,
        Claim::Unknown,
    ];
    let limits = (0..rng.random_range(0..4))
        .map(|_| Limit {
            claim: claims[rng.random_range(0..claims.len())],
            severity: if rng.random_bool(0.5) {
                Severity::Warning
            } else {
                Severity::Rejected
            },
            reset_at: rng.random_range(900..1100),
        })
        .collect();
    Account {
        enabled: rng.random_bool(0.85),
        limits,
        overage: [
            Overage::Absent,
            Overage::Allowed,
            Overage::Warning,
            Overage::Rejected,
        ][rng.random_range(0..4)],
        blocked_until: if rng.random_bool(0.2) {
            rng.random_range(900..1100)
        } else {
            0
        },
    }
}

fn random_case(rng: &mut StdRng) -> (Policy, Family, Vec<Account>, Option<usize>, usize) {
    let n = rng.random_range(0..7);
    let pool = (0..n).map(|_| random_account(rng)).collect();
    let family = [Family::Opus, Family::Sonnet, Family::Other][rng.random_range(0..3)];
    let current = rng.random_bool(0.6).then(|| rng.random_range(0..8));
    let policy = Policy {
        allow_overage: rng.random_bool(0.5),
    };
    (policy, family, pool, current, rng.random_range(0..9))
}

#[test]
fn select_properties_hold_over_random_pools() {
    let mut rng = StdRng::seed_from_u64(0xA11CE);
    for _ in 0..5000 {
        let (policy, family, pool, current, cursor) = random_case(&mut rng);
        let tiers: Vec<Option<Tier>> = pool.iter().map(|a| tier(&policy, NOW, family, a)).collect();
        let best = tiers.iter().flatten().map(|t| t.rank()).min();
        let got = select(&policy, NOW, family, &pool, current, cursor);
        assert_eq!(got.is_some(), best.is_some(), "liveness");
        let Some(i) = got else { continue };
        let t = tiers[i].expect("eligible");
        assert_eq!(Some(t.rank()), best, "best tier");
        if !policy.allow_overage {
            assert_ne!(t, Tier::Overage);
        }
        if let Some(c) = current {
            if tiers.get(c).copied().flatten().map(|t| t.rank()) == best {
                assert_eq!(i, c, "sticky");
            }
        }
    }
}

#[test]
fn attempt_never_repeats_and_is_bounded() {
    let mut rng = StdRng::seed_from_u64(0xBEEF);
    for _ in 0..3000 {
        let (policy, family, mut pool, current, cursor) = random_case(&mut rng);
        let n = pool.len();
        let sent = attempt(&policy, NOW, family, &mut pool, current, cursor, |_| {
            rng.random_bool(0.7).then_some(NOW + 500)
        });
        assert!(sent.len() <= n);
        let mut seen = sent.clone();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), sent.len(), "no repeats");
    }
}

#[test]
fn family_and_claim_spellings() {
    assert_eq!(Family::of_model("claude-OPUS-4"), Family::Opus);
    assert_eq!(Family::of_model("claude-sonnet-4-5"), Family::Sonnet);
    assert_eq!(Family::of_model("claude-haiku-4-5"), Family::Other);
    assert_eq!(Claim::parse("seven_day_opus"), Claim::SevenDayOpus);
    assert_eq!(Claim::parse("bogus"), Claim::Unknown);
    assert!(Claim::Unknown.binds(Family::Other));
    assert!(!Claim::SevenDayOpus.binds(Family::Sonnet));
}

#[test]
fn reject_blocks_until_next_tick_at_least() {
    let mut a = Account {
        enabled: true,
        limits: vec![],
        overage: Overage::Absent,
        blocked_until: 5,
    };
    reject(10, 0, &mut a);
    assert_eq!(a.blocked_until, 11);
    reject(10, 50, &mut a);
    assert_eq!(a.blocked_until, 50);
    reject(u64::MAX, 0, &mut a);
    assert_eq!(a.blocked_until, u64::MAX);
}
