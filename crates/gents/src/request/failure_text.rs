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

/// One already-rendered failure text, redacted and cut to
/// [`FAILURE_TEXT_MAX_BYTES`].
pub fn bounded_text(text: &str) -> String {
    truncate(&redact(text), FAILURE_TEXT_MAX_BYTES)
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

/// Credentials and token-bearing URL parts removed from `text`: bearer and
/// basic credentials, authorization / API-key header or field values (also
/// inside escaped JSON), prefixed API keys, and URL query strings and
/// userinfo. Prose that only names a scheme or the word "authorization"
/// keeps its meaning.
pub fn redact(text: &str) -> String {
    static HEADER: OnceLock<Regex> = OnceLock::new();
    static AUTHORIZATION: OnceLock<Regex> = OnceLock::new();
    static SCHEME: OnceLock<Regex> = OnceLock::new();
    static PREFIXED_KEY: OnceLock<Regex> = OnceLock::new();
    static URL_SECRETS: OnceLock<Regex> = OnceLock::new();
    let header = HEADER.get_or_init(|| {
        Regex::new(
            r#"(?i)(\\?["']?\b(?:x-api-key|api[_-]?key|x-goog-api-key|access[_-]?token|refresh[_-]?token|client[_-]?secret|cookie|set-cookie)\b\\?["']?\s*[:=]\s*)(?:\\"(?:[^"\\\r\n]|\\[^"])*\\"|"[^"\r\n]*"|'[^'\r\n]*'|[^\s,;}\]\\]+(?:[ \t]+[^\s,;}\]\\]+)?)"#,
        )
        .expect("valid header redactor")
    });
    let authorization = AUTHORIZATION.get_or_init(|| {
        Regex::new(
            r#"(?i)(\\?["']?\b(?:proxy-)?authorization\b\\?["']?\s*[:=]\s*)(\\"(?:[^"\\\r\n]|\\[^"])*\\"|"[^"\r\n]*"|'[^'\r\n]*'|([A-Za-z][A-Za-z0-9_-]*)[ \t]+([A-Za-z0-9._~+/=-]+))"#,
        )
        .expect("valid authorization redactor")
    });
    let scheme = SCHEME.get_or_init(|| {
        Regex::new(r"(?i)\b(bearer|basic)\s+([A-Za-z0-9._~+/=-]+)").expect("valid scheme redactor")
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
    let text = authorization.replace_all(&text, |captures: &regex::Captures<'_>| {
        match (captures.get(3), captures.get(4)) {
            (Some(scheme), Some(credential))
                if !token_like(credential.as_str(), 8)
                    && !scheme_credential(scheme.as_str(), credential.as_str()) =>
            {
                captures[0].to_string()
            }
            _ => format!("{}{REDACTED}", &captures[1]),
        }
    });
    let text = scheme.replace_all(&text, |captures: &regex::Captures<'_>| {
        if scheme_credential(&captures[1], &captures[2]) {
            format!("{} {REDACTED}", &captures[1])
        } else {
            captures[0].to_string()
        }
    });
    prefixed_key.replace_all(&text, REDACTED).into_owned()
}

/// Whether `candidate`, the word after the `scheme` (`Bearer` or `Basic`),
/// is a credential rather than prose such as "Bearer authentication" or
/// "basic configuration". A Bearer word is one if [`token_like`] at 16 bytes
/// or, whatever its case, at least [`BEARER_PLAIN_MINIMUM`] bytes; a Basic
/// word if [`token_like`] at 8 bytes or [`basic_user_pass`]. Other schemes
/// are never credentials here.
fn scheme_credential(scheme: &str, candidate: &str) -> bool {
    if scheme.eq_ignore_ascii_case("bearer") {
        token_like(candidate, 16) || candidate.len() >= BEARER_PLAIN_MINIMUM
    } else if scheme.eq_ignore_ascii_case("basic") {
        token_like(candidate, 8) || basic_user_pass(candidate)
    } else {
        false
    }
}

/// Bytes from which a Bearer word of any case is a credential: random
/// lowercase tokens are at least this long, while English words that follow
/// "Bearer" in prose ("token", "auth", "authentication") are well short.
const BEARER_PLAIN_MINIMUM: usize = 20;

/// Whether `candidate` is RFC 7617 Basic credentials: standard base64, padded
/// or not, of `user:pass` text. Prose words that happen to be valid base64
/// decode to bytes that are not control-free text containing `:`.
fn basic_user_pass(candidate: &str) -> bool {
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    use base64::Engine as _;
    const STANDARD_ANY_PADDING: GeneralPurpose = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    STANDARD_ANY_PADDING
        .decode(candidate)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .is_some_and(|text| text.contains(':') && !text.chars().any(char::is_control))
}

/// A credential-shaped word: long enough, and not a plain lowercase or
/// capitalised word ("basic configuration", "Bearer authentication").
fn token_like(candidate: &str, minimum: usize) -> bool {
    let rest = candidate.chars().skip(1);
    candidate.len() >= minimum
        && (candidate
            .chars()
            .any(|character| character.is_ascii_digit() || "+/=._~-".contains(character))
            || rest.clone().any(|character| character.is_ascii_uppercase())
                && candidate
                    .chars()
                    .any(|character| character.is_ascii_lowercase()))
}

#[cfg(test)]
#[path = "failure_text_tests.rs"]
mod tests;
