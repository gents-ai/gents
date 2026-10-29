//! Claude Code's retry arithmetic: backoff, `retry-after`, the interactive
//! cliff and the context-overflow `max_tokens` adjustment (`fF`, `r_`, `mve`).
//!
//! Pure functions over numbers and strings of Claude Code 2.1.311; no clock,
//! no RNG, no I/O. Invariants: [`backoff_ms`] reproduces `fF` in f64 arithmetic
//! (`Math.round` is `floor(x + 0.5)`, `parseInt` is [`js_parse_int`]) and never
//! returns less than the computed backoff; every conversion to `u64` saturates.

/// Retries before a request fails (`hur`).
pub const DEFAULT_MAX_RETRIES: u32 = 10;
/// Upper clamp of `CLAUDE_CODE_MAX_RETRIES` (`VXe`).
pub const MAX_RETRIES_CLAMP: u32 = 15;
/// Retries of a watchdog (unattended) request (`yur`).
pub const WATCHDOG_MAX_RETRIES: u32 = 300;
/// Consecutive 529 responses before the model fallback applies (`hZ`).
pub const CONSECUTIVE_OVERLOADED_LIMIT: u32 = 3;
/// OAuth refresh attempts after a 401 (`Sur`).
pub const OAUTH_REFRESH_ATTEMPTS: u32 = 2;
/// Smallest `max_tokens` a context-overflow retry may use (`efn`).
pub const MAX_TOKENS_FLOOR: u64 = 3000;
/// Tokens left unused when shrinking `max_tokens` to fit the context (`kur`).
pub const CONTEXT_OVERFLOW_MARGIN: u64 = 1000;
/// Longest wait an interactive session accepts before failing (`xur`).
pub const INTERACTIVE_MAX_DELAY_MS: u64 = 60_000;
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

/// Whether a wait of `delay_ms` makes an interactive session fail instead of
/// waiting: it exceeds the larger of [`INTERACTIVE_MAX_DELAY_MS`] and the
/// overload cap grown by the jitter ratio.
pub fn exceeds_interactive_cliff(delay_ms: u64, overload_cap_ms: Option<u64>) -> bool {
    let grown = (overload_cap_ms.unwrap_or(0) as f64 * (1.0 + JITTER_RATIO)).ceil();
    delay_ms as f64 > (INTERACTIVE_MAX_DELAY_MS as f64).max(grown)
}

/// Why a context-overflow retry cannot shrink `max_tokens`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ContextOverflowError {
    /// The conversation leaves too little room for a reply.
    #[error(
        "the conversation leaves {available} tokens for a reply, below the minimum of {MAX_TOKENS_FLOOR}"
    )]
    BelowFloor {
        /// Tokens left for the reply.
        available: u64,
    },
    /// Shrinking `max_tokens` again would not make progress.
    #[error(
        "the context limit still leaves {available} tokens, no lower than the {previous} already requested"
    )]
    NoProgress {
        /// Tokens left for the reply.
        available: u64,
        /// The `max_tokens` already tried.
        previous: u64,
    },
}

const OVERFLOW_PREFIX: &str = "input length and `max_tokens` exceed context limit: ";

/// Parse a run of ASCII digits at the start of `s`, saturating at `u64::MAX`.
fn leading_number(s: &str) -> Option<(u64, &str)> {
    let end = s
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    let n = s[..end].bytes().fold(0u64, |n, b| {
        n.saturating_mul(10).saturating_add(u64::from(b - b'0'))
    });
    Some((n, &s[end..]))
}

/// `(input, max_tokens, limit)` of the first context-overflow message in `message`.
fn parse_overflow(message: &str) -> Option<(u64, u64, u64)> {
    message.match_indices(OVERFLOW_PREFIX).find_map(|(at, _)| {
        let rest = &message[at + OVERFLOW_PREFIX.len()..];
        let (input, rest) = leading_number(rest)?;
        let (max_tokens, rest) = leading_number(rest.strip_prefix(" + ")?)?;
        let (limit, _) = leading_number(rest.strip_prefix(" > ")?)?;
        Some((input, max_tokens, limit))
    })
}

/// The `max_tokens` override to retry with after an HTTP 400 context overflow:
/// `limit - input - margin`. `Ok(None)` when `message` is not an overflow.
pub fn adjust_for_context_overflow(
    message: &str,
    previous_override: Option<u64>,
) -> Result<Option<u64>, ContextOverflowError> {
    let Some((input, _, limit)) = parse_overflow(message) else {
        return Ok(None);
    };
    let available = limit
        .saturating_sub(input)
        .saturating_sub(CONTEXT_OVERFLOW_MARGIN);
    if available < MAX_TOKENS_FLOOR {
        return Err(ContextOverflowError::BelowFloor { available });
    }
    match previous_override {
        Some(previous) if available >= previous => Err(ContextOverflowError::NoProgress {
            available,
            previous,
        }),
        _ => Ok(Some(available)),
    }
}

/// Whether the SDK retries a response (`shouldRetry`, `chunk-wcddya4j.js`):
/// `x-should-retry` of `true` or `false` decides, else 408, 409, 429 and 5xx.
pub fn retryable_status(status: u16, should_retry_header: Option<&str>) -> bool {
    match should_retry_header {
        Some("true") => true,
        Some("false") => false,
        _ => matches!(status, 408 | 409 | 429 | 500..),
    }
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod tests;
