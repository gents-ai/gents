use super::*;
use crate::rotation::{Claim, Severity};
use gents_lean_vocab::families;
use gents_lean_vocab::family::recode;
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

const _: fn() = assert_send_sync::<AccountPool<String>>;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum PoolOp {
    Choose,
    Observe,
    Reject,
    EarliestReset,
    ForgetAccount,
    ForgetSession,
}

#[test]
fn cursor_action_matches_lean() {
    families::account_rotation_cursor_action::replay(|i| cursor_action(i.has_current));
}

#[test]
fn session_map_action_matches_lean() {
    families::account_rotation_session_map_action::replay(|i| {
        session_map_action(i.switched, i.recorded, i.cap)
    });
}

#[test]
fn generated_pool_trace_cases_match_runtime_pool() {
    use families::account_rotation_contracts_pool_trace::{PoolChoice, PoolStepResult};

    families::account_rotation_contracts_pool_trace::replay::<
        Claim,
        Family,
        Overage,
        PoolOp,
        Severity,
    >(|i| {
        let pool = AccountPool::<String>::new(i.session_cap as usize);
        i.steps
            .iter()
            .map(|step| match step.op {
                PoolOp::Choose => {
                    let policy: Policy = recode(&step.policy);
                    let choice = pool
                        .choose(
                            &step.accounts,
                            &step.session,
                            step.family,
                            step.now,
                            &policy,
                        )
                        .map(|c| PoolChoice {
                            key: c.key,
                            index: c.index as u64,
                            switched: c.switched,
                        });
                    PoolStepResult {
                        choice,
                        earliest_reset: None,
                        cursor: pool.cursor.load(Ordering::Relaxed) as u64,
                        session_count: pool.sessions.len() as u64,
                        recorded_session_account: pool.sessions.get(&step.session),
                    }
                }
                PoolOp::Observe => {
                    let reported: Vec<Limit> = recode(&step.reported);
                    pool.observe(&step.account, step.family, &reported, step.overage);
                    PoolStepResult {
                        choice: None,
                        earliest_reset: None,
                        cursor: pool.cursor.load(Ordering::Relaxed) as u64,
                        session_count: pool.sessions.len() as u64,
                        recorded_session_account: None,
                    }
                }
                PoolOp::Reject => {
                    pool.reject(&step.account, step.now, step.reset_at);
                    PoolStepResult {
                        choice: None,
                        earliest_reset: None,
                        cursor: pool.cursor.load(Ordering::Relaxed) as u64,
                        session_count: pool.sessions.len() as u64,
                        recorded_session_account: None,
                    }
                }
                PoolOp::EarliestReset => {
                    let policy: Policy = recode(&step.policy);
                    let earliest_reset =
                        pool.earliest_reset(&step.accounts, step.family, step.now, &policy);
                    PoolStepResult {
                        choice: None,
                        earliest_reset,
                        cursor: pool.cursor.load(Ordering::Relaxed) as u64,
                        session_count: pool.sessions.len() as u64,
                        recorded_session_account: None,
                    }
                }
                PoolOp::ForgetAccount => {
                    pool.forget_account(&step.account);
                    PoolStepResult {
                        choice: None,
                        earliest_reset: None,
                        cursor: pool.cursor.load(Ordering::Relaxed) as u64,
                        session_count: pool.sessions.len() as u64,
                        recorded_session_account: None,
                    }
                }
                PoolOp::ForgetSession => {
                    pool.forget_session(&step.session);
                    PoolStepResult {
                        choice: None,
                        earliest_reset: None,
                        cursor: pool.cursor.load(Ordering::Relaxed) as u64,
                        session_count: pool.sessions.len() as u64,
                        recorded_session_account: pool.sessions.get(&step.session),
                    }
                }
            })
            .collect()
    });
}
