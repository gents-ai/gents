use super::*;
use gents_lean_vocab::families;
use gents_lean_vocab::family::recode;

#[test]
fn generated_rotation_select_cases_choose_the_account() {
    families::account_rotation_contracts_selection::replay::<Claim, Family, Overage, Severity>(
        |i| {
            let pool: Vec<Account> = recode(&i.pool);
            select(
                &recode(&i.policy),
                i.now,
                i.family,
                &pool,
                i.current.map(|current| current as usize),
                i.cursor as usize,
            )
            .map(|account| account as u64)
        },
    );
}

#[test]
fn generated_rotation_attempt_cases_walk_the_pool() {
    families::account_rotation_contracts_attempts::replay::<Claim, Family, Overage, Severity>(
        |i| {
            let mut pool: Vec<Account> = recode(&i.pool);
            attempt(
                &recode(&i.policy),
                i.now,
                i.family,
                &mut pool,
                i.current.map(|current| current as usize),
                i.cursor as usize,
                |account| {
                    i.rejects
                        .iter()
                        .find(|reject| reject.account as usize == account)
                        .map(|reject| reject.reset_at)
                },
            )
            .into_iter()
            .map(|account| account as u64)
            .collect()
        },
    );
}

#[test]
fn generated_rotation_observe_cases_fold_limits() {
    families::account_rotation_contracts_observation::replay::<Claim, Family, Overage, Severity>(
        |i| {
            let mut account: Account = recode(&i.account);
            let reported: Vec<Limit> = recode(&i.reported);
            observe(i.family, &reported, i.overage, &mut account);
            recode(&account)
        },
    );
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
