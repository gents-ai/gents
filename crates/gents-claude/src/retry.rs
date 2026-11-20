//! Claude Code's retry arithmetic: backoff and `retry-after` (`fF`, `r_`).
//!
//! Pure functions over numbers and strings of Claude Code 2.1.311; no clock,
//! no RNG, no I/O. Invariants: [`backoff_ms`] reproduces `fF` in f64 arithmetic
//! (`Math.round` is `floor(x + 0.5)`, `parseInt` is [`js_parse_int`]) and never
//! returns less than the computed backoff; every conversion to `u64` saturates.

/// Resends of one request after a 401 OAuth refresh (`hur`), bounding the Claude Messages replay.
pub const DEFAULT_MAX_RETRIES: u32 = 10;
/// OAuth refresh attempts after a 401 (`Sur`).
pub const OAUTH_REFRESH_ATTEMPTS: u32 = 2;
/// First backoff delay (`de`).
pub const BASE_DELAY_MS: u64 = 500;
/// Backoff cap (`fF` default `n`).
pub const MAX_DELAY_MS: u64 = 32_000;
/// Proportional jitter ratio (`res`).
pub const JITTER_RATIO: f64 = 0.25;

/// ECMAScript `WhiteSpace` or `LineTerminator`.
fn js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// JavaScript `parseInt(value, 10)`; `None` is NaN. Huge digit runs give infinity.
pub fn js_parse_int(value: &str) -> Option<f64> {
    let s = value.trim_start_matches(js_whitespace);
    let (negative, s) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let end = s
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    let magnitude: f64 = s[..end].parse().ok()?;
    Some(if negative { -magnitude } else { magnitude })
}

/// Delay before retry `attempt` (1-based; 0 is treated as `2^-1`): exponential
/// from `base_ms` capped at `cap_ms`, plus `random` (in `[0, 1)`) times
/// [`JITTER_RATIO`] of that, raised to a `retry-after` header's seconds.
pub fn backoff_ms(
    attempt: u32,
    retry_after: Option<&str>,
    cap_ms: u64,
    base_ms: u64,
    random: f64,
) -> u64 {
    let exponent = i32::try_from(attempt).unwrap_or(i32::MAX) - 1;
    let step = (cap_ms as f64).min(base_ms as f64 * 2f64.powi(exponent));
    let delay = (step + random * JITTER_RATIO * step + 0.5).floor();
    match retry_after.and_then(js_parse_int) {
        Some(seconds) => (seconds * 1000.0).max(delay) as u64,
        None => delay as u64,
    }
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod tests;
