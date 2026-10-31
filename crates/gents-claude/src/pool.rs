//! Runtime state that applies the account rotation policy across concurrent requests.
//!
//! Holds the last observed limit state per account and the account each session
//! is on, both in lock-free maps. Invariants: every call is O(pool size) and
//! allocation-bounded by it, the session map never exceeds its cap, and the
//! policy decision is always made by [`rotation::select`], never re-derived here.

use std::fmt;
use std::hash::Hash;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use kovan_map::HopscotchMap;
use rapidhash::fast::RandomState;

use crate::ratelimit;
use crate::rotation::{self, Account, Family, Limit, Overage, Policy, Severity};

/// Default bound on remembered sessions.
pub const DEFAULT_SESSION_CAP: usize = 4096;

/// The account a request goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice<K> {
    /// The chosen account.
    pub key: K,
    /// Its position in the caller's account list.
    pub index: usize,
    /// Whether this differs from the account the session was on.
    pub switched: bool,
}

/// Observed limit state per account and session stickiness, shared across threads.
///
/// Concurrent writers to one account race last-write-wins, which is acceptable
/// because every response carries the server's current state. Session
/// stickiness is a prompt-cache locality optimization: clearing it at the cap
/// costs each live session one cache write on its next call, never correctness.
pub struct AccountPool<K>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
{
    limits: HopscotchMap<K, Arc<Account>, RandomState>,
    sessions: HopscotchMap<String, K, RandomState>,
    cursor: AtomicUsize,
    session_cap: usize,
}

fn fresh() -> Account {
    Account {
        enabled: true,
        limits: Vec::new(),
        overage: Overage::Absent,
        blocked_until: 0,
    }
}

impl<K> AccountPool<K>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
{
    /// An empty pool remembering at most `session_cap` sessions.
    pub fn new(session_cap: usize) -> Self {
        Self {
            limits: HopscotchMap::with_hasher(RandomState::new()),
            sessions: HopscotchMap::with_hasher(RandomState::new()),
            cursor: AtomicUsize::new(0),
            session_cap,
        }
    }

    fn snapshot(&self, key: &K) -> Account {
        self.limits
            .get(key)
            .map_or_else(fresh, |a| Account::clone(&a))
    }

    fn update<F>(&self, key: &K, apply: F)
    where
        F: FnOnce(&mut Account),
    {
        let mut account = self.snapshot(key);
        account.enabled = true;
        apply(&mut account);
        self.limits.insert(key.clone(), Arc::new(account));
    }

    /// Pick the account for the next call of `session` under `policy`, or `None` when every listed account is exhausted.
    pub fn choose(
        &self,
        accounts: &[K],
        session: &str,
        family: Family,
        now: u64,
        policy: &Policy,
    ) -> Option<Choice<K>> {
        let list: Vec<Account> = accounts
            .iter()
            .map(|k| {
                let mut a = self.snapshot(k);
                a.enabled = true;
                a
            })
            .collect();
        let recorded = self.sessions.get(session);
        let current = recorded
            .as_ref()
            .and_then(|r| accounts.iter().position(|k| k == r));
        let index = rotation::select(
            policy,
            now,
            family,
            &list,
            current,
            self.cursor.load(Ordering::Relaxed),
        )?;
        let key = accounts.get(index)?.clone();
        if current.is_none() {
            self.cursor
                .store(index.saturating_add(1), Ordering::Relaxed);
        }
        let switched = current != Some(index);
        if switched {
            if recorded.is_none() && self.sessions.len() >= self.session_cap {
                self.sessions.clear();
            }
            self.sessions.insert(session.to_owned(), key.clone());
        }
        Some(Choice {
            key,
            index,
            switched,
        })
    }

    /// Fold a response's reported limits for `family` into the account's state.
    pub fn observe(&self, key: &K, family: Family, reported: &[Limit], overage: Overage) {
        self.update(key, |a| rotation::observe(family, reported, overage, a));
    }

    /// Fold a response's unified rate-limit headers into the account; a headerless response changes nothing.
    pub fn observe_headers<'a, F>(&self, key: &K, family: Family, get: F, now: u64)
    where
        F: Fn(&str) -> Option<&'a str>,
    {
        let observed = ratelimit::observe_headers(get, now);
        if !observed.is_empty() {
            self.observe(key, family, &observed.limits(now), observed.overage);
        }
    }

    /// Block the account until `reset_at` (at least the next tick) after a provider rejection.
    pub fn reject(&self, key: &K, now: u64, reset_at: u64) {
        self.update(key, |a| rotation::reject(now, reset_at, a));
    }

    /// The soonest time any listed account can serve `family` again under `policy`; `now` when one already can.
    pub fn earliest_reset(
        &self,
        accounts: &[K],
        family: Family,
        now: u64,
        policy: &Policy,
    ) -> Option<u64> {
        accounts
            .iter()
            .map(|k| {
                let mut a = self.snapshot(k);
                a.enabled = true;
                if rotation::tier(policy, now, family, &a).is_some() {
                    return now;
                }
                a.limits
                    .iter()
                    .filter(|l| {
                        l.severity == Severity::Rejected
                            && now < l.reset_at
                            && l.claim.binds(family)
                    })
                    .map(|l| l.reset_at)
                    .fold(a.blocked_until, u64::max)
            })
            .min()
    }

    /// Drop an account's observed state.
    pub fn forget_account(&self, key: &K) {
        self.limits.remove(key);
    }

    /// Drop a session's stickiness.
    pub fn forget_session(&self, session: &str) {
        self.sessions.remove(session);
    }

    /// The last observed state of an account, if any response or rejection was recorded.
    pub fn account_state(&self, key: &K) -> Option<Arc<Account>> {
        self.limits.get(key)
    }
}

impl<K> Default for AccountPool<K>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new(DEFAULT_SESSION_CAP)
    }
}

impl<K> fmt::Debug for AccountPool<K>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountPool")
            .field("accounts", &self.limits.len())
            .field("sessions", &self.sessions.len())
            .finish()
    }
}

/// Compile-time proof that the pool is shareable across threads.
#[cfg(test)]
fn assert_send_sync<T>()
where
    T: Send + Sync,
{
}

#[cfg(test)]
#[path = "pool_tests.rs"]
mod tests;
