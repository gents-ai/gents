//! Account-aware rotation over a pool of Claude subscription accounts.
//! Lean: `AccountRotation`.
//!
//! A port of `Proofs/AccountRotation/Policy.lean`, bound by its generated
//! conformance cases. Invariants: `select` never returns a disabled, blocked or
//! limit-exhausted account, returns `Some` whenever any account is eligible,
//! prefers the best tier, and keeps a session on its account while that account
//! holds the best tier. `select` is O(pool x limits) and allocates nothing.

/// Model family a request is made for; limits bind per family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// Opus models.
    Opus,
    /// Sonnet models.
    Sonnet,
    /// Any other model; bound only by account-wide claims.
    Other,
}

impl Family {
    /// Family of a model id, by case-insensitive substring.
    pub fn of_model(model: &str) -> Family {
        let lower = model.to_ascii_lowercase();
        if lower.contains("opus") {
            Family::Opus
        } else if lower.contains("sonnet") {
            Family::Sonnet
        } else {
            Family::Other
        }
    }
}

/// The window a limit applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    /// The five hour session window.
    FiveHour,
    /// The seven day window.
    SevenDay,
    /// The seven day Opus window.
    SevenDayOpus,
    /// The seven day Sonnet window.
    SevenDaySonnet,
    /// A claim this client does not recognise; binds every family.
    Unknown,
}

impl Claim {
    /// Whether a limit on this claim constrains requests of `family`.
    pub fn binds(self, family: Family) -> bool {
        match self {
            Claim::SevenDayOpus => family == Family::Opus,
            Claim::SevenDaySonnet => family == Family::Sonnet,
            _ => true,
        }
    }

    /// Parse a `representative-claim` header spelling; anything else is `Unknown`.
    pub fn parse(value: &str) -> Claim {
        match value {
            "five_hour" => Claim::FiveHour,
            "seven_day" => Claim::SevenDay,
            "seven_day_opus" => Claim::SevenDayOpus,
            "seven_day_sonnet" => Claim::SevenDaySonnet,
            _ => Claim::Unknown,
        }
    }
}

/// How severe a reported limit is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// Past the early-warning threshold but still serving.
    Warning,
    /// Exhausted.
    Rejected,
}

/// One reported limit, live until `reset_at` (epoch seconds).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit {
    /// The window it applies to.
    pub claim: Claim,
    /// Its severity.
    pub severity: Severity,
    /// Epoch seconds at which the window resets.
    pub reset_at: u64,
}

/// State of paid extra usage on an account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overage {
    /// Not reported.
    Absent,
    /// Available.
    Allowed,
    /// Available and past its early warning.
    Warning,
    /// Exhausted.
    Rejected,
}

impl Overage {
    /// Whether paid extra usage can still serve requests.
    pub fn serves(self) -> bool {
        matches!(self, Overage::Allowed | Overage::Warning)
    }
}

/// The last observed state of one account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    /// Whether the operator enabled the account.
    pub enabled: bool,
    /// Last reported limits.
    pub limits: Vec<Limit>,
    /// Last reported overage state.
    pub overage: Overage,
    /// A provider rejection blocks the account until this time even without a reset header.
    pub blocked_until: u64,
}

/// Eligibility tier of an account, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// No live limit.
    Clear,
    /// A live early-warning limit.
    Warned,
    /// Subscription exhausted, serving through paid overage.
    Overage,
}

impl Tier {
    /// Order of preference; lower is better.
    pub fn rank(self) -> u8 {
        match self {
            Tier::Clear => 0,
            Tier::Warned => 1,
            Tier::Overage => 2,
        }
    }
}

/// Rotation policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    /// Paid extra usage is an operator choice and is off by default.
    pub allow_overage: bool,
}

/// Tier of `account` for `family` at `now`, or `None` when it cannot serve.
pub fn tier(policy: &Policy, now: u64, family: Family, account: &Account) -> Option<Tier> {
    if !account.enabled || now < account.blocked_until {
        return None;
    }
    let mut warned = false;
    for limit in account
        .limits
        .iter()
        .filter(|l| now < l.reset_at && l.claim.binds(family))
    {
        match limit.severity {
            Severity::Rejected => {
                return (policy.allow_overage && account.overage.serves()).then_some(Tier::Overage);
            }
            Severity::Warning => warned = true,
        }
    }
    Some(if warned { Tier::Warned } else { Tier::Clear })
}

/// The account for the next model call: the best tier, sticky on `current`,
/// otherwise round-robin from `current + 1` (or `cursor` without a current).
pub fn select(
    policy: &Policy,
    now: u64,
    family: Family,
    pool: &[Account],
    current: Option<usize>,
    cursor: usize,
) -> Option<usize> {
    let rank = |a: &Account| tier(policy, now, family, a).map(Tier::rank);
    let best = pool.iter().filter_map(rank).min()?;
    let hit = |i: usize| pool.get(i).and_then(rank) == Some(best);
    let n = pool.len();
    let (start, stay) = match current {
        Some(c) => (c.saturating_add(1), hit(c).then_some(c)),
        None => (cursor, None),
    };
    stay.or_else(|| (start..n).chain(0..start.min(n)).find(|&i| hit(i)))
}

/// Fold one response's report into the account, superseding limits that bind `family`.
pub fn observe(family: Family, reported: &[Limit], overage: Overage, account: &mut Account) {
    account.limits.retain(|l| !l.claim.binds(family));
    account.limits.extend_from_slice(reported);
    account.overage = overage;
}

/// Block the account until the later of `reset_at` and the next tick.
pub fn reject(now: u64, reset_at: u64, account: &mut Account) {
    account.blocked_until = account
        .blocked_until
        .max(reset_at.max(now.saturating_add(1)));
}

/// The accounts one provider attempt sends to; `rejects(i)` is `Some(reset)`
/// when the provider refuses account `i`. At most `pool.len()` entries, no repeats.
pub fn attempt<F>(
    policy: &Policy,
    now: u64,
    family: Family,
    pool: &mut [Account],
    current: Option<usize>,
    cursor: usize,
    mut rejects: F,
) -> Vec<usize>
where
    F: FnMut(usize) -> Option<u64>,
{
    let mut sent = Vec::new();
    let mut current = current;
    for _ in 0..pool.len() {
        let Some(i) = select(policy, now, family, pool, current, cursor) else {
            break;
        };
        sent.push(i);
        match rejects(i) {
            None => break,
            Some(reset_at) => {
                reject(now, reset_at, &mut pool[i]);
                current = Some(i);
            }
        }
    }
    sent
}

#[cfg(test)]
#[path = "rotation_tests.rs"]
mod tests;
