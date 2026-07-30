//! Env-overridable reconciler intervals.
//!
//! Production defaults match the historic constants; tests and live e2e runs can
//! compress convergence by setting the env vars. Lean-neutral: no transition,
//! invariant, or provider-input depends on these wall-clock values.

use std::time::Duration;

pub const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(30);
pub const DEFAULT_SWEEP: Duration = Duration::from_secs(30);
pub const DEFAULT_STALE_MULTIPLE: u32 = 3;

/// Parse a millisecond override. A non-positive (`0`) or unparsable value is
/// rejected (returns `None`, so the default applies): `Duration::ZERO` would
/// panic `tokio::time::interval`, and a zero interval is never a meaningful
/// cadence — an operator typo must not crash the reconciler daemons.
fn parse_ms(raw: &str) -> Option<Duration> {
    raw.trim()
        .parse::<u64>()
        .ok()
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
}

fn env_ms(key: &str) -> Option<Duration> {
    std::env::var(key).ok().and_then(|value| parse_ms(&value))
}

pub fn heartbeat_interval() -> Duration {
    env_ms("GENTS_REGISTRY_HEARTBEAT_MS").unwrap_or(DEFAULT_HEARTBEAT)
}

pub fn sweep_interval() -> Duration {
    env_ms("GENTS_PAIRING_SWEEP_MS").unwrap_or(DEFAULT_SWEEP)
}

pub fn endpoint_interval() -> Duration {
    env_ms("GENTS_ENDPOINT_HEARTBEAT_MS").unwrap_or_else(heartbeat_interval)
}

pub fn stale_after() -> Duration {
    env_ms("GENTS_REGISTRY_STALE_MS")
        .unwrap_or_else(|| heartbeat_interval() * DEFAULT_STALE_MULTIPLE)
}

pub const DEFAULT_RECIPROCAL_STALE: Duration = Duration::from_secs(24 * 60 * 60);

pub fn reciprocal_stale_after() -> Duration {
    env_ms("GENTS_RECIPROCAL_STALE_MS").unwrap_or(DEFAULT_RECIPROCAL_STALE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    const KEYS: &[&str] = &[
        "GENTS_REGISTRY_HEARTBEAT_MS",
        "GENTS_PAIRING_SWEEP_MS",
        "GENTS_ENDPOINT_HEARTBEAT_MS",
        "GENTS_REGISTRY_STALE_MS",
        "GENTS_RECIPROCAL_STALE_MS",
    ];

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvGuard {
        _guard: MutexGuard<'static, ()>,
        previous: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn clear() -> Self {
            let guard = env_lock().lock().expect("env lock poisoned");
            let previous = KEYS
                .iter()
                .map(|&key| {
                    let value = std::env::var(key).ok();
                    std::env::remove_var(key);
                    (key, value)
                })
                .collect();
            Self {
                _guard: guard,
                previous,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.previous.drain(..) {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[test]
    fn defaults_match_historic_values() {
        let _env = EnvGuard::clear();
        assert_eq!(heartbeat_interval(), Duration::from_secs(30));
        assert_eq!(sweep_interval(), Duration::from_secs(30));
        assert_eq!(endpoint_interval(), Duration::from_secs(30));
        assert_eq!(stale_after(), Duration::from_secs(90));
        assert_eq!(reciprocal_stale_after(), Duration::from_secs(24 * 60 * 60));
    }

    #[test]
    fn env_overrides_are_in_milliseconds() {
        let _env = EnvGuard::clear();
        std::env::set_var("GENTS_REGISTRY_HEARTBEAT_MS", "1250");
        std::env::set_var("GENTS_PAIRING_SWEEP_MS", "500");
        std::env::set_var("GENTS_ENDPOINT_HEARTBEAT_MS", "750");
        std::env::set_var("GENTS_REGISTRY_STALE_MS", "2500");
        std::env::set_var("GENTS_RECIPROCAL_STALE_MS", "4500");

        assert_eq!(heartbeat_interval(), Duration::from_millis(1250));
        assert_eq!(sweep_interval(), Duration::from_millis(500));
        assert_eq!(endpoint_interval(), Duration::from_millis(750));
        assert_eq!(stale_after(), Duration::from_millis(2500));
        assert_eq!(reciprocal_stale_after(), Duration::from_millis(4500));
    }

    #[test]
    fn endpoint_and_stale_fallback_follow_heartbeat() {
        let _env = EnvGuard::clear();
        std::env::set_var("GENTS_REGISTRY_HEARTBEAT_MS", "2000");

        assert_eq!(endpoint_interval(), Duration::from_secs(2));
        assert_eq!(stale_after(), Duration::from_secs(6));
    }

    #[test]
    fn zero_or_garbage_override_falls_back_to_default_not_panic() {
        // `parse_ms` is the pure parser; `0` and unparsable values are rejected
        // so the default applies (a 0ms interval would panic tokio's timer).
        assert_eq!(parse_ms("0"), None);
        assert_eq!(parse_ms("  0 "), None);
        assert_eq!(parse_ms(""), None);
        assert_eq!(parse_ms("nope"), None);
        assert_eq!(parse_ms("-5"), None);
        assert_eq!(parse_ms("250"), Some(Duration::from_millis(250)));

        // End-to-end: a `0` env override must not yield a zero interval.
        let _env = EnvGuard::clear();
        std::env::set_var("GENTS_PAIRING_SWEEP_MS", "0");
        assert_eq!(sweep_interval(), DEFAULT_SWEEP);
    }
}
