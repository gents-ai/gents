//! Bounded, redacted renderings of error chains for durable failure reasons
//! and shipped logs.
//!
//! A request failure's cause chain can carry a whole provider response body
//! (`InvalidStatusCodeWithMessage(status, body)`), which may echo user text,
//! URLs or credentials. `failure_reason` replicates and logs ship to Loki, so
//! both get the chain with every cause redacted and capped, and the whole
//! rendering capped.

use std::fmt;
use std::sync::OnceLock;

use regex::Regex;

/// Bytes kept of each cause in the chain.
pub const FAILURE_CAUSE_MAX_BYTES: usize = 1024;
/// Bytes kept of the whole rendered chain, marker included.
pub const FAILURE_TEXT_MAX_BYTES: usize = 4096;
/// Appended where text was cut.
pub const TRUNCATION_MARKER: &str = "…(truncated)";
const REDACTED: &str = "[REDACTED]";

/// The error and each of its causes, as anyhow's `{:#}` joins them, each
/// cause redacted and cut to [`FAILURE_CAUSE_MAX_BYTES`], the whole cut to
/// [`FAILURE_TEXT_MAX_BYTES`].
pub fn bounded_error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut causes = Vec::new();
    let mut cause = Some(error);
    while let Some(current) = cause {
        causes.push(truncate(
            &redact(&current.to_string()),
            FAILURE_CAUSE_MAX_BYTES,
        ));
        cause = current.source();
    }
    truncate(&causes.join(": "), FAILURE_TEXT_MAX_BYTES)
}

/// [`bounded_error_chain`] as a `Display` value for tracing fields.
pub struct BoundedErrorChain<'a>(pub &'a (dyn std::error::Error + 'static));

impl fmt::Display for BoundedErrorChain<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&bounded_error_chain(self.0))
    }
}

/// `text` cut to at most `max_bytes` on a character boundary, ending in
/// [`TRUNCATION_MARKER`] when cut.
pub fn truncate(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes.saturating_sub(TRUNCATION_MARKER.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATION_MARKER}", &text[..end])
}

/// Credentials and token-bearing URL parts removed from `text`: bearer
/// tokens, authorization / API-key header or field values, prefixed API
/// keys, and URL query strings and userinfo.
pub fn redact(text: &str) -> String {
    static HEADER: OnceLock<Regex> = OnceLock::new();
    static BEARER: OnceLock<Regex> = OnceLock::new();
    static PREFIXED_KEY: OnceLock<Regex> = OnceLock::new();
    static URL_SECRETS: OnceLock<Regex> = OnceLock::new();
    let header = HEADER.get_or_init(|| {
        Regex::new(
            r#"(?i)(["']?\b(?:authorization|proxy-authorization|x-api-key|api[_-]?key|x-goog-api-key|access[_-]?token|refresh[_-]?token|client[_-]?secret|cookie|set-cookie)\b["']?\s*[:=]\s*)(?:"[^"\r\n]*"|'[^'\r\n]*'|[^\s,;}\]]+(?:[ \t]+[^\s,;}\]]+)?)"#,
        )
        .expect("valid header redactor")
    });
    let bearer = BEARER.get_or_init(|| {
        Regex::new(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{4,}").expect("valid bearer redactor")
    });
    let prefixed_key = PREFIXED_KEY.get_or_init(|| {
        Regex::new(r"\b(?:sk-ant-|sk-proj-|sk-|xai-|ghp_|github_pat_|AIza)[A-Za-z0-9_\-]{8,}")
            .expect("valid key redactor")
    });
    let url_secrets = URL_SECRETS.get_or_init(|| {
        Regex::new(r#"(?i)\b(https?|wss?)://(?:[^\s/@"'<>]*@)?([^\s/?#"'<>]*)([^\s?#"'<>]*)(?:\?[^\s#"'<>]*)?"#)
            .expect("valid url redactor")
    });
    let text = url_secrets.replace_all(text, "$1://$2$3");
    let text = header.replace_all(&text, format!("${{1}}{REDACTED}"));
    let text = bearer.replace_all(&text, format!("${{1}} {REDACTED}"));
    prefixed_key.replace_all(&text, REDACTED).into_owned()
}

#[cfg(test)]
#[path = "failure_text_tests.rs"]
mod tests;
