//! Unified rate-limit headers (`anthropic-ratelimit-unified-*`) folded into
//! the account limit state the rotation reads.
//!
//! Pure parsing over a header lookup closure; no I/O, no clock. Invariants:
//! malformed numbers are ignored, never guessed; a limit without a reset
//! header stays live for its window's period from `now`, so it expires no
//! earlier than the window must have rolled; limits are unique per
//! (claim, severity) with the later reset kept. A response with no unified
//! header at all ([`HeaderObservation::is_empty`]), such as a 429 reading
//! "Usage credits are required for long context requests.", is not an account
//! exhaustion: the caller must not rotate on it.

use crate::rotation::{Claim, Limit, Overage, Severity};

const FIVE_HOUR_SECS: u64 = 18_000;
const SEVEN_DAY_SECS: u64 = 604_800;

const STATUS: &str = "anthropic-ratelimit-unified-status";
const RESET: &str = "anthropic-ratelimit-unified-reset";
const CLAIM: &str = "anthropic-ratelimit-unified-representative-claim";
const OVERAGE_STATUS: &str = "anthropic-ratelimit-unified-overage-status";
const OVERAGE_RESET: &str = "anthropic-ratelimit-unified-overage-reset";
const OVERAGE_REASON: &str = "anthropic-ratelimit-unified-overage-disabled-reason";
const OVERAGE_IN_USE: &str = "anthropic-ratelimit-unified-overage-in-use";

/// Header names of one window: (claim, status, reset, utilization, surpassed-threshold).
type WindowNames = (
    Claim,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);

macro_rules! window {
    ($claim:expr, $w:literal) => {
        (
            $claim,
            concat!("anthropic-ratelimit-unified-", $w, "-status"),
            concat!("anthropic-ratelimit-unified-", $w, "-reset"),
            concat!("anthropic-ratelimit-unified-", $w, "-utilization"),
            concat!("anthropic-ratelimit-unified-", $w, "-surpassed-threshold"),
        )
    };
}

const WINDOWS: [WindowNames; 4] = [
    window!(Claim::FiveHour, "5h"),
    window!(Claim::SevenDay, "7d"),
    window!(Claim::SevenDayOpus, "7d_opus"),
    window!(Claim::SevenDaySonnet, "7d_sonnet"),
];

/// A `status` header value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnifiedStatus {
    /// Serving normally.
    Allowed,
    /// Serving, past the early-warning threshold.
    AllowedWarning,
    /// Not serving.
    Rejected,
}

impl UnifiedStatus {
    /// Parse a status spelling; anything else is `None`.
    pub fn parse(value: &str) -> Option<UnifiedStatus> {
        match value {
            "allowed" => Some(UnifiedStatus::Allowed),
            "allowed_warning" => Some(UnifiedStatus::AllowedWarning),
            "rejected" => Some(UnifiedStatus::Rejected),
            _ => None,
        }
    }
}

/// What the headers report for one window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowObservation {
    /// The window.
    pub claim: Claim,
    /// `-{w}-status`.
    pub status: Option<UnifiedStatus>,
    /// `-{w}-reset`, epoch seconds.
    pub reset: Option<u64>,
    /// `-{w}-utilization`, a fraction.
    pub utilization: Option<f64>,
    /// `-{w}-surpassed-threshold`, a fraction.
    pub surpassed_threshold: Option<f64>,
}

/// Everything the unified headers of one response report.
#[derive(Clone, Debug, PartialEq)]
pub struct HeaderObservation {
    /// Overall `status`.
    pub status: Option<UnifiedStatus>,
    /// The binding window (`representative-claim`).
    pub claim: Option<Claim>,
    /// Overall `reset`, epoch seconds.
    pub reset: Option<u64>,
    /// Windows with at least one parsed header.
    pub windows: Vec<WindowObservation>,
    /// `overage-status`; `Absent` when not reported.
    pub overage: Overage,
    /// `overage-reset`, epoch seconds.
    pub overage_reset: Option<u64>,
    /// `overage-disabled-reason`.
    pub overage_disabled_reason: Option<String>,
    /// `overage-in-use`.
    pub overage_in_use: Option<bool>,
    present: bool,
}

fn period(claim: Claim) -> u64 {
    match claim {
        Claim::FiveHour | Claim::Unknown => FIVE_HOUR_SECS,
        _ => SEVEN_DAY_SECS,
    }
}

fn parse_secs(value: &str) -> Option<u64> {
    value.trim().parse().ok()
}

fn parse_fraction(value: &str) -> Option<f64> {
    value.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Read the unified headers of a response; `get` looks a lowercase name up.
/// `_now` is unused: defaults that need the clock apply in [`HeaderObservation::limits`].
pub fn observe_headers<'a, F>(get: F, _now: u64) -> HeaderObservation
where
    F: Fn(&str) -> Option<&'a str>,
{
    let mut present = false;
    let mut fetch = |name: &str| {
        let value = get(name);
        present |= value.is_some();
        value
    };
    let status = fetch(STATUS).and_then(UnifiedStatus::parse);
    let reset = fetch(RESET).and_then(parse_secs);
    let claim = fetch(CLAIM).map(Claim::parse);
    let overage = match fetch(OVERAGE_STATUS) {
        Some("allowed") => Overage::Allowed,
        Some("allowed_warning") => Overage::Warning,
        Some("rejected") => Overage::Rejected,
        _ => Overage::Absent,
    };
    let overage_reset = fetch(OVERAGE_RESET).and_then(parse_secs);
    let overage_disabled_reason = fetch(OVERAGE_REASON).map(str::to_owned);
    let overage_in_use = fetch(OVERAGE_IN_USE).and_then(|v| match v.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    });
    let mut windows = Vec::new();
    for (claim, status, reset, utilization, threshold) in WINDOWS {
        let w = WindowObservation {
            claim,
            status: fetch(status).and_then(UnifiedStatus::parse),
            reset: fetch(reset).and_then(parse_secs),
            utilization: fetch(utilization).and_then(parse_fraction),
            surpassed_threshold: fetch(threshold).and_then(parse_fraction),
        };
        if w.status.is_some()
            || w.reset.is_some()
            || w.utilization.is_some()
            || w.surpassed_threshold.is_some()
        {
            windows.push(w);
        }
    }
    HeaderObservation {
        status,
        claim,
        reset,
        windows,
        overage,
        overage_reset,
        overage_disabled_reason,
        overage_in_use,
        present,
    }
}

fn push_limit(out: &mut Vec<Limit>, limit: Limit) {
    match out
        .iter_mut()
        .find(|l| l.claim == limit.claim && l.severity == limit.severity)
    {
        Some(existing) => existing.reset_at = existing.reset_at.max(limit.reset_at),
        None => out.push(limit),
    }
}

impl HeaderObservation {
    /// True when the response carried no unified header at all; it says
    /// nothing about the account and the caller must not rotate on it.
    pub fn is_empty(&self) -> bool {
        !self.present
    }

    /// The limits to record. A missing reset uses the window's period from
    /// `now` (five hours for `5h` and unknown, seven days otherwise), so the
    /// limit stays live until the window must have rolled. Unique per
    /// (claim, severity), the later reset kept.
    pub fn limits(&self, now: u64) -> Vec<Limit> {
        let mut out = Vec::with_capacity(self.windows.len() + 1);
        let severity = |s| match s {
            UnifiedStatus::Rejected => Some(Severity::Rejected),
            UnifiedStatus::AllowedWarning => Some(Severity::Warning),
            UnifiedStatus::Allowed => None,
        };
        if let Some(sev) = self.status.and_then(severity) {
            let claim = self.claim.unwrap_or(Claim::Unknown);
            let reset_at = self
                .reset
                .unwrap_or_else(|| now.saturating_add(period(claim)));
            push_limit(
                &mut out,
                Limit {
                    claim,
                    severity: sev,
                    reset_at,
                },
            );
        }
        for w in &self.windows {
            let sev = match w.status {
                Some(UnifiedStatus::Rejected) => Some(Severity::Rejected),
                Some(UnifiedStatus::AllowedWarning) => Some(Severity::Warning),
                _ => w.surpassed_threshold.map(|_| Severity::Warning),
            };
            if let Some(severity) = sev {
                let reset_at = w
                    .reset
                    .unwrap_or_else(|| now.saturating_add(period(w.claim)));
                push_limit(
                    &mut out,
                    Limit {
                        claim: w.claim,
                        severity,
                        reset_at,
                    },
                );
            }
        }
        out
    }
}

#[cfg(test)]
#[path = "ratelimit_tests.rs"]
mod tests;
