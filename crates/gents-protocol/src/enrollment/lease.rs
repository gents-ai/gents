use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use super::canonical::parse_canonical_timestamp;

/// An authorization lease inside this window is due for renewal. Renewal is a
/// superseding re-enrollment of the same member: a fresh offer, a request the
/// member signs, and an approval that takes the member's next authorization
/// sequence. Every protocol-2 node, RC0 included, accepts that generation.
pub const ENROLLMENT_RENEWAL_WINDOW_SECONDS: u64 = 7 * 24 * 60 * 60;

/// How a signed authorization lease reads at one observation time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationLeaseState {
    /// More than the renewal window remains.
    Current,
    /// Still fresh, but inside the renewal window.
    RenewalDue,
    /// At or past its expiry; every gate refuses it.
    Expired,
    /// Not a canonical UTC timestamp; every gate refuses it.
    Invalid,
}

/// Observation-time reading of a lease, consistent with
/// [`super::authorization_lease_is_fresh_at`]: a lease is fresh strictly
/// before its expiry, and an invalid expiry fails closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorizationLease {
    pub state: AuthorizationLeaseState,
    /// Time left before expiry; zero once expired or invalid.
    pub remaining: Duration,
}

impl AuthorizationLease {
    pub fn at(expires_at: &str, now: DateTime<Utc>) -> Self {
        let Ok(expires_at) = parse_canonical_timestamp("authorization_expires_at", expires_at)
        else {
            return Self {
                state: AuthorizationLeaseState::Invalid,
                remaining: Duration::zero(),
            };
        };
        let remaining = expires_at.with_timezone(&Utc) - now;
        let window = Duration::seconds(ENROLLMENT_RENEWAL_WINDOW_SECONDS as i64);
        let state = if remaining <= Duration::zero() {
            AuthorizationLeaseState::Expired
        } else if remaining <= window {
            AuthorizationLeaseState::RenewalDue
        } else {
            AuthorizationLeaseState::Current
        };
        Self {
            state,
            remaining: remaining.max(Duration::zero()),
        }
    }

    /// Whole days left, rounded down; zero inside the last day.
    pub fn remaining_days(&self) -> i64 {
        self.remaining.num_days()
    }

    pub fn needs_renewal(&self) -> bool {
        self.state != AuthorizationLeaseState::Current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrollment::authorization_lease_is_fresh_at;

    fn at(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }

    #[test]
    fn a_thirty_day_lease_is_current_until_its_last_week() {
        let expires = "2026-11-29T19:07:47Z";
        let issued = at("2026-10-30T19:07:47Z");
        let lease = AuthorizationLease::at(expires, issued);
        assert_eq!(lease.state, AuthorizationLeaseState::Current);
        assert_eq!(lease.remaining_days(), 30);

        let just_outside = at("2026-11-22T19:07:46Z");
        assert_eq!(
            AuthorizationLease::at(expires, just_outside).state,
            AuthorizationLeaseState::Current
        );
        let window_opens = at("2026-11-22T19:07:47Z");
        let lease = AuthorizationLease::at(expires, window_opens);
        assert_eq!(lease.state, AuthorizationLeaseState::RenewalDue);
        assert_eq!(lease.remaining_days(), 7);
        assert!(lease.needs_renewal());
    }

    #[test]
    fn the_expiry_instant_is_expired_and_matches_the_freshness_gate() {
        let expires = "2026-11-29T19:07:47Z";
        let last_second = at("2026-11-29T19:07:46Z");
        let lease = AuthorizationLease::at(expires, last_second);
        assert_eq!(lease.state, AuthorizationLeaseState::RenewalDue);
        assert_eq!(lease.remaining_days(), 0);
        assert!(authorization_lease_is_fresh_at(expires, last_second));

        for now in [at("2026-11-29T19:07:47Z"), at("2026-12-25T00:00:00Z")] {
            let lease = AuthorizationLease::at(expires, now);
            assert_eq!(lease.state, AuthorizationLeaseState::Expired);
            assert_eq!(lease.remaining, Duration::zero());
            assert!(!authorization_lease_is_fresh_at(expires, now));
        }
    }

    #[test]
    fn a_non_canonical_expiry_fails_closed() {
        let now = at("2026-11-01T00:00:00Z");
        for invalid in [
            "",
            "soon",
            "2026-11-29T19:07:47+00:00",
            "2026-11-29T19:07:47.000Z",
        ] {
            let lease = AuthorizationLease::at(invalid, now);
            assert_eq!(lease.state, AuthorizationLeaseState::Invalid, "{invalid:?}");
            assert!(lease.needs_renewal());
            assert!(!authorization_lease_is_fresh_at(invalid, now));
        }
    }

    #[test]
    fn reading_a_lease_is_idempotent_and_a_later_generation_clears_renewal() {
        let now = at("2026-11-25T00:00:00Z");
        let expiring = "2026-11-29T19:07:47Z";
        assert_eq!(
            AuthorizationLease::at(expiring, now),
            AuthorizationLease::at(expiring, now)
        );
        assert!(AuthorizationLease::at(expiring, now).needs_renewal());
        // The superseding generation approved now with the default lease.
        let renewed = (now
            + Duration::seconds(
                crate::enrollment::DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS as i64,
            ))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let lease = AuthorizationLease::at(&renewed, now);
        assert_eq!(lease.state, AuthorizationLeaseState::Current);
        assert_eq!(lease.remaining_days(), 30);
    }
}
